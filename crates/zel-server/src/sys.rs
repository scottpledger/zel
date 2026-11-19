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

//! Native POSIX and Windows process, signal, rlimit, and OS inspection helpers
//! shared by the client and the daemon (zero external crates).

use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

pub const SIGHUP: i32 = 1;
pub const SIGINT: i32 = 2;
pub const SIGQUIT: i32 = 3;
pub const SIGKILL: i32 = 9;
pub const SIGPIPE: i32 = 13;
pub const SIGTERM: i32 = 15;
pub const SIGWINCH: i32 = 28;

/// Last signal delivered to a handler installed with [`install_handler`] (0 = none).
pub static LAST_SIGNAL: AtomicI32 = AtomicI32::new(0);
/// Total number of signals delivered to handlers installed with [`install_handler`].
pub static SIGNAL_COUNT: AtomicUsize = AtomicUsize::new(0);
/// Number of `SIGINT` signals delivered.
pub static SIGINT_COUNT: AtomicUsize = AtomicUsize::new(0);
/// Number of `SIGTERM` signals delivered.
pub static SIGTERM_COUNT: AtomicUsize = AtomicUsize::new(0);
/// Number of `SIGQUIT` signals delivered.
pub static SIGQUIT_COUNT: AtomicUsize = AtomicUsize::new(0);
/// Number of `SIGWINCH` signals delivered.
pub static SIGWINCH_COUNT: AtomicUsize = AtomicUsize::new(0);

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
    fn killpg(pgrp: i32, sig: i32) -> i32;
    fn signal(sig: i32, handler: usize) -> usize;
    fn sysv_signal(sig: i32, handler: usize) -> usize;
    fn setsid() -> i32;
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    fn open(path: *const std::os::raw::c_char, oflag: i32, ...) -> i32;
    fn dup2(oldfd: i32, newfd: i32) -> i32;
    fn close(fd: i32) -> i32;
    fn sigemptyset(set: *mut SigSet) -> i32;
    fn sigaddset(set: *mut SigSet, signum: i32) -> i32;
    fn sigprocmask(how: i32, set: *const SigSet, oldset: *mut SigSet) -> i32;
    fn geteuid() -> u32;
    fn getpwuid(uid: u32) -> *const Passwd;
}

#[cfg(unix)]
#[repr(C)]
struct SigSet {
    bits: [u64; 16],
}

#[cfg(all(unix, target_os = "linux"))]
#[repr(C)]
struct Passwd {
    pw_name: *const std::os::raw::c_char,
    pw_passwd: *const std::os::raw::c_char,
    pw_uid: u32,
    pw_gid: u32,
    pw_gecos: *const std::os::raw::c_char,
    pw_dir: *const std::os::raw::c_char,
    pw_shell: *const std::os::raw::c_char,
}

#[cfg(all(unix, target_os = "macos"))]
#[repr(C)]
struct Passwd {
    pw_name: *const std::os::raw::c_char,
    pw_passwd: *const std::os::raw::c_char,
    pw_uid: u32,
    pw_gid: u32,
    pw_change: isize,
    pw_class: *const std::os::raw::c_char,
    pw_gecos: *const std::os::raw::c_char,
    pw_dir: *const std::os::raw::c_char,
    pw_shell: *const std::os::raw::c_char,
    pw_expire: isize,
    pw_fields: i32,
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
#[repr(C)]
struct Passwd {
    pw_name: *const std::os::raw::c_char,
    pw_passwd: *const std::os::raw::c_char,
    pw_uid: u32,
    pw_gid: u32,
    pw_dir: *const std::os::raw::c_char,
}

fn record_signal(sig: i32) {
    LAST_SIGNAL.store(sig, Ordering::SeqCst);
    match sig {
        SIGINT => {
            SIGINT_COUNT.fetch_add(1, Ordering::SeqCst);
            SIGNAL_COUNT.fetch_add(1, Ordering::SeqCst);
        }
        SIGTERM => {
            SIGTERM_COUNT.fetch_add(1, Ordering::SeqCst);
            SIGNAL_COUNT.fetch_add(1, Ordering::SeqCst);
        }
        SIGQUIT => {
            SIGQUIT_COUNT.fetch_add(1, Ordering::SeqCst);
        }
        SIGWINCH => {
            SIGWINCH_COUNT.fetch_add(1, Ordering::SeqCst);
        }
        _ => {
            SIGNAL_COUNT.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[cfg(unix)]
extern "C" fn on_signal(sig: i32) {
    #[cfg(target_os = "linux")]
    unsafe {
        let _ = signal(sig, on_signal as *const () as usize);
    }
    record_signal(sig);
}

#[cfg(windows)]
extern "system" fn console_ctrl_handler(ctrl_type: u32) -> i32 {
    match ctrl_type {
        // CTRL_C_EVENT
        0 => {
            record_signal(SIGINT);
            1
        }
        // CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_SHUTDOWN_EVENT
        1 | 2 | 6 => {
            record_signal(SIGTERM);
            1
        }
        _ => 0,
    }
}

/// Unblocks `SIGINT`, `SIGTERM`, `SIGQUIT`, `SIGPIPE`, and `SIGWINCH` in the current thread's signal mask.
pub fn unblock_client_signals() {
    #[cfg(unix)]
    unsafe {
        #[cfg(target_os = "macos")]
        const SIG_UNBLOCK: i32 = 2;
        #[cfg(not(target_os = "macos"))]
        const SIG_UNBLOCK: i32 = 1;
        let mut set: SigSet = std::mem::zeroed();
        sigemptyset(&mut set);
        for s in [SIGINT, SIGTERM, SIGQUIT, SIGPIPE, SIGWINCH] {
            sigaddset(&mut set, s);
        }
        sigprocmask(SIG_UNBLOCK, &set, std::ptr::null_mut());
    }
}

/// Installs a handler for `sig` that records it in [`LAST_SIGNAL`] and the per-signal counters.
pub fn install_handler(sig: i32) {
    #[cfg(unix)]
    unsafe {
        let _ = sysv_signal;
        signal(sig, on_signal as *const () as usize);
    }
    #[cfg(windows)]
    {
        static INSTALLED: std::sync::Once = std::sync::Once::new();
        let _ = sig;
        INSTALLED.call_once(|| unsafe {
            let _ = SetConsoleCtrlHandler(Some(console_ctrl_handler), 1);
        });
    }
    #[cfg(all(not(unix), not(windows)))]
    let _ = sig;
}

/// Restores default disposition (`SIG_DFL`) for `sig`.
pub fn restore_default_signal(sig: i32) {
    #[cfg(unix)]
    unsafe {
        signal(sig, 0);
    }
    #[cfg(not(unix))]
    let _ = sig;
}

/// Ignores `sig` in this process.
pub fn ignore_signal(sig: i32) {
    #[cfg(unix)]
    unsafe {
        signal(sig, 1);
    }
    #[cfg(not(unix))]
    let _ = sig;
}

/// Detaches a child process into a new session (`setsid`) and ignores `SIGHUP` (`daemonize.cc`).
///
/// # Safety
/// Async-signal-safe; intended for `CommandExt::pre_exec` in a forked child before `exec`.
#[cfg(unix)]
pub unsafe fn setsid_and_ignore_sighup() {
    let _ = setsid();
    let _ = signal(SIGHUP, 1);
}

/// Ensures file descriptors 0, 1, and 2 are open by binding closed ones to `/dev/null` (`SetupStdStreams`).
pub fn setup_std_streams() {
    #[cfg(unix)]
    unsafe {
        const F_GETFD: i32 = 1;
        const O_RDONLY: i32 = 0;
        const O_WRONLY: i32 = 1;
        let devnull = b"/dev/null\0";
        for fd in 0..=2 {
            if fcntl(fd, F_GETFD) == -1 {
                let flags = if fd == 0 { O_RDONLY } else { O_WRONLY };
                let opened = open(devnull.as_ptr() as *const std::os::raw::c_char, flags);
                if opened >= 0 && opened != fd {
                    let _ = dup2(opened, fd);
                    let _ = close(opened);
                }
            }
        }
    }
}

/// Home directory fallback via `getpwuid(geteuid())` on Unix when `HOME` is unset.
pub fn passwd_home_dir() -> Option<String> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    unsafe {
        let pw = getpwuid(geteuid());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        let s = std::ffi::CStr::from_ptr((*pw).pw_dir)
            .to_string_lossy()
            .into_owned();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(
        desired_access: u32,
        inherit_handle: i32,
        process_id: u32,
    ) -> *mut core::ffi::c_void;
    fn GetExitCodeProcess(process: *mut core::ffi::c_void, exit_code: *mut u32) -> i32;
    fn TerminateProcess(process: *mut core::ffi::c_void, exit_code: u32) -> i32;
    fn CloseHandle(object: *mut core::ffi::c_void) -> i32;
    fn SetConsoleCtrlHandler(
        handler_routine: Option<extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}

/// True if a process with `pid` exists (signal 0 probe; EPERM counts as alive).
pub fn process_alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        if pid <= 0 {
            return false;
        }
        let rc = unsafe { kill(pid, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(1)
    }
    #[cfg(windows)]
    {
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        const STILL_ACTIVE: u32 = 259;
        if pid <= 0 {
            return false;
        }
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
            if h.is_null() {
                return false;
            }
            let mut code: u32 = 0;
            let ok = GetExitCodeProcess(h, &mut code);
            CloseHandle(h);
            ok != 0 && code == STILL_ACTIVE
        }
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        let _ = pid;
        false
    }
}

/// Sends `sig` to `pid`; returns whether the call succeeded.
pub fn send_signal(pid: i32, sig: i32) -> bool {
    #[cfg(unix)]
    {
        if pid <= 0 {
            return false;
        }
        unsafe { kill(pid, sig) == 0 }
    }
    #[cfg(windows)]
    {
        const PROCESS_TERMINATE: u32 = 0x0001;
        if pid <= 0 || (sig != SIGKILL && sig != SIGTERM) {
            return false;
        }
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid as u32);
            if h.is_null() {
                return false;
            }
            let ok = TerminateProcess(h, 1);
            CloseHandle(h);
            ok != 0
        }
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        let _ = (pid, sig);
        false
    }
}

/// Sends `sig` to the process group `pgid` on Unix (and `pid` fallback on Windows).
pub fn send_process_group_signal(pgid: i32, sig: i32) -> bool {
    #[cfg(unix)]
    {
        if pgid <= 0 {
            return false;
        }
        let grp_ok = unsafe { killpg(pgid, sig) == 0 };
        let pid_ok = unsafe { kill(pgid, sig) == 0 };
        grp_ok || pid_ok
    }
    #[cfg(not(unix))]
    {
        send_signal(pgid, sig)
    }
}

/// Returns the current working directory of `pid` (`GetProcessCWD` in `blaze_util_linux.cc` / `blaze_util_darwin.cc`).
pub fn get_process_cwd(pid: i32) -> Option<PathBuf> {
    if pid <= 0 {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }
    #[cfg(target_os = "macos")]
    {
        extern "C" {
            fn proc_pidinfo(
                pid: i32,
                flavor: i32,
                arg: u64,
                buffer: *mut std::ffi::c_void,
                buffersize: i32,
            ) -> i32;
        }
        const PROC_PIDVNODEPATHINFO: i32 = 9;
        // `struct proc_vnodepathinfo` on 64-bit Darwin is 2352 bytes:
        // `pvi_cdir` (`vnode_info_path`: 152-byte `vnode_info` + 1024-byte `vip_path`) + `pvi_rdir` (1176 bytes).
        #[repr(C)]
        struct VnodeInfoPath {
            vip_vi: [u8; 152],
            vip_path: [u8; 1024],
        }
        #[repr(C)]
        struct ProcVnodePathInfo {
            pvi_cdir: VnodeInfoPath,
            pvi_rdir: VnodeInfoPath,
        }
        unsafe {
            let mut info: ProcVnodePathInfo = std::mem::zeroed();
            let sz = std::mem::size_of::<ProcVnodePathInfo>() as i32;
            let rc = proc_pidinfo(
                pid,
                PROC_PIDVNODEPATHINFO,
                0,
                &mut info as *mut ProcVnodePathInfo as *mut std::ffi::c_void,
                sz,
            );
            if rc == sz {
                let nul = info
                    .pvi_cdir
                    .vip_path
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(info.pvi_cdir.vip_path.len());
                if nul > 0 {
                    let s = String::from_utf8_lossy(&info.pvi_cdir.vip_path[..nul]).into_owned();
                    return Some(PathBuf::from(s));
                }
            }
        }
        None
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Queries `_CS_DARWIN_USER_TEMP_DIR` and `_CS_DARWIN_USER_CACHE_DIR` via `confstr(3)` on macOS.
pub fn darwin_user_dirs() -> (Option<String>, Option<String>) {
    #[cfg(target_os = "macos")]
    {
        extern "C" {
            fn confstr(name: i32, buf: *mut std::os::raw::c_char, len: usize) -> usize;
        }
        const CS_DARWIN_USER_TEMP_DIR: i32 = 65537;
        const CS_DARWIN_USER_CACHE_DIR: i32 = 65538;
        fn get_one(name: i32) -> Option<String> {
            let mut buf = vec![0u8; 1024];
            let n = unsafe { confstr(name, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n == 0 || n > buf.len() {
                return None;
            }
            let end = buf.iter().position(|&b| b == 0).unwrap_or(n);
            let s = String::from_utf8_lossy(&buf[..end]).into_owned();
            (!s.is_empty()).then_some(s)
        }
        (
            get_one(CS_DARWIN_USER_TEMP_DIR),
            get_one(CS_DARWIN_USER_CACHE_DIR),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        (None, None)
    }
}

#[cfg(unix)]
extern "C" {
    fn isatty(fd: i32) -> i32;
    fn ioctl(fd: i32, req: u64, ...) -> i32;
}

/// True if `fd` refers to a terminal.
pub fn is_tty(fd: i32) -> bool {
    #[cfg(unix)]
    unsafe {
        isatty(fd) == 1
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        false
    }
}

/// Terminal `(columns, rows)` of `fd` (`TIOCGWINSZ`), falling back to `$COLUMNS`/`$LINES`, then `(80, 24)`.
pub fn terminal_size(fd: i32) -> (usize, usize) {
    #[cfg(not(unix))]
    let _ = fd;
    #[allow(unused_mut)]
    let mut cols: Option<usize> = None;
    #[allow(unused_mut)]
    let mut rows: Option<usize> = None;
    #[cfg(unix)]
    {
        #[repr(C)]
        #[derive(Default)]
        struct Winsize {
            row: u16,
            col: u16,
            x: u16,
            y: u16,
        }
        #[cfg(target_os = "macos")]
        const TIOCGWINSZ: u64 = 0x4008_7468;
        #[cfg(not(target_os = "macos"))]
        const TIOCGWINSZ: u64 = 0x5413;
        let mut ws = Winsize::default();
        let rc = unsafe { ioctl(fd, TIOCGWINSZ, &mut ws as *mut Winsize) };
        if rc == 0 {
            if ws.col > 0 {
                cols = Some(ws.col as usize);
            }
            if ws.row > 0 {
                rows = Some(ws.row as usize);
            }
        }
    }
    let cols = cols
        .or_else(|| {
            std::env::var("COLUMNS")
                .ok()
                .and_then(|c| c.parse().ok())
                .filter(|&c| c > 0)
        })
        .unwrap_or(80);
    let rows = rows
        .or_else(|| {
            std::env::var("LINES")
                .ok()
                .and_then(|r| r.parse().ok())
                .filter(|&r| r > 0)
        })
        .unwrap_or(24);
    (cols, rows)
}

/// Terminal width of `fd` (`TIOCGWINSZ`), falling back to `$COLUMNS`, then 80.
pub fn terminal_columns(fd: i32) -> usize {
    terminal_size(fd).0
}

#[cfg(unix)]
#[repr(C)]
#[derive(Default)]
struct Rusage {
    utime: [i64; 2],
    stime: [i64; 2],
    maxrss: i64,
    rest: [i64; 13],
}

#[cfg(unix)]
extern "C" {
    fn getrusage(who: i32, usage: *mut Rusage) -> i32;
    fn getrlimit(resource: i32, rlim: *mut [u64; 2]) -> i32;
    fn setrlimit(resource: i32, rlim: *const [u64; 2]) -> i32;
}

/// Peak resident set size of this process in bytes (`ru_maxrss`; bytes on macOS, KiB on Linux).
pub fn resident_bytes() -> u64 {
    #[cfg(unix)]
    {
        let mut ru = Rusage::default();
        if unsafe { getrusage(0, &mut ru) } == 0 {
            let v = ru.maxrss.max(0) as u64;
            return if cfg!(target_os = "macos") {
                v
            } else {
                v * 1024
            };
        }
    }
    0
}

#[cfg(unix)]
extern "C" {
    fn getloadavg(loadavg: *mut f64, nelem: i32) -> i32;
}

/// One-minute system load average (the `System load average` profile counter).
pub fn load_average() -> f64 {
    #[cfg(unix)]
    {
        let mut l = [0f64; 3];
        if unsafe { getloadavg(l.as_mut_ptr(), 3) } >= 1 {
            return l[0];
        }
    }
    0.0
}

/// CPU seconds (user + system) consumed by this process so far.
pub fn cpu_seconds() -> f64 {
    #[cfg(unix)]
    {
        let mut ru = Rusage::default();
        if unsafe { getrusage(0, &mut ru) } == 0 {
            return (ru.utime[0] + ru.stime[0]) as f64 + (ru.utime[1] + ru.stime[1]) as f64 / 1e6;
        }
    }
    0.0
}

/// Lifts soft `RLIMIT_NOFILE` and `RLIMIT_NPROC` up to their hard limits (`UnlimitResources` in
/// `blaze_util_posix.cc`, capped on macOS by `kern.maxfilesperproc` and `kern.maxprocperuid`).
pub fn unlimit_resources() {
    #[cfg(unix)]
    unsafe {
        #[cfg(target_os = "macos")]
        const RLIMIT_NOFILE: i32 = 8;
        #[cfg(not(target_os = "macos"))]
        const RLIMIT_NOFILE: i32 = 7;

        #[cfg(target_os = "macos")]
        const RLIMIT_NPROC: i32 = 7;
        #[cfg(not(target_os = "macos"))]
        const RLIMIT_NPROC: i32 = 6;

        let mut lim = [0u64; 2];
        if getrlimit(RLIMIT_NOFILE, &mut lim) == 0 {
            #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
            let mut target = lim[1];
            #[cfg(target_os = "macos")]
            {
                let cap = sysctl_i32("kern.maxfilesperproc")
                    .filter(|&n| n > 0)
                    .map(|n| n as u64)
                    .unwrap_or(10_240);
                if target > cap {
                    target = cap;
                }
            }
            if lim[0] < target {
                let next = [target, lim[1]];
                let _ = setrlimit(RLIMIT_NOFILE, &next);
            }
        }
        if getrlimit(RLIMIT_NPROC, &mut lim) == 0 {
            #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
            let mut target = lim[1];
            #[cfg(target_os = "macos")]
            if let Some(cap) = sysctl_i32("kern.maxprocperuid")
                .filter(|&n| n > 0)
                .map(|n| n as u64)
            {
                if target > cap {
                    target = cap;
                }
            }
            if lim[0] < target {
                let next = [target, lim[1]];
                let _ = setrlimit(RLIMIT_NPROC, &next);
            }
        }
    }
}

/// Raises the soft core-dump size limit to the hard limit (`--unlimit_coredumps` / `UnlimitCoredumps`).
pub fn unlimit_coredumps() {
    #[cfg(unix)]
    unsafe {
        const RLIMIT_CORE: i32 = 4;
        let mut lim = [0u64; 2];
        if getrlimit(RLIMIT_CORE, &mut lim) == 0 && lim[0] < lim[1] {
            let next = [lim[1], lim[1]];
            let _ = setrlimit(RLIMIT_CORE, &next);
        }
    }
}

#[cfg(target_os = "macos")]
extern "C" {
    fn sysctlbyname(
        name: *const std::os::raw::c_char,
        oldp: *mut std::ffi::c_void,
        oldlenp: *mut usize,
        newp: *mut std::ffi::c_void,
        newlen: usize,
    ) -> i32;
}

#[cfg(target_os = "macos")]
fn sysctl_u64(name: &str) -> Option<u64> {
    let c = std::ffi::CString::new(name).ok()?;
    let mut v: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let rc = unsafe {
        sysctlbyname(
            c.as_ptr(),
            &mut v as *mut u64 as *mut _,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0).then_some(v)
}

#[cfg(target_os = "macos")]
fn sysctl_i32(name: &str) -> Option<i32> {
    let c = std::ffi::CString::new(name).ok()?;
    let mut v: i32 = 0;
    let mut len = std::mem::size_of::<i32>();
    let rc = unsafe {
        sysctlbyname(
            c.as_ptr(),
            &mut v as *mut i32 as *mut _,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0).then_some(v)
}

/// Fraction (0.0..=1.0) of system memory that is free / available, if it can be determined.
pub fn free_memory_fraction() -> Option<f64> {
    #[cfg(target_os = "macos")]
    {
        let total = sysctl_u64("hw.memsize")? as f64;
        let page = sysctl_u64("hw.pagesize").unwrap_or(4096) as f64;
        let free = sysctl_u64("vm.page_free_count")? as f64;
        let spec = sysctl_u64("vm.page_speculative_count").unwrap_or(0) as f64;
        return Some(((free + spec) * page / total).min(1.0));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let (avail_kb, total_kb) = linux_available_and_total_kb()?;
        Some((avail_kb / total_kb).clamp(0.0, 1.0))
    }
}

#[cfg(not(target_os = "macos"))]
fn linux_available_and_total_kb() -> Option<(f64, f64)> {
    let t = std::fs::read_to_string("/proc/meminfo").ok()?;
    let get = |k: &str| -> Option<f64> {
        t.lines().find_map(|l| {
            l.strip_prefix(k)?
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse()
                .ok()
        })
    };
    let total = get("MemTotal:")?;
    if total <= 0.0 {
        return None;
    }
    let avail = match get("MemAvailable:") {
        Some(a) => a,
        None => {
            let active = get("Active:").unwrap_or(0.0);
            let inactive = get("Inactive:").unwrap_or(0.0);
            let slab = get("Slab:").unwrap_or(0.0);
            (total - active - 0.3 * inactive - 0.8 * slab).max(0.0)
        }
    };
    Some((avail, total))
}

/// Bazel `SystemMemoryPressureMonitor` / `--shutdown_on_low_sys_mem`: true when available
/// system memory is both `< 5%` of total RAM and `< 1 GiB`.
pub fn is_low_sys_mem() -> bool {
    #[cfg(target_os = "macos")]
    {
        let Some(total) = sysctl_u64("hw.memsize").map(|n| n as f64) else {
            return false;
        };
        if total <= 0.0 {
            return false;
        }
        let page = sysctl_u64("hw.pagesize").unwrap_or(4096) as f64;
        let Some(free) = sysctl_u64("vm.page_free_count").map(|n| n as f64) else {
            return false;
        };
        let spec = sysctl_u64("vm.page_speculative_count").unwrap_or(0) as f64;
        let avail_bytes = (free + spec) * page;
        (avail_bytes / total) < 0.05 && avail_bytes < (1024.0 * 1024.0 * 1024.0)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let Some((avail_kb, total_kb)) = linux_available_and_total_kb() else {
            return false;
        };
        (avail_kb / total_kb) < 0.05 && avail_kb < (1024.0 * 1024.0)
    }
}

#[cfg(target_os = "macos")]
extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}

/// `--macos_qos_class`: applies the QoS class to the calling thread (inherited by threads and
/// child processes created afterwards). Returns false for an unknown class name.
pub fn set_macos_qos_class(name: &str) -> bool {
    let class: u32 = match name {
        "user-interactive" => 0x21,
        "user-initiated" => 0x19,
        "default" => 0x15,
        "utility" => 0x11,
        "background" => 0x09,
        _ => return false,
    };
    #[cfg(target_os = "macos")]
    unsafe {
        pthread_set_qos_class_self_np(class, 0);
    }
    let _ = class;
    true
}

#[cfg(target_os = "linux")]
extern "C" {
    fn sched_setscheduler(pid: i32, policy: i32, param: *const i32) -> i32;
    fn syscall(num: i64, ...) -> i64;
}

/// `--batch_cpu_scheduling`: `SCHED_BATCH` for this process (Linux only; no-op elsewhere).
pub fn set_batch_cpu_scheduling() {
    #[cfg(target_os = "linux")]
    {
        let param: i32 = 0;
        unsafe {
            sched_setscheduler(0, 3, &param);
        }
    }
}

/// `--io_nice_level=N`: best-effort I/O priority level N (0..=7) for this process (Linux only).
pub fn set_io_nice_level(level: i32) {
    #[cfg(target_os = "linux")]
    {
        if !(0..=7).contains(&level) {
            return;
        }
        #[cfg(target_arch = "x86_64")]
        const SYS_IOPRIO_SET: i64 = 251;
        #[cfg(target_arch = "aarch64")]
        const SYS_IOPRIO_SET: i64 = 30;
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        unsafe {
            syscall(SYS_IOPRIO_SET, 1i64, 0i64, ((2i64) << 13) | level as i64);
        }
    }
    let _ = level;
}
