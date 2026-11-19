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

//! Native port of `src/main/tools/linux-sandbox*.cc` (`linux-sandbox-options.cc`,
//! `linux-sandbox.cc`, and `linux-sandbox-pid1.cc`).

use crate::linux::NetworkNamespace;
use crate::process_wrapper::{ResourceUsage, Termination};

/// Parsed command-line options for `linux-sandbox` (`struct Options` in `linux-sandbox-options.h`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinuxSandboxOptions {
    pub working_dir: Option<String>,
    pub timeout_secs: i64,
    pub kill_delay_secs: i64,
    pub sigint_sends_sigterm: bool,
    pub stdout_path: Option<String>,
    pub stderr_path: Option<String>,
    pub writable_files: Vec<String>,
    pub tmpfs_dirs: Vec<String>,
    /// `(source, target)` pairs from `-M` and optional `-m`.
    pub bind_mounts: Vec<(String, String)>,
    pub stats_path: Option<String>,
    pub hermetic_sandbox_dir: Option<String>,
    pub persistent_process: bool,
    pub cgroups_dirs: Vec<String>,
    pub fake_hostname: bool,
    pub create_netns: NetworkNamespace,
    pub fake_root: bool,
    pub fake_username: bool,
    pub enable_pty: bool,
    pub debug_path: Option<String>,
    pub args: Vec<String>,
}

pub fn linux_sandbox_usage(program: &str, error: &str) -> String {
    format!(
        "{error}\n\nUsage: {program} [-W working-dir] [-T timeout] [-t kill-delay] [-l stdout] [-L stderr] \
         [-w writable] [-e tmpfs] [-M source [-m target]] [-S stats] [-h sandbox-root] [-H] [-n|-N] \
         [-R|-U] [-P] [-i] [-p] [-C cgroup-dir] [-D debug-file] -- command arg1 ...\n"
    )
}

/// Expands `@paramfile` arguments before `--` (`ParseOptions` in `linux-sandbox-options.cc`).
pub fn expand_param_files(raw_args: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::with_capacity(raw_args.len());
    let mut after_ddash = false;
    for arg in raw_args {
        if after_ddash {
            out.push(arg.clone());
            continue;
        }
        if arg == "--" {
            after_ddash = true;
            out.push(arg.clone());
            continue;
        }
        if let Some(path) = arg.strip_prefix('@') {
            if !path.is_empty() {
                let content = std::fs::read_to_string(path)
                    .map_err(|e| format!("Failed to open param file '{path}': {e}"))?;
                for line in content.lines() {
                    let line = line.strip_suffix('\r').unwrap_or(line);
                    if !line.is_empty() {
                        out.push(line.to_string());
                    }
                }
                continue;
            }
        }
        out.push(arg.clone());
    }
    Ok(out)
}

/// Parses `argv[1..]` according to `linux-sandbox-options.cc`.
pub fn parse_linux_sandbox_args(
    program: &str,
    raw_args: &[String],
) -> Result<LinuxSandboxOptions, String> {
    let args = expand_param_files(raw_args).map_err(|e| linux_sandbox_usage(program, &e))?;
    let mut opt = LinuxSandboxOptions::default();
    let mut pending_mount_source: Option<String> = None;
    let mut i = 0;

    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            i += 1;
            break;
        }
        if !a.starts_with('-') || a == "-" {
            break;
        }
        let mut chars = a[1..].chars();
        let flag = chars.next().unwrap();
        let inline: String = chars.collect();
        let takes_arg = matches!(
            flag,
            'W' | 'T' | 't' | 'l' | 'L' | 'w' | 'e' | 'M' | 'm' | 'S' | 'h' | 'C' | 'D'
        );
        let is_bool = matches!(flag, 'i' | 'p' | 'H' | 'n' | 'N' | 'R' | 'U' | 'P');
        if !takes_arg && !is_bool {
            return Err(linux_sandbox_usage(
                program,
                &format!("Unrecognized argument: -{flag}"),
            ));
        }

        if flag != 'm' {
            if let Some(src) = pending_mount_source.take() {
                opt.bind_mounts.push((src.clone(), src));
            }
        }

        if is_bool {
            let mut bundled = vec![flag];
            bundled.extend(inline.chars());
            for b in bundled {
                match b {
                    'i' => opt.sigint_sends_sigterm = true,
                    'p' => opt.persistent_process = true,
                    'H' => opt.fake_hostname = true,
                    'n' => {
                        if opt.create_netns == NetworkNamespace::NetnsWithLoopback {
                            return Err(linux_sandbox_usage(
                                program,
                                "Cannot use -n and -N at the same time.",
                            ));
                        }
                        opt.create_netns = NetworkNamespace::Netns;
                    }
                    'N' => {
                        if opt.create_netns == NetworkNamespace::Netns {
                            return Err(linux_sandbox_usage(
                                program,
                                "Cannot use -n and -N at the same time.",
                            ));
                        }
                        opt.create_netns = NetworkNamespace::NetnsWithLoopback;
                    }
                    'R' => {
                        if opt.fake_username {
                            return Err(linux_sandbox_usage(
                                program,
                                "Cannot use -R and -U at the same time.",
                            ));
                        }
                        opt.fake_root = true;
                    }
                    'U' => {
                        if opt.fake_root {
                            return Err(linux_sandbox_usage(
                                program,
                                "Cannot use -R and -U at the same time.",
                            ));
                        }
                        opt.fake_username = true;
                    }
                    'P' => opt.enable_pty = true,
                    other => {
                        return Err(linux_sandbox_usage(
                            program,
                            &format!("Unrecognized argument: -{other}"),
                        ));
                    }
                }
            }
            i += 1;
            continue;
        }

        let val = if !inline.is_empty() {
            inline
        } else {
            i += 1;
            args.get(i).cloned().ok_or_else(|| {
                linux_sandbox_usage(program, &format!("Flag -{flag} requires an argument."))
            })?
        };

        match flag {
            'W' => opt.working_dir = Some(val),
            'T' => {
                let v = val.parse::<i64>().map_err(|_| {
                    linux_sandbox_usage(program, &format!("Invalid timeout (-T) value: {val}"))
                })?;
                if v < 0 {
                    return Err(linux_sandbox_usage(
                        program,
                        "Timeout (-T) must not be negative.",
                    ));
                }
                opt.timeout_secs = v;
            }
            't' => {
                let v = val.parse::<i64>().map_err(|_| {
                    linux_sandbox_usage(program, &format!("Invalid kill delay (-t) value: {val}"))
                })?;
                if v < 0 {
                    return Err(linux_sandbox_usage(
                        program,
                        "Kill delay (-t) must not be negative.",
                    ));
                }
                opt.kill_delay_secs = v;
            }
            'l' => {
                if opt.stdout_path.as_deref().is_some_and(|s| !s.is_empty()) {
                    return Err(linux_sandbox_usage(
                        program,
                        "Cannot redirect stdout (-l) to more than one destination.",
                    ));
                }
                if !val.is_empty() {
                    opt.stdout_path = Some(val);
                }
            }
            'L' => {
                if opt.stderr_path.as_deref().is_some_and(|s| !s.is_empty()) {
                    return Err(linux_sandbox_usage(
                        program,
                        "Cannot redirect stderr (-L) to more than one destination.",
                    ));
                }
                if !val.is_empty() {
                    opt.stderr_path = Some(val);
                }
            }
            'w' => opt.writable_files.push(val),
            'e' => opt.tmpfs_dirs.push(val),
            'M' => {
                pending_mount_source = Some(val);
            }
            'm' => match pending_mount_source.take() {
                Some(src) => opt.bind_mounts.push((src, val)),
                None => {
                    return Err(linux_sandbox_usage(
                        program,
                        "The -m option must be preceded by an -M option.",
                    ));
                }
            },
            'S' => {
                if opt.stats_path.as_deref().is_some_and(|s| !s.is_empty()) {
                    return Err(linux_sandbox_usage(
                        program,
                        "Cannot write stats (-S) to more than one destination.",
                    ));
                }
                if !val.is_empty() {
                    opt.stats_path = Some(val);
                }
            }
            'h' => opt.hermetic_sandbox_dir = Some(val),
            'C' => opt.cgroups_dirs.push(val),
            'D' => opt.debug_path = Some(val),
            _ => unreachable!(),
        }
        i += 1;
    }

    if let Some(src) = pending_mount_source.take() {
        opt.bind_mounts.push((src.clone(), src));
    }

    opt.args = args[i.min(args.len())..].to_vec();
    if opt.args.is_empty() {
        return Err(linux_sandbox_usage(program, "No command specified."));
    }
    Ok(opt)
}

/// Places `pid` into every cgroup directory passed via `-C` by writing `<pid>` to
/// `<cgroup_dir>/cgroup.procs` (`MaybeAddChildProcessToCgroup` in `linux-sandbox.cc:131-137`).
pub fn join_cgroups(cgroups_dirs: &[String], pid: i32) -> Result<(), String> {
    let content = pid.to_string();
    for dir in cgroups_dirs {
        let procs_path = std::path::Path::new(dir).join("cgroup.procs");
        std::fs::write(&procs_path, &content)
            .map_err(|e| format!("write '{}': {e}", procs_path.display()))?;
    }
    Ok(())
}

/// Unescapes octal sequences (`\040`, `\011`, `\012`, `\134`) in `/proc/self/mounts` fields,
/// matching `getmntent(3)`.
pub fn unescape_mount_path(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && (b'0'..=b'7').contains(&bytes[i + 1])
            && (b'0'..=b'7').contains(&bytes[i + 2])
            && (b'0'..=b'7').contains(&bytes[i + 3])
        {
            let val =
                ((bytes[i + 1] - b'0') << 6) | ((bytes[i + 2] - b'0') << 3) | (bytes[i + 3] - b'0');
            out.push(val);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parses `/proc/self/mounts` entries into `(mount_point, preserved_remount_flags)`, preserving
/// per-mount flags (`nodev`, `noexec`, `nosuid`, `noatime`, `nodiratime`, `relatime`) alongside
/// `MS_BIND | MS_REMOUNT` (`MakeFilesystemMostlyReadOnly` in `linux-sandbox-pid1.cc:366-396`).
pub fn parse_proc_mounts(contents: &str) -> Vec<(String, u64)> {
    const FLAG_NOSUID: u64 = 2;
    const FLAG_NODEV: u64 = 4;
    const FLAG_NOEXEC: u64 = 8;
    const FLAG_REMOUNT: u64 = 32;
    const FLAG_NOATIME: u64 = 1024;
    const FLAG_NODIRATIME: u64 = 2048;
    const FLAG_BIND: u64 = 4096;
    const FLAG_RELATIME: u64 = 1 << 21;

    let mut entries = Vec::new();
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        let Some(_fsname) = fields.next() else {
            continue;
        };
        let Some(mnt_dir_raw) = fields.next() else {
            continue;
        };
        let Some(_fstype) = fields.next() else {
            continue;
        };
        let Some(opts) = fields.next() else {
            continue;
        };
        let mut flags = FLAG_BIND | FLAG_REMOUNT;
        for tok in opts.split(',') {
            match tok {
                "nodev" => flags |= FLAG_NODEV,
                "noexec" => flags |= FLAG_NOEXEC,
                "nosuid" => flags |= FLAG_NOSUID,
                "noatime" => flags |= FLAG_NOATIME,
                "nodiratime" => flags |= FLAG_NODIRATIME,
                "relatime" => flags |= FLAG_RELATIME,
                _ => {}
            }
        }
        entries.push((unescape_mount_path(mnt_dir_raw), flags));
    }
    entries
}

/// Parses the `tty` group GID from `/etc/group` text (`SetupUserNamespace` in `linux-sandbox-pid1.cc:246-257`).
pub fn lookup_tty_gid_from_etc_group(contents: &str) -> Option<u32> {
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split(':');
        if parts.next() == Some("tty") {
            let _passwd = parts.next();
            if let Some(gid_str) = parts.next() {
                if let Ok(gid) = gid_str.parse::<u32>() {
                    return Some(gid);
                }
            }
        }
    }
    None
}

/// Resolves the host `tty` group GID from `/etc/group` (defaulting to `5` if absent).
pub fn lookup_tty_gid() -> u32 {
    std::fs::read_to_string("/etc/group")
        .ok()
        .and_then(|s| lookup_tty_gid_from_etc_group(&s))
        .unwrap_or(5)
}

/// Reads peak memory usage (bytes) from cgroup v2 (`memory.peak`) or v1
/// (`memory.max_usage_in_bytes`) directories passed via `-C`.
pub fn read_cgroup_memory_peak_bytes(cgroups_dirs: &[String]) -> Option<i64> {
    let mut max_bytes: Option<i64> = None;
    for dir in cgroups_dirs {
        let p = std::path::Path::new(dir);
        for file in ["memory.peak", "memory.max_usage_in_bytes"] {
            if let Ok(raw) = std::fs::read_to_string(p.join(file)) {
                if let Ok(bytes) = raw.trim().parse::<i64>() {
                    if bytes > 0 {
                        max_bytes = Some(match max_bytes {
                            Some(prev) => prev.max(bytes),
                            None => bytes,
                        });
                    }
                }
            }
        }
    }
    max_bytes
}

/// Encodes `ExecutionStatistics` with `rusage` and optional cgroup memory peak.
pub fn encode_sandbox_statistics(usage: &ResourceUsage, cgroups_dirs: &[String]) -> Vec<u8> {
    let mut adjusted = *usage;
    if let Some(peak_bytes) = read_cgroup_memory_peak_bytes(cgroups_dirs) {
        let peak_kb = peak_bytes / 1024;
        if peak_kb > adjusted.maxrss {
            adjusted.maxrss = peak_kb;
        }
    }
    adjusted.encode_execution_statistics()
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use crate::process_wrapper::{
        effective_redirect_path, open_redirect, reset_child_signals_and_mask, SIGALRM, SIGINT,
        SIGKILL, SIGTERM,
    };
    use std::collections::HashSet;
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int, c_ulong, c_void};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::IntoRawFd;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::time::{Duration, Instant};

    const CLONE_NEWNS: c_int = 0x0002_0000;
    const CLONE_NEWUTS: c_int = 0x0400_0000;
    const CLONE_NEWIPC: c_int = 0x0800_0000;
    const CLONE_NEWUSER: c_int = 0x1000_0000;
    const CLONE_NEWPID: c_int = 0x2000_0000;
    const CLONE_NEWNET: c_int = 0x4000_0000;

    const MS_RDONLY: c_ulong = 1;
    const MS_NOSUID: c_ulong = 2;
    const MS_NODEV: c_ulong = 4;
    const MS_NOEXEC: c_ulong = 8;
    const MS_NOATIME: c_ulong = 1024;
    const MS_BIND: c_ulong = 4096;
    const MS_REC: c_ulong = 16384;
    const MS_PRIVATE: c_ulong = 1 << 18;
    const MNT_DETACH: c_int = 2;

    const SIGPIPE: c_int = 13;
    const SIGTTIN: c_int = 21;
    const SIGTTOU: c_int = 22;
    const SIG_IGN_HANDLER: usize = 1;

    const PR_SET_PDEATHSIG: c_int = 1;
    const F_GETFD: c_int = 1;
    const O_RDWR: c_int = 2;
    const WNOHANG: c_int = 1;

    const AF_INET: c_int = 2;
    const SOCK_DGRAM: c_int = 2;
    const SIOCGIFFLAGS: c_ulong = 0x8913;
    const SIOCSIFFLAGS: c_ulong = 0x8914;
    const IFF_UP: i16 = 0x1;
    const IFF_RUNNING: i16 = 0x40;

    #[cfg(target_arch = "x86_64")]
    const SYS_PIVOT_ROOT: i64 = 155;
    #[cfg(target_arch = "aarch64")]
    const SYS_PIVOT_ROOT: i64 = 41;
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    const SYS_PIVOT_ROOT: i64 = -1;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Timeval {
        tv_sec: i64,
        tv_usec: i64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rusage {
        ru_utime: Timeval,
        ru_stime: Timeval,
        ru_maxrss: i64,
        ru_ixrss: i64,
        ru_idrss: i64,
        ru_isrss: i64,
        ru_minflt: i64,
        ru_majflt: i64,
        ru_nswap: i64,
        ru_inblock: i64,
        ru_oublock: i64,
        ru_msgsnd: i64,
        ru_msgrcv: i64,
        ru_nsignals: i64,
        ru_nvcsw: i64,
        ru_nivcsw: i64,
    }

    #[repr(C)]
    struct IfReq {
        ifr_name: [u8; 16],
        ifr_flags: i16,
        _pad: [u8; 22],
    }

    extern "C" {
        fn unshare(flags: c_int) -> c_int;
        fn fork() -> c_int;
        fn _exit(status: c_int) -> !;
        fn getpid() -> c_int;
        fn geteuid() -> u32;
        fn getegid() -> u32;
        fn pipe(pipefd: *mut c_int) -> c_int;
        fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
        fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
        fn setpgid(pid: c_int, pgid: c_int) -> c_int;
        fn tcsetpgrp(fd: c_int, pgrp: c_int) -> c_int;
        fn umask(mask: u32) -> u32;
        fn chdir(path: *const c_char) -> c_int;
        fn chroot(path: *const c_char) -> c_int;
        fn sethostname(name: *const c_char, len: usize) -> c_int;
        fn mount(
            source: *const c_char,
            target: *const c_char,
            filesystemtype: *const c_char,
            mountflags: c_ulong,
            data: *const c_void,
        ) -> c_int;
        fn umount2(target: *const c_char, flags: c_int) -> c_int;
        fn syscall(num: i64, ...) -> i64;
        fn prctl(option: c_int, arg2: usize, arg3: usize, arg4: usize, arg5: usize) -> c_int;
        fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
        fn open(path: *const c_char, flags: c_int, ...) -> c_int;
        fn dup2(oldfd: c_int, newfd: c_int) -> c_int;
        fn close(fd: c_int) -> c_int;
        fn execvp(file: *const c_char, argv: *const *const c_char) -> c_int;
        fn kill(pid: c_int, sig: c_int) -> c_int;
        fn signal(sig: c_int, handler: usize) -> usize;
        fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
        fn wait4(pid: c_int, status: *mut c_int, options: c_int, rusage: *mut Rusage) -> c_int;
        fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int;
        fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    }

    static OUTER_SIG: AtomicI32 = AtomicI32::new(0);
    static PID1_FWD_PID: AtomicI32 = AtomicI32::new(0);

    extern "C" fn on_outer_sig(sig: c_int) {
        OUTER_SIG.store(sig, Ordering::SeqCst);
    }

    extern "C" fn on_pid1_sig(sig: c_int) {
        let child = PID1_FWD_PID.load(Ordering::SeqCst);
        if child > 0 {
            unsafe {
                let _ = kill(-child, sig);
                let _ = kill(child, sig);
            }
        }
    }

    fn cstr(s: &str) -> Result<CString, String> {
        CString::new(s).map_err(|_| format!("path contains NUL: {s}"))
    }

    fn cpath(p: &Path) -> Result<CString, String> {
        CString::new(p.as_os_str().as_bytes())
            .map_err(|_| format!("path contains NUL: {}", p.display()))
    }

    fn normalize_mnt_key(p: &str) -> String {
        let trimmed = p.trim_end_matches('/');
        if trimmed.is_empty() {
            "/".to_string()
        } else {
            trimmed.to_string()
        }
    }

    fn insert_writable_mount(writable_mounts: &mut HashSet<String>, path: &str) {
        writable_mounts.insert(normalize_mnt_key(path));
        if let Ok(canon) = std::fs::canonicalize(path) {
            writable_mounts.insert(normalize_mnt_key(&canon.to_string_lossy()));
        }
    }

    fn to_hermetic_paths(sbx: &str, raw_path: &str) -> (PathBuf, String) {
        let norm_raw = normalize_mnt_key(raw_path);
        if norm_raw == sbx || norm_raw == "/" {
            return (PathBuf::from(sbx), "/".to_string());
        }
        if let Some(rel) = norm_raw.strip_prefix(sbx).and_then(|r| r.strip_prefix('/')) {
            return (PathBuf::from(&norm_raw), format!("/{rel}"));
        }
        let rel = norm_raw.trim_start_matches('/');
        (Path::new(sbx).join(rel), format!("/{rel}"))
    }

    unsafe fn ensure_standard_fds() {
        let devnull = c"/dev/null".as_ptr();
        for fd in 0..=2 {
            if fcntl(fd, F_GETFD) < 0 {
                let opened = open(devnull, O_RDWR);
                if opened >= 0 && opened != fd {
                    let _ = dup2(opened, fd);
                    let _ = close(opened);
                }
            }
        }
    }

    fn write_proc_id_maps(
        host_uid: u32,
        host_gid: u32,
        fake_root: bool,
        fake_username: bool,
        tty_gid: Option<u32>,
    ) -> Result<(), String> {
        let inner_uid = if fake_root {
            0
        } else if fake_username {
            65534
        } else {
            host_uid
        };
        // Per `SetupUserNamespace` (`linux-sandbox-pid1.cc:246-257`), `-P` overrides `inner_gid`
        // to the `tty` group regardless of `-R` / `-U`.
        let inner_gid = if let Some(gid) = tty_gid {
            gid
        } else if fake_root {
            0
        } else if fake_username {
            65534
        } else {
            host_gid
        };
        if Path::new("/proc/self/setgroups").exists() {
            let _ = std::fs::write("/proc/self/setgroups", "deny\n");
        }
        std::fs::write("/proc/self/uid_map", format!("{inner_uid} {host_uid} 1\n"))
            .map_err(|e| format!("writing /proc/self/uid_map: {e}"))?;
        std::fs::write("/proc/self/gid_map", format!("{inner_gid} {host_gid} 1\n"))
            .map_err(|e| format!("writing /proc/self/gid_map: {e}"))?;
        Ok(())
    }

    unsafe fn bring_up_loopback() -> Result<(), String> {
        let fd = socket(AF_INET, SOCK_DGRAM, 0);
        if fd < 0 {
            return Err(format!(
                "socket(AF_INET) for loopback failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut ifr = IfReq {
            ifr_name: [0u8; 16],
            ifr_flags: 0,
            _pad: [0u8; 22],
        };
        ifr.ifr_name[0] = b'l';
        ifr.ifr_name[1] = b'o';
        let rc_get = ioctl(fd, SIOCGIFFLAGS, &mut ifr as *mut IfReq);
        if rc_get < 0 {
            let err = std::io::Error::last_os_error();
            let _ = close(fd);
            return Err(format!("SIOCGIFFLAGS(lo) failed: {err}"));
        }
        ifr.ifr_flags |= IFF_UP | IFF_RUNNING;
        let rc_set = ioctl(fd, SIOCSIFFLAGS, &ifr as *const IfReq);
        let err = std::io::Error::last_os_error();
        let _ = close(fd);
        if rc_set < 0 {
            return Err(format!("SIOCSIFFLAGS(lo) failed: {err}"));
        }
        Ok(())
    }

    unsafe fn ensure_mount_target(src_p: &Path, dst_p: &Path) {
        if !dst_p.exists() {
            if src_p.is_dir() {
                let _ = std::fs::create_dir_all(dst_p);
            } else {
                if let Some(parent) = dst_p.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::File::create(dst_p);
            }
        }
    }

    unsafe fn setup_hermetic_dev(sbx: &Path) -> Result<(), String> {
        let dev_dir = sbx.join("dev");
        std::fs::create_dir_all(&dev_dir)
            .map_err(|e| format!("mkdir {}: {e}", dev_dir.display()))?;
        for dev_name in ["null", "random", "urandom", "zero", "full", "ptmx"] {
            let host_dev = PathBuf::from(format!("/dev/{dev_name}"));
            if std::fs::symlink_metadata(&host_dev).is_err() {
                continue;
            }
            let dst_dev = dev_dir.join(dev_name);
            if !dst_dev.exists() {
                let _ = std::fs::File::create(&dst_dev);
            }
            let cs = cpath(&host_dev)?;
            let cd = cpath(&dst_dev)?;
            if mount(
                cs.as_ptr(),
                cd.as_ptr(),
                std::ptr::null(),
                MS_BIND,
                std::ptr::null(),
            ) < 0
            {
                return Err(format!(
                    "bind mount /dev/{dev_name} -> {}: {}",
                    dst_dev.display(),
                    std::io::Error::last_os_error()
                ));
            }
        }
        if Path::new("/dev/pts").is_dir() {
            let dst_pts = dev_dir.join("pts");
            let _ = std::fs::create_dir_all(&dst_pts);
            let cd = cpath(&dst_pts)?;
            let _ = mount(
                c"/dev/pts".as_ptr(),
                cd.as_ptr(),
                std::ptr::null(),
                MS_BIND | MS_REC,
                std::ptr::null(),
            );
        }
        let dst_shm = dev_dir.join("shm");
        if std::fs::create_dir_all(&dst_shm).is_ok() {
            let cd = cpath(&dst_shm)?;
            let fstype = c"tmpfs".as_ptr();
            let mode = c"mode=1777".as_ptr() as *const c_void;
            if mount(fstype, cd.as_ptr(), fstype, MS_NOSUID | MS_NODEV, mode) < 0 {
                return Err(format!(
                    "mount tmpfs on {}: {}",
                    dst_shm.display(),
                    std::io::Error::last_os_error()
                ));
            }
        }
        for (target, link_name) in [
            ("/proc/self/fd", "fd"),
            ("/proc/self/fd/0", "stdin"),
            ("/proc/self/fd/1", "stdout"),
            ("/proc/self/fd/2", "stderr"),
        ] {
            let link_path = dev_dir.join(link_name);
            if std::fs::symlink_metadata(&link_path).is_err() {
                let _ = std::os::unix::fs::symlink(target, &link_path);
            }
        }
        Ok(())
    }

    /// Remounts every mount in `/proc/self/mounts` not in `writable_mounts` with `MS_RDONLY`,
    /// retrying `EBUSY` up to 5 times and ignoring benign overlapped/pseudo-mount errors
    /// (`MakeFilesystemMostlyReadOnly` in `linux-sandbox-pid1.cc:365-453`).
    unsafe fn make_filesystem_mostly_read_only(
        writable_mounts: &HashSet<String>,
    ) -> Result<(), String> {
        const EPERM: i32 = 1;
        const ENOENT: i32 = 2;
        const EACCES: i32 = 13;
        const EBUSY: i32 = 16;
        const ENODEV: i32 = 19;
        const EINVAL: i32 = 22;
        const EROFS: i32 = 30;
        const ESTALE: i32 = 116;

        let raw_mounts = std::fs::read_to_string("/proc/self/mounts")
            .map_err(|e| format!("read /proc/self/mounts: {e}"))?;
        for (mnt_dir, base_flags) in parse_proc_mounts(&raw_mounts) {
            let key = normalize_mnt_key(&mnt_dir);
            if writable_mounts.contains(&key) {
                continue;
            }
            let Ok(cmnt) = cstr(&mnt_dir) else {
                continue;
            };
            let flags = (base_flags as c_ulong) | MS_RDONLY;
            let mut rc: c_int = -1;
            let mut last_err = std::io::Error::last_os_error();
            for attempt in 0..5 {
                rc = mount(
                    std::ptr::null(),
                    cmnt.as_ptr(),
                    std::ptr::null(),
                    flags,
                    std::ptr::null(),
                );
                if rc == 0 {
                    break;
                }
                last_err = std::io::Error::last_os_error();
                if last_err.raw_os_error() != Some(EBUSY) || attempt == 4 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if rc < 0 {
                match last_err.raw_os_error() {
                    Some(EACCES | EPERM | EINVAL | ENOENT | ESTALE | ENODEV | EROFS) => {}
                    _ => {
                        return Err(format!(
                            "remount({}, MS_RDONLY) failed: {last_err}",
                            mnt_dir
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    unsafe fn run_pid1(
        opt: &LinuxSandboxOptions,
        pipe_from_child: [c_int; 2],
        pipe_to_child: [c_int; 2],
    ) -> ! {
        let _ = close(pipe_from_child[0]);
        let _ = close(pipe_to_child[1]);
        let _ = signal(SIGPIPE, SIG_IGN_HANDLER);
        if !opt.persistent_process && prctl(PR_SET_PDEATHSIG, SIGKILL as usize, 0, 0, 0) < 0 {
            let _ = close(pipe_from_child[1]);
            let _ = close(pipe_to_child[0]);
            _exit(1);
        }
        let _ = setpgid(0, 0);
        // Two-pipe handshake with the outer supervisor (`SpawnPid1` / `SetupSelfDestruction` in
        // `linux-sandbox.cc:170-217` and `linux-sandbox-pid1.cc:180-199`): proves the parent was
        // still alive after `PR_SET_PDEATHSIG` was armed (`getppid()` inside a new PID namespace
        // is `0`, not the outer supervisor's host PID).
        let ready: u8 = b'R';
        let wrote = write(
            pipe_from_child[1],
            (&ready as *const u8).cast::<c_void>(),
            1,
        );
        let _ = close(pipe_from_child[1]);
        if wrote != 1 {
            let _ = close(pipe_to_child[0]);
            _exit(1);
        }
        let mut go: u8 = 0;
        let got = read(pipe_to_child[0], (&mut go as *mut u8).cast::<c_void>(), 1);
        let _ = close(pipe_to_child[0]);
        if got != 1 {
            _exit(1);
        }

        // Create private mount, IPC, UTS, and optional network namespaces inside `pid1` so the
        // outer supervisor process remains in the host mount namespace when writing `-S` stats.
        let mut inner_ns = CLONE_NEWNS | CLONE_NEWIPC | CLONE_NEWUTS;
        if opt.create_netns != NetworkNamespace::NoNetns {
            inner_ns |= CLONE_NEWNET;
        }
        if unshare(inner_ns) < 0 {
            eprintln!(
                "linux-sandbox-pid1: unshare(mnt/ipc/uts/net): {}",
                std::io::Error::last_os_error()
            );
            _exit(1);
        }

        let root_c = c"/".as_ptr();
        if mount(
            std::ptr::null(),
            root_c,
            std::ptr::null(),
            MS_REC | MS_PRIVATE,
            std::ptr::null(),
        ) < 0
        {
            eprintln!(
                "linux-sandbox-pid1: mount(/, MS_REC|MS_PRIVATE): {}",
                std::io::Error::last_os_error()
            );
            _exit(1);
        }

        if opt.create_netns == NetworkNamespace::NetnsWithLoopback {
            if let Err(e) = bring_up_loopback() {
                eprintln!("linux-sandbox-pid1: {e}");
                _exit(1);
            }
        }

        if opt.fake_hostname {
            let localhost = b"localhost";
            if sethostname(localhost.as_ptr() as *const c_char, localhost.len()) < 0 {
                eprintln!(
                    "linux-sandbox-pid1: sethostname(localhost): {}",
                    std::io::Error::last_os_error()
                );
                _exit(1);
            }
        }

        let default_cwd = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string());
        let host_wd = opt.working_dir.as_deref().unwrap_or(&default_cwd);

        let target_cwd = if let Some(sbx_raw) = &opt.hermetic_sandbox_dir {
            let sbx = normalize_mnt_key(sbx_raw);
            let sp = Path::new(&sbx);
            let Ok(cs) = cpath(sp) else {
                _exit(1);
            };
            if mount(
                cs.as_ptr(),
                cs.as_ptr(),
                std::ptr::null(),
                MS_BIND | MS_REC,
                std::ptr::null(),
            ) < 0
            {
                eprintln!(
                    "linux-sandbox-pid1: bind mount sandbox root {sbx}: {}",
                    std::io::Error::last_os_error()
                );
                _exit(1);
            }

            if let Err(e) = setup_hermetic_dev(sp) {
                eprintln!("linux-sandbox-pid1: {e}");
                _exit(1);
            }

            let proc_in_sbx = sp.join("proc");
            let sys_in_sbx = sp.join("sys");
            let _ = std::fs::create_dir_all(&proc_in_sbx);
            let _ = std::fs::create_dir_all(&sys_in_sbx);
            if let Ok(cp) = cpath(&proc_in_sbx) {
                if opt.persistent_process {
                    if Path::new("/proc").is_dir() {
                        let _ = mount(
                            c"/proc".as_ptr(),
                            cp.as_ptr(),
                            std::ptr::null(),
                            MS_BIND | MS_REC,
                            std::ptr::null(),
                        );
                    }
                } else if mount(
                    c"proc".as_ptr(),
                    cp.as_ptr(),
                    c"proc".as_ptr(),
                    MS_NOSUID | MS_NODEV | MS_NOEXEC,
                    std::ptr::null(),
                ) < 0
                {
                    eprintln!(
                        "linux-sandbox-pid1: mount proc on {}: {}",
                        proc_in_sbx.display(),
                        std::io::Error::last_os_error()
                    );
                    _exit(1);
                }
            }
            if let Ok(csys) = cpath(&sys_in_sbx) {
                if opt.create_netns != NetworkNamespace::NoNetns {
                    let _ = mount(
                        c"none".as_ptr(),
                        csys.as_ptr(),
                        c"sysfs".as_ptr(),
                        MS_NOEXEC | MS_NOSUID | MS_NODEV | MS_RDONLY,
                        std::ptr::null(),
                    );
                } else if Path::new("/sys").is_dir() {
                    let _ = mount(
                        c"/sys".as_ptr(),
                        csys.as_ptr(),
                        std::ptr::null(),
                        MS_BIND | MS_REC,
                        std::ptr::null(),
                    );
                }
            }

            let mut writable_mounts = HashSet::new();
            writable_mounts.insert("/proc".to_string());
            writable_mounts.insert("/dev/shm".to_string());
            if opt.enable_pty {
                writable_mounts.insert("/dev/pts".to_string());
            }

            for (src, dst) in &opt.bind_mounts {
                let src_p = Path::new(src);
                let (host_dst, _in_sbx_dst) = to_hermetic_paths(&sbx, dst);
                ensure_mount_target(src_p, &host_dst);
                let (Ok(cs_m), Ok(cd_m)) = (cpath(src_p), cpath(&host_dst)) else {
                    _exit(1);
                };
                if mount(
                    cs_m.as_ptr(),
                    cd_m.as_ptr(),
                    std::ptr::null(),
                    MS_BIND | MS_REC,
                    std::ptr::null(),
                ) < 0
                {
                    eprintln!(
                        "linux-sandbox-pid1: bind mount {} -> {}: {}",
                        src,
                        host_dst.display(),
                        std::io::Error::last_os_error()
                    );
                    _exit(1);
                }
            }

            for tmpfs_dir in &opt.tmpfs_dirs {
                let (host_tmpfs, in_sbx_tmpfs) = to_hermetic_paths(&sbx, tmpfs_dir);
                let _ = std::fs::create_dir_all(&host_tmpfs);
                let Ok(cd_t) = cpath(&host_tmpfs) else {
                    _exit(1);
                };
                let fstype = c"tmpfs".as_ptr();
                if mount(
                    fstype,
                    cd_t.as_ptr(),
                    fstype,
                    MS_NOSUID | MS_NODEV | MS_NOATIME,
                    std::ptr::null(),
                ) < 0
                {
                    eprintln!(
                        "linux-sandbox-pid1: tmpfs mount {}: {}",
                        host_tmpfs.display(),
                        std::io::Error::last_os_error()
                    );
                    _exit(1);
                }
                writable_mounts.insert(in_sbx_tmpfs);
            }

            let in_sbx_wd = if let Some(explicit_wd) = &opt.working_dir {
                let (host_wd_p, inner_wd) = to_hermetic_paths(&sbx, explicit_wd);
                if inner_wd != "/" {
                    let _ = std::fs::create_dir_all(&host_wd_p);
                    let Ok(cwd_c) = cpath(&host_wd_p) else {
                        _exit(1);
                    };
                    if mount(
                        cwd_c.as_ptr(),
                        cwd_c.as_ptr(),
                        std::ptr::null(),
                        MS_BIND | MS_REC,
                        std::ptr::null(),
                    ) < 0
                    {
                        eprintln!(
                            "linux-sandbox-pid1: bind mount working dir {}: {}",
                            host_wd_p.display(),
                            std::io::Error::last_os_error()
                        );
                        _exit(1);
                    }
                }
                writable_mounts.insert(inner_wd.clone());
                inner_wd
            } else {
                "/".to_string()
            };

            for w in &opt.writable_files {
                let (host_w, in_sbx_w) = to_hermetic_paths(&sbx, w);
                if std::fs::symlink_metadata(&host_w).is_ok() {
                    if let Ok(cw) = cpath(&host_w) {
                        if mount(
                            cw.as_ptr(),
                            cw.as_ptr(),
                            std::ptr::null(),
                            MS_BIND | MS_REC,
                            std::ptr::null(),
                        ) < 0
                        {
                            eprintln!(
                                "linux-sandbox-pid1: writable bind mount {}: {}",
                                host_w.display(),
                                std::io::Error::last_os_error()
                            );
                            _exit(1);
                        }
                    }
                    if let Ok(canon) = std::fs::canonicalize(&host_w) {
                        let (_, canon_inner) = to_hermetic_paths(&sbx, &canon.to_string_lossy());
                        writable_mounts.insert(canon_inner);
                    }
                }
                writable_mounts.insert(in_sbx_w);
            }

            if chdir(cs.as_ptr()) < 0 {
                eprintln!(
                    "linux-sandbox-pid1: chdir({sbx}): {}",
                    std::io::Error::last_os_error()
                );
                _exit(1);
            }
            let dot = c".".as_ptr();
            let pivoted = SYS_PIVOT_ROOT > 0 && syscall(SYS_PIVOT_ROOT, dot, dot) == 0;
            if pivoted {
                let _ = chroot(dot);
                let _ = umount2(dot, MNT_DETACH);
            } else if chroot(dot) < 0 {
                eprintln!(
                    "linux-sandbox-pid1: pivot_root/chroot({sbx}): {}",
                    std::io::Error::last_os_error()
                );
                _exit(1);
            }
            let _ = chdir(root_c);

            if let Err(e) = make_filesystem_mostly_read_only(&writable_mounts) {
                eprintln!("linux-sandbox-pid1: {e}");
                _exit(1);
            }
            in_sbx_wd
        } else {
            let mut writable_mounts = HashSet::new();
            writable_mounts.insert("/proc".to_string());
            if opt.enable_pty {
                writable_mounts.insert("/dev/pts".to_string());
            }

            for (src, dst) in &opt.bind_mounts {
                let src_p = Path::new(src);
                let dst_p = Path::new(dst);
                ensure_mount_target(src_p, dst_p);
                let (Ok(cs), Ok(cd)) = (cpath(src_p), cpath(dst_p)) else {
                    _exit(1);
                };
                if mount(
                    cs.as_ptr(),
                    cd.as_ptr(),
                    std::ptr::null(),
                    MS_BIND | MS_REC,
                    std::ptr::null(),
                ) < 0
                {
                    eprintln!(
                        "linux-sandbox-pid1: bind mount {} -> {}: {}",
                        src,
                        dst,
                        std::io::Error::last_os_error()
                    );
                    _exit(1);
                }
            }

            for tmpfs_dir in &opt.tmpfs_dirs {
                let p = Path::new(tmpfs_dir);
                let _ = std::fs::create_dir_all(p);
                let Ok(cd) = cpath(p) else {
                    _exit(1);
                };
                let fstype = c"tmpfs".as_ptr();
                if mount(
                    fstype,
                    cd.as_ptr(),
                    fstype,
                    MS_NOSUID | MS_NODEV | MS_NOATIME,
                    std::ptr::null(),
                ) < 0
                {
                    eprintln!(
                        "linux-sandbox-pid1: tmpfs mount {}: {}",
                        tmpfs_dir,
                        std::io::Error::last_os_error()
                    );
                    _exit(1);
                }
                insert_writable_mount(&mut writable_mounts, tmpfs_dir);
            }

            let wd_path = Path::new(host_wd);
            if wd_path.exists() {
                let Ok(cwd_c) = cpath(wd_path) else {
                    _exit(1);
                };
                if mount(
                    cwd_c.as_ptr(),
                    cwd_c.as_ptr(),
                    std::ptr::null(),
                    MS_BIND | MS_REC,
                    std::ptr::null(),
                ) < 0
                {
                    eprintln!(
                        "linux-sandbox-pid1: bind mount working dir {host_wd}: {}",
                        std::io::Error::last_os_error()
                    );
                    _exit(1);
                }
                insert_writable_mount(&mut writable_mounts, host_wd);
            }

            for w in &opt.writable_files {
                let wp = Path::new(w);
                if std::fs::symlink_metadata(wp).is_ok() {
                    let Ok(cw) = cpath(wp) else {
                        _exit(1);
                    };
                    if mount(
                        cw.as_ptr(),
                        cw.as_ptr(),
                        std::ptr::null(),
                        MS_BIND | MS_REC,
                        std::ptr::null(),
                    ) < 0
                    {
                        eprintln!(
                            "linux-sandbox-pid1: writable bind mount {w}: {}",
                            std::io::Error::last_os_error()
                        );
                        _exit(1);
                    }
                    insert_writable_mount(&mut writable_mounts, w);
                }
            }

            if !opt.persistent_process && Path::new("/proc").is_dir() {
                let proc_c = c"/proc".as_ptr();
                let proc_fs = c"proc".as_ptr();
                let _ = mount(
                    proc_fs,
                    proc_c,
                    proc_fs,
                    MS_NOSUID | MS_NODEV | MS_NOEXEC,
                    std::ptr::null(),
                );
            }

            if let Err(e) = make_filesystem_mostly_read_only(&writable_mounts) {
                eprintln!("linux-sandbox-pid1: {e}");
                _exit(1);
            }
            host_wd.to_string()
        };

        let Ok(cwd_c) = cstr(&target_cwd) else {
            _exit(1);
        };
        if chdir(cwd_c.as_ptr()) < 0 {
            eprintln!(
                "linux-sandbox-pid1: chdir({target_cwd}): {}",
                std::io::Error::last_os_error()
            );
            _exit(1);
        }

        let _ = signal(SIGTTIN, SIG_IGN_HANDLER);
        let _ = signal(SIGTTOU, SIG_IGN_HANDLER);

        let cmd_pid = fork();
        if cmd_pid < 0 {
            eprintln!(
                "linux-sandbox-pid1: fork: {}",
                std::io::Error::last_os_error()
            );
            _exit(1);
        }
        if cmd_pid == 0 {
            let _ = setpgid(0, 0);
            let _ = tcsetpgrp(0, getpid());
            reset_child_signals_and_mask();
            umask(0o022);
            let c_args: Vec<CString> = opt
                .args
                .iter()
                .map(|s| CString::new(s.as_bytes()).unwrap_or_default())
                .collect();
            let mut ptrs: Vec<*const c_char> = c_args.iter().map(|s| s.as_ptr()).collect();
            ptrs.push(std::ptr::null());
            let _ = execvp(ptrs[0], ptrs.as_ptr());
            let err = std::io::Error::last_os_error();
            eprintln!(
                "src/main/tools/linux-sandbox-pid1.cc:584: \"execvp({}, ...)\": {err}",
                opt.args[0]
            );
            _exit(1);
        }

        PID1_FWD_PID.store(cmd_pid, Ordering::SeqCst);
        let _ = signal(SIGTERM, on_pid1_sig as *const () as usize);
        let _ = signal(SIGINT, on_pid1_sig as *const () as usize);

        let mut cmd_status: c_int = 1 << 8;
        loop {
            let mut st: c_int = 0;
            let waited = waitpid(-1, &mut st, 0);
            if waited == cmd_pid {
                cmd_status = st;
                break;
            }
            if waited < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
        }
        let _ = kill(-cmd_pid, SIGKILL);
        if cmd_status & 0x7f == 0 {
            _exit((cmd_status >> 8) & 0xff);
        } else {
            _exit(128 + (cmd_status & 0x7f));
        }
    }

    pub fn run_linux_sandbox(
        opt: &LinuxSandboxOptions,
    ) -> Result<(Termination, Option<i32>), String> {
        OUTER_SIG.store(0, Ordering::SeqCst);

        unsafe {
            ensure_standard_fds();
        }

        if let Some(p) = effective_redirect_path(opt.stdout_path.as_deref()) {
            let f = open_redirect(p).map_err(|e| format!("redirect stdout '{p}': {e}"))?;
            let raw = f.into_raw_fd();
            if raw != 1 {
                unsafe {
                    let _ = dup2(raw, 1);
                    let _ = close(raw);
                }
            }
        }
        if let Some(p) = effective_redirect_path(opt.stderr_path.as_deref()) {
            let f = open_redirect(p).map_err(|e| format!("redirect stderr '{p}': {e}"))?;
            let raw = f.into_raw_fd();
            if raw != 2 {
                unsafe {
                    let _ = dup2(raw, 2);
                    let _ = close(raw);
                }
            }
        }

        let outer_pid = unsafe { getpid() };
        // Place this sandbox supervisor (and therefore its forked `pid1` and `cmd_pid` children)
        // into all requested `-C` cgroups while still in the host user, mount, and PID namespaces.
        join_cgroups(&opt.cgroups_dirs, outer_pid)?;
        let tty_gid = opt.enable_pty.then(lookup_tty_gid);

        let host_uid = unsafe { geteuid() };
        let host_gid = unsafe { getegid() };

        let mut ns_flags = CLONE_NEWUSER;
        if !opt.persistent_process {
            ns_flags |= CLONE_NEWPID;
        }

        if unsafe { unshare(ns_flags) } < 0 {
            return Err(format!(
                "unshare namespaces failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        write_proc_id_maps(
            host_uid,
            host_gid,
            opt.fake_root,
            opt.fake_username,
            tty_gid,
        )?;

        unsafe {
            let _ = signal(SIGTTIN, SIG_IGN_HANDLER);
            let _ = signal(SIGTTOU, SIG_IGN_HANDLER);
            let _ = signal(SIGALRM, on_outer_sig as *const () as usize);
            let _ = signal(SIGTERM, on_outer_sig as *const () as usize);
            let _ = signal(SIGINT, on_outer_sig as *const () as usize);
        }

        let mut pipe_from_child = [-1_i32; 2];
        let mut pipe_to_child = [-1_i32; 2];
        if unsafe { pipe(pipe_from_child.as_mut_ptr()) } < 0 {
            return Err(format!(
                "pipe(from_child) failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        if unsafe { pipe(pipe_to_child.as_mut_ptr()) } < 0 {
            unsafe {
                let _ = close(pipe_from_child[0]);
                let _ = close(pipe_from_child[1]);
            }
            return Err(format!(
                "pipe(to_child) failed: {}",
                std::io::Error::last_os_error()
            ));
        }

        let started = Instant::now();
        let pid1 = unsafe { fork() };
        if pid1 < 0 {
            let err = std::io::Error::last_os_error();
            unsafe {
                let _ = close(pipe_from_child[0]);
                let _ = close(pipe_from_child[1]);
                let _ = close(pipe_to_child[0]);
                let _ = close(pipe_to_child[1]);
            }
            return Err(format!("fork pid1 failed: {err}"));
        }
        if pid1 == 0 {
            unsafe { run_pid1(opt, pipe_from_child, pipe_to_child) }
        }

        unsafe {
            let _ = close(pipe_from_child[1]);
            let _ = close(pipe_to_child[0]);
            let _ = signal(SIGPIPE, SIG_IGN_HANDLER);
            let mut ready: u8 = 0;
            loop {
                let n = read(
                    pipe_from_child[0],
                    (&mut ready as *mut u8).cast::<c_void>(),
                    1,
                );
                if n < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                    continue;
                }
                break;
            }
            let _ = close(pipe_from_child[0]);
            let go: u8 = b'G';
            let _ = write(pipe_to_child[1], (&go as *const u8).cast::<c_void>(), 1);
            let _ = close(pipe_to_child[1]);
        }

        let timeout = (opt.timeout_secs > 0).then(|| Duration::from_secs(opt.timeout_secs as u64));
        let kill_delay = Duration::from_secs(opt.kill_delay_secs.max(0) as u64);
        let mut term_sent: Option<Instant> = None;
        let mut killed = false;
        let mut wrapper_sig: Option<i32> = None;

        let mut status: c_int = 0;
        let mut ru = Rusage::default();
        loop {
            let r = unsafe { wait4(pid1, &mut status, WNOHANG, &mut ru) };
            if r == pid1 {
                break;
            }
            if r < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!("wait4(pid1) failed: {e}"));
            }
            let got = OUTER_SIG.swap(0, Ordering::SeqCst);
            if got != 0 {
                wrapper_sig = Some(got);
                let graceful = got != SIGINT || opt.sigint_sends_sigterm;
                if graceful {
                    if term_sent.is_none() {
                        unsafe {
                            let _ = kill(pid1, SIGTERM);
                        }
                        term_sent = Some(Instant::now());
                    }
                } else if !killed {
                    unsafe {
                        let _ = kill(pid1, SIGKILL);
                    }
                    killed = true;
                }
            }
            if let Some(t) = timeout {
                if wrapper_sig.is_none() && started.elapsed() >= t {
                    wrapper_sig = Some(SIGALRM);
                    if term_sent.is_none() {
                        unsafe {
                            let _ = kill(pid1, SIGTERM);
                        }
                        term_sent = Some(Instant::now());
                    }
                }
            }
            if let Some(at) = term_sent {
                if !killed && at.elapsed() >= kill_delay {
                    unsafe {
                        let _ = kill(pid1, SIGKILL);
                    }
                    killed = true;
                }
            }
            std::thread::sleep(Duration::from_micros(500));
        }

        let usage = ResourceUsage {
            utime_sec: ru.ru_utime.tv_sec,
            utime_usec: ru.ru_utime.tv_usec,
            stime_sec: ru.ru_stime.tv_sec,
            stime_usec: ru.ru_stime.tv_usec,
            maxrss: ru.ru_maxrss,
            ixrss: ru.ru_ixrss,
            idrss: ru.ru_idrss,
            isrss: ru.ru_isrss,
            minflt: ru.ru_minflt,
            majflt: ru.ru_majflt,
            nswap: ru.ru_nswap,
            inblock: ru.ru_inblock,
            oublock: ru.ru_oublock,
            msgsnd: ru.ru_msgsnd,
            msgrcv: ru.ru_msgrcv,
            nsignals: ru.ru_nsignals,
            nvcsw: ru.ru_nvcsw,
            nivcsw: ru.ru_nivcsw,
        };
        if let Some(stats_file) = opt.stats_path.as_deref().filter(|s| !s.is_empty()) {
            std::fs::write(
                stats_file,
                encode_sandbox_statistics(&usage, &opt.cgroups_dirs),
            )
            .map_err(|e| format!("write stats '{stats_file}': {e}"))?;
        }

        let termination = if status & 0x7f == 0 {
            Termination::Exited((status >> 8) & 0xff)
        } else {
            Termination::Signaled(status & 0x7f)
        };
        Ok((termination, wrapper_sig))
    }
}

#[cfg(target_os = "linux")]
pub use imp::run_linux_sandbox;

#[cfg(not(target_os = "linux"))]
pub fn run_linux_sandbox(_opt: &LinuxSandboxOptions) -> Result<(Termination, Option<i32>), String> {
    Err("linux-sandbox is only supported on Linux".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_all_linux_sandbox_flags_and_param_files() {
        let dir = std::env::temp_dir().join(format!("zel-ls-opt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pf = dir.join("params");
        std::fs::write(&pf, "-W\n/work\n-M\n/src1\n-m\n/dst1\n-M\n/same\n").unwrap();
        let pf_arg = format!("@{}", pf.display());

        let opt = parse_linux_sandbox_args(
            "linux-sandbox",
            &s(&[
                &pf_arg,
                "-T",
                "30",
                "-t",
                "5",
                "-l",
                "/out",
                "-L",
                "/err",
                "-w",
                "/rw",
                "-e",
                "/tmp",
                "-S",
                "/stats",
                "-h",
                "/sbx",
                "-C",
                "/sys/fs/cgroup/test",
                "-HNipU",
                "--",
                "/bin/echo",
                "hello",
            ]),
        )
        .unwrap();
        assert_eq!(opt.working_dir.as_deref(), Some("/work"));
        assert_eq!(opt.timeout_secs, 30);
        assert_eq!(opt.kill_delay_secs, 5);
        assert_eq!(opt.stdout_path.as_deref(), Some("/out"));
        assert_eq!(opt.stderr_path.as_deref(), Some("/err"));
        assert_eq!(opt.writable_files, vec!["/rw"]);
        assert_eq!(opt.tmpfs_dirs, vec!["/tmp"]);
        assert_eq!(
            opt.bind_mounts,
            vec![
                ("/src1".to_string(), "/dst1".to_string()),
                ("/same".to_string(), "/same".to_string()),
            ]
        );
        assert_eq!(opt.stats_path.as_deref(), Some("/stats"));
        assert_eq!(opt.hermetic_sandbox_dir.as_deref(), Some("/sbx"));
        assert_eq!(opt.cgroups_dirs, vec!["/sys/fs/cgroup/test"]);
        assert!(opt.fake_hostname);
        assert_eq!(opt.create_netns, NetworkNamespace::NetnsWithLoopback);
        assert!(opt.sigint_sends_sigterm);
        assert!(opt.persistent_process);
        assert!(opt.fake_username);
        assert_eq!(opt.args, vec!["/bin/echo", "hello"]);

        // Mutual exclusions and `-m` ordering checks.
        assert!(parse_linux_sandbox_args("ls", &s(&["-R", "-U", "--", "true"])).is_err());
        assert!(parse_linux_sandbox_args("ls", &s(&["-n", "-N", "--", "true"])).is_err());
        assert!(parse_linux_sandbox_args("ls", &s(&["-m", "/dst", "--", "true"])).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cgroup_memory_peak_merges_into_execution_statistics() {
        let dir = std::env::temp_dir().join(format!("zel-ls-cg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("memory.peak"), "10485760\n").unwrap(); // 10 MiB = 10240 KiB
        let u = ResourceUsage {
            utime_sec: 2,
            maxrss: 1024,
            ..Default::default()
        };
        let encoded = encode_sandbox_statistics(&u, &[dir.to_string_lossy().into_owned()]);
        let decoded = ResourceUsage::decode_execution_statistics(&encoded).unwrap();
        assert_eq!(decoded.utime_sec, 2);
        assert_eq!(decoded.maxrss, 10240);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn join_cgroups_mounts_parsing_and_tty_gid_lookup() {
        let dir = std::env::temp_dir().join(format!("zel-ls-join-cg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cg1 = dir.join("cg1");
        let cg2 = dir.join("cg2");
        std::fs::create_dir_all(&cg1).unwrap();
        std::fs::create_dir_all(&cg2).unwrap();

        join_cgroups(
            &[
                cg1.to_string_lossy().into_owned(),
                cg2.to_string_lossy().into_owned(),
            ],
            4242,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(cg1.join("cgroup.procs")).unwrap(),
            "4242"
        );
        assert_eq!(
            std::fs::read_to_string(cg2.join("cgroup.procs")).unwrap(),
            "4242"
        );
        assert!(join_cgroups(&[dir.join("missing").to_string_lossy().into_owned()], 4242).is_err());

        std::fs::write(cg1.join("memory.peak"), "4096000\n").unwrap();
        std::fs::write(cg2.join("memory.max_usage_in_bytes"), "8192000\n").unwrap();
        assert_eq!(
            read_cgroup_memory_peak_bytes(&[
                cg1.to_string_lossy().into_owned(),
                cg2.to_string_lossy().into_owned(),
            ]),
            Some(8_192_000)
        );

        assert_eq!(
            unescape_mount_path(r"/mnt/with\040space\134slash"),
            r"/mnt/with space\slash"
        );
        let parsed = parse_proc_mounts(
            "/dev/sda1 / ext4 rw,relatime 0 0\n\
             devpts /dev/pts devpts rw,nosuid,noexec,relatime 0 0\n\
             tmpfs /mnt/my\\040tmp tmpfs rw,nodev,noatime 0 0\n",
        );
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].0, "/");
        assert_ne!(parsed[0].1 & (1 << 21), 0); // MS_RELATIME
        assert_eq!(parsed[1].0, "/dev/pts");
        assert_ne!(parsed[1].1 & 2, 0); // MS_NOSUID
        assert_ne!(parsed[1].1 & 8, 0); // MS_NOEXEC
        assert_eq!(parsed[2].0, "/mnt/my tmp");
        assert_ne!(parsed[2].1 & 4, 0); // MS_NODEV
        assert_ne!(parsed[2].1 & 1024, 0); // MS_NOATIME

        assert_eq!(
            lookup_tty_gid_from_etc_group("root:x:0:\ntty:x:5:syslog\nnogroup:x:65534:\n"),
            Some(5)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
