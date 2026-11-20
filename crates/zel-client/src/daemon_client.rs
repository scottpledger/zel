// Copyright 2026 The Bazel Authors. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Client side of the persistent server: output-base locking, server discovery / (re)start /
//! kill (`blaze.cc`), and the gRPC `CommandServer` calls (`Run`, `Ping`, `Cancel`) over HTTP/2.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zel_options::StartupOptions;
use zel_proto::command_server::{
    CancelRequest, PingRequest, PingResponse, RunRequest, RunResponse, ServerInfo,
    TerminalSizeRequest, TerminalSizeResponse,
};
use zel_remote::grpc::{CallOptions, Channel, GrpcStatus};
use zel_remote::transport::TcpConnector;
use zel_server::daemon::SERVICE;
use zel_server::sys;
use zel_util::ExitCode;

/// Everything the client needs to find / start the server for one workspace.
pub struct ServerSpec<'a> {
    pub exe: PathBuf,
    pub startup: &'a StartupOptions,
    pub workspace_dir: &'a str,
    pub output_base: &'a str,
    pub install_base: &'a str,
    pub output_user_root: &'a str,
    pub install_md5: &'a str,
}

fn startup_raw(startup: &StartupOptions, name: &str) -> Option<String> {
    startup
        .parsed_options
        .get(name)
        .and_then(|d| d.raw_values.last().cloned())
}

fn startup_secs(startup: &StartupOptions, name: &str, default: u64) -> u64 {
    startup_raw(startup, name)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

impl ServerSpec<'_> {
    pub fn server_dir(&self) -> PathBuf {
        Path::new(self.output_base).join("server")
    }

    pub fn connect_timeout(&self) -> Duration {
        Duration::from_secs(startup_secs(self.startup, "connect_timeout_secs", 30))
    }

    pub fn local_startup_timeout(&self) -> Duration {
        Duration::from_secs(startup_secs(
            self.startup,
            "local_startup_timeout_secs",
            120,
        ))
    }

    pub fn jvm_out(&self) -> PathBuf {
        if let Some(ref p) = self.startup.server_jvm_out {
            if !p.is_empty() {
                return PathBuf::from(p.path_string());
            }
        }
        match startup_raw(self.startup, "server_jvm_out") {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            _ => self.server_dir().join("jvm.out"),
        }
    }

    /// The canonical server argument list (`GetServerExeArgs`), compared on every command to
    /// decide whether a running server must be restarted.
    pub fn server_args(&self) -> Vec<String> {
        let mut a = vec![
            format!("--install_base={}", self.install_base),
            format!("--install_md5={}", self.install_md5),
            format!("--output_base={}", self.output_base),
            format!("--output_user_root={}", self.output_user_root),
            format!("--workspace_directory={}", self.workspace_dir),
            format!("--max_idle_secs={}", self.startup.max_idle_secs),
            format!(
                "--connect_timeout_secs={}",
                startup_secs(self.startup, "connect_timeout_secs", 30)
            ),
            "--product_name=Bazel".to_string(),
        ];
        // Every other server-relevant startup option, rendered `--name=value` like the real
        // client does.  Client-only options never reach the server.
        const CLIENT_ONLY: &[&str] = &[
            "bazelrc",
            "ignore_all_rc_files",
            "system_rc",
            "workspace_rc",
            "home_rc",
            "batch",
            "block_for_lock",
            "quiet",
            "output_base",
            "output_user_root",
            "install_base",
            "max_idle_secs",
            "connect_timeout_secs",
            "invocation_policy",
            "macos_qos_class",
            "server_javabase",
            "unlimit_coredumps",
        ];
        for (name, desc) in &self.startup.parsed_options {
            if CLIENT_ONLY.contains(&name.as_str()) {
                continue;
            }
            for v in &desc.raw_values {
                a.push(format!("--{name}={v}"));
            }
        }
        a
    }
}

fn is_volatile(arg: &str) -> bool {
    const V: &[&str] = &[
        "--option_sources=",
        "--max_idle_secs=",
        "--connect_timeout_secs=",
        "--local_startup_timeout_secs=",
        "--client_debug=",
        "--preemptible=",
    ];
    let key = match arg.find('=') {
        Some(i) => &arg[..=i],
        None => arg,
    };
    V.contains(&key)
}

/// `AreStartupOptionsDifferent`: multiset difference ignoring volatile args,
/// plus vector length comparison (`running_server_args.size() != requested_args.size()`).
pub fn startup_options_diff(old: &[String], new: &[String]) -> (Vec<String>, Vec<String>) {
    let mut old_left: Vec<String> = old.iter().filter(|a| !is_volatile(a)).cloned().collect();
    let mut only_new = Vec::new();
    for a in new.iter().filter(|a| !is_volatile(a)) {
        if let Some(i) = old_left.iter().position(|o| o == a) {
            old_left.remove(i);
        } else {
            only_new.push(a.clone());
        }
    }
    (old_left, only_new)
}

pub fn are_startup_options_different(
    old: &[String],
    new: &[String],
) -> Option<(Vec<String>, Vec<String>)> {
    let (only_old, only_new) = startup_options_diff(old, new);
    if old.len() != new.len() || !only_old.is_empty() || !only_new.is_empty() {
        Some((only_old, only_new))
    } else {
        None
    }
}

// ------------------------------------------------------------------------------------ locking

const LOCK_METADATA_MAX_LEN: usize = 4096;
/// On Windows, `LockFileEx` byte-range locks are mandatory, so locking byte `LOCK_METADATA_MAX_LEN`
/// leaves `[0..LOCK_METADATA_MAX_LEN)` unlocked for owner metadata reads and updates.
#[cfg(windows)]
const WINDOWS_LOCK_OFFSET: u32 = LOCK_METADATA_MAX_LEN as u32;

/// A held file lock handle (`AcquireLock`); released on drop.
pub struct OutputBaseLock {
    _file: File,
}

#[cfg(windows)]
impl Drop for OutputBaseLock {
    fn drop(&mut self) {
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        struct Overlapped {
            internal: usize,
            internal_high: usize,
            offset: u32,
            offset_high: u32,
            event: *mut core::ffi::c_void,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn UnlockFileEx(
                h_file: *mut core::ffi::c_void,
                dw_reserved: u32,
                n_low: u32,
                n_high: u32,
                lp_overlapped: *mut Overlapped,
            ) -> i32;
        }
        let mut ov = Overlapped {
            internal: 0,
            internal_high: 0,
            offset: WINDOWS_LOCK_OFFSET,
            offset_high: 0,
            event: std::ptr::null_mut(),
        };
        unsafe {
            let _ = UnlockFileEx(self._file.as_raw_handle(), 0, 1, 0, &mut ov);
        }
    }
}

pub struct ClientLocks {
    pub install_lock: Option<OutputBaseLock>,
    pub output_lock: OutputBaseLock,
}

#[cfg(unix)]
extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    fn isatty(fd: i32) -> i32;
    fn ttyname(fd: i32) -> *const i8;
}

#[cfg(all(unix, not(target_os = "macos")))]
#[repr(C)]
struct PosixFlock {
    l_type: i16,
    l_whence: i16,
    l_start: i64,
    l_len: i64,
    l_pid: i32,
}

#[cfg(all(unix, not(target_os = "macos")))]
fn try_fcntl_lock(fd: i32, exclusive: bool) -> Result<bool, io::Error> {
    #[cfg(target_os = "linux")]
    const F_OFD_SETLK: i32 = 37;
    const F_SETLK: i32 = 6;
    const F_RDLCK: i16 = 0;
    const F_WRLCK: i16 = 1;
    const SEEK_SET: i16 = 0;

    let mut fl = PosixFlock {
        l_type: if exclusive { F_WRLCK } else { F_RDLCK },
        l_whence: SEEK_SET,
        l_start: 0,
        l_len: 1,
        l_pid: 0,
    };
    #[cfg(target_os = "linux")]
    {
        let rc = unsafe { fcntl(fd, F_OFD_SETLK, &mut fl) };
        if rc == 0 {
            return Ok(true);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(c) if c == 13 || c == 11 => return Ok(false), // EACCES / EAGAIN
            Some(22) => {}                                     // EINVAL -> fallback F_SETLK
            _ => return Err(err),
        }
    }
    let rc = unsafe { fcntl(fd, F_SETLK, &mut fl) };
    if rc == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(c) if c == 13 || c == 11 || c == 35 => Ok(false),
        _ => Err(err),
    }
}

#[cfg(unix)]
fn try_lock_fd(file: &File, exclusive: bool) -> Result<bool, io::Error> {
    use std::os::unix::io::AsRawFd;
    const LOCK_SH: i32 = 1;
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    let fd = file.as_raw_fd();
    // On macOS/BSD, fcntl(F_SETLK) (process-scoped) and flock(2) (open-file-description-scoped)
    // share the same vnode lock table with distinct owner IDs, so holding F_WRLCK causes a
    // subsequent flock(LOCK_EX | LOCK_NB) on the same fd to fail with EAGAIN indefinitely.
    #[cfg(not(target_os = "macos"))]
    if !try_fcntl_lock(fd, exclusive)? {
        return Ok(false);
    }
    let op = if exclusive {
        LOCK_EX | LOCK_NB
    } else {
        LOCK_SH | LOCK_NB
    };
    let rc = unsafe { flock(fd, op) };
    if rc == 0 {
        Ok(true)
    } else {
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(c) if c == 11 || c == 35 || c == 13 => Ok(false), // EAGAIN / EWOULDBLOCK / EACCES
            _ => Err(err),
        }
    }
}

#[cfg(windows)]
fn try_lock_fd(file: &File, exclusive: bool) -> Result<bool, io::Error> {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: *mut core::ffi::c_void,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LockFileEx(
            h_file: *mut core::ffi::c_void,
            dw_flags: u32,
            dw_reserved: u32,
            n_low: u32,
            n_high: u32,
            lp_overlapped: *mut Overlapped,
        ) -> i32;
    }
    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 1;
    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 2;
    let flags = LOCKFILE_FAIL_IMMEDIATELY
        | if exclusive {
            LOCKFILE_EXCLUSIVE_LOCK
        } else {
            0
        };
    let mut ov = Overlapped {
        internal: 0,
        internal_high: 0,
        offset: WINDOWS_LOCK_OFFSET,
        offset_high: 0,
        event: std::ptr::null_mut(),
    };
    let ok = unsafe { LockFileEx(file.as_raw_handle(), flags, 0, 1, 0, &mut ov) };
    if ok != 0 {
        Ok(true)
    } else {
        let err = io::Error::last_os_error();
        // ERROR_LOCK_VIOLATION = 33
        if err.raw_os_error() == Some(33) {
            Ok(false)
        } else {
            Err(err)
        }
    }
}

#[cfg(all(not(unix), not(windows)))]
fn try_lock_fd(_file: &File, _exclusive: bool) -> Result<bool, io::Error> {
    Ok(true)
}

#[cfg(unix)]
fn current_tty() -> Option<String> {
    unsafe {
        if isatty(0) != 1 {
            return None;
        }
        let p = ttyname(0);
        if p.is_null() {
            return None;
        }
        let s = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

#[cfg(not(unix))]
fn current_tty() -> Option<String> {
    None
}

fn read_lock_owner(file: &mut File) -> String {
    let mut s = String::new();
    let _ = file.seek(SeekFrom::Start(0));
    let _ = file
        .take(LOCK_METADATA_MAX_LEN as u64)
        .read_to_string(&mut s);
    s
}

/// `AcquireLock` (`blaze_util_posix.cc` / `blaze_util_windows.cc`): open (with `O_CLOEXEC` unless
/// `batch_mode` on Unix, or `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE` on Windows),
/// lock (`fcntl` OFD with POSIX fallback + `flock` on Unix, `LockFileEx` at byte 4096 on Windows),
/// verify `st_nlink > 0`, write owner metadata (`[0..len)`, `len <= 4096`) for exclusive locks,
/// poll every 500 ms. Returns `(lock, wait_ms)`.
pub fn acquire_lock(
    name: &str,
    path: &Path,
    exclusive: bool,
    batch_mode: bool,
    block: bool,
    err: &mut dyn Write,
) -> Result<(OutputBaseLock, u64), i32> {
    #[cfg(unix)]
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    #[cfg(all(unix, target_os = "macos"))]
    const O_CLOEXEC: i32 = 0x0100_0000;
    #[cfg(all(unix, not(target_os = "macos")))]
    const O_CLOEXEC: i32 = 0x80000;
    #[cfg(unix)]
    const F_SETFD: i32 = 2;
    let start = Instant::now();
    let mut multiple_attempts = false;
    let mut owner: Option<String> = None;
    loop {
        let mut oo = OpenOptions::new();
        oo.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            oo.mode(0o644);
            if !batch_mode {
                oo.custom_flags(O_CLOEXEC);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
            oo.share_mode(0x0000_0001 | 0x0000_0002 | 0x0000_0004);
        }
        let mut file = match oo.open(path) {
            Ok(f) => f,
            Err(e) => {
                let _ = writeln!(
                    err,
                    "FATAL: open failed for {name} lock: {}",
                    errno_string(&e)
                );
                return Err(ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code());
            }
        };
        #[cfg(unix)]
        if batch_mode {
            use std::os::unix::io::AsRawFd;
            unsafe {
                let _ = fcntl(file.as_raw_fd(), F_SETFD, 0);
            }
        }
        #[cfg(not(unix))]
        let _ = batch_mode;
        match try_lock_fd(&file, exclusive) {
            Ok(true) => {
                #[cfg(unix)]
                let nlink = match file.metadata() {
                    Ok(m) => m.nlink(),
                    Err(e) => {
                        let _ = writeln!(
                            err,
                            "FATAL: fstat failed for {name} lock: {}",
                            errno_string(&e)
                        );
                        return Err(ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code());
                    }
                };
                #[cfg(not(unix))]
                let nlink = 1u64;
                if nlink > 0 {
                    if exclusive {
                        let cwd = std::env::current_dir()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default();
                        let mut payload =
                            format!("pid={}\nowner=client\ncwd={cwd}\n", std::process::id());
                        if let Some(tty) = current_tty() {
                            use std::fmt::Write as _;
                            let _ = writeln!(payload, "tty={tty}");
                        }
                        if payload.len() > LOCK_METADATA_MAX_LEN {
                            let mut cut = LOCK_METADATA_MAX_LEN;
                            while !payload.is_char_boundary(cut) {
                                cut -= 1;
                            }
                            payload.truncate(cut);
                        }
                        let _ = file.seek(SeekFrom::Start(0));
                        let _ = file.write_all(payload.as_bytes());
                        let _ = file.set_len(payload.len() as u64);
                        let _ = file.flush();
                    }
                    let wait_ms = if multiple_attempts {
                        start.elapsed().as_millis() as u64
                    } else {
                        0
                    };
                    return Ok((OutputBaseLock { _file: file }, wait_ms));
                }
            }
            Ok(false) => {}
            Err(e) => {
                let syscall = if cfg!(windows) { "LockFileEx" } else { "fcntl" };
                let _ = writeln!(
                    err,
                    "FATAL: {syscall} failed for {name} lock: {}",
                    errno_string(&e)
                );
                return Err(ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code());
            }
        }
        let new_owner = read_lock_owner(&mut file);
        let changed = owner.as_deref() != Some(new_owner.as_str());
        if changed {
            let _ = writeln!(err, "Another command holds the {name} lock: \n{new_owner}");
            owner = Some(new_owner);
        }
        if !block {
            let _ = writeln!(
                err,
                "FATAL: Exiting because the {name} lock is held and --noblock_for_lock was given."
            );
            return Err(ExitCode::LOCK_HELD_NOBLOCK_FOR_LOCK.numeric_exit_code());
        }
        if changed {
            let _ = writeln!(err, "Waiting for it to complete...");
            let _ = err.flush();
        }
        multiple_attempts = true;
        drop(file);
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub fn acquire_output_base_lock(
    output_base: &Path,
    batch_mode: bool,
    block: bool,
    err: &mut dyn Write,
) -> Result<(OutputBaseLock, u64), i32> {
    let _ = fs::create_dir_all(output_base);
    acquire_lock(
        "output base",
        &output_base.join("lock"),
        true,
        batch_mode,
        block,
        err,
    )
}

/// `lock_install_base`: blocking shared lock on `<install_base>.lock` (a sibling of the install base).
pub fn acquire_install_base_lock(
    install_base: &str,
    batch_mode: bool,
    err: &mut dyn Write,
) -> Result<(Option<OutputBaseLock>, u64), i32> {
    let p = Path::new(install_base);
    let Some(parent) = p.parent() else {
        return Ok((None, 0));
    };
    if let Err(e) = make_directories(parent, 0o777) {
        let _ = writeln!(
            err,
            "FATAL: mkdir('{}'): {}",
            parent.display(),
            errno_string(&e)
        );
        return Err(ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code());
    }
    let Some(name) = p.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return Ok((None, 0));
    };
    let (lock, wait_ms) = acquire_lock(
        "install base",
        &parent.join(format!("{name}.lock")),
        false,
        batch_mode,
        true,
        err,
    )?;
    Ok((Some(lock), wait_ms))
}

/// `GetLastErrorString`: `(error: <errno>): <strerror>`.
pub fn errno_string(e: &std::io::Error) -> String {
    let code = e.raw_os_error().unwrap_or(0);
    let text = e.to_string();
    let text = text.split(" (os error").next().unwrap_or(&text).to_string();
    format!("(error: {code}): {text}")
}

#[cfg(unix)]
#[cfg(target_os = "macos")]
type ModeT = u16;
#[cfg(unix)]
#[cfg(not(target_os = "macos"))]
type ModeT = u32;

#[cfg(unix)]
extern "C" {
    fn geteuid() -> u32;
    fn umask(mask: ModeT) -> ModeT;
}

/// `GetDirectoryStat` (file_posix.cc): the path must be a directory; with `check_perms` it must also be
/// owned by the effective user and have `mode` (minus the umask), which is fixed up with `chmod`.
#[cfg(unix)]
fn get_directory_stat(path: &Path, mode: u32, check_perms: bool) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let st = fs::metadata(path)?;
    if !st.is_dir() {
        return Err(io::Error::from_raw_os_error(20));
    }
    if check_perms {
        let link = fs::symlink_metadata(path)?;
        // SAFETY: plain libc calls without pointers.
        if link.uid() != unsafe { geteuid() } {
            return Err(io::Error::from_raw_os_error(13));
        }
        let mask = unsafe {
            let m = umask(0o022);
            umask(m);
            m as u32
        };
        let want = mode & !mask;
        if st.mode() & 0o777 != want {
            fs::set_permissions(path, fs::Permissions::from_mode(want))?;
        }
    }
    Ok(())
}

/// `MakeDirectories` (file_posix.cc), with the same errno behaviour.
pub fn make_directories(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        make_directories_impl(path, mode, true)
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        if path.exists() && !path.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "not a directory",
            ));
        }
        fs::create_dir_all(path)
    }
}

#[cfg(unix)]
fn make_directories_impl(path: &Path, mode: u32, childmost: bool) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if path.as_os_str().is_empty() || path == Path::new("/") {
        return Err(io::Error::from_raw_os_error(13));
    }
    match get_directory_stat(path, mode, childmost) {
        Ok(()) => return Ok(()),
        Err(e) if e.raw_os_error() == Some(2) => {}
        Err(e) => return Err(e),
    }
    let parent = path.parent().unwrap_or(Path::new("/"));
    make_directories_impl(parent, mode, false)?;
    match fs::DirBuilder::new().mode(mode).create(path) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(17) => {
            if childmost {
                get_directory_stat(path, mode, childmost)
            } else {
                Ok(())
            }
        }
        Err(e) => Err(e),
    }
}

/// `PrepareDirectories`: creates `output_user_root` and `output_base` (printing the same `FATAL`s and
/// returning the same exit code, 36, as blaze.cc when that is impossible).
pub fn prepare_directories(
    output_user_root: &Path,
    output_base: &Path,
    err: &mut dyn Write,
) -> Result<(), i32> {
    let fatal = ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code();
    if output_user_root.contains_space() {
        let _ = writeln!(err, "WARNING: Output user root \"{}\" contains a space. This will probably break the build. You should set a different --output_user_root.", output_user_root.display());
    } else if output_base.contains_space() {
        let _ = writeln!(err, "WARNING: Output base \"{}\" contains a space. This will probably break the build. You should not set --output_base and let Bazel use the default, or set --output_base to a path without space.", output_base.display());
    }
    if let Err(e) = make_directories(output_user_root, 0o755) {
        let _ = writeln!(
            err,
            "FATAL: mkdir('{}'): {}",
            output_user_root.display(),
            errno_string(&e)
        );
        return Err(fatal);
    }
    if !output_base.exists() {
        if let Err(e) = make_directories(output_base, 0o777) {
            let _ = writeln!(
                err,
                "FATAL: Output base directory '{}' could not be created: {}",
                output_base.display(),
                errno_string(&e)
            );
            return Err(fatal);
        }
    } else if !output_base.is_dir() {
        let _ = writeln!(err, "FATAL: Output base directory '{}' could not be created. It exists but is not a directory.", output_base.display());
        return Err(fatal);
    }
    Ok(())
}

trait ContainsSpace {
    fn contains_space(&self) -> bool;
}
impl ContainsSpace for Path {
    fn contains_space(&self) -> bool {
        self.to_string_lossy().contains(' ')
    }
}

// ------------------------------------------------------------------------- server discovery

#[derive(Debug, Clone)]
pub struct RunningServer {
    pub info: ServerInfo,
    pub restart_reason: &'static str,
}

fn read_server_info(server_dir: &Path) -> Option<ServerInfo> {
    let bytes = fs::read(server_dir.join("server_info.rawproto")).ok()?;
    ServerInfo::decode(&bytes).ok()
}

fn read_pid(server_dir: &Path) -> Option<i32> {
    fs::read_to_string(server_dir.join("server.pid.txt"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

pub fn is_localhost_address(addr: &str) -> bool {
    addr.starts_with("127.0.0.1:")
        || addr.starts_with("[::1]:")
        || addr.starts_with("[0:0:0:0:0:0:0:1]:")
}

fn channel_for(addr: &str, timeout: Duration) -> Option<Channel> {
    if !is_localhost_address(addr) {
        return None;
    }
    let (raw_host, port) = addr.rsplit_once(':')?;
    let host = raw_host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(raw_host);
    Some(Channel::new(
        Arc::new(TcpConnector {
            host: host.to_string(),
            port: port.parse().ok()?,
            connect_timeout: timeout,
        }),
        1,
        vec![],
    ))
}

fn proc_start_time(pid: i32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rparen = stat.rfind(')')?;
    let after = stat.get(rparen + 2..)?;
    after.split_whitespace().nth(19).map(String::from)
}

pub fn verify_server_process(pid: i32, server_dir: &Path) -> bool {
    if pid <= 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        let Some(actual) = proc_start_time(pid) else {
            return false;
        };
        if let Ok(recorded) = fs::read_to_string(server_dir.join("server.starttime")) {
            return recorded.trim() == actual;
        }
        true
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = server_dir;
        sys::process_alive(pid)
    }
}

fn paths_equivalent(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

/// `Ping` RPC: true iff the server answers with the expected response cookie.
pub fn ping(info: &ServerInfo, timeout: Duration) -> bool {
    let Some(ch) = channel_for(&info.address, timeout) else {
        return false;
    };
    let req = PingRequest {
        cookie: info.request_cookie.clone(),
    };
    let r = ch.unary(
        &format!("{SERVICE}Ping"),
        &req.encode_to_vec(),
        &CallOptions {
            timeout: Some(timeout),
            ..Default::default()
        },
    );
    ch.close();
    match r {
        Ok(b) => PingResponse::decode(&b)
            .map(|p| p.cookie == info.response_cookie)
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// `UpdateTerminalSize` RPC: forwards updated `(columns, rows)` for an in-flight `command_id`.
pub fn update_terminal_size(
    info: &ServerInfo,
    command_id: &str,
    columns: i32,
    rows: i32,
    timeout: Duration,
) -> Result<TerminalSizeResponse, GrpcStatus> {
    let Some(ch) = channel_for(&info.address, timeout) else {
        return Err(GrpcStatus {
            code: 14,
            message: "failed to connect to server".into(),
        });
    };
    let req = TerminalSizeRequest {
        cookie: info.request_cookie.clone(),
        command_id: command_id.to_string(),
        columns,
        rows,
    };
    let r = ch.unary(
        &format!("{SERVICE}UpdateTerminalSize"),
        &req.encode_to_vec(),
        &CallOptions {
            timeout: Some(timeout),
            ..Default::default()
        },
    );
    ch.close();
    let bytes = r?;
    TerminalSizeResponse::decode(&bytes).map_err(|e| GrpcStatus {
        code: 13,
        message: e.to_string(),
    })
}

const POST_SHUTDOWN_GRACE_SECS: u64 = 60;
const POST_KILL_GRACE_SECS: u64 = 10;

fn wait_for_exit(pid: i32, server_dir: &Path, within: Duration) -> bool {
    let start = Instant::now();
    let sig_base = sys::SIGNAL_COUNT.load(Ordering::SeqCst);
    while verify_server_process(pid, server_dir) && sys::process_alive(pid) {
        if start.elapsed() > within {
            return false;
        }
        if sys::SIGNAL_COUNT.load(Ordering::SeqCst) > sig_base {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    true
}

fn remove_status_files(server_dir: &Path) {
    for n in [
        "command_port",
        "request_cookie",
        "response_cookie",
        "server.pid.txt",
        "server.starttime",
        "server_info.rawproto",
        "server_info.rawproto.tmp",
        "cmdline",
    ] {
        let _ = fs::remove_file(server_dir.join(n));
    }
}

fn send_shutdown_rpc(info: &ServerInfo, block_for_lock: bool, timeout: Duration) -> bool {
    let Some(ch) = channel_for(&info.address, timeout) else {
        return false;
    };
    let req = RunRequest {
        cookie: info.request_cookie.clone(),
        arg: vec![b"shutdown".to_vec()],
        block_for_lock,
        client_description: format!("pid={} (for shutdown)", std::process::id()),
        startup_options: vec![zel_proto::command_server::StartupOption {
            option: b"--nowrite_command_log".to_vec(),
            source: Vec::new(),
        }],
        ..Default::default()
    };
    let mut reqs = std::iter::once(req.encode_to_vec());
    let mut termination_expected = false;
    let res = ch.call(
        &format!("{SERVICE}Run"),
        &mut reqs,
        &CallOptions {
            timeout: Some(Duration::from_secs(10)),
            ..Default::default()
        },
        &mut |m| {
            if let Ok(r) = RunResponse::decode(&m) {
                if r.finished {
                    termination_expected = r.termination_expected;
                }
            }
            Ok(())
        },
    );
    ch.close();
    res.is_ok() && termination_expected
}

/// `KillRunningServer`: if connected, send `Run("shutdown")` RPC and wait up to 60 s, then `SIGKILL`
/// the server process group and wait up to 10 s (`kPostKillGracePeriodSeconds`), then clean status files.
pub fn kill_running_server(
    server: Option<&RunningServer>,
    server_dir: &Path,
    block_for_lock: bool,
    connect_timeout: Duration,
) {
    if let Some(srv) = server {
        let _ = send_shutdown_rpc(&srv.info, block_for_lock, connect_timeout);
        if !wait_for_exit(
            srv.info.pid,
            server_dir,
            Duration::from_secs(POST_SHUTDOWN_GRACE_SECS),
        ) {
            sys::send_process_group_signal(srv.info.pid, sys::SIGKILL);
            wait_for_exit(
                srv.info.pid,
                server_dir,
                Duration::from_secs(POST_KILL_GRACE_SECS),
            );
        }
    } else if let Some(pid) = read_pid(server_dir) {
        if verify_server_process(pid, server_dir) && sys::process_alive(pid) {
            sys::send_process_group_signal(pid, sys::SIGKILL);
            wait_for_exit(pid, server_dir, Duration::from_secs(POST_KILL_GRACE_SECS));
        }
    }
    remove_status_files(server_dir);
}

/// Legacy helper for callers without a `RunningServer`.
pub fn kill_server(server_dir: &Path) {
    kill_running_server(None, server_dir, true, Duration::from_secs(30));
}

/// Finds a live matching server (`BlazeServer::Connect`).
pub fn find_running_server_with_timeout(
    server_dir: &Path,
    timeout: Duration,
) -> Option<RunningServer> {
    if let Some(info) = read_server_info(server_dir) {
        if !is_localhost_address(&info.address) || info.pid <= 0 {
            return None;
        }
        if verify_server_process(info.pid, server_dir) && ping(&info, timeout) {
            return Some(RunningServer {
                info,
                restart_reason: "no_restart",
            });
        }
    }
    None
}

pub fn find_running_server(server_dir: &Path) -> Option<RunningServer> {
    find_running_server_with_timeout(server_dir, Duration::from_secs(30))
}

/// `EnsureCorrectRunningVersion`: verifies `<output_base>/install` symlink/junction and
/// `<install_base>/install_base_key`; kills any connected server if missing or mismatched,
/// and creates/updates the link and key. Returns `true` if a connected server was restarted.
pub fn ensure_correct_running_version(
    spec: &ServerSpec<'_>,
    connected: &mut Option<RunningServer>,
) -> Result<bool, (i32, String)> {
    let ib = Path::new(spec.install_base);
    let key_ok = fs::read_to_string(ib.join("install_base_key"))
        .map(|k| k == spec.install_md5)
        .unwrap_or(false);
    let link = Path::new(spec.output_base).join("install");
    #[cfg(unix)]
    let link_ok = fs::read_link(&link)
        .map(|t| paths_equivalent(&t, ib))
        .unwrap_or(false);
    #[cfg(windows)]
    let link_ok = zel_vfs::junction_or_symlink_points_to(&link, ib);
    #[cfg(all(not(unix), not(windows)))]
    let link_ok = {
        let _ = &link;
        true
    };
    let mut killed = false;
    if !key_ok || !link_ok {
        if let Some(srv) = connected.take() {
            killed = true;
            kill_running_server(
                Some(&srv),
                &spec.server_dir(),
                spec.startup.block_for_lock,
                spec.connect_timeout(),
            );
        }
        if !key_ok {
            let _ = fs::create_dir_all(ib);
            fs::write(ib.join("install_base_key"), spec.install_md5).map_err(|e| {
                (
                    ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code(),
                    format!("cannot write install_base_key: {e}"),
                )
            })?;
        }
        if !link_ok {
            let _ = fs::create_dir_all(Path::new(spec.output_base));
            zel_vfs::unlink_path_for_write(&link);
            #[cfg(unix)]
            std::os::unix::fs::symlink(ib, &link).map_err(|e| {
                (
                    ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code(),
                    format!(
                        "failed to create installation symlink '{}': {}",
                        link.display(),
                        errno_string(&e)
                    ),
                )
            })?;
            #[cfg(windows)]
            zel_vfs::create_directory_junction_or_symlink(ib, &link).map_err(|e| {
                (
                    ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code(),
                    format!(
                        "failed to create installation junction '{}': {}",
                        link.display(),
                        errno_string(&e)
                    ),
                )
            })?;
        }
    }
    Ok(killed)
}

pub fn ensure_server_cwd_matches(
    spec: &ServerSpec<'_>,
    connected: &mut Option<RunningServer>,
) -> bool {
    let Some(srv) = connected.as_ref() else {
        return false;
    };
    if spec.workspace_dir.is_empty() {
        return false;
    }
    if let Some(cwd) = sys::get_process_cwd(srv.info.pid) {
        let cwd_s = cwd.to_string_lossy();
        if cwd_s.contains(" (deleted)") || !paths_equivalent(&cwd, Path::new(spec.workspace_dir)) {
            let srv = connected.take().unwrap();
            kill_running_server(
                Some(&srv),
                &spec.server_dir(),
                spec.startup.block_for_lock,
                spec.connect_timeout(),
            );
            return true;
        }
    }
    false
}

pub fn connect_or_start(
    spec: &ServerSpec<'_>,
    err: &mut dyn Write,
) -> Result<RunningServer, (i32, String)> {
    Ok(connect_or_start_for_command(spec, false, err)?
        .expect("non-shutdown command must connect or start server"))
}

pub fn connect_or_start_for_command(
    spec: &ServerSpec<'_>,
    is_shutdown: bool,
    err: &mut dyn Write,
) -> Result<Option<RunningServer>, (i32, String)> {
    let server_dir = spec.server_dir();
    let new_args = spec.server_args();
    let mut connected = find_running_server_with_timeout(&server_dir, spec.connect_timeout());
    let mut restart_reason: &'static str = if connected.is_some() {
        "no_restart"
    } else {
        "no_daemon"
    };
    if ensure_correct_running_version(spec, &mut connected)? {
        restart_reason = "new_version";
    }
    if let Some(srv) = connected.take() {
        let old: Vec<String> = fs::read(server_dir.join("cmdline"))
            .map(|b| {
                String::from_utf8_lossy(&b)
                    .split('\0')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        if let Some((only_old, only_new)) = are_startup_options_different(&old, &new_args) {
            let _ = writeln!(
                err,
                "WARNING: Running Bazel server needs to be killed, because the following \
                 startup options are different:\n  - Only in old server: {}\n  - Only in new server: {}",
                only_old.join(" "),
                only_new.join(" ")
            );
            restart_reason = "new_options";
            kill_running_server(
                Some(&srv),
                &server_dir,
                spec.startup.block_for_lock,
                spec.connect_timeout(),
            );
        } else {
            connected = Some(srv);
        }
    } else if read_pid(&server_dir).is_some() {
        if restart_reason == "no_daemon" {
            restart_reason = "pid_file_but_no_server";
        }
        kill_running_server(
            None,
            &server_dir,
            spec.startup.block_for_lock,
            spec.connect_timeout(),
        );
    }
    if ensure_server_cwd_matches(spec, &mut connected) {
        restart_reason = "daemon_disconnected";
    }
    if let Some(mut srv) = connected {
        srv.restart_reason = restart_reason;
        return Ok(Some(srv));
    }
    if is_shutdown {
        return Ok(None);
    }
    let mut srv = start_server(spec, &new_args, err)?;
    srv.restart_reason = restart_reason;
    let mut tmp = None;
    let _ = ensure_correct_running_version(spec, &mut tmp)?;
    Ok(Some(srv))
}

fn random_cookie() -> String {
    let mut buf = [0u8; 16];
    if let Ok(mut f) = File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    } else {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        buf.copy_from_slice(&t.to_le_bytes());
    }
    let mut s = String::with_capacity(buf.len() * 2);
    for b in &buf {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(windows)]
pub(crate) fn spawn_without_inherited_stdio(cmd: &mut Command) -> io::Result<std::process::Child> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(n_std_handle: u32) -> *mut core::ffi::c_void;
        fn GetHandleInformation(h_object: *mut core::ffi::c_void, lpdw_flags: *mut u32) -> i32;
        fn SetHandleInformation(
            h_object: *mut core::ffi::c_void,
            dw_mask: u32,
            dw_flags: u32,
        ) -> i32;
    }
    const STD_INPUT_HANDLE: u32 = (-10i32) as u32;
    const STD_OUTPUT_HANDLE: u32 = (-11i32) as u32;
    const STD_ERROR_HANDLE: u32 = (-12i32) as u32;
    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    let invalid = (-1isize) as *mut core::ffi::c_void;

    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    struct Restore([(*mut core::ffi::c_void, u32); 3]);
    impl Drop for Restore {
        fn drop(&mut self) {
            for &(h, old) in &self.0 {
                if !h.is_null() {
                    unsafe {
                        let _ =
                            SetHandleInformation(h, HANDLE_FLAG_INHERIT, old & HANDLE_FLAG_INHERIT);
                    }
                }
            }
        }
    }
    let mut saved = [(std::ptr::null_mut(), 0u32); 3];
    for (i, id) in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
        .into_iter()
        .enumerate()
    {
        let h = unsafe { GetStdHandle(id) };
        if !h.is_null() && h != invalid {
            let mut flags = 0u32;
            if unsafe { GetHandleInformation(h, &mut flags) } != 0 {
                saved[i] = (h, flags);
                unsafe {
                    let _ = SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
                }
            }
        }
    }
    let _restore = Restore(saved);
    cmd.spawn()
}

fn start_server(
    spec: &ServerSpec<'_>,
    args: &[String],
    err: &mut dyn Write,
) -> Result<RunningServer, (i32, String)> {
    let server_dir = spec.server_dir();
    let env_err = |m: String| (ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code(), m);
    fs::create_dir_all(&server_dir)
        .map_err(|e| env_err(format!("cannot create {}: {e}", server_dir.display())))?;
    // Minimal install base: marker with the key (the binary's digest).
    let ib = Path::new(spec.install_base);
    let _ = fs::create_dir_all(ib);
    let _ = fs::write(ib.join("install_base_key"), spec.install_md5);

    let jvm_out = spec.jvm_out();
    if let Some(p) = jvm_out.parent() {
        let _ = fs::create_dir_all(p);
    }
    let out = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&jvm_out)
        .map_err(|e| {
            env_err(format!(
                "cannot open server output {}: {e}",
                jvm_out.display()
            ))
        })?;
    let out2 = out.try_clone().map_err(|e| env_err(e.to_string()))?;
    let req_cookie = random_cookie();
    let resp_cookie = random_cookie();
    let mut cmd = Command::new(&spec.exe);
    cmd.arg("--internal_server_daemon")
        .arg(format!("--request_cookie={req_cookie}"))
        .arg(format!("--response_cookie={resp_cookie}"))
        .args(args)
        .current_dir(if spec.workspace_dir.is_empty() {
            "/"
        } else {
            spec.workspace_dir
        })
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(out2));
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            sys::setsid_and_ignore_sighup();
            Ok(())
        });
    }
    #[cfg(windows)]
    let mut child = {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        spawn_without_inherited_stdio(&mut cmd)
    }
    .map_err(|e| env_err(format!("cannot start server {}: {e}", spec.exe.display())))?;
    #[cfg(not(windows))]
    let mut child = cmd
        .spawn()
        .map_err(|e| env_err(format!("cannot start server {}: {e}", spec.exe.display())))?;
    if let Some(st) = proc_start_time(child.id() as i32) {
        let _ = fs::write(server_dir.join("server.starttime"), st);
    }
    let start_time = Instant::now();
    let timeout = spec.local_startup_timeout();
    let deadline = start_time + timeout;
    let mut last_msg = start_time;
    loop {
        if let Some(info) = read_server_info(&server_dir) {
            if info.pid as u32 == child.id()
                && is_localhost_address(&info.address)
                && ping(&info, spec.connect_timeout())
            {
                let _ = fs::write(server_dir.join("cmdline"), args.join("\0"));
                // `child` is intentionally leaked: the daemon outlives this client.
                std::mem::forget(child);
                return Ok(RunningServer {
                    info,
                    restart_reason: "no_daemon",
                });
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err((
                ExitCode::BLAZE_INTERNAL_ERROR.numeric_exit_code(),
                format!(
                    "Server crashed during startup ({status}). Now printing '{}' and '{}':\n{}",
                    jvm_out.display(),
                    spec.output_base,
                    fs::read_to_string(&jvm_out).unwrap_or_default()
                ),
            ));
        }
        let now = Instant::now();
        if now >= deadline {
            let _ = child.kill();
            return Err((
                ExitCode::BLAZE_INTERNAL_ERROR.numeric_exit_code(),
                format!(
                    "couldn't connect to server ({}) after {} seconds.",
                    child.id(),
                    timeout.as_secs()
                ),
            ));
        }
        if now.duration_since(last_msg) >= Duration::from_secs(10) {
            let prod = if spec.startup.product_name.is_empty() {
                "Bazel"
            } else {
                &spec.startup.product_name
            };
            let _ = writeln!(
                err,
                "... still trying to connect to local {prod} server ({}) after {} seconds ...",
                child.id(),
                now.duration_since(start_time).as_secs()
            );
            let _ = err.flush();
            last_msg = now;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ------------------------------------------------------------------------------- Run / Cancel

static PRODUCT_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Product name used in the signal messages (`--product_name`, default `Bazel`).
pub fn set_product_name(name: &str) {
    let _ = PRODUCT_NAME.set(name.to_string());
}

fn product_name() -> String {
    PRODUCT_NAME
        .get()
        .cloned()
        .unwrap_or_else(|| "Bazel".to_string())
}

/// Result of a `Run` call.
pub struct RunOutcome {
    pub response: RunResponse,
    /// The stream ended without a terminal `finished` message (server died).
    pub abrupt: Option<GrpcStatus>,
}

#[derive(Clone, Copy)]
struct SignalSnapshot {
    sigint: usize,
    sigterm: usize,
    sigquit: usize,
    sigwinch: usize,
}

trait SignalSource: Send + Sync {
    fn snapshot(&self) -> SignalSnapshot;
}

struct ProcessSignalSource;

impl SignalSource for ProcessSignalSource {
    fn snapshot(&self) -> SignalSnapshot {
        SignalSnapshot {
            sigint: sys::SIGINT_COUNT.load(Ordering::SeqCst),
            sigterm: sys::SIGTERM_COUNT.load(Ordering::SeqCst),
            sigquit: sys::SIGQUIT_COUNT.load(Ordering::SeqCst),
            sigwinch: sys::SIGWINCH_COUNT.load(Ordering::SeqCst),
        }
    }
}

fn send_cancel_async(info: ServerInfo, command_id: String) {
    let _ = std::thread::Builder::new()
        .name("zel-client-cancel".into())
        .spawn(move || {
            if let Some(c) = channel_for(&info.address, Duration::from_secs(5)) {
                let r = CancelRequest {
                    cookie: info.request_cookie,
                    command_id,
                };
                let _ = c.unary(
                    &format!("{SERVICE}Cancel"),
                    &r.encode_to_vec(),
                    &CallOptions {
                        timeout: Some(Duration::from_secs(5)),
                        ..Default::default()
                    },
                );
                c.close();
            }
        });
}

fn send_terminal_size_async(info: ServerInfo, command_id: String, in_flight: Arc<AtomicBool>) {
    struct ClearOnDrop(Arc<AtomicBool>);
    impl Drop for ClearOnDrop {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let clear = ClearOnDrop(in_flight);
    let _ = std::thread::Builder::new()
        .name("zel-client-winch".into())
        .spawn(move || {
            let _clear = clear;
            let fd = if sys::is_tty(2) { 2 } else { 1 };
            let (cols, rows) = sys::terminal_size(fd);
            let _ = update_terminal_size(
                &info,
                &command_id,
                cols as i32,
                rows as i32,
                Duration::from_secs(5),
            );
        });
}

/// Streams a `Run` call: forwards stdout/stderr chunks as they arrive; `SIGINT` and `SIGTERM`
/// send a `Cancel` RPC (three `SIGINT`s kill the server process group; `SIGQUIT` is forwarded).
pub fn run_command(
    info: &ServerInfo,
    req: &RunRequest,
    out: &mut dyn Write,
    err: &mut dyn Write,
    connect_timeout: Duration,
    locks: Option<ClientLocks>,
) -> RunOutcome {
    sys::unblock_client_signals();
    sys::install_handler(sys::SIGINT);
    sys::install_handler(sys::SIGTERM);
    sys::install_handler(sys::SIGQUIT);
    sys::install_handler(sys::SIGWINCH);
    let signals = Arc::new(ProcessSignalSource);
    run_command_inner(
        info,
        req,
        out,
        err,
        connect_timeout,
        locks,
        signals,
        Arc::new(|srv| {
            // Only `SIGINT` increments the 3-strike server kill (`blaze_util_posix.cc:143-155`).
            eprintln!(
                "\n{} caught third interrupt signal; server killed. (This may be expensive, see https://bazel.build/advanced/performance/iteration-speed#avoid-ctrl-c.)\n",
                product_name()
            );
            sys::send_process_group_signal(srv.pid, sys::SIGKILL);
            std::process::exit(1);
        }),
    )
}

#[allow(clippy::too_many_arguments)]
fn run_command_inner(
    info: &ServerInfo,
    req: &RunRequest,
    out: &mut dyn Write,
    err: &mut dyn Write,
    connect_timeout: Duration,
    mut locks: Option<ClientLocks>,
    signals: Arc<dyn SignalSource>,
    on_third_sigint: Arc<dyn Fn(&ServerInfo) + Send + Sync>,
) -> RunOutcome {
    let mut req = req.clone();
    req.cookie = info.request_cookie.clone();
    let ch = channel_for(&info.address, connect_timeout).expect("server address");
    let base = signals.snapshot();
    let command_id = Arc::new(Mutex::new(String::new()));
    let watch_stop = Arc::new(AtomicBool::new(false));
    let watcher = {
        let id = Arc::clone(&command_id);
        let info = info.clone();
        let stop = Arc::clone(&watch_stop);
        let signals = Arc::clone(&signals);
        let on_third_sigint = Arc::clone(&on_third_sigint);
        std::thread::spawn(move || {
            let mut cancelled = false;
            let mut announced = false;
            let mut forwarded_sigquit = 0usize;
            let mut forwarded_sigwinch = 0usize;
            let winch_in_flight = Arc::new(AtomicBool::new(false));
            while !stop.load(Ordering::SeqCst) {
                let snap = signals.snapshot();
                let sigint_n = snap.sigint.saturating_sub(base.sigint);
                let sigterm_n = snap.sigterm.saturating_sub(base.sigterm);
                let sigquit_n = snap.sigquit.saturating_sub(base.sigquit);
                let sigwinch_n = snap.sigwinch.saturating_sub(base.sigwinch);

                if sigint_n >= 3 {
                    on_third_sigint(&info);
                    break;
                }
                if (sigint_n >= 1 || sigterm_n >= 1) && !announced {
                    announced = true;
                    let what = if sigterm_n >= 1 && sigint_n == 0 {
                        "terminate"
                    } else {
                        "interrupt"
                    };
                    eprintln!(
                        "\n{} caught {what} signal; cancelling pending invocation.\n",
                        product_name()
                    );
                }
                if (sigint_n >= 1 || sigterm_n >= 1) && !cancelled {
                    let cid = id.lock().unwrap().clone();
                    if !cid.is_empty() {
                        cancelled = true;
                        send_cancel_async(info.clone(), cid);
                    }
                }
                if sigquit_n > forwarded_sigquit {
                    forwarded_sigquit = sigquit_n;
                    if info.pid > 0 {
                        sys::send_signal(info.pid, sys::SIGQUIT);
                    }
                }
                if sigwinch_n > forwarded_sigwinch && !winch_in_flight.load(Ordering::SeqCst) {
                    let cid = id.lock().unwrap().clone();
                    if !cid.is_empty() {
                        forwarded_sigwinch = sigwinch_n;
                        winch_in_flight.store(true, Ordering::SeqCst);
                        send_terminal_size_async(info.clone(), cid, Arc::clone(&winch_in_flight));
                    }
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        })
    };

    let mut final_resp: Option<RunResponse> = None;
    let mut reqs = std::iter::once(req.encode_to_vec());
    let mut on_started = || {
        // Release both client-side locks only after the `Run` RPC has started (`blaze.cc`).
        let _ = locks.take();
    };
    let result = ch.call_with_started(
        &format!("{SERVICE}Run"),
        &mut reqs,
        &CallOptions::default(),
        &mut on_started,
        &mut |m| {
            let Ok(r) = RunResponse::decode(&m) else {
                return Err(GrpcStatus::new(13, "undecodable RunResponse"));
            };
            if r.cookie != info.response_cookie {
                return Err(GrpcStatus::new(7, "Server response cookie mismatch"));
            }
            if !r.command_id.is_empty() {
                *command_id.lock().unwrap() = r.command_id.clone();
            }
            if !r.standard_output.is_empty() {
                let _ = out.write_all(&r.standard_output);
                let _ = out.flush();
            }
            if !r.standard_error.is_empty() {
                let _ = err.write_all(&r.standard_error);
                let _ = err.flush();
            }
            if r.finished {
                final_resp = Some(r);
            }
            Ok(())
        },
    );
    watch_stop.store(true, Ordering::SeqCst);
    let _ = watcher.join();
    ch.close();
    match (final_resp, result) {
        (Some(r), _) => RunOutcome {
            response: r,
            abrupt: None,
        },
        (None, r) => RunOutcome {
            response: RunResponse::default(),
            abrupt: Some(
                r.err()
                    .unwrap_or_else(|| GrpcStatus::new(14, "Socket closed")),
            ),
        },
    }
}

/// After a `shutdown`-style command wait up to `kPostShutdownGracePeriodSeconds` (60 s) for the server to exit,
/// escalating to `SIGKILL` if it fails to terminate gracefully (`blaze.cc` `AwaitServerProcessTermination`).
pub fn await_server_exit(server_dir: &Path, pid: i32) {
    if !wait_for_exit(
        pid,
        server_dir,
        Duration::from_secs(POST_SHUTDOWN_GRACE_SECS),
    ) {
        sys::send_process_group_signal(pid, sys::SIGKILL);
        wait_for_exit(pid, server_dir, Duration::from_secs(POST_KILL_GRACE_SECS));
        remove_status_files(server_dir);
    }
}

/// Environment passed to the server in `RunRequest` is the daemon's own; we only need this to
/// keep the signature tidy for callers.
pub fn client_env() -> HashMap<String, String> {
    std::env::vars().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localhost_and_startup_diff_checks_match_blaze() {
        assert!(is_localhost_address("127.0.0.1:1234"));
        assert!(is_localhost_address("[::1]:1234"));
        assert!(is_localhost_address("[0:0:0:0:0:0:0:1]:1234"));
        assert!(!is_localhost_address("192.168.1.1:1234"));

        // Different vector lengths force a restart even when only volatile args differ.
        let a = vec![
            "--install_base=/a".to_string(),
            "--max_idle_secs=100".to_string(),
        ];
        let b = vec![
            "--install_base=/a".to_string(),
            "--max_idle_secs=100".to_string(),
            "--client_debug=1".to_string(),
        ];
        assert!(are_startup_options_different(&a, &b).is_some());
    }

    #[test]
    fn ensure_correct_running_version_replaces_install_link_without_deleting_old_install_base() {
        let dir =
            std::env::temp_dir().join(format!("zel-install-link-replace-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let ob = dir.join("ob");
        let old_ib = dir.join("install_old");
        let new_ib = dir.join("install_new");
        fs::create_dir_all(&ob).unwrap();
        fs::create_dir_all(old_ib.join("embedded_tools")).unwrap();
        fs::write(old_ib.join("install_base_key"), "oldmd5").unwrap();
        fs::write(old_ib.join("embedded_tools/keep.txt"), b"must-survive").unwrap();

        let install_link = ob.join("install");
        zel_vfs::create_directory_junction_or_symlink(&old_ib, &install_link).unwrap();
        assert!(zel_vfs::junction_or_symlink_points_to(
            &install_link,
            &old_ib
        ));

        let startup = StartupOptions::default();
        let ob_str = ob.to_string_lossy().into_owned();
        let new_ib_str = new_ib.to_string_lossy().into_owned();
        let spec = ServerSpec {
            exe: std::path::PathBuf::from("zel"),
            startup: &startup,
            workspace_dir: "",
            output_base: &ob_str,
            install_base: &new_ib_str,
            output_user_root: &ob_str,
            install_md5: "newmd5",
        };
        let mut connected = None;
        ensure_correct_running_version(&spec, &mut connected).unwrap();

        assert!(zel_vfs::junction_or_symlink_points_to(
            &install_link,
            &new_ib
        ));
        assert_eq!(
            fs::read(old_ib.join("embedded_tools/keep.txt")).unwrap(),
            b"must-survive",
            "replacing a stale install link/junction must never delete old install_base contents"
        );
        assert_eq!(
            fs::read_to_string(new_ib.join("install_base_key")).unwrap(),
            "newmd5"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn exclusive_output_base_lock_writes_and_reports_blocking_owner_metadata() {
        let dir =
            std::env::temp_dir().join(format!("zel-client-lock-owner-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let ob = dir.join("ob");
        fs::create_dir_all(&ob).unwrap();

        let mut holder_err = Vec::new();
        let (held_lock, wait0) = acquire_output_base_lock(&ob, false, false, &mut holder_err)
            .expect("initial exclusive lock must succeed");
        assert_eq!(wait0, 0);
        assert!(holder_err.is_empty());

        let expected_pid = format!("pid={}", std::process::id());
        let expected_cwd = format!(
            "cwd={}",
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        );

        // Competing non-blocking client fails with exit code 9 and reports the holder's metadata.
        let mut contender_err = Vec::new();
        let res = acquire_output_base_lock(&ob, false, false, &mut contender_err);
        assert_eq!(
            res.err(),
            Some(ExitCode::LOCK_HELD_NOBLOCK_FOR_LOCK.numeric_exit_code())
        );
        let msg = String::from_utf8_lossy(&contender_err);
        assert!(
            msg.contains("Another command holds the output base lock:"),
            "{msg}"
        );
        assert!(msg.contains(&expected_pid), "{msg}");
        assert!(msg.contains("owner=client"), "{msg}");
        assert!(msg.contains(&expected_cwd), "{msg}");
        assert!(
            msg.contains(
                "Exiting because the output base lock is held and --noblock_for_lock was given."
            ),
            "{msg}"
        );

        // Blocking client reports wait message with owner metadata, then acquires once dropped.
        let ob_wait = ob.clone();
        let waiter = std::thread::spawn(move || {
            let mut wait_err = Vec::new();
            let (_lock, waited_ms) = acquire_output_base_lock(&ob_wait, false, true, &mut wait_err)
                .expect("blocking lock must succeed after holder drops");
            (waited_ms, String::from_utf8(wait_err).unwrap())
        });
        std::thread::sleep(Duration::from_millis(150));
        drop(held_lock);

        let (waited_ms, wait_msg) = waiter.join().unwrap();
        assert!(waited_ms >= 100, "waited_ms={waited_ms}");
        assert!(
            wait_msg.contains("Another command holds the output base lock:"),
            "{wait_msg}"
        );
        assert!(wait_msg.contains(&expected_pid), "{wait_msg}");
        assert!(wait_msg.contains(&expected_cwd), "{wait_msg}");
        assert!(
            wait_msg.contains("Waiting for it to complete..."),
            "{wait_msg}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hung_update_terminal_size_does_not_block_sigint_cancel_or_three_strike_kill() {
        use std::net::{Shutdown, TcpListener};
        use std::sync::atomic::AtomicUsize;
        use std::sync::Condvar;
        use zel_proto::command_server::CancelResponse;
        use zel_remote::grpc::{frame_message, MessageParser};
        use zel_remote::hpack::Header;
        use zel_remote::http2::{Conn, Event, LocalSettings, Stream};
        use zel_remote::transport::Transport;

        #[derive(Default)]
        struct TestSignals {
            sigint: AtomicUsize,
            sigterm: AtomicUsize,
            sigquit: AtomicUsize,
            sigwinch: AtomicUsize,
        }

        impl SignalSource for TestSignals {
            fn snapshot(&self) -> SignalSnapshot {
                SignalSnapshot {
                    sigint: self.sigint.load(Ordering::SeqCst),
                    sigterm: self.sigterm.load(Ordering::SeqCst),
                    sigquit: self.sigquit.load(Ordering::SeqCst),
                    sigwinch: self.sigwinch.load(Ordering::SeqCst),
                }
            }
        }

        struct Gate {
            flag: Mutex<bool>,
            cv: Condvar,
        }

        impl Gate {
            fn new() -> Self {
                Self {
                    flag: Mutex::new(false),
                    cv: Condvar::new(),
                }
            }

            fn signal(&self) {
                *self.flag.lock().unwrap() = true;
                self.cv.notify_all();
            }

            fn wait_timeout(&self, dur: Duration) -> bool {
                let mut g = self.flag.lock().unwrap();
                let deadline = Instant::now() + dur;
                while !*g {
                    let Some(rem) = deadline.checked_duration_since(Instant::now()) else {
                        return false;
                    };
                    let (ng, _) = self.cv.wait_timeout(g, rem).unwrap();
                    g = ng;
                }
                true
            }
        }

        struct MockState {
            hang_cancel: bool,
            finish_on_cancel: bool,
            run_started: Gate,
            winch_entered: Gate,
            cancel_entered: Gate,
            finish_run: Gate,
            release_hung_rpcs: Gate,
            stop_accept: AtomicBool,
        }

        fn send_grpc_ok_headers(st: &Stream) -> io::Result<()> {
            st.send_headers(
                &[
                    (":status".into(), "200".into()),
                    ("content-type".into(), "application/grpc".into()),
                ],
                false,
            )
        }

        fn finish_grpc_ok(st: &Stream) -> io::Result<()> {
            st.send_headers(&[("grpc-status".into(), "0".into())], true)
        }

        fn read_unary_request(st: &Stream) -> io::Result<String> {
            let mut headers: Vec<Header> = Vec::new();
            let mut parser = MessageParser::default();
            loop {
                match st.recv(None)? {
                    Event::Headers(h) => headers = h,
                    Event::Data(d) => {
                        parser.push(&d);
                        while let Ok(Some(_)) = parser.next() {}
                    }
                    Event::End => break,
                    Event::Trailers(_) => {}
                    Event::Reset(_) => return Ok(String::new()),
                }
            }
            Ok(headers
                .iter()
                .find(|(k, _)| k == ":path")
                .map(|(_, v)| v.clone())
                .unwrap_or_default())
        }

        fn start_mock_server(
            hang_cancel: bool,
            finish_on_cancel: bool,
        ) -> (String, Arc<MockState>, std::thread::JoinHandle<()>) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let _ = listener.set_nonblocking(true);
            let addr = listener.local_addr().unwrap().to_string();
            let state = Arc::new(MockState {
                hang_cancel,
                finish_on_cancel,
                run_started: Gate::new(),
                winch_entered: Gate::new(),
                cancel_entered: Gate::new(),
                finish_run: Gate::new(),
                release_hung_rpcs: Gate::new(),
                stop_accept: AtomicBool::new(false),
            });
            let st_srv = Arc::clone(&state);
            let accept_handle = std::thread::spawn(move || {
                while !st_srv.stop_accept.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((tcp, _)) => {
                            let _ = tcp.set_nonblocking(false);
                            let _ = tcp.set_nodelay(true);
                            let (Ok(r), Ok(w)) = (tcp.try_clone(), tcp.try_clone()) else {
                                continue;
                            };
                            let st_conn = Arc::clone(&st_srv);
                            std::thread::spawn(move || {
                                let t = Transport {
                                    reader: Box::new(r),
                                    writer: Box::new(w),
                                    shutdown: Box::new(move || {
                                        let _ = tcp.shutdown(Shutdown::Both);
                                    }),
                                };
                                let Ok((conn, rx)) = Conn::server_from(t, LocalSettings::default())
                                else {
                                    return;
                                };
                                while let Ok(stream) = rx.recv() {
                                    let st_s = Arc::clone(&st_conn);
                                    std::thread::spawn(move || {
                                        let Ok(path) = read_unary_request(&stream) else {
                                            return;
                                        };
                                        match path.strip_prefix(SERVICE) {
                                            Some("Run") => {
                                                let _ = send_grpc_ok_headers(&stream);
                                                let init = RunResponse {
                                                    cookie: "resp-cookie".into(),
                                                    command_id: "cmd-hang-test".into(),
                                                    finished: false,
                                                    ..Default::default()
                                                };
                                                let _ = stream.send_data(
                                                    &frame_message(&init.encode_to_vec()),
                                                    false,
                                                    None,
                                                );
                                                st_s.run_started.signal();
                                                let _ = st_s
                                                    .finish_run
                                                    .wait_timeout(Duration::from_secs(10));
                                                let done = RunResponse {
                                                    cookie: "resp-cookie".into(),
                                                    command_id: "cmd-hang-test".into(),
                                                    exit_code: 8,
                                                    finished: true,
                                                    ..Default::default()
                                                };
                                                let _ = stream.send_data(
                                                    &frame_message(&done.encode_to_vec()),
                                                    false,
                                                    None,
                                                );
                                                let _ = finish_grpc_ok(&stream);
                                            }
                                            Some("UpdateTerminalSize") => {
                                                st_s.winch_entered.signal();
                                                let _ = st_s
                                                    .release_hung_rpcs
                                                    .wait_timeout(Duration::from_secs(10));
                                                let _ = send_grpc_ok_headers(&stream);
                                                let resp = TerminalSizeResponse {
                                                    cookie: "resp-cookie".into(),
                                                };
                                                let _ = stream.send_data(
                                                    &frame_message(&resp.encode_to_vec()),
                                                    false,
                                                    None,
                                                );
                                                let _ = finish_grpc_ok(&stream);
                                            }
                                            Some("Cancel") => {
                                                st_s.cancel_entered.signal();
                                                if st_s.hang_cancel {
                                                    let _ = st_s
                                                        .release_hung_rpcs
                                                        .wait_timeout(Duration::from_secs(10));
                                                } else if st_s.finish_on_cancel {
                                                    st_s.finish_run.signal();
                                                }
                                                let _ = send_grpc_ok_headers(&stream);
                                                let resp = CancelResponse {
                                                    cookie: "resp-cookie".into(),
                                                };
                                                let _ = stream.send_data(
                                                    &frame_message(&resp.encode_to_vec()),
                                                    false,
                                                    None,
                                                );
                                                let _ = finish_grpc_ok(&stream);
                                            }
                                            _ => {}
                                        }
                                    });
                                }
                                conn.close();
                            });
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            });
            (addr, state, accept_handle)
        }

        // Case 1: Hung `UpdateTerminalSize` RPC must not delay `SIGINT` `Cancel` or `run_command` exit.
        {
            let (addr, state, accept_handle) = start_mock_server(false, true);
            let signals = Arc::new(TestSignals::default());
            let info = ServerInfo {
                pid: 0,
                address: addr,
                request_cookie: "req-cookie".into(),
                response_cookie: "resp-cookie".into(),
            };
            let sig_client = Arc::clone(&signals);
            let client = std::thread::spawn(move || {
                let mut out = Vec::new();
                let mut err = Vec::new();
                run_command_inner(
                    &info,
                    &RunRequest::default(),
                    &mut out,
                    &mut err,
                    Duration::from_secs(5),
                    None,
                    sig_client,
                    Arc::new(|_| {}),
                )
            });

            assert!(state.run_started.wait_timeout(Duration::from_secs(5)));
            signals.sigwinch.fetch_add(1, Ordering::SeqCst);
            assert!(
                state.winch_entered.wait_timeout(Duration::from_secs(5)),
                "UpdateTerminalSize must reach server and block"
            );

            let t0 = Instant::now();
            signals.sigint.fetch_add(1, Ordering::SeqCst);
            assert!(
                state.cancel_entered.wait_timeout(Duration::from_secs(2)),
                "Cancel RPC must not wait behind hung UpdateTerminalSize RPC"
            );
            let outcome = client.join().unwrap();
            let elapsed = t0.elapsed();
            assert!(outcome.abrupt.is_none(), "{:?}", outcome.abrupt);
            assert_eq!(outcome.response.exit_code, 8);
            assert!(
                elapsed < Duration::from_secs(2),
                "SIGINT cancel took {elapsed:?} despite non-blocking UpdateTerminalSize"
            );
            assert!(
                !*state.release_hung_rpcs.flag.lock().unwrap(),
                "UpdateTerminalSize was still hung while Cancel completed"
            );

            state.stop_accept.store(true, Ordering::SeqCst);
            state.release_hung_rpcs.signal();
            let _ = accept_handle.join();
        }

        // Case 2: Hung `UpdateTerminalSize` AND hung `Cancel` must not block 3-strike `SIGINT` kill.
        {
            let (addr, state, accept_handle) = start_mock_server(true, false);
            let signals = Arc::new(TestSignals::default());
            let info = ServerInfo {
                pid: 424242,
                address: addr,
                request_cookie: "req-cookie".into(),
                response_cookie: "resp-cookie".into(),
            };
            let third_strike_pid = Arc::new(Mutex::new(None));
            let third_strike_gate = Arc::new(Gate::new());
            let pid_slot = Arc::clone(&third_strike_pid);
            let strike_gate = Arc::clone(&third_strike_gate);
            let state_kill = Arc::clone(&state);
            let sig_client = Arc::clone(&signals);
            let client = std::thread::spawn(move || {
                let mut out = Vec::new();
                let mut err = Vec::new();
                run_command_inner(
                    &info,
                    &RunRequest::default(),
                    &mut out,
                    &mut err,
                    Duration::from_secs(5),
                    None,
                    sig_client,
                    Arc::new(move |srv| {
                        *pid_slot.lock().unwrap() = Some(srv.pid);
                        strike_gate.signal();
                        state_kill.finish_run.signal();
                    }),
                )
            });

            assert!(state.run_started.wait_timeout(Duration::from_secs(5)));
            signals.sigwinch.fetch_add(1, Ordering::SeqCst);
            assert!(state.winch_entered.wait_timeout(Duration::from_secs(5)));

            signals.sigint.fetch_add(1, Ordering::SeqCst);
            assert!(state.cancel_entered.wait_timeout(Duration::from_secs(2)));

            let t0 = Instant::now();
            signals.sigint.fetch_add(2, Ordering::SeqCst);
            assert!(
                third_strike_gate.wait_timeout(Duration::from_secs(2)),
                "third SIGINT kill must not wait behind hung UpdateTerminalSize or Cancel RPCs"
            );
            let elapsed = t0.elapsed();
            assert_eq!(*third_strike_pid.lock().unwrap(), Some(424242));
            assert!(
                elapsed < Duration::from_secs(2),
                "3-strike SIGINT took {elapsed:?}"
            );
            let _ = client.join().unwrap();

            state.stop_accept.store(true, Ordering::SeqCst);
            state.release_hung_rpcs.signal();
            let _ = accept_handle.join();
        }

        // Case 3: `SIGWINCH` arriving after `Cancel` while `Run` is still winding down is forwarded
        // (`BlazeServer::CancelThread` in `src/main/cpp/blaze.cc:1873-1904`).
        {
            let (addr, state, accept_handle) = start_mock_server(false, false);
            let signals = Arc::new(TestSignals::default());
            let info = ServerInfo {
                pid: 0,
                address: addr,
                request_cookie: "req-cookie".into(),
                response_cookie: "resp-cookie".into(),
            };
            let sig_client = Arc::clone(&signals);
            let client = std::thread::spawn(move || {
                let mut out = Vec::new();
                let mut err = Vec::new();
                run_command_inner(
                    &info,
                    &RunRequest::default(),
                    &mut out,
                    &mut err,
                    Duration::from_secs(5),
                    None,
                    sig_client,
                    Arc::new(|_| {}),
                )
            });

            assert!(state.run_started.wait_timeout(Duration::from_secs(5)));
            signals.sigint.fetch_add(1, Ordering::SeqCst);
            assert!(state.cancel_entered.wait_timeout(Duration::from_secs(2)));

            signals.sigwinch.fetch_add(1, Ordering::SeqCst);
            assert!(
                state.winch_entered.wait_timeout(Duration::from_secs(2)),
                "UpdateTerminalSize must still be forwarded after Cancel while Run is active"
            );

            state.release_hung_rpcs.signal();
            state.finish_run.signal();
            let outcome = client.join().unwrap();
            assert!(outcome.abrupt.is_none(), "{:?}", outcome.abrupt);
            assert_eq!(outcome.response.exit_code, 8);

            state.stop_accept.store(true, Ordering::SeqCst);
            let _ = accept_handle.join();
        }
    }
}
