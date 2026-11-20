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

//! Port of `src/main/tools/process-wrapper*.cc` (the legacy wrapper that Bazel uses on Unix):
//!
//! * option parsing (`-g/--graceful_sigterm`, `-t/--timeout`, `-k/--kill_delay`, `-o/--stdout`,
//!   `-e/--stderr`, `-s/--stats`, `-d/--debug`, `--`) with getopt's `+:` semantics and the same
//!   usage errors,
//! * the child runs in its own session/process group with `umask(022)`,
//! * a timeout delivers `SIGTERM` to the whole group, waits `kill_delay` seconds and then sends
//!   `SIGKILL`; the wrapper then reports termination by `SIGALRM` (exit status 142 as seen by a
//!   parent), exactly like the C++ wrapper that re-raises the last signal,
//! * after the child exits the whole process group is killed (orphan cleanup),
//! * resource usage of the child (`wait4`) written as an `ExecutionStatistics` protobuf
//!   (`src/main/protobuf/execution_statistics.proto`).

use std::fs::OpenOptions;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `SIGINT` / `SIGPIPE` / `SIGALRM` / `SIGTERM` / `SIGKILL` numbers (identical on Linux and macOS).
pub const SIGINT: i32 = 2;
pub const SIGKILL: i32 = 9;
pub const SIGPIPE: i32 = 13;
pub const SIGALRM: i32 = 14;
pub const SIGTERM: i32 = 15;

#[cfg(unix)]
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Timeval {
    tv_sec: i64,
    tv_usec: i32,
}
#[cfg(unix)]
#[cfg(not(target_os = "macos"))]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Timeval {
    tv_sec: i64,
    tv_usec: i64,
}

#[cfg(unix)]
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

#[cfg(unix)]
#[repr(C)]
#[derive(Clone, Copy)]
struct SigSet {
    #[cfg(target_os = "macos")]
    bits: u32,
    #[cfg(not(target_os = "macos"))]
    bits: [usize; 16],
}

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
    fn getpid() -> i32;
    fn setsid() -> i32;
    fn umask(mask: u32) -> u32;
    fn signal(sig: i32, handler: usize) -> usize;
    fn sigemptyset(set: *mut SigSet) -> i32;
    fn sigaddset(set: *mut SigSet, signum: i32) -> i32;
    fn sigprocmask(how: i32, set: *const SigSet, oldset: *mut SigSet) -> i32;
    fn wait4(pid: i32, status: *mut i32, options: i32, rusage: *mut Rusage) -> i32;
    #[cfg(target_os = "linux")]
    fn prctl(option: i32, arg2: usize, arg3: usize, arg4: usize, arg5: usize) -> i32;
}

#[cfg(unix)]
const WNOHANG: i32 = 1;

#[cfg(unix)]
static GOT_SIGTERM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_wrapper_sigterm(sig: i32) {
    #[cfg(target_os = "linux")]
    unsafe {
        let _ = signal(sig, on_wrapper_sigterm as *const () as usize);
    }
    record_wrapper_signal(sig);
    GOT_SIGTERM.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(unix)]
struct SigtermGuard {
    installed: bool,
    prev: usize,
}

#[cfg(unix)]
impl SigtermGuard {
    fn install(enable: bool) -> Self {
        GOT_SIGTERM.store(false, std::sync::atomic::Ordering::SeqCst);
        if !enable {
            return Self {
                installed: false,
                prev: 0,
            };
        }
        let prev = unsafe { signal(SIGTERM, on_wrapper_sigterm as *const () as usize) };
        Self {
            installed: prev != usize::MAX,
            prev,
        }
    }
}

#[cfg(unix)]
impl Drop for SigtermGuard {
    fn drop(&mut self) {
        if self.installed {
            unsafe {
                let _ = signal(SIGTERM, self.prev);
            }
        }
    }
}

/// Async-signal-safe child reset (`ClearSignalMask` in `process-tools.cc`): restores default
/// disposition (`SIG_DFL`) for catchable signals (including `SIGPIPE`, which Rust sets to
/// `SIG_IGN`) and clears the signal mask before `exec`.
///
/// # Safety
/// Intended to be called in a forked child process prior to `exec`.
#[cfg(unix)]
pub unsafe fn reset_child_signals_and_mask() {
    #[cfg(target_os = "macos")]
    const MAX_SIG: i32 = 31;
    #[cfg(not(target_os = "macos"))]
    const MAX_SIG: i32 = 64;
    #[cfg(target_os = "macos")]
    const SIGSTOP_NUM: i32 = 17;
    #[cfg(not(target_os = "macos"))]
    const SIGSTOP_NUM: i32 = 19;
    #[cfg(target_os = "macos")]
    const SIG_SETMASK: i32 = 3;
    #[cfg(not(target_os = "macos"))]
    const SIG_SETMASK: i32 = 2;

    for sig in 1..=MAX_SIG {
        if sig == SIGKILL || sig == SIGSTOP_NUM {
            continue;
        }
        #[cfg(target_os = "linux")]
        if sig == 32 || sig == 33 {
            // NPTL internal signals on glibc/musl.
            continue;
        }
        let _ = signal(sig, 0);
    }
    let mut empty: SigSet = std::mem::zeroed();
    let _ = sigemptyset(&mut empty);
    let _ = sigprocmask(SIG_SETMASK, &empty, std::ptr::null_mut());
}

/// Restores `SIG_DFL`, unblocks `sig`, and sends `sig` to the current process (`KillMyself`).
///
/// # Safety
/// Must be called with a valid POSIX signal number right before terminating the current process.
#[cfg(unix)]
pub unsafe fn reraise_signal_to_self(sig: i32) {
    #[cfg(target_os = "macos")]
    const SIG_UNBLOCK: i32 = 2;
    #[cfg(not(target_os = "macos"))]
    const SIG_UNBLOCK: i32 = 1;

    let _ = signal(sig, 0);
    let mut set: SigSet = std::mem::zeroed();
    let _ = sigemptyset(&mut set);
    let _ = sigaddset(&mut set, sig);
    let _ = sigprocmask(SIG_UNBLOCK, &set, std::ptr::null_mut());
    let _ = kill(getpid(), sig);
}

/// Sends `sig` to the process group `pgid` (`kill(-pgid, sig)`).
pub fn signal_process_group(pgid: u32, sig: i32) {
    #[cfg(unix)]
    // SAFETY: plain kill(2).
    unsafe {
        kill(-(pgid as i32), sig);
    }
    #[cfg(not(unix))]
    let _ = (pgid, sig);
}

static GLOBAL_CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static WRAPPER_SIGNAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Records an incoming `SIGINT` / `SIGTERM` received by the standalone `process-wrapper` binary.
pub fn record_wrapper_signal(sig: i32) {
    let _ = WRAPPER_SIGNAL.compare_exchange(
        0,
        sig,
        std::sync::atomic::Ordering::SeqCst,
        std::sync::atomic::Ordering::SeqCst,
    );
}

pub fn take_wrapper_signal() -> i32 {
    WRAPPER_SIGNAL.swap(0, std::sync::atomic::Ordering::SeqCst)
}

/// Server-wide cancellation (`Cancel` RPC / client SIGINT): every spawn that is running gets its process
/// group killed and no new one starts until [`clear_global_cancel`] (at the start of the next command).
pub fn request_global_cancel() {
    GLOBAL_CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn clear_global_cancel() {
    GLOBAL_CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
}

pub fn global_cancel_requested() -> bool {
    GLOBAL_CANCEL.load(std::sync::atomic::Ordering::SeqCst)
}

/// Shared cancellation flag (compared by identity so options stay `PartialEq`).
#[derive(Debug, Clone, Default)]
pub struct CancelFlag(pub std::sync::Arc<std::sync::atomic::AtomicBool>);

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl PartialEq for CancelFlag {
    fn eq(&self, o: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &o.0)
    }
}

/// Live output chunk callback invoked while the wrapped process writes to stdout/stderr.
#[derive(Clone)]
pub struct StreamOutCallback(pub crate::StreamOutHook);

impl std::fmt::Debug for StreamOutCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StreamOutCallback(..)")
    }
}

impl PartialEq for StreamOutCallback {
    fn eq(&self, o: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &o.0)
    }
}

/// Parsed `process-wrapper` command line (`struct Options` in `process-wrapper-options.h`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessWrapperOptions {
    pub graceful_sigterm: bool,
    pub timeout_secs: f64,
    pub kill_delay_secs: f64,
    pub stdout_path: Option<String>,
    pub stderr_path: Option<String>,
    pub stats_path: Option<String>,
    pub debug: bool,
    pub args: Vec<String>,
    /// When set to true while the command runs, its process group is killed (dynamic execution
    /// cancels the losing branch, the equivalent of Bazel interrupting the spawn thread).
    pub cancel: Option<CancelFlag>,
    /// Close child stdin (`/dev/null`) instead of inheriting the parent's stdin (`LocalSpawnRunner`).
    pub close_stdin: bool,
    /// Optional live output sink receiving stdout/stderr chunks while the child is running.
    pub stream_out: Option<StreamOutCallback>,
}

impl Default for ProcessWrapperOptions {
    fn default() -> Self {
        Self {
            graceful_sigterm: false,
            timeout_secs: 0.0,
            kill_delay_secs: 0.0,
            stdout_path: None,
            stderr_path: None,
            stats_path: None,
            debug: false,
            args: Vec::new(),
            cancel: None,
            close_stdin: false,
            stream_out: None,
        }
    }
}

/// The usage text printed by `Usage()` after the error message.
pub fn usage_text(program: &str, error: &str) -> String {
    format!(
        "{error}\n\nUsage: {program} -- command arg1 @args\n\nPossible arguments:\n  \
         -g/--graceful_sigterm  propagate SIGTERM to the subprocess and delay the corresponding SIGKILL until --kill_delay has passed\n  \
         -t/--timeout <timeout>  timeout after which the child process will be terminated with SIGTERM\n  \
         -k/--kill_delay <timeout>  in case timeout occurs, how long to wait before killing the child with SIGKILL\n  \
         -o/--stdout <file>  redirect stdout to a file\n  \
         -e/--stderr <file>  redirect stderr to a file\n  \
         -s/--stats <file>  if set, write stats in protobuf format to a file\n  \
         -d/--debug  if set, debug info will be printed\n  \
         --  command to run inside sandbox, followed by arguments\n"
    )
}

/// `sscanf(optarg, "%lf", &x) == 1` semantics: a leading floating point number; trailing garbage
/// is ignored.
fn scan_double(s: &str) -> Option<f64> {
    let t = s.trim_start();
    let mut end = 0;
    let b = t.as_bytes();
    let mut seen_digit = false;
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        seen_digit = true;
        end = i;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            seen_digit = true;
            end = i;
        }
    }
    if !seen_digit {
        return None;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let ds = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > ds {
            end = j;
        }
    }
    t[..end].parse().ok()
}

/// Parses `argv[1..]` like `ParseCommandLine`/`ParseOptions`. Errors are the full `Usage()` text
/// (without the program-specific prefix handling).
pub fn parse_args(program: &str, args: &[String]) -> Result<ProcessWrapperOptions, String> {
    let mut opt = ProcessWrapperOptions::default();
    let mut i = 0;
    // getopt "+" => stop at the first non-option; "--" terminates options.
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            i += 1;
            break;
        }
        if !a.starts_with('-') || a == "-" {
            break;
        }
        // Normalize into (short-char, optional inline value).
        let (c, inline): (char, Option<String>) = if let Some(long) = a.strip_prefix("--") {
            let (name, val) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            let mapped = match name {
                "graceful_sigterm" => 'g',
                "timeout" => 't',
                "kill_delay" => 'k',
                "stdout" => 'o',
                "stderr" => 'e',
                "stats" => 's',
                "debug" => 'd',
                _ => {
                    return Err(usage_text(
                        program,
                        &format!("Unrecognized argument: -? ({})", i + 1),
                    ))
                }
            };
            (mapped, val)
        } else {
            let mut chars = a[1..].chars();
            let c = chars.next().unwrap();
            let rest: String = chars.collect();
            (c, if rest.is_empty() { None } else { Some(rest) })
        };
        let takes_arg = matches!(c, 't' | 'k' | 'o' | 'e' | 's');
        let known = takes_arg || matches!(c, 'g' | 'd');
        if !known {
            return Err(usage_text(
                program,
                &format!("Unrecognized argument: -{c} ({})", i + 1),
            ));
        }
        let mut value = None;
        if takes_arg {
            value = match inline {
                Some(v) => Some(v),
                None => {
                    i += 1;
                    match args.get(i) {
                        Some(v) => Some(v.clone()),
                        None => {
                            return Err(usage_text(
                                program,
                                &format!("Flag -{c} requires an argument"),
                            ))
                        }
                    }
                }
            };
        } else if inline.is_some() && !a.starts_with("--") {
            // Bundled short flags such as `-gd`.
            let mut flags = vec![c];
            flags.extend(inline.as_ref().unwrap().chars());
            for f in flags {
                match f {
                    'g' => opt.graceful_sigterm = true,
                    'd' => opt.debug = true,
                    other => {
                        return Err(usage_text(
                            program,
                            &format!("Unrecognized argument: -{other} ({})", i + 1),
                        ))
                    }
                }
            }
            i += 1;
            continue;
        }
        match c {
            'g' => opt.graceful_sigterm = true,
            'd' => opt.debug = true,
            't' => {
                let v = value.unwrap();
                opt.timeout_secs = scan_double(&v).ok_or_else(|| {
                    usage_text(program, &format!("Invalid timeout (-t) value: {v}"))
                })?;
            }
            'k' => {
                let v = value.unwrap();
                opt.kill_delay_secs = scan_double(&v).ok_or_else(|| {
                    usage_text(program, &format!("Invalid kill delay (-k) value: {v}"))
                })?;
            }
            'o' => {
                if opt.stdout_path.as_deref().is_some_and(|s| !s.is_empty()) {
                    return Err(usage_text(
                        program,
                        "Cannot redirect stdout (-o) to more than one destination.",
                    ));
                }
                opt.stdout_path = value.filter(|v| !v.is_empty());
            }
            'e' => {
                if opt.stderr_path.as_deref().is_some_and(|s| !s.is_empty()) {
                    return Err(usage_text(
                        program,
                        "Cannot redirect stderr (-e) to more than one destination.",
                    ));
                }
                opt.stderr_path = value.filter(|v| !v.is_empty());
            }
            's' => {
                if opt.stats_path.as_deref().is_some_and(|s| !s.is_empty()) {
                    return Err(usage_text(
                        program,
                        "Cannot write stats (-s) to more than one destination.",
                    ));
                }
                opt.stats_path = value.filter(|v| !v.is_empty());
            }
            _ => unreachable!(),
        }
        i += 1;
    }
    opt.args = args[i.min(args.len())..].to_vec();
    if opt.args.is_empty() {
        return Err(usage_text(program, "No command specified."));
    }
    Ok(opt)
}

/// `ProcessWrapper.CommandLineBuilder`: `[wrapper, --timeout=N, --kill_delay=N, --stdout=P,
/// --stderr=P, --stats=P, --graceful_sigterm, args...]` (durations are whole seconds, like
/// `Duration.toSeconds()`).
#[derive(Debug, Clone, Default)]
pub struct CommandLineBuilder {
    pub wrapper: String,
    pub args: Vec<String>,
    pub timeout: Option<Duration>,
    pub kill_delay: Option<Duration>,
    pub stdout_path: Option<String>,
    pub stderr_path: Option<String>,
    pub stats_path: Option<String>,
    pub graceful_sigterm: bool,
}

impl CommandLineBuilder {
    pub fn build(&self) -> Vec<String> {
        let mut c = vec![self.wrapper.clone()];
        if let Some(t) = self.timeout {
            c.push(format!("--timeout={}", t.as_secs()));
        }
        if let Some(k) = self.kill_delay {
            c.push(format!("--kill_delay={}", k.as_secs()));
        }
        if let Some(p) = &self.stdout_path {
            c.push(format!("--stdout={p}"));
        }
        if let Some(p) = &self.stderr_path {
            c.push(format!("--stderr={p}"));
        }
        if let Some(p) = &self.stats_path {
            c.push(format!("--stats={p}"));
        }
        if self.graceful_sigterm {
            c.push("--graceful_sigterm".to_string());
        }
        c.extend(self.args.iter().cloned());
        c
    }
}

/// `tools::protos::ResourceUsage` of the wrapped child.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceUsage {
    pub utime_sec: i64,
    pub utime_usec: i64,
    pub stime_sec: i64,
    pub stime_usec: i64,
    pub maxrss: i64,
    pub ixrss: i64,
    pub idrss: i64,
    pub isrss: i64,
    pub minflt: i64,
    pub majflt: i64,
    pub nswap: i64,
    pub inblock: i64,
    pub oublock: i64,
    pub msgsnd: i64,
    pub msgrcv: i64,
    pub nsignals: i64,
    pub nvcsw: i64,
    pub nivcsw: i64,
}

impl ResourceUsage {
    #[cfg(unix)]
    #[allow(clippy::unnecessary_cast)]
    fn from_rusage(r: &Rusage) -> Self {
        Self {
            utime_sec: r.ru_utime.tv_sec,
            utime_usec: r.ru_utime.tv_usec as i64,
            stime_sec: r.ru_stime.tv_sec,
            stime_usec: r.ru_stime.tv_usec as i64,
            maxrss: r.ru_maxrss,
            ixrss: r.ru_ixrss,
            idrss: r.ru_idrss,
            isrss: r.ru_isrss,
            minflt: r.ru_minflt,
            majflt: r.ru_majflt,
            nswap: r.ru_nswap,
            inblock: r.ru_inblock,
            oublock: r.ru_oublock,
            msgsnd: r.ru_msgsnd,
            msgrcv: r.ru_msgrcv,
            nsignals: r.ru_nsignals,
            nvcsw: r.ru_nvcsw,
            nivcsw: r.ru_nivcsw,
        }
    }

    fn normalize_timeval(sec: i64, usec: i64) -> Duration {
        let total_us = (sec as i128)
            .saturating_mul(1_000_000)
            .saturating_add(usec as i128);
        if total_us <= 0 {
            return Duration::ZERO;
        }
        let secs = u64::try_from(total_us / 1_000_000).unwrap_or(u64::MAX);
        let nanos = ((total_us % 1_000_000) as u32) * 1000;
        Duration::new(secs, nanos)
    }

    pub fn user_time(&self) -> Duration {
        Self::normalize_timeval(self.utime_sec, self.utime_usec)
    }
    pub fn system_time(&self) -> Duration {
        Self::normalize_timeval(self.stime_sec, self.stime_usec)
    }

    /// Serialized `ExecutionStatistics { resource_usage = 1 }` (proto3: zero fields omitted).
    pub fn encode_execution_statistics(&self) -> Vec<u8> {
        use zel_proto::wire;
        let mut ru = Vec::new();
        let fields = [
            self.utime_sec,
            self.utime_usec,
            self.stime_sec,
            self.stime_usec,
            self.maxrss,
            self.ixrss,
            self.idrss,
            self.isrss,
            self.minflt,
            self.majflt,
            self.nswap,
            self.inblock,
            self.oublock,
            self.msgsnd,
            self.msgrcv,
            self.nsignals,
            self.nvcsw,
            self.nivcsw,
        ];
        for (i, v) in fields.iter().enumerate() {
            wire::encode_int64(i as u32 + 1, *v, &mut ru);
        }
        let mut out = Vec::new();
        wire::encode_bytes_always(1, &ru, &mut out);
        out
    }

    pub fn decode_execution_statistics(buf: &[u8]) -> Result<Self, zel_proto::DecodeError> {
        use zel_proto::wire;
        let mut pos = 0;
        let mut res = Self::default();
        while pos < buf.len() {
            let (f, w) = wire::decode_tag(buf, &mut pos)?;
            if f == 1 && w == wire::WIRE_TYPE_LENGTH_DELIMITED {
                let sub = wire::decode_bytes(buf, &mut pos)?;
                let mut p = 0;
                while p < sub.len() {
                    let (f2, w2) = wire::decode_tag(sub, &mut p)?;
                    if w2 == wire::WIRE_TYPE_VARINT {
                        let v = wire::decode_varint(sub, &mut p)? as i64;
                        match f2 {
                            1 => res.utime_sec = v,
                            2 => res.utime_usec = v,
                            3 => res.stime_sec = v,
                            4 => res.stime_usec = v,
                            5 => res.maxrss = v,
                            6 => res.ixrss = v,
                            7 => res.idrss = v,
                            8 => res.isrss = v,
                            9 => res.minflt = v,
                            10 => res.majflt = v,
                            11 => res.nswap = v,
                            12 => res.inblock = v,
                            13 => res.oublock = v,
                            14 => res.msgsnd = v,
                            15 => res.msgrcv = v,
                            16 => res.nsignals = v,
                            17 => res.nvcsw = v,
                            18 => res.nivcsw = v,
                            _ => {}
                        }
                    } else {
                        wire::skip_field(w2, sub, &mut p)?;
                    }
                }
            } else {
                wire::skip_field(w, buf, &mut pos)?;
            }
        }
        Ok(res)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    Exited(i32),
    Signaled(i32),
}

impl Termination {
    /// Exit code as a shell / Bazel's `TerminationStatus.getRawExitCode` would see it.
    pub fn exit_code(self) -> i32 {
        match self {
            Termination::Exited(c) => c,
            Termination::Signaled(s) => 128 + s,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WrapperOutcome {
    pub termination: Termination,
    pub timed_out: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub usage: Option<ResourceUsage>,
    pub wall: Duration,
    /// The command was killed because `ProcessWrapperOptions::cancel` was raised.
    pub cancelled: bool,
}

/// Normalizes `-o` / `-e` redirect targets (`Redirect` in `process-wrapper-legacy.cc`):
/// empty `""` and `"-"` mean "do not redirect".
pub fn effective_redirect_path(path: Option<&str>) -> Option<&str> {
    path.filter(|p| !p.is_empty() && *p != "-")
}

/// Opens a `-o` / `-e` redirect destination (`O_WRONLY | O_CREAT | O_TRUNC`, mode `0666` before
/// umask), without `O_APPEND` or `ftruncate` so character devices such as `/dev/null` succeed.
pub fn open_redirect(path: &str) -> std::io::Result<std::fs::File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

#[cfg(target_os = "linux")]
fn linux_reparented_children() -> Vec<i32> {
    let raw = std::fs::read_to_string("/proc/thread-self/children")
        .or_else(|_| {
            std::fs::read_to_string(format!("/proc/self/task/{}/children", unsafe { getpid() }))
        })
        .unwrap_or_default();
    raw.split_whitespace()
        .filter_map(|tok| tok.parse::<i32>().ok())
        .filter(|&pid| pid > 0)
        .collect()
}

/// Sends `sig` to `-pgid` and (when this process is a Linux subreaper) to any reparented
/// descendants recorded in `/proc/thread-self/children`.
#[cfg(unix)]
fn kill_everything_once(pgid: u32, sig: i32, include_subreaper_children: bool) {
    signal_process_group(pgid, sig);
    #[cfg(target_os = "linux")]
    if include_subreaper_children {
        for _ in 0..3 {
            let children = linux_reparented_children();
            if children.is_empty() {
                break;
            }
            for child_pid in children {
                unsafe {
                    let _ = kill(-child_pid, sig);
                    let _ = kill(child_pid, sig);
                }
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = include_subreaper_children;
}

/// macOS `KERN_PROC_PGRP` sweep (`KillEverything` on Darwin while the leader pins `pgid`).
#[cfg(target_os = "macos")]
fn darwin_kill_pgrp_members(pgid: i32, sig: i32) {
    signal_process_group(pgid as u32, sig);
}

static STANDALONE_WRAPPER_SIG: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn on_wrapper_signal(sig: i32) {
    STANDALONE_WRAPPER_SIG.store(sig, std::sync::atomic::Ordering::SeqCst);
}

/// Runs `process-wrapper` as a standalone supervisor process (`zel-process-wrapper` binary):
/// inherits live unbuffered stdout/stderr when unredirected, installs `SIGALRM`/`SIGTERM`/`SIGINT`
/// forwarding, enables `PR_SET_CHILD_SUBREAPER` on Linux to catch double-forked descendants, and
/// returns `(termination, wrapper_signal)` so `main` can re-raise the signal if needed.
pub fn run_standalone(opt: &ProcessWrapperOptions) -> std::io::Result<(Termination, Option<i32>)> {
    STANDALONE_WRAPPER_SIG.store(0, std::sync::atomic::Ordering::SeqCst);

    #[cfg(unix)]
    unsafe {
        let _ = signal(SIGALRM, on_wrapper_signal as *const () as usize);
        let _ = signal(SIGTERM, on_wrapper_signal as *const () as usize);
        let _ = signal(SIGINT, on_wrapper_signal as *const () as usize);
    }

    #[cfg(target_os = "linux")]
    unsafe {
        const PR_SET_CHILD_SUBREAPER: i32 = 36;
        let _ = prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
    }

    let mut cmd = build_wrapped_command(&opt.args);
    cmd.stdin(Stdio::inherit());
    match effective_redirect_path(opt.stdout_path.as_deref()) {
        Some(p) => {
            cmd.stdout(Stdio::from(open_redirect(p)?));
        }
        None => {
            cmd.stdout(Stdio::inherit());
        }
    }
    match effective_redirect_path(opt.stderr_path.as_deref()) {
        Some(p) => {
            cmd.stderr(Stdio::from(open_redirect(p)?));
        }
        None => {
            cmd.stderr(Stdio::inherit());
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                reset_child_signals_and_mask();
                if setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                umask(0o022);
                Ok(())
            });
        }
    }

    let started = Instant::now();
    #[cfg_attr(unix, allow(unused_mut))]
    let mut child = cmd.spawn()?;
    let pid = child.id() as i32;
    let timeout = (opt.timeout_secs > 0.0).then(|| Duration::from_secs_f64(opt.timeout_secs));
    let kill_delay = Duration::from_secs_f64(opt.kill_delay_secs.max(0.0));
    let mut term_sent: Option<Instant> = None;
    let mut killed = false;
    let mut wrapper_sig: Option<i32> = None;

    #[cfg(unix)]
    let (termination, usage) = {
        let mut status: i32 = 0;
        let mut ru = Rusage::default();
        loop {
            let r = unsafe { wait4(pid, &mut status, WNOHANG, &mut ru) };
            if r == pid {
                break;
            }
            if r < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            let got_sig = STANDALONE_WRAPPER_SIG.swap(0, std::sync::atomic::Ordering::SeqCst);
            if got_sig != 0 {
                wrapper_sig = Some(got_sig);
                let graceful = got_sig == SIGALRM || (got_sig == SIGTERM && opt.graceful_sigterm);
                if graceful {
                    if term_sent.is_none() {
                        kill_everything_once(pid as u32, SIGTERM, true);
                        term_sent = Some(Instant::now());
                    }
                } else if !killed {
                    kill_everything_once(pid as u32, SIGKILL, true);
                    killed = true;
                }
            }
            if let Some(t) = timeout {
                if wrapper_sig.is_none() && started.elapsed() >= t {
                    wrapper_sig = Some(SIGALRM);
                    if term_sent.is_none() {
                        kill_everything_once(pid as u32, SIGTERM, true);
                        term_sent = Some(Instant::now());
                    }
                }
            }
            if let Some(at) = term_sent {
                if !killed && at.elapsed() >= kill_delay {
                    kill_everything_once(pid as u32, SIGKILL, true);
                    killed = true;
                }
            }
            std::thread::sleep(Duration::from_micros(500));
        }
        let usage = ResourceUsage::from_rusage(&ru);
        if let Some(p) = opt.stats_path.as_deref().filter(|s| !s.is_empty()) {
            std::fs::write(p, usage.encode_execution_statistics())?;
        }
        #[cfg(target_os = "macos")]
        darwin_kill_pgrp_members(pid, SIGKILL);
        kill_everything_once(pid as u32, SIGKILL, true);
        // Reap any reparented grandchildren adopted via PR_SET_CHILD_SUBREAPER.
        loop {
            let mut sub_status: i32 = 0;
            let mut sub_ru = Rusage::default();
            let r = unsafe { wait4(-1, &mut sub_status, 0, &mut sub_ru) };
            if r < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            kill_everything_once(pid as u32, SIGKILL, true);
        }
        let termination = if status & 0x7f == 0 {
            Termination::Exited((status >> 8) & 0xff)
        } else {
            Termination::Signaled(status & 0x7f)
        };
        (termination, usage)
    };
    #[cfg(not(unix))]
    let (termination, usage) = {
        let _ = (pid, &mut term_sent, kill_delay);
        let st = loop {
            if let Some(st) = child.try_wait()? {
                break st;
            }
            if let Some(t) = timeout {
                if wrapper_sig.is_none() && started.elapsed() >= t {
                    wrapper_sig = Some(SIGALRM);
                    let _ = child.kill();
                    killed = true;
                }
            }
            let _ = killed;
            std::thread::sleep(Duration::from_millis(1));
        };
        let usage = ResourceUsage::default();
        if let Some(p) = opt.stats_path.as_deref().filter(|s| !s.is_empty()) {
            std::fs::write(p, usage.encode_execution_statistics())?;
        }
        (Termination::Exited(st.code().unwrap_or(1)), usage)
    };
    let _ = usage;
    drop(child);
    Ok((termination, wrapper_sig))
}

/// Normalizes `argv[0]` for Windows `CreateProcessW`, matching Bazel's
/// `WindowsSubprocessFactory.processArgv0` and `AsExecutablePathForCreateProcess`: any executable
/// path containing `'/'` or `'\\'` has `'/'` converted to `'\\'` so Windows command-line parsers
/// such as `cmd.exe` do not treat `'/'` segments in their own `argv[0]` as switches.
pub fn normalize_windows_argv0(argv0: &str) -> String {
    if argv0.contains('/') || argv0.contains('\\') {
        argv0.replace('/', "\\")
    } else {
        argv0.to_string()
    }
}

fn build_wrapped_command(args: &[String]) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let prog = normalize_windows_argv0(&args[0]);
        let mut c = Command::new(&prog);
        // `WindowsSubprocessFactory.escapeArgvRest`: when `argv0.equals("cmd.exe")`, pass
        // remaining arguments verbatim without `CommandLineToArgvW` backslash-quote escaping.
        if prog == "cmd.exe" {
            for arg in &args[1..] {
                c.raw_arg(arg);
            }
        } else {
            c.args(&args[1..]);
        }
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new(&args[0]);
        c.args(&args[1..]);
        c
    }
}

/// Runs the wrapped command (`LegacyProcessWrapper::RunCommand`).
///
/// `env`: when `Some`, the child environment is cleared and replaced by it. `cwd`: working
/// directory. Output is captured unless the options redirect it to files.
pub fn run(
    opt: &ProcessWrapperOptions,
    env: Option<&std::collections::BTreeMap<String, String>>,
    cwd: Option<&str>,
) -> std::io::Result<WrapperOutcome> {
    if global_cancel_requested() {
        return Err(crate::cancelled_error());
    }
    let started = Instant::now();
    let mut cmd = build_wrapped_command(&opt.args);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    if let Some(e) = env {
        cmd.env_clear().envs(e);
    }
    if opt.close_stdin {
        cmd.stdin(Stdio::null());
    } else {
        cmd.stdin(Stdio::inherit());
    }
    let stdout_redir = effective_redirect_path(opt.stdout_path.as_deref());
    let stderr_redir = effective_redirect_path(opt.stderr_path.as_deref());
    let capture_out = stdout_redir.is_none();
    let capture_err = stderr_redir.is_none();
    match stdout_redir {
        Some(p) => {
            cmd.stdout(Stdio::from(open_redirect(p)?));
        }
        None => {
            cmd.stdout(Stdio::piped());
        }
    }
    match stderr_redir {
        Some(p) => {
            cmd.stderr(Stdio::from(open_redirect(p)?));
        }
        None => {
            cmd.stderr(Stdio::piped());
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: reset_child_signals_and_mask, setsid, and umask are async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                reset_child_signals_and_mask();
                if setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                umask(0o022);
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn()?;
    let pid = child.id() as i32;
    let readers: Vec<std::thread::JoinHandle<Vec<u8>>> = {
        let mut v = Vec::new();
        if capture_out {
            let mut p = child.stdout.take().unwrap();
            let stream_cb = opt.stream_out.clone();
            v.push(std::thread::spawn(move || {
                let mut b = Vec::new();
                let mut chunk = [0u8; 4096];
                while let Ok(n) = p.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    b.extend_from_slice(&chunk[..n]);
                    if let Some(cb) = &stream_cb {
                        (cb.0)(&chunk[..n]);
                    }
                }
                b
            }));
        }
        if capture_err {
            let mut p = child.stderr.take().unwrap();
            let stream_cb = opt.stream_out.clone();
            v.push(std::thread::spawn(move || {
                let mut b = Vec::new();
                let mut chunk = [0u8; 4096];
                while let Ok(n) = p.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    b.extend_from_slice(&chunk[..n]);
                    if let Some(cb) = &stream_cb {
                        (cb.0)(&chunk[..n]);
                    }
                }
                b
            }));
        }
        v
    };

    let timeout = (opt.timeout_secs > 0.0).then(|| Duration::from_secs_f64(opt.timeout_secs));
    let kill_delay = Duration::from_secs_f64(opt.kill_delay_secs.max(0.0));
    let mut term_sent: Option<Instant> = None;
    let mut killed = false;
    let mut timed_out = false;
    let mut cancelled = false;
    #[cfg(unix)]
    let (termination, usage) = {
        let _sigterm_guard = SigtermGuard::install(opt.graceful_sigterm);
        let mut forwarded_sigterm = false;
        let mut status: i32 = 0;
        let mut ru = Rusage::default();
        loop {
            // SAFETY: valid pointers to locals.
            let r = unsafe { wait4(pid, &mut status, WNOHANG, &mut ru) };
            if r == pid {
                break;
            }
            if r < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if opt.graceful_sigterm
                && !forwarded_sigterm
                && GOT_SIGTERM.load(std::sync::atomic::Ordering::SeqCst)
            {
                forwarded_sigterm = true;
                signal_process_group(pid as u32, SIGTERM);
                if term_sent.is_none() {
                    term_sent = Some(Instant::now());
                }
            }
            if let Some(t) = timeout {
                if !timed_out && started.elapsed() >= t {
                    timed_out = true;
                    // SIGALRM => OnGracefulSignal => KillEverything(pid, graceful=true, delay).
                    signal_process_group(pid as u32, SIGTERM);
                    if term_sent.is_none() {
                        term_sent = Some(Instant::now());
                    }
                }
            }
            let ext_sig = WRAPPER_SIGNAL.load(std::sync::atomic::Ordering::SeqCst);
            let cancel_now = global_cancel_requested()
                || opt.cancel.as_ref().is_some_and(|c| c.is_cancelled())
                || ext_sig != 0;
            if !killed && cancel_now {
                if ext_sig == 0 {
                    cancelled = true;
                }
                if opt.graceful_sigterm {
                    forwarded_sigterm = true;
                    if term_sent.is_none() {
                        signal_process_group(pid as u32, SIGTERM);
                        term_sent = Some(Instant::now());
                    }
                } else {
                    signal_process_group(pid as u32, SIGKILL);
                    killed = true;
                }
            }
            if let Some(at) = term_sent {
                if !killed && at.elapsed() >= kill_delay {
                    signal_process_group(pid as u32, SIGKILL);
                    killed = true;
                }
            }
            std::thread::sleep(Duration::from_micros(500));
        }
        let usage = ResourceUsage::from_rusage(&ru);
        if let Some(p) = opt.stats_path.as_deref().filter(|s| !s.is_empty()) {
            std::fs::write(p, usage.encode_execution_statistics())?;
        }
        // The child is done for, but may have grandchildren that we still have to kill.
        signal_process_group(pid as u32, SIGKILL);
        let received_sig = take_wrapper_signal();
        // Decode the wait status (WIFEXITED / WTERMSIG).
        let termination = if timed_out {
            Termination::Signaled(SIGALRM)
        } else if received_sig != 0 {
            Termination::Signaled(received_sig)
        } else if forwarded_sigterm {
            Termination::Signaled(SIGTERM)
        } else if status & 0x7f == 0 {
            Termination::Exited((status >> 8) & 0xff)
        } else {
            Termination::Signaled(status & 0x7f)
        };
        (termination, usage)
    };
    #[cfg(not(unix))]
    let (termination, usage) = {
        let _ = (pid, &mut term_sent, kill_delay);
        let exit_status = loop {
            if let Some(st) = child.try_wait()? {
                break st;
            }
            if let Some(t) = timeout {
                if !timed_out && started.elapsed() >= t {
                    timed_out = true;
                    let _ = child.kill();
                    killed = true;
                }
            }
            if !killed && global_cancel_requested() {
                cancelled = true;
                let _ = child.kill();
                killed = true;
            }
            if let Some(c) = &opt.cancel {
                if !killed && c.is_cancelled() {
                    cancelled = true;
                    let _ = child.kill();
                    killed = true;
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        let termination = if timed_out {
            Termination::Signaled(SIGALRM)
        } else {
            Termination::Exited(exit_status.code().unwrap_or(1))
        };
        let usage = ResourceUsage::default();
        if let Some(p) = opt.stats_path.as_deref().filter(|s| !s.is_empty()) {
            std::fs::write(p, usage.encode_execution_statistics())?;
        }
        (termination, usage)
    };
    let mut outs = readers.into_iter().map(|h| h.join().unwrap_or_default());
    let stdout = if capture_out {
        outs.next().unwrap_or_default()
    } else {
        Vec::new()
    };
    let stderr = if capture_err {
        outs.next().unwrap_or_default()
    } else {
        Vec::new()
    };
    drop(child);

    Ok(WrapperOutcome {
        termination,
        timed_out,
        stdout,
        stderr,
        usage: Some(usage),
        wall: started.elapsed(),
        cancelled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_all_options_and_command() {
        let o = parse_args(
            "pw",
            &s(&[
                "-g",
                "-t",
                "2.5",
                "--kill_delay=3",
                "-o",
                "out",
                "--stderr",
                "err",
                "-s",
                "st",
                "-d",
                "--",
                "echo",
                "-n",
                "x",
            ]),
        )
        .unwrap();
        assert!(o.graceful_sigterm && o.debug);
        assert_eq!(o.timeout_secs, 2.5);
        assert_eq!(o.kill_delay_secs, 3.0);
        assert_eq!(o.stdout_path.as_deref(), Some("out"));
        assert_eq!(o.stderr_path.as_deref(), Some("err"));
        assert_eq!(o.stats_path.as_deref(), Some("st"));
        assert_eq!(o.args, s(&["echo", "-n", "x"]));
    }

    #[test]
    fn plus_getopt_stops_at_first_non_option() {
        let o = parse_args("pw", &s(&["-t", "1", "ls", "-t", "9"])).unwrap();
        assert_eq!(o.args, s(&["ls", "-t", "9"]));
        assert_eq!(o.timeout_secs, 1.0);
    }

    #[test]
    fn usage_errors_match_c_messages() {
        let e = parse_args("pw", &s(&["-t", "abc", "--", "x"])).unwrap_err();
        assert!(e.starts_with("Invalid timeout (-t) value: abc\n\nUsage: pw -- command arg1 @args"));
        let e = parse_args("pw", &s(&["-o", "a", "-o", "b", "x"])).unwrap_err();
        assert!(e.starts_with("Cannot redirect stdout (-o) to more than one destination."));
        let e = parse_args("pw", &s(&["-e", "a", "-e", "b", "x"])).unwrap_err();
        assert!(e.starts_with("Cannot redirect stderr (-e) to more than one destination."));
        let e = parse_args("pw", &s(&["-s", "a", "-s", "b", "x"])).unwrap_err();
        assert!(e.starts_with("Cannot write stats (-s) to more than one destination."));
        let e = parse_args("pw", &s(&["-t"])).unwrap_err();
        assert!(e.starts_with("Flag -t requires an argument"));
        let e = parse_args("pw", &s(&["-z", "x"])).unwrap_err();
        assert!(e.starts_with("Unrecognized argument: -z (1)"));
        let e = parse_args("pw", &s(&["-d"])).unwrap_err();
        assert!(e.starts_with("No command specified."));
    }

    #[test]
    fn sscanf_lf_semantics() {
        assert_eq!(scan_double("10s"), Some(10.0));
        assert_eq!(scan_double(".5"), Some(0.5));
        assert_eq!(scan_double("1e2"), Some(100.0));
        assert_eq!(scan_double("x"), None);
    }

    #[test]
    fn stats_proto_round_trip() {
        let u = ResourceUsage {
            utime_sec: 1,
            utime_usec: 500,
            maxrss: 4096,
            nvcsw: 7,
            ..Default::default()
        };
        let b = u.encode_execution_statistics();
        assert_eq!(ResourceUsage::decode_execution_statistics(&b).unwrap(), u);
    }

    #[cfg(unix)]
    #[test]
    fn redirects_stats_and_exit_code() {
        let d = std::env::temp_dir().join(format!("zel-pw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let o = ProcessWrapperOptions {
            stdout_path: Some(d.join("o").to_str().unwrap().into()),
            stderr_path: Some(d.join("e").to_str().unwrap().into()),
            stats_path: Some(d.join("s").to_str().unwrap().into()),
            args: s(&["/bin/sh", "-c", "echo out; echo err >&2; exit 7"]),
            ..Default::default()
        };
        let r = run(&o, None, None).unwrap();
        assert_eq!(r.termination, Termination::Exited(7));
        assert_eq!(std::fs::read_to_string(d.join("o")).unwrap(), "out\n");
        assert_eq!(std::fs::read_to_string(d.join("e")).unwrap(), "err\n");
        let st = ResourceUsage::decode_execution_statistics(&std::fs::read(d.join("s")).unwrap())
            .unwrap();
        assert!(st.maxrss > 0);
    }

    #[cfg(unix)]
    #[test]
    fn timeout_sends_term_then_kill_after_delay_and_reports_sigalrm() {
        // Child ignores SIGTERM, so only SIGKILL after the kill delay ends it.
        let o = ProcessWrapperOptions {
            timeout_secs: 0.2,
            kill_delay_secs: 0.4,
            args: s(&["/bin/sh", "-c", "trap '' TERM; sleep 30 & wait"]),
            ..Default::default()
        };
        let t = Instant::now();
        let r = run(&o, None, None).unwrap();
        assert!(r.timed_out);
        assert_eq!(r.termination, Termination::Signaled(SIGALRM));
        assert_eq!(r.termination.exit_code(), 142);
        let el = t.elapsed().as_secs_f64();
        assert!((0.55..5.0).contains(&el), "elapsed {el}");
    }

    #[cfg(unix)]
    #[test]
    fn graceful_term_lets_child_exit_within_delay() {
        let o = ProcessWrapperOptions {
            timeout_secs: 0.2,
            kill_delay_secs: 5.0,
            args: s(&[
                "/bin/sh",
                "-c",
                "trap 'echo bye; exit 0' TERM; while :; do sleep 0.05; done",
            ]),
            ..Default::default()
        };
        let t = Instant::now();
        let r = run(&o, None, None).unwrap();
        assert!(r.timed_out);
        assert!(t.elapsed().as_secs_f64() < 3.0);
        assert!(String::from_utf8_lossy(&r.stdout).contains("bye"));
    }

    #[cfg(unix)]
    #[test]
    fn orphans_are_killed_and_signal_death_is_reported() {
        let o = ProcessWrapperOptions {
            args: s(&["/bin/sh", "-c", "kill -9 $$"]),
            ..Default::default()
        };
        let r = run(&o, None, None).unwrap();
        assert_eq!(r.termination, Termination::Signaled(9));
        let o = ProcessWrapperOptions {
            args: s(&["/bin/sh", "-c", "(sleep 30 &) ; exit 0"]),
            ..Default::default()
        };
        let t = Instant::now();
        let r = run(&o, None, None).unwrap();
        assert_eq!(r.termination, Termination::Exited(0));
        assert!(t.elapsed().as_secs() < 5);
    }

    #[cfg(unix)]
    #[test]
    fn child_umask_is_022_and_runs_in_new_session() {
        let o = ProcessWrapperOptions {
            args: s(&["/bin/sh", "-c", "umask"]),
            ..Default::default()
        };
        let r = run(&o, None, None).unwrap();
        assert_eq!(String::from_utf8_lossy(&r.stdout).trim(), "0022");
    }

    #[cfg(unix)]
    #[test]
    fn empty_and_dash_redirects_dev_null_and_child_sigpipe_default() {
        // `-o "" -o real` accepts the second `-o` because empty string means unset.
        let parsed = parse_args("pw", &s(&["-o", "", "-o", "/dev/null", "--", "true"])).unwrap();
        assert_eq!(parsed.stdout_path.as_deref(), Some("/dev/null"));

        // Redirecting to `/dev/null` works without `ftruncate` EINVAL, and `-` means no file redirect.
        let o = ProcessWrapperOptions {
            stdout_path: Some("-".into()),
            stderr_path: Some("/dev/null".into()),
            args: s(&["/bin/sh", "-c", "echo live_out; echo discarded_err >&2"]),
            ..Default::default()
        };
        let r = run(&o, None, None).unwrap();
        assert_eq!(r.termination, Termination::Exited(0));
        assert_eq!(r.stdout, b"live_out\n");
        assert!(r.stderr.is_empty());

        // Child receives SIGPIPE with default disposition instead of inheriting Rust's SIG_IGN.
        let o = ProcessWrapperOptions {
            args: s(&["/bin/sh", "-c", "kill -PIPE $$"]),
            ..Default::default()
        };
        let r = run(&o, None, None).unwrap();
        assert_eq!(r.termination, Termination::Signaled(SIGPIPE));
    }

    #[test]
    fn command_line_builder() {
        let b = CommandLineBuilder {
            wrapper: "/pw".into(),
            args: s(&["a", "b"]),
            timeout: Some(Duration::from_secs(5)),
            kill_delay: Some(Duration::from_secs(15)),
            stats_path: Some("/s".into()),
            ..Default::default()
        };
        assert_eq!(
            b.build(),
            s(&[
                "/pw",
                "--timeout=5",
                "--kill_delay=15",
                "--stats=/s",
                "a",
                "b"
            ])
        );
    }

    #[test]
    fn windows_argv0_normalization_replaces_forward_slashes() {
        assert_eq!(
            normalize_windows_argv0("C:/Windows/system32/cmd.exe"),
            r"C:\Windows\system32\cmd.exe"
        );
        assert_eq!(
            normalize_windows_argv0("bazel-out/x64_windows-fastbuild/bin/tool.exe"),
            r"bazel-out\x64_windows-fastbuild\bin\tool.exe"
        );
        assert_eq!(normalize_windows_argv0("cmd.exe"), "cmd.exe");
    }

    #[cfg(windows)]
    #[test]
    fn windows_cmd_exe_accepts_forward_slashed_comspec_and_raw_args() {
        let dir = std::env::temp_dir().join(format!("zel-pw-cmd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub dir")).unwrap();
        std::fs::write(dir.join("a.txt"), "hello ").unwrap();
        std::fs::write(dir.join("b_nospace.txt"), "world\n").unwrap();
        std::fs::write(dir.join("sub dir").join("b.txt"), "world\n").unwrap();

        let comspec = std::env::var("ComSpec")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| r"C:\Windows\System32\cmd.exe".to_string())
            .replace('\\', "/");
        let o1 = ProcessWrapperOptions {
            args: vec![
                comspec,
                "/d".into(),
                "/s".into(),
                "/c".into(),
                r"copy /y /b a.txt+b_nospace.txt out1.txt >nul".into(),
            ],
            ..Default::default()
        };
        let r1 = run(&o1, None, Some(dir.to_str().unwrap())).unwrap();
        assert_eq!(
            r1.termination,
            Termination::Exited(0),
            "stderr={}",
            String::from_utf8_lossy(&r1.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("out1.txt")).unwrap(),
            "hello world\n"
        );

        let o2 = ProcessWrapperOptions {
            args: vec![
                "cmd.exe".into(),
                "/S".into(),
                "/E:ON".into(),
                "/V:ON".into(),
                "/D".into(),
                "/c".into(),
                r#"copy /y /b "a.txt"+"sub dir\b.txt" "out2.txt" >nul"#.into(),
            ],
            ..Default::default()
        };
        let r2 = run(&o2, None, Some(dir.to_str().unwrap())).unwrap();
        assert_eq!(
            r2.termination,
            Termination::Exited(0),
            "stderr={}",
            String::from_utf8_lossy(&r2.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("out2.txt")).unwrap(),
            "hello world\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn graceful_sigterm_forwards_term_and_reports_sigterm() {
        let cancel = CancelFlag::new();
        let cancel_bg = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            cancel_bg.cancel();
        });
        let o = ProcessWrapperOptions {
            graceful_sigterm: true,
            kill_delay_secs: 5.0,
            cancel: Some(cancel),
            args: s(&[
                "/bin/sh",
                "-c",
                "trap 'echo got_term; exit 0' TERM; while :; do sleep 0.05; done",
            ]),
            ..Default::default()
        };
        let t = Instant::now();
        let r = run(&o, None, None).unwrap();
        assert!(r.cancelled);
        assert_eq!(r.termination, Termination::Signaled(SIGTERM));
        assert_eq!(r.termination.exit_code(), 143);
        assert!(t.elapsed().as_secs_f64() < 3.0);
        assert!(String::from_utf8_lossy(&r.stdout).contains("got_term"));
    }
}
