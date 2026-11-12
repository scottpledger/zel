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
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zel_options::StartupOptions;
use zel_proto::command_server::{
    CancelRequest, PingRequest, PingResponse, RunRequest, RunResponse, ServerInfo,
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
    startup.parsed_options.get(name).and_then(|d| d.raw_values.last().cloned())
}

fn startup_secs(startup: &StartupOptions, name: &str, default: u64) -> u64 {
    startup_raw(startup, name).and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl ServerSpec<'_> {
    pub fn server_dir(&self) -> PathBuf {
        Path::new(self.output_base).join("server")
    }

    pub fn connect_timeout(&self) -> Duration {
        Duration::from_secs(startup_secs(self.startup, "connect_timeout_secs", 30))
    }

    pub fn local_startup_timeout(&self) -> Duration {
        Duration::from_secs(startup_secs(self.startup, "local_startup_timeout_secs", 120))
    }

    pub fn jvm_out(&self) -> PathBuf {
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

/// `AreStartupOptionsDifferent`: multiset difference ignoring volatile args.
pub fn startup_options_diff(old: &[String], new: &[String]) -> (Vec<String>, Vec<String>) {
    let mut old_left: Vec<String> =
        old.iter().filter(|a| !is_volatile(a)).cloned().collect();
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

// ------------------------------------------------------------------------------------ locking

/// An exclusive `flock(2)` on `<output_base>/lock`; released on drop.
pub struct OutputBaseLock {
    _file: File,
}

#[cfg(unix)]
extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

#[cfg(unix)]
fn try_lock_exclusive(file: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    let rc = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn try_lock_exclusive(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn lock_shared_blocking(file: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    const LOCK_SH: i32 = 1;
    let rc = unsafe { flock(file.as_raw_fd(), LOCK_SH) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn lock_shared_blocking(_file: &File) -> io::Result<()> {
    Ok(())
}

fn read_lock_owner(file: &mut File) -> String {
    let mut s = String::new();
    let _ = file.seek(SeekFrom::Start(0));
    let _ = file.take(4096).read_to_string(&mut s);
    s
}

/// `AcquireLock`: `Err(exit_code)` after printing the diagnostics Bazel prints.
pub fn acquire_output_base_lock(
    output_base: &Path,
    block: bool,
    err: &mut dyn Write,
) -> Result<OutputBaseLock, i32> {
    let _ = fs::create_dir_all(output_base);
    let path = output_base.join("lock");
    let mut owner = String::new();
    loop {
        let mut file = match OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path) {
            Ok(f) => f,
            Err(e) => {
                let _ = writeln!(err, "FATAL: open failed for output base lock: {e}");
                return Err(ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code());
            }
        };
        if try_lock_exclusive(&file).is_ok() {
            let _ = file.set_len(0);
            let _ = file.seek(SeekFrom::Start(0));
            let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
            let _ = write!(file, "pid={}\nowner=client\ncwd={cwd}\n", std::process::id());
            let _ = file.flush();
            return Ok(OutputBaseLock { _file: file });
        }
        let new_owner = read_lock_owner(&mut file);
        let changed = new_owner != owner;
        if changed {
            let _ = writeln!(err, "Another command holds the output base lock: \n{new_owner}");
            owner = new_owner;
        }
        let sleep = Duration::from_millis(500);
        if !block {
            let _ = writeln!(
                err,
                "FATAL: Exiting because the output base lock is held and --noblock_for_lock was given."
            );
            return Err(ExitCode::LOCK_HELD_NOBLOCK_FOR_LOCK.numeric_exit_code());
        }
        if changed {
            let _ = writeln!(err, "Waiting for it to complete...");
            let _ = err.flush();
        }
        drop(file);
        std::thread::sleep(sleep);
    }
}

/// `lock_install_base`: shared `flock` on `<install_base>.lock` (a sibling of the install base),
/// held for the lifetime of the returned handle.
pub fn acquire_install_base_lock(install_base: &str) -> Result<Option<File>, std::io::Error> {
    let p = Path::new(install_base);
    let Some(parent) = p.parent() else { return Ok(None) };
    let _ = fs::create_dir_all(parent);
    let Some(name) = p.file_name().map(|n| n.to_string_lossy().into_owned()) else { return Ok(None) };
    let f = OpenOptions::new()
        .read(true)
        .create(true)
        .write(true)
        .truncate(false)
        .open(parent.join(format!("{name}.lock")))?;
    lock_shared_blocking(&f)?;
    Ok(Some(f))
}

/// `GetLastErrorString`: `(error: <errno>): <strerror>`.
pub fn errno_string(e: &std::io::Error) -> String {
    let code = e.raw_os_error().unwrap_or(0);
    let text = e.to_string();
    let text = text.split(" (os error").next().unwrap_or(&text).to_string();
    format!("(error: {code}): {text}")
}

#[cfg(target_os = "macos")]
type ModeT = u16;
#[cfg(not(target_os = "macos"))]
type ModeT = u32;

extern "C" {
    fn geteuid() -> u32;
    fn umask(mask: ModeT) -> ModeT;
}

/// `GetDirectoryStat` (file_posix.cc): the path must be a directory; with `check_perms` it must also be
/// owned by the effective user and have `mode` (minus the umask), which is fixed up with `chmod`.
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
    make_directories_impl(path, mode, true)
}

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
pub fn prepare_directories(output_user_root: &Path, output_base: &Path, err: &mut dyn Write) -> Result<(), i32> {
    let fatal = ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code();
    if output_user_root.contains_space() {
        let _ = writeln!(err, "WARNING: Output user root \"{}\" contains a space. This will probably break the build. You should set a different --output_user_root.", output_user_root.display());
    } else if output_base.contains_space() {
        let _ = writeln!(err, "WARNING: Output base \"{}\" contains a space. This will probably break the build. You should not set --output_base and let Bazel use the default, or set --output_base to a path without space.", output_base.display());
    }
    if let Err(e) = make_directories(output_user_root, 0o755) {
        let _ = writeln!(err, "FATAL: mkdir('{}'): {}", output_user_root.display(), errno_string(&e));
        return Err(fatal);
    }
    if !output_base.exists() {
        if let Err(e) = make_directories(output_base, 0o777) {
            let _ = writeln!(err, "FATAL: Output base directory '{}' could not be created: {}", output_base.display(), errno_string(&e));
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
}

fn read_server_info(server_dir: &Path) -> Option<ServerInfo> {
    let bytes = fs::read(server_dir.join("server_info.rawproto")).ok()?;
    ServerInfo::decode(&bytes).ok()
}

fn read_pid(server_dir: &Path) -> Option<i32> {
    fs::read_to_string(server_dir.join("server.pid.txt")).ok()?.trim().parse().ok()
}

fn channel_for(addr: &str, timeout: Duration) -> Option<Channel> {
    let (host, port) = addr.rsplit_once(':')?;
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

/// `Ping` RPC: true iff the server answers with the expected response cookie.
pub fn ping(info: &ServerInfo, timeout: Duration) -> bool {
    let Some(ch) = channel_for(&info.address, timeout) else {
        return false;
    };
    let req = PingRequest { cookie: info.request_cookie.clone() };
    let r = ch.unary(
        &format!("{SERVICE}Ping"),
        &req.encode_to_vec(),
        &CallOptions { timeout: Some(timeout), ..Default::default() },
    );
    ch.close();
    match r {
        Ok(b) => PingResponse::decode(&b).map(|p| p.cookie == info.response_cookie).unwrap_or(false),
        Err(_) => false,
    }
}

fn wait_for_exit(pid: i32, within: Duration) -> bool {
    let start = Instant::now();
    while sys::process_alive(pid) {
        if start.elapsed() > within {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

fn remove_status_files(server_dir: &Path) {
    for n in [
        "command_port",
        "request_cookie",
        "response_cookie",
        "server.pid.txt",
        "server_info.rawproto",
        "server_info.rawproto.tmp",
        "cmdline",
    ] {
        let _ = fs::remove_file(server_dir.join(n));
    }
}

/// `KillRunningServer`: SIGTERM, then SIGKILL, then clean the status files.
pub fn kill_server(server_dir: &Path) {
    if let Some(pid) = read_pid(server_dir) {
        if sys::process_alive(pid) {
            sys::send_signal(pid, sys::SIGTERM);
            if !wait_for_exit(pid, Duration::from_secs(5)) {
                sys::send_signal(pid, sys::SIGKILL);
                wait_for_exit(pid, Duration::from_secs(5));
            }
        }
    }
    remove_status_files(server_dir);
}

/// Finds a live matching server, restarting it when startup options / install base changed,
/// or starts a new one.  The caller must hold the output base lock.
pub fn find_running_server(server_dir: &Path) -> Option<RunningServer> {
    if let Some(info) = read_server_info(server_dir) {
        let alive = read_pid(server_dir).map(sys::process_alive).unwrap_or(false);
        if alive && ping(&info, Duration::from_secs(3)) {
            return Some(RunningServer { info });
        }
    }
    None
}

pub fn connect_or_start(
    spec: &ServerSpec<'_>,
    err: &mut dyn Write,
) -> Result<RunningServer, (i32, String)> {
    let server_dir = spec.server_dir();
    let new_args = spec.server_args();
    if let Some(info) = read_server_info(&server_dir) {
        let alive = read_pid(&server_dir).map(sys::process_alive).unwrap_or(false);
        if alive && ping(&info, Duration::from_secs(3)) {
            // EnsureCorrectRunningVersion: the `install` symlink in the output base records
            // which installation the server was started from.
            let link = Path::new(spec.output_base).join("install");
            let stale_install = fs::read_link(&link)
                .map(|t| t.to_string_lossy() != spec.install_base)
                .unwrap_or(false);
            if stale_install {
                kill_server(&server_dir);
            } else {
                let old: Vec<String> = fs::read(server_dir.join("cmdline"))
                    .map(|b| {
                        String::from_utf8_lossy(&b)
                            .split('\0')
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default();
                let (only_old, only_new) = startup_options_diff(&old, &new_args);
                if only_old.is_empty() && only_new.is_empty() {
                    return Ok(RunningServer { info });
                }
                let _ = writeln!(
                    err,
                    "WARNING: Running Bazel server needs to be killed, because the following \
                     startup options are different:\n  - Only in old server: {}\n  - Only in new server: {}",
                    only_old.join(" "),
                    only_new.join(" ")
                );
                kill_server(&server_dir);
            }
        } else {
            // Stale pid / status files (crashed server): reap whatever is left.
            kill_server(&server_dir);
        }
    } else if read_pid(&server_dir).is_some() {
        kill_server(&server_dir);
    }
    start_server(spec, &new_args)
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
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn start_server(
    spec: &ServerSpec<'_>,
    args: &[String],
) -> Result<RunningServer, (i32, String)> {
    let server_dir = spec.server_dir();
    let env_err = |m: String| (ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code(), m);
    fs::create_dir_all(&server_dir).map_err(|e| env_err(format!("cannot create {}: {e}", server_dir.display())))?;
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
        .map_err(|e| env_err(format!("cannot open server output {}: {e}", jvm_out.display())))?;
    let out2 = out.try_clone().map_err(|e| env_err(e.to_string()))?;
    let req_cookie = random_cookie();
    let resp_cookie = random_cookie();
    let mut cmd = Command::new(&spec.exe);
    cmd.arg("--internal_server_daemon")
        .arg(format!("--request_cookie={req_cookie}"))
        .arg(format!("--response_cookie={resp_cookie}"))
        .args(args)
        .current_dir(if spec.workspace_dir.is_empty() { "/" } else { spec.workspace_dir })
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(out2))
        .process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| env_err(format!("cannot start server {}: {e}", spec.exe.display())))?;
    let deadline = Instant::now() + spec.local_startup_timeout();
    loop {
        if let Some(info) = read_server_info(&server_dir) {
            if info.pid as u32 == child.id() && ping(&info, Duration::from_secs(5)) {
                let _ = fs::write(server_dir.join("cmdline"), args.join("\0"));
                // `child` is intentionally leaked: the daemon outlives this client.
                std::mem::forget(child);
                return Ok(RunningServer { info });
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
        if Instant::now() > deadline {
            let _ = child.kill();
            return Err((
                ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code(),
                format!(
                    "couldn't connect to server (pid={}) after {} seconds.",
                    child.id(),
                    spec.local_startup_timeout().as_secs()
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ------------------------------------------------------------------------------- Run / Cancel

static PRODUCT_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Product name used in the signal messages (`--product_name`, default `Bazel`).
pub fn set_product_name(name: &str) {
    let _ = PRODUCT_NAME.set(name.to_string());
}

fn product_name() -> String {
    PRODUCT_NAME.get().cloned().unwrap_or_else(|| "Bazel".to_string())
}

static CANCEL_WATCH_STOP: AtomicBool = AtomicBool::new(false);

/// Result of a `Run` call.
pub struct RunOutcome {
    pub response: RunResponse,
    /// The stream ended without a terminal `finished` message (server died).
    pub abrupt: Option<GrpcStatus>,
}

/// Streams a `Run` call: forwards stdout/stderr chunks as they arrive; SIGINT / SIGTERM send a
/// `Cancel` RPC (three signals abort the client with exit code 8).
pub fn run_command(
    info: &ServerInfo,
    req: &RunRequest,
    out: &mut dyn Write,
    err: &mut dyn Write,
    connect_timeout: Duration,
) -> RunOutcome {
    let mut req = req.clone();
    req.cookie = info.request_cookie.clone();
    let ch = channel_for(&info.address, connect_timeout).expect("server address");
    sys::install_handler(sys::SIGINT);
    sys::install_handler(sys::SIGTERM);
    let baseline = sys::SIGNAL_COUNT.load(Ordering::SeqCst);
    let command_id = Arc::new(Mutex::new(String::new()));
    CANCEL_WATCH_STOP.store(false, Ordering::SeqCst);
    let watcher = {
        let id = Arc::clone(&command_id);
        let info = info.clone();
        std::thread::spawn(move || {
            let mut cancelled = false;
            let mut announced = false;
            while !CANCEL_WATCH_STOP.load(Ordering::SeqCst) {
                let n = sys::SIGNAL_COUNT.load(Ordering::SeqCst).saturating_sub(baseline);
                if n >= 3 {
                    // blaze_util_posix.cc `handler`: the third SIGINT kills the server.
                    eprintln!(
                        "\n{} caught third interrupt signal; server killed. (This may be expensive, see https://bazel.build/advanced/performance/iteration-speed#avoid-ctrl-c.)\n",
                        product_name()
                    );
                    sys::send_signal(info.pid, sys::SIGKILL);
                    std::process::exit(1);
                }
                if n >= 1 && !announced {
                    announced = true;
                    // blaze_util_posix.cc `handler`: SigPrintf, then CancelServer.
                    let what = if sys::LAST_SIGNAL.load(Ordering::SeqCst) == sys::SIGTERM { "terminate" } else { "interrupt" };
                    eprintln!("\n{} caught {what} signal; cancelling pending invocation.\n", product_name());
                }
                if n >= 1 && !cancelled {
                    let cid = id.lock().unwrap().clone();
                    if !cid.is_empty() {
                        cancelled = true;
                        if let Some(c) = channel_for(&info.address, Duration::from_secs(5)) {
                            let r = CancelRequest {
                                cookie: info.request_cookie.clone(),
                                command_id: cid,
                            };
                            let _ = c.unary(
                                &format!("{SERVICE}Cancel"),
                                &r.encode_to_vec(),
                                &CallOptions { timeout: Some(Duration::from_secs(5)), ..Default::default() },
                            );
                            c.close();
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        })
    };

    let mut final_resp: Option<RunResponse> = None;
    let mut reqs = std::iter::once(req.encode_to_vec());
    let result = ch.call(
        &format!("{SERVICE}Run"),
        &mut reqs,
        &CallOptions::default(),
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
    CANCEL_WATCH_STOP.store(true, Ordering::SeqCst);
    let _ = watcher.join();
    ch.close();
    match (final_resp, result) {
        (Some(r), _) => RunOutcome { response: r, abrupt: None },
        (None, r) => RunOutcome {
            response: RunResponse::default(),
            abrupt: Some(r.err().unwrap_or_else(|| GrpcStatus::new(14, "Socket closed"))),
        },
    }
}

/// After a `shutdown`-style command wait for the server to disappear (client does the same).
pub fn await_server_exit(server_dir: &Path, pid: i32) {
    wait_for_exit(pid, Duration::from_secs(10));
    let _ = server_dir;
}

/// Environment passed to the server in `RunRequest` is the daemon's own; we only need this to
/// keep the signature tidy for callers.
pub fn client_env() -> HashMap<String, String> {
    std::env::vars().collect()
}
