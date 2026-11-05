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

//! Minimal POSIX process/signal helpers shared by the client and the daemon (no external crates).

use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

pub const SIGINT: i32 = 2;
pub const SIGKILL: i32 = 9;
pub const SIGTERM: i32 = 15;

/// Last signal delivered to a handler installed with [`install_handler`] (0 = none).
pub static LAST_SIGNAL: AtomicI32 = AtomicI32::new(0);
/// Number of signals delivered to handlers installed with [`install_handler`].
pub static SIGNAL_COUNT: AtomicUsize = AtomicUsize::new(0);

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
    fn signal(sig: i32, handler: usize) -> usize;
}

#[cfg(unix)]
extern "C" fn on_signal(sig: i32) {
    LAST_SIGNAL.store(sig, Ordering::SeqCst);
    SIGNAL_COUNT.fetch_add(1, Ordering::SeqCst);
}

/// Installs a handler for `sig` that records it in [`LAST_SIGNAL`] / [`SIGNAL_COUNT`].
pub fn install_handler(sig: i32) {
    #[cfg(unix)]
    // SAFETY: `on_signal` is async-signal-safe (only atomic stores).
    unsafe {
        signal(sig, on_signal as usize);
    }
    #[cfg(not(unix))]
    let _ = sig;
}

/// Ignores `sig` in this process.
pub fn ignore_signal(sig: i32) {
    #[cfg(unix)]
    // SAFETY: SIG_IGN is the value 1.
    unsafe {
        signal(sig, 1);
    }
    #[cfg(not(unix))]
    let _ = sig;
}

/// True if a process with `pid` exists (signal 0 probe; EPERM counts as alive).
pub fn process_alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        if pid <= 0 {
            return false;
        }
        // SAFETY: signal 0 performs only an existence/permission check.
        let rc = unsafe { kill(pid, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(1)
    }
    #[cfg(not(unix))]
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
        // SAFETY: plain kill(2).
        unsafe { kill(pid, sig) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, sig);
        false
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
    // SAFETY: isatty(3) only inspects the descriptor.
    unsafe {
        isatty(fd) == 1
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        false
    }
}

/// Terminal width of `fd` (TIOCGWINSZ), falling back to `$COLUMNS`, then 80.
pub fn terminal_columns(fd: i32) -> usize {
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
        // SAFETY: TIOCGWINSZ writes a `struct winsize` into the provided pointer.
        let rc = unsafe { ioctl(fd, TIOCGWINSZ, &mut ws as *mut Winsize) };
        if rc == 0 && ws.col > 0 {
            return ws.col as usize;
        }
    }
    std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()).filter(|c| *c > 0).unwrap_or(80)
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
    fn setrlimit(resource: i32, rlim: *const [u64; 2]) -> i32;
}

/// Peak resident set size of this process in bytes (`ru_maxrss`; bytes on macOS, KiB on Linux).
pub fn resident_bytes() -> u64 {
    #[cfg(unix)]
    {
        let mut ru = Rusage::default();
        // SAFETY: RUSAGE_SELF with a correctly sized, zero-initialised struct.
        if unsafe { getrusage(0, &mut ru) } == 0 {
            let v = ru.maxrss.max(0) as u64;
            return if cfg!(target_os = "macos") { v } else { v * 1024 };
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
        // SAFETY: getloadavg writes at most 3 doubles.
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
        // SAFETY: RUSAGE_SELF with a correctly sized, zero-initialised struct.
        if unsafe { getrusage(0, &mut ru) } == 0 {
            return (ru.utime[0] + ru.stime[0]) as f64 + (ru.utime[1] + ru.stime[1]) as f64 / 1e6;
        }
    }
    0.0
}

/// Removes the core-dump size limit (`--unlimit_coredumps`).
pub fn unlimit_coredumps() {
    #[cfg(unix)]
    {
        const RLIMIT_CORE: i32 = 4;
        // RLIM_INFINITY
        let inf: u64 = if cfg!(target_os = "macos") { u64::MAX >> 1 } else { u64::MAX };
        let lim = [inf, inf];
        // SAFETY: setrlimit with a valid 2-word rlimit.
        unsafe {
            setrlimit(RLIMIT_CORE, &lim);
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
    // SAFETY: valid NUL-terminated name and an 8-byte output buffer.
    let rc = unsafe { sysctlbyname(c.as_ptr(), &mut v as *mut u64 as *mut _, &mut len, std::ptr::null_mut(), 0) };
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
        let t = std::fs::read_to_string("/proc/meminfo").ok()?;
        let get = |k: &str| -> Option<f64> {
            t.lines().find_map(|l| l.strip_prefix(k)?.trim().trim_end_matches("kB").trim().parse().ok())
        };
        Some(get("MemAvailable:")? / get("MemTotal:")?)
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
    // SAFETY: plain libpthread call with a valid QoS constant.
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
        // SAFETY: SCHED_BATCH (3) with priority 0 is valid for unprivileged processes.
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
        // SAFETY: ioprio_set(IOPRIO_WHO_PROCESS, self, BE class | level).
        unsafe {
            syscall(SYS_IOPRIO_SET, 1i64, 0i64, ((2i64) << 13) | level as i64);
        }
    }
    let _ = level;
}
