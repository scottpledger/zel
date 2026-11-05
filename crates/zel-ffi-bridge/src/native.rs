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

//! Rust equivalents of Bazel's native layer (`src/main/native`), used directly by Zel crates
//! instead of through JNI:
//!
//! | Bazel native source                        | Here                                         |
//! |--------------------------------------------|----------------------------------------------|
//! | `unix_jni.cc` `NativePosixFilesServiceImpl` | [`posix`] (stat with ns times, readdir types, readlink, symlink, link, chmod, utimensat, mkfifo, getxattr, deleteTreesBelow) |
//! | `darwin/fsevents.cc`                       | [`watch::FileWatcher`] (kqueue on macOS, inotify on Linux) |
//! | `darwin/system_memory_pressure_jni.cc`     | [`system::memory_pressure`]                  |
//! | `darwin/system_load_advisory_monitor_jni.cc` | [`system::load_average`]                   |
//! | `darwin/system_disk_space_monitor_jni.cc`  | [`system::disk_space`]                       |
//! | `darwin/system_cpu_speed_monitor_jni.cc`   | [`system::cpu_speed_mhz`]                    |
//! | `unix_jni_linux.cc` network stats          | [`system::net_io_counters`] (Linux)          |
//! | `unix_jni.cc` process helpers              | [`process`]                                  |
//!
//! Not implemented (documented gaps): sleep prevention, suspension and thermal monitors
//! (macOS IOKit/CoreFoundation notification APIs), FSEvents proper (the kqueue watcher gives the
//! same "what changed" answers for the sizes of trees Zel watches, without a CF run loop),
//! Windows, and BLAKE3 (`blake3_jni.cc`).

#![cfg(unix)]

use std::ffi::CString;
use std::io;
use std::os::raw::{c_char, c_int, c_long, c_uint, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

fn cpath(p: &Path) -> io::Result<CString> {
    CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

fn errno() -> io::Error {
    io::Error::last_os_error()
}

extern "C" {
    fn mkfifo(path: *const c_char, mode: c_uint) -> c_int;
    fn getloadavg(loadavg: *mut f64, nelem: c_int) -> c_int;
    fn kill(pid: c_int, sig: c_int) -> c_int;
    fn getppid() -> c_int;
    fn getpriority(which: c_int, who: c_uint) -> c_int;
    fn setpriority(which: c_int, who: c_uint, prio: c_int) -> c_int;
    fn sysconf(name: c_int) -> c_long;
    fn getrusage(who: c_int, usage: *mut RUsage) -> c_int;
    fn statvfs(path: *const c_char, buf: *mut StatVfs) -> c_int;
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct TimeVal {
    sec: i64,
    usec: i64,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct RUsage {
    utime: TimeVal,
    stime: TimeVal,
    maxrss: c_long,
    rest: [c_long; 13],
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Default)]
struct StatVfs {
    bsize: u64,
    frsize: u64,
    blocks: u32,
    bfree: u32,
    bavail: u32,
    files: u32,
    ffree: u32,
    favail: u32,
    fsid: u64,
    flag: u64,
    namemax: u64,
}

#[cfg(not(target_os = "macos"))]
#[repr(C)]
#[derive(Default)]
struct StatVfs {
    bsize: u64,
    frsize: u64,
    blocks: u64,
    bfree: u64,
    bavail: u64,
    files: u64,
    ffree: u64,
    favail: u64,
    fsid: u64,
    flag: u64,
    namemax: u64,
    spare: [c_int; 6],
}

/// `NativePosixFilesServiceImpl` equivalents.
pub mod posix {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    /// `FileStatus` as returned by native `stat`/`lstat` (nanosecond timestamps).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StatNs {
        pub mode: u32,
        pub size: u64,
        pub mtime_ns: i128,
        pub atime_ns: i128,
        pub ctime_ns: i128,
        pub dev: u64,
        pub ino: u64,
        pub nlink: u64,
        pub is_dir: bool,
        pub is_file: bool,
        pub is_symlink: bool,
    }

    fn convert(m: &std::fs::Metadata) -> StatNs {
        let ft = m.file_type();
        StatNs {
            mode: m.mode(),
            size: m.size(),
            mtime_ns: m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128,
            atime_ns: m.atime() as i128 * 1_000_000_000 + m.atime_nsec() as i128,
            ctime_ns: m.ctime() as i128 * 1_000_000_000 + m.ctime_nsec() as i128,
            dev: m.dev(),
            ino: m.ino(),
            nlink: m.nlink(),
            is_dir: ft.is_dir(),
            is_file: ft.is_file(),
            is_symlink: ft.is_symlink(),
        }
    }

    pub fn stat(p: &Path) -> io::Result<StatNs> {
        std::fs::metadata(p).map(|m| convert(&m))
    }

    pub fn lstat(p: &Path) -> io::Result<StatNs> {
        std::fs::symlink_metadata(p).map(|m| convert(&m))
    }

    pub fn readlink(p: &Path) -> io::Result<std::path::PathBuf> {
        std::fs::read_link(p)
    }

    pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    pub fn link(existing: &Path, new: &Path) -> io::Result<()> {
        std::fs::hard_link(existing, new)
    }

    pub fn chmod(p: &Path, mode: u32) -> io::Result<()> {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
    }

    /// `utimensat`: sets the modification time (`None` = now, like `UTIME_NOW`).
    pub fn utimensat(p: &Path, mtime_ns: Option<i128>) -> io::Result<()> {
        let t = match mtime_ns {
            None => std::time::SystemTime::now(),
            Some(ns) if ns >= 0 => std::time::UNIX_EPOCH + std::time::Duration::new((ns / 1_000_000_000) as u64, (ns % 1_000_000_000) as u32),
            Some(ns) => std::time::UNIX_EPOCH - std::time::Duration::from_nanos((-ns) as u64),
        };
        std::fs::File::options().write(true).open(p).or_else(|_| std::fs::File::open(p))?.set_modified(t)
    }

    /// `mkdir`; returns `Ok(false)` when the directory already exists (like the native call).
    pub fn mkdir(p: &Path, mode: u32) -> io::Result<bool> {
        use std::os::unix::fs::DirBuilderExt;
        match std::fs::DirBuilder::new().mode(mode).create(p) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && p.is_dir() => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub fn mkfifo(p: &Path, mode: u32) -> io::Result<()> {
        let c = cpath(p)?;
        // SAFETY: `c` is a valid NUL-terminated path.
        if unsafe { super::mkfifo(c.as_ptr(), mode) } == 0 { Ok(()) } else { Err(errno()) }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DirentType {
        File,
        Directory,
        Symlink,
        Unknown,
    }

    /// `readdir` with entry types (sorted by name for determinism).
    pub fn readdir(p: &Path) -> io::Result<Vec<(String, DirentType)>> {
        let mut v = Vec::new();
        for e in std::fs::read_dir(p)? {
            let e = e?;
            let t = match e.file_type() {
                Ok(t) if t.is_symlink() => DirentType::Symlink,
                Ok(t) if t.is_dir() => DirentType::Directory,
                Ok(t) if t.is_file() => DirentType::File,
                _ => DirentType::Unknown,
            };
            v.push((e.file_name().to_string_lossy().into_owned(), t));
        }
        v.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(v)
    }

    /// `deleteTreesBelow`: deletes everything inside `dir` (not `dir` itself), fixing
    /// permissions of unreadable directories on the way.
    pub fn delete_trees_below(dir: &Path) -> io::Result<()> {
        let _ = chmod(dir, 0o755);
        for (name, t) in readdir(dir)? {
            let p = dir.join(&name);
            if t == DirentType::Directory {
                delete_trees_below(&p)?;
                std::fs::remove_dir(&p)?;
            } else {
                std::fs::remove_file(&p)?;
            }
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    extern "C" {
        fn getxattr(path: *const c_char, name: *const c_char, value: *mut c_void, size: usize, position: u32, options: c_int) -> isize;
    }
    #[cfg(not(target_os = "macos"))]
    extern "C" {
        fn getxattr(path: *const c_char, name: *const c_char, value: *mut c_void, size: usize) -> isize;
        fn lgetxattr(path: *const c_char, name: *const c_char, value: *mut c_void, size: usize) -> isize;
    }

    /// `getxattr`/`lgetxattr`: `Ok(None)` when the attribute does not exist.
    pub fn getxattr_value(p: &Path, name: &str, follow_symlinks: bool) -> io::Result<Option<Vec<u8>>> {
        let c = cpath(p)?;
        let n = CString::new(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad xattr name"))?;
        let mut buf = vec![0u8; 4096];
        loop {
            #[cfg(target_os = "macos")]
            // SAFETY: valid C strings and a writable buffer of `buf.len()` bytes.
            let r = unsafe { getxattr(c.as_ptr(), n.as_ptr(), buf.as_mut_ptr() as *mut c_void, buf.len(), 0, if follow_symlinks { 0 } else { 0x0001 }) };
            #[cfg(not(target_os = "macos"))]
            // SAFETY: valid C strings and a writable buffer of `buf.len()` bytes.
            let r = unsafe {
                if follow_symlinks {
                    getxattr(c.as_ptr(), n.as_ptr(), buf.as_mut_ptr() as *mut c_void, buf.len())
                } else {
                    lgetxattr(c.as_ptr(), n.as_ptr(), buf.as_mut_ptr() as *mut c_void, buf.len())
                }
            };
            if r >= 0 {
                buf.truncate(r as usize);
                return Ok(Some(buf));
            }
            let e = errno();
            match e.raw_os_error() {
                Some(c) if c == 34 /* ERANGE */ => buf.resize(buf.len() * 4, 0),
                #[cfg(target_os = "macos")]
                Some(93) => return Ok(None), // ENOATTR
                #[cfg(not(target_os = "macos"))]
                Some(61) => return Ok(None), // ENODATA
                _ => return Err(e),
            }
        }
    }
}

/// Process helpers (`unix_jni.cc`, `processes-jni.cc` equivalents).
pub mod process {
    use super::*;

    pub fn pid() -> u32 {
        std::process::id()
    }

    pub fn parent_pid() -> u32 {
        // SAFETY: getppid has no preconditions.
        unsafe { getppid() as u32 }
    }

    /// Whether `pid` exists (signal 0).
    pub fn is_alive(pid: u32) -> bool {
        // SAFETY: signal 0 only checks permissions/existence.
        let r = unsafe { kill(pid as c_int, 0) };
        r == 0 || errno().raw_os_error() == Some(1) /* EPERM */
    }

    pub fn terminate(pid: u32) -> io::Result<()> {
        // SAFETY: plain kill(2).
        if unsafe { kill(pid as c_int, 15) } == 0 { Ok(()) } else { Err(errno()) }
    }

    pub fn kill_hard(pid: u32) -> io::Result<()> {
        // SAFETY: plain kill(2).
        if unsafe { kill(pid as c_int, 9) } == 0 { Ok(()) } else { Err(errno()) }
    }

    pub fn nice() -> i32 {
        // SAFETY: PRIO_PROCESS=0, who=0 is the calling process.
        unsafe { getpriority(0, 0) }
    }

    pub fn set_nice(n: i32) -> io::Result<()> {
        // SAFETY: as above.
        if unsafe { setpriority(0, 0, n) } == 0 { Ok(()) } else { Err(errno()) }
    }

    /// `(user_cpu, system_cpu, max_rss_bytes)` of this process.
    pub fn resource_usage() -> (std::time::Duration, std::time::Duration, u64) {
        let mut ru = RUsage::default();
        // SAFETY: `ru` is a correctly sized, writable rusage buffer (RUSAGE_SELF = 0).
        unsafe { getrusage(0, &mut ru) };
        let d = |t: TimeVal| std::time::Duration::new(t.sec as u64, (t.usec as u32) * 1000);
        // macOS reports bytes, Linux kilobytes.
        let rss = if cfg!(target_os = "macos") { ru.maxrss as u64 } else { ru.maxrss as u64 * 1024 };
        (d(ru.utime), d(ru.stime), rss)
    }

    pub fn cpu_count() -> usize {
        // SAFETY: sysconf(_SC_NPROCESSORS_ONLN).
        let n = unsafe { sysconf(if cfg!(target_os = "macos") { 58 } else { 84 }) };
        if n > 0 { n as usize } else { std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) }
    }
}

/// Host monitors (memory pressure, load, disk space, CPU speed, network).
pub mod system {
    use super::*;

    /// `SystemMemoryPressure` levels (macOS `kern.memorystatus_vm_pressure_level`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub enum MemoryPressure {
        Normal,
        Warning,
        Critical,
    }

    #[cfg(target_os = "macos")]
    extern "C" {
        fn sysctlbyname(name: *const c_char, oldp: *mut c_void, oldlenp: *mut usize, newp: *mut c_void, newlen: usize) -> c_int;
    }

    #[cfg(target_os = "macos")]
    pub fn sysctl_u64(name: &str) -> Option<u64> {
        let c = CString::new(name).ok()?;
        let mut v: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        // SAFETY: `v`/`len` describe a valid 8-byte output buffer; we accept 4-byte results too.
        let r = unsafe { sysctlbyname(c.as_ptr(), &mut v as *mut u64 as *mut c_void, &mut len, std::ptr::null_mut(), 0) };
        if r != 0 {
            return None;
        }
        Some(if len == 4 { v & 0xffff_ffff } else { v })
    }

    /// Current system memory pressure.
    pub fn memory_pressure() -> MemoryPressure {
        #[cfg(target_os = "macos")]
        {
            match sysctl_u64("kern.memorystatus_vm_pressure_level") {
                Some(2) => MemoryPressure::Warning,
                Some(4) => MemoryPressure::Critical,
                _ => MemoryPressure::Normal,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            // PSI: "some avg10=X ..." percentage of time stalled on memory.
            let avg10 = std::fs::read_to_string("/proc/pressure/memory")
                .ok()
                .and_then(|s| {
                    let line = s.lines().find(|l| l.starts_with("some"))?;
                    let v = line.split_whitespace().find_map(|w| w.strip_prefix("avg10="))?;
                    v.parse::<f64>().ok()
                })
                .unwrap_or(0.0);
            if avg10 >= 40.0 {
                MemoryPressure::Critical
            } else if avg10 >= 10.0 {
                MemoryPressure::Warning
            } else {
                MemoryPressure::Normal
            }
        }
    }

    /// Total physical memory in bytes.
    pub fn total_memory() -> Option<u64> {
        #[cfg(target_os = "macos")]
        {
            sysctl_u64("hw.memsize")
        }
        #[cfg(not(target_os = "macos"))]
        {
            let s = std::fs::read_to_string("/proc/meminfo").ok()?;
            let l = s.lines().find(|l| l.starts_with("MemTotal:"))?;
            l.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kb| kb * 1024)
        }
    }

    /// 1-, 5- and 15-minute load averages.
    pub fn load_average() -> Option<[f64; 3]> {
        let mut l = [0f64; 3];
        // SAFETY: `l` has room for 3 doubles.
        let n = unsafe { getloadavg(l.as_mut_ptr(), 3) };
        (n == 3).then_some(l)
    }

    /// `(free_bytes_for_unprivileged, total_bytes)` of the file system containing `p`.
    pub fn disk_space(p: &Path) -> io::Result<(u64, u64)> {
        let c = cpath(p)?;
        let mut s = StatVfs::default();
        // SAFETY: `s` matches the platform's `struct statvfs` layout.
        if unsafe { statvfs(c.as_ptr(), &mut s) } != 0 {
            return Err(errno());
        }
        let bs = if s.frsize != 0 { s.frsize } else { s.bsize };
        Ok((s.bavail as u64 * bs, s.blocks as u64 * bs))
    }

    /// CPU frequency in MHz, if the OS exposes it.
    pub fn cpu_speed_mhz() -> Option<u64> {
        #[cfg(target_os = "macos")]
        {
            sysctl_u64("hw.cpufrequency").map(|hz| hz / 1_000_000)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let s = std::fs::read_to_string("/proc/cpuinfo").ok()?;
            let l = s.lines().find(|l| l.starts_with("cpu MHz"))?;
            l.split(':').nth(1)?.trim().parse::<f64>().ok().map(|m| m as u64)
        }
    }

    /// Per-interface `(name, bytes_received, bytes_sent)` (Linux: `/proc/net/dev`). Returns an
    /// empty list where the OS offers no cheap source (macOS needs `sysctl NET_RT_IFLIST2`).
    pub fn net_io_counters() -> Vec<(String, u64, u64)> {
        let Ok(s) = std::fs::read_to_string("/proc/net/dev") else { return Vec::new() };
        s.lines()
            .skip(2)
            .filter_map(|l| {
                let (name, rest) = l.split_once(':')?;
                let f: Vec<&str> = rest.split_whitespace().collect();
                Some((name.trim().to_string(), f.first()?.parse().ok()?, f.get(8)?.parse().ok()?))
            })
            .collect()
    }
}

/// Recursive change notification (`FsEventsNativeDepsServiceImpl` equivalent).
pub mod watch {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::{Duration, Instant};

    /// What happened to a path under a watched root.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub enum ChangeKind {
        Created,
        Modified,
        Deleted,
    }

    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    mod imp {
        use super::*;

        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct KEvent {
            pub ident: usize,
            pub filter: i16,
            pub flags: u16,
            pub fflags: u32,
            pub data: isize,
            pub udata: *mut c_void,
        }

        #[repr(C)]
        pub struct TimeSpec {
            pub sec: i64,
            pub nsec: i64,
        }

        extern "C" {
            pub fn kqueue() -> c_int;
            pub fn kevent(kq: c_int, changelist: *const KEvent, nchanges: c_int, eventlist: *mut KEvent, nevents: c_int, timeout: *const TimeSpec) -> c_int;
            pub fn open(path: *const c_char, flags: c_int, ...) -> c_int;
            pub fn close(fd: c_int) -> c_int;
        }

        pub const EVFILT_VNODE: i16 = -4;
        pub const EV_ADD: u16 = 0x1;
        pub const EV_CLEAR: u16 = 0x20;
        pub const EV_ENABLE: u16 = 0x4;
        pub const NOTE_ALL: u32 = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x20 | 0x40; // DELETE|WRITE|EXTEND|ATTRIB|LINK|RENAME|REVOKE
        #[cfg(target_os = "macos")]
        pub const O_EVTONLY: c_int = 0x8000;
        #[cfg(not(target_os = "macos"))]
        pub const O_EVTONLY: c_int = 0;
    }

    #[cfg(target_os = "linux")]
    mod imp {
        use super::*;
        extern "C" {
            pub fn inotify_init1(flags: c_int) -> c_int;
            pub fn inotify_add_watch(fd: c_int, path: *const c_char, mask: u32) -> c_int;
            pub fn read(fd: c_int, buf: *mut c_void, n: usize) -> isize;
            pub fn poll(fds: *mut PollFd, n: u64, timeout: c_int) -> c_int;
            pub fn close(fd: c_int) -> c_int;
        }
        #[repr(C)]
        pub struct PollFd {
            pub fd: c_int,
            pub events: i16,
            pub revents: i16,
        }
        pub const IN_NONBLOCK: c_int = 0o4000;
        pub const IN_ALL: u32 = 0x2 | 0x4 | 0x8 | 0x40 | 0x80 | 0x100 | 0x200 | 0x400 | 0x800; // MODIFY|ATTRIB|CLOSE_WRITE|MOVED_FROM|MOVED_TO|CREATE|DELETE|DELETE_SELF|MOVE_SELF
    }

    /// Snapshot-diffing watcher: the OS only says "something changed in X"; the watcher then
    /// rescans X to report the exact paths (and, on Linux/kqueue, registers new subdirectories).
    pub struct FileWatcher {
        root: std::path::PathBuf,
        fd: c_int,
        #[cfg(any(target_os = "macos", target_os = "freebsd"))]
        fds: BTreeMap<std::path::PathBuf, c_int>,
        #[cfg(target_os = "linux")]
        wds: BTreeMap<c_int, std::path::PathBuf>,
        snapshot: BTreeMap<std::path::PathBuf, (u64, u64, i128)>,
        ignore_dirs: BTreeSet<String>,
    }

    type Snap = BTreeMap<std::path::PathBuf, (u64, u64, i128)>;

    fn scan(root: &Path, ignore: &BTreeSet<String>, out: &mut Snap) {
        let Ok(rd) = std::fs::read_dir(root) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(m) = std::fs::symlink_metadata(&p) else { continue };
            use std::os::unix::fs::MetadataExt;
            out.insert(p.clone(), (m.ino(), m.size(), m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128));
            if m.is_dir() && !ignore.contains(&e.file_name().to_string_lossy().into_owned()) {
                scan(&p, ignore, out);
            }
        }
    }

    impl FileWatcher {
        /// Starts watching `root` recursively. Directories named in `ignore_dir_names` (e.g.
        /// `.git`, `bazel-out`) are not descended into.
        pub fn new(root: &Path, ignore_dir_names: &[&str]) -> io::Result<Self> {
            let ignore: BTreeSet<String> = ignore_dir_names.iter().map(|s| s.to_string()).collect();
            let mut w = Self {
                root: root.to_path_buf(),
                #[cfg(any(target_os = "macos", target_os = "freebsd"))]
                fd: {
                    // SAFETY: kqueue() takes no arguments.
                    let fd = unsafe { imp::kqueue() };
                    if fd < 0 {
                        return Err(errno());
                    }
                    fd
                },
                #[cfg(target_os = "linux")]
                fd: {
                    // SAFETY: plain syscall wrapper.
                    let fd = unsafe { imp::inotify_init1(imp::IN_NONBLOCK) };
                    if fd < 0 {
                        return Err(errno());
                    }
                    fd
                },
                #[cfg(any(target_os = "macos", target_os = "freebsd"))]
                fds: BTreeMap::new(),
                #[cfg(target_os = "linux")]
                wds: BTreeMap::new(),
                snapshot: BTreeMap::new(),
                ignore_dirs: ignore,
            };
            scan(root, &w.ignore_dirs, &mut w.snapshot);
            w.register_dir(root)?;
            let paths: Vec<std::path::PathBuf> = w
                .snapshot
                .keys()
                .filter(|p| std::fs::symlink_metadata(p).map(|m| !m.file_type().is_symlink()).unwrap_or(false))
                .cloned()
                .collect();
            for d in paths {
                let _ = w.register_dir(&d);
            }
            Ok(w)
        }

        #[cfg(any(target_os = "macos", target_os = "freebsd"))]
        fn register_dir(&mut self, dir: &Path) -> io::Result<()> {
            // kqueue needs one descriptor per watched vnode: directories report entry changes,
            // files report content changes. Cap the descriptor use.
            if self.fds.contains_key(dir) || self.fds.len() >= 4096 {
                return Ok(());
            }
            let c = cpath(dir)?;
            // SAFETY: valid C path; O_EVTONLY opens for notifications only.
            let fd = unsafe { imp::open(c.as_ptr(), imp::O_EVTONLY) };
            if fd < 0 {
                return Err(errno());
            }
            let ev = imp::KEvent { ident: fd as usize, filter: imp::EVFILT_VNODE, flags: imp::EV_ADD | imp::EV_ENABLE | imp::EV_CLEAR, fflags: imp::NOTE_ALL, data: 0, udata: std::ptr::null_mut() };
            // SAFETY: a single valid changelist entry; no events requested.
            let r = unsafe { imp::kevent(self.fd, &ev, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
            if r < 0 {
                // SAFETY: closing the fd we just opened.
                unsafe { imp::close(fd) };
                return Err(errno());
            }
            self.fds.insert(dir.to_path_buf(), fd);
            Ok(())
        }

        #[cfg(target_os = "linux")]
        fn register_dir(&mut self, dir: &Path) -> io::Result<()> {
            if !dir.is_dir() || self.wds.values().any(|p| p == dir) {
                return Ok(());
            }
            let c = cpath(dir)?;
            // SAFETY: valid C path.
            let wd = unsafe { imp::inotify_add_watch(self.fd, c.as_ptr(), imp::IN_ALL) };
            if wd < 0 {
                return Err(errno());
            }
            self.wds.insert(wd, dir.to_path_buf());
            Ok(())
        }

        /// Waits up to `timeout` for the OS to report activity, then returns the exact set of
        /// created/modified/deleted paths found by rescanning. Empty when nothing changed.
        pub fn poll(&mut self, timeout: Duration) -> Vec<(std::path::PathBuf, ChangeKind)> {
            let deadline = Instant::now() + timeout;
            let mut signaled = false;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if self.wait_os(left) {
                    signaled = true;
                    // Coalesce bursts.
                    while self.wait_os(Duration::from_millis(20)) {}
                    break;
                }
                if left.is_zero() {
                    break;
                }
            }
            if !signaled {
                return Vec::new();
            }
            self.diff()
        }

        #[cfg(any(target_os = "macos", target_os = "freebsd"))]
        fn wait_os(&self, timeout: Duration) -> bool {
            let ts = imp::TimeSpec { sec: timeout.as_secs() as i64, nsec: timeout.subsec_nanos() as i64 };
            let mut out = [imp::KEvent { ident: 0, filter: 0, flags: 0, fflags: 0, data: 0, udata: std::ptr::null_mut() }; 16];
            // SAFETY: `out` has room for 16 events and `ts` is a valid timeout.
            let n = unsafe { imp::kevent(self.fd, std::ptr::null(), 0, out.as_mut_ptr(), 16, &ts) };
            n > 0
        }

        #[cfg(target_os = "linux")]
        fn wait_os(&self, timeout: Duration) -> bool {
            let mut p = imp::PollFd { fd: self.fd, events: 1, revents: 0 };
            // SAFETY: one valid pollfd.
            let n = unsafe { imp::poll(&mut p, 1, timeout.as_millis().min(i32::MAX as u128) as c_int) };
            if n > 0 {
                let mut buf = [0u8; 4096];
                // SAFETY: draining the inotify fd into a local buffer.
                while unsafe { imp::read(self.fd, buf.as_mut_ptr() as *mut c_void, buf.len()) } > 0 {}
                true
            } else {
                false
            }
        }

        fn diff(&mut self) -> Vec<(std::path::PathBuf, ChangeKind)> {
            let mut now = Snap::new();
            scan(&self.root, &self.ignore_dirs, &mut now);
            let mut changes = Vec::new();
            for (p, v) in &now {
                match self.snapshot.get(p) {
                    None => changes.push((p.clone(), ChangeKind::Created)),
                    Some(old) if old != v => changes.push((p.clone(), ChangeKind::Modified)),
                    _ => {}
                }
            }
            for p in self.snapshot.keys() {
                if !now.contains_key(p) {
                    changes.push((p.clone(), ChangeKind::Deleted));
                }
            }
            // Register newly created directories so their contents are watched too.
            let new_dirs: Vec<std::path::PathBuf> = changes
                .iter()
                .filter(|(p, k)| *k == ChangeKind::Created)
                .map(|(p, _)| p.clone())
                .collect();
            self.snapshot = now;
            for d in new_dirs {
                let _ = self.register_dir(&d);
            }
            changes.sort();
            changes
        }
    }

    impl Drop for FileWatcher {
        fn drop(&mut self) {
            #[cfg(any(target_os = "macos", target_os = "freebsd"))]
            for fd in self.fds.values() {
                // SAFETY: closing fds opened by this watcher.
                unsafe { imp::close(*fd) };
            }
            // SAFETY: closing the queue/inotify fd owned by this watcher.
            unsafe { imp::close(self.fd) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("zel-native-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn posix_ops() {
        let d = tmp("posix");
        std::fs::write(d.join("f"), b"abc").unwrap();
        let st = posix::stat(&d.join("f")).unwrap();
        assert!(st.is_file && st.size == 3 && st.mtime_ns > 0 && st.nlink == 1);
        posix::symlink(Path::new("f"), &d.join("l")).unwrap();
        assert!(posix::lstat(&d.join("l")).unwrap().is_symlink);
        assert!(!posix::stat(&d.join("l")).unwrap().is_symlink);
        assert_eq!(posix::readlink(&d.join("l")).unwrap(), Path::new("f"));
        posix::link(&d.join("f"), &d.join("h")).unwrap();
        assert_eq!(posix::stat(&d.join("f")).unwrap().nlink, 2);
        posix::chmod(&d.join("f"), 0o600).unwrap();
        assert_eq!(posix::stat(&d.join("f")).unwrap().mode & 0o777, 0o600);
        posix::utimensat(&d.join("f"), Some(1_000_000_123_456_789_000)).unwrap_or(());
        assert!(posix::mkdir(&d.join("sub"), 0o755).unwrap());
        assert!(!posix::mkdir(&d.join("sub"), 0o755).unwrap());
        posix::mkfifo(&d.join("fifo"), 0o600).unwrap();
        let ents = posix::readdir(&d).unwrap();
        assert!(ents.contains(&("sub".to_string(), posix::DirentType::Directory)));
        assert!(ents.contains(&("l".to_string(), posix::DirentType::Symlink)));
        assert_eq!(posix::getxattr_value(&d.join("f"), "user.zel.none", true).unwrap(), None);
        std::fs::create_dir_all(d.join("sub/a/b")).unwrap();
        std::fs::write(d.join("sub/a/b/x"), b"1").unwrap();
        posix::delete_trees_below(&d.join("sub")).unwrap();
        assert!(d.join("sub").exists() && std::fs::read_dir(d.join("sub")).unwrap().count() == 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn process_and_system() {
        assert!(process::is_alive(process::pid()));
        assert!(!process::is_alive(0x7fff_fff0));
        assert!(process::cpu_count() >= 1);
        let (u, s, rss) = process::resource_usage();
        assert!(rss > 0 && (u + s) >= Duration::ZERO);
        let _ = system::memory_pressure();
        assert!(system::total_memory().unwrap_or(1) > 0);
        assert!(system::load_average().is_some());
        let (free, total) = system::disk_space(Path::new("/")).unwrap();
        assert!(total > 0 && free <= total);
    }

    #[test]
    fn watcher_reports_exact_changes() {
        let d = tmp("watch");
        std::fs::create_dir_all(d.join("a")).unwrap();
        std::fs::write(d.join("a/one"), b"1").unwrap();
        std::fs::create_dir_all(d.join(".git")).unwrap();
        let mut w = watch::FileWatcher::new(&d, &[".git"]).unwrap();
        assert!(w.poll(Duration::from_millis(50)).is_empty());
        std::fs::write(d.join("a/two"), b"2").unwrap();
        let ch = w.poll(Duration::from_secs(5));
        assert!(ch.contains(&(d.join("a/two"), watch::ChangeKind::Created)), "{ch:?}");
        std::fs::write(d.join("a/one"), b"changed!").unwrap();
        let ch = w.poll(Duration::from_secs(5));
        assert!(ch.contains(&(d.join("a/one"), watch::ChangeKind::Modified)), "{ch:?}");
        // New directories are watched recursively.
        std::fs::create_dir_all(d.join("n")).unwrap();
        let _ = w.poll(Duration::from_secs(5));
        std::fs::write(d.join("n/deep"), b"x").unwrap();
        let ch = w.poll(Duration::from_secs(5));
        assert!(ch.contains(&(d.join("n/deep"), watch::ChangeKind::Created)), "{ch:?}");
        std::fs::remove_file(d.join("a/two")).unwrap();
        let ch = w.poll(Duration::from_secs(5));
        assert!(ch.contains(&(d.join("a/two"), watch::ChangeKind::Deleted)), "{ch:?}");
        // Ignored directories produce nothing.
        std::fs::write(d.join(".git/HEAD"), b"x").unwrap();
        assert!(w.poll(Duration::from_millis(300)).iter().all(|(p, _)| !p.starts_with(d.join(".git/HEAD"))));
        drop(w);
        let _ = std::fs::remove_dir_all(&d);
    }
}
