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

/// Reads peak memory usage (bytes) from cgroup v2 (`memory.peak`) or v1
/// (`memory.max_usage_in_bytes`) directories passed via `-C`.
pub fn read_cgroup_memory_peak_bytes(cgroups_dirs: &[String]) -> Option<i64> {
    for dir in cgroups_dirs {
        let p = std::path::Path::new(dir);
        for file in ["memory.peak", "memory.max_usage_in_bytes"] {
            if let Ok(raw) = std::fs::read_to_string(p.join(file)) {
                if let Ok(bytes) = raw.trim().parse::<i64>() {
                    if bytes > 0 {
                        return Some(bytes);
                    }
                }
            }
        }
    }
    None
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
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int, c_ulong, c_void};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::IntoRawFd;
    use std::path::Path;
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
    const MS_REMOUNT: c_ulong = 32;
    const MS_NOATIME: c_ulong = 1024;
    const MS_BIND: c_ulong = 4096;
    const MS_REC: c_ulong = 16384;
    const MS_PRIVATE: c_ulong = 1 << 18;
    const MNT_DETACH: c_int = 2;

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
        fn geteuid() -> u32;
        fn getegid() -> u32;
        fn getppid() -> c_int;
        fn setsid() -> c_int;
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
        fn posix_openpt(flags: c_int) -> c_int;
        fn grantpt(fd: c_int) -> c_int;
        fn unlockpt(fd: c_int) -> c_int;
        fn ptsname(fd: c_int) -> *const c_char;
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
    ) -> Result<(), String> {
        let inner_uid = if fake_root {
            0
        } else if fake_username {
            65534
        } else {
            host_uid
        };
        let inner_gid = if fake_root {
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

    unsafe fn setup_pty() -> Result<(), String> {
        const TIOCSCTTY: c_ulong = 0x540E;
        let master = posix_openpt(O_RDWR);
        if master < 0 {
            return Err(format!(
                "posix_openpt failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        if grantpt(master) < 0 || unlockpt(master) < 0 {
            let err = std::io::Error::last_os_error();
            let _ = close(master);
            return Err(format!("grantpt/unlockpt failed: {err}"));
        }
        let slave_ptr = ptsname(master);
        if slave_ptr.is_null() {
            let err = std::io::Error::last_os_error();
            let _ = close(master);
            return Err(format!("ptsname failed: {err}"));
        }
        let _ = setsid();
        let slave = open(slave_ptr, O_RDWR);
        if slave < 0 {
            let err = std::io::Error::last_os_error();
            let _ = close(master);
            return Err(format!("open(pts) failed: {err}"));
        }
        let _ = ioctl(slave, TIOCSCTTY, 0);
        for fd in 0..=2 {
            let _ = dup2(slave, fd);
        }
        if slave > 2 {
            let _ = close(slave);
        }
        let _ = close(master);
        Ok(())
    }

    unsafe fn run_pid1(opt: &LinuxSandboxOptions) -> ! {
        if prctl(PR_SET_PDEATHSIG, SIGKILL as usize, 0, 0, 0) < 0 || getppid() == 1 {
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

        for (src, dst) in &opt.bind_mounts {
            let src_p = Path::new(src);
            let dst_p = Path::new(dst);
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
        }

        for w in &opt.writable_files {
            let wp = Path::new(w);
            if wp.exists() {
                if let Ok(cw) = cpath(wp) {
                    let _ = mount(
                        cw.as_ptr(),
                        cw.as_ptr(),
                        std::ptr::null(),
                        MS_BIND | MS_REC,
                        std::ptr::null(),
                    );
                    let _ = mount(
                        cw.as_ptr(),
                        cw.as_ptr(),
                        std::ptr::null(),
                        MS_BIND | MS_REMOUNT,
                        std::ptr::null(),
                    );
                    let _ = MS_RDONLY;
                }
            }
        }

        if let Some(sbx) = &opt.hermetic_sandbox_dir {
            let sp = Path::new(sbx);
            let proc_in_sbx = sp.join("proc");
            let _ = std::fs::create_dir_all(&proc_in_sbx);
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
                let _ = umount2(dot, MNT_DETACH);
            } else if chroot(dot) < 0 {
                eprintln!(
                    "linux-sandbox-pid1: pivot_root/chroot({sbx}): {}",
                    std::io::Error::last_os_error()
                );
                _exit(1);
            }
            let _ = chdir(root_c);
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

        if let Some(wd) = &opt.working_dir {
            let Ok(cwd_c) = cstr(wd) else {
                _exit(1);
            };
            if chdir(cwd_c.as_ptr()) < 0 {
                eprintln!(
                    "linux-sandbox-pid1: chdir({wd}): {}",
                    std::io::Error::last_os_error()
                );
                _exit(1);
            }
        }

        let cmd_pid = fork();
        if cmd_pid < 0 {
            eprintln!(
                "linux-sandbox-pid1: fork: {}",
                std::io::Error::last_os_error()
            );
            _exit(1);
        }
        if cmd_pid == 0 {
            reset_child_signals_and_mask();
            umask(0o022);
            if opt.enable_pty {
                if let Err(e) = setup_pty() {
                    eprintln!("linux-sandbox-pid1: {e}");
                    _exit(1);
                }
            } else {
                let _ = setsid();
            }
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

        let host_uid = unsafe { geteuid() };
        let host_gid = unsafe { getegid() };

        let mut ns_flags = CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWIPC | CLONE_NEWUTS;
        if !opt.persistent_process {
            ns_flags |= CLONE_NEWPID;
        }
        if opt.create_netns != NetworkNamespace::NoNetns {
            ns_flags |= CLONE_NEWNET;
        }

        if unsafe { unshare(ns_flags) } < 0 {
            return Err(format!(
                "unshare namespaces failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        write_proc_id_maps(host_uid, host_gid, opt.fake_root, opt.fake_username)?;

        unsafe {
            let _ = signal(SIGALRM, on_outer_sig as *const () as usize);
            let _ = signal(SIGTERM, on_outer_sig as *const () as usize);
            let _ = signal(SIGINT, on_outer_sig as *const () as usize);
        }

        let started = Instant::now();
        let pid1 = unsafe { fork() };
        if pid1 < 0 {
            return Err(format!(
                "fork pid1 failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        if pid1 == 0 {
            unsafe { run_pid1(opt) }
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
}
