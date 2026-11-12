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

//! Multi-process end-to-end tests of the persistent server, run through the real `zel` binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

struct Env {
    root: PathBuf,
    ws: PathBuf,
    ob: PathBuf,
}

impl Env {
    fn new(name: &str) -> Env {
        let root = std::env::temp_dir().join(format!("zel-daemon-e2e-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let ws = root.join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("MODULE.bazel"), "module(name = \"t\")\n").unwrap();
        let ob = root.join("ob");
        Env { root, ws, ob }
    }

    fn run(&self, startup: &[&str], cmd: &[&str]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_zel"));
        c.current_dir(&self.ws)
            .env("ZEL_INSTALL_MD5_OVERRIDE", "e2etestinstallmd5000000000000000")
            .arg(format!("--output_base={}", self.ob.display()))
            .arg(format!("--install_base={}", self.root.join("install").display()))
            .args(startup)
            .args(cmd);
        c.output().unwrap()
    }

    fn pid(&self) -> Option<i32> {
        fs::read_to_string(self.ob.join("server/server.pid.txt")).ok()?.trim().parse().ok()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.run(&[], &["shutdown"]);
        if let Some(p) = self.pid() {
            let _ = Command::new("kill").args(["-9", &p.to_string()]).status();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn alive(pid: i32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).output().map(|o| o.status.success()).unwrap_or(false)
}

fn wait_dead(pid: i32, secs: u64) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn s(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn server_persists_across_commands_and_is_detached() {
    let e = Env::new("persist");
    let o = e.run(&[], &["version"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    assert!(s(&o.stdout).contains("Build label:"));
    let pid = e.pid().expect("server.pid.txt written");
    assert!(alive(pid));
    for f in ["request_cookie", "response_cookie", "command_port", "server_info.rawproto", "cmdline", "jvm.out"] {
        assert!(e.ob.join("server").join(f).exists(), "missing server/{f}");
    }
    let o2 = e.run(&[], &["info", "output_base"]);
    assert_eq!(o2.status.code(), Some(0), "{}", s(&o2.stderr));
    assert_eq!(e.pid(), Some(pid), "second command must reuse the same server");
    assert!(alive(pid));
    // Detached: reparented away from the (finished) client.
    let ppid = Command::new("ps").args(["-o", "ppid=", "-p", &pid.to_string()]).output().unwrap();
    assert_ne!(s(&ppid.stdout).trim(), std::process::id().to_string());
}

#[test]
fn shutdown_command_stops_server_and_removes_status_files() {
    let e = Env::new("shutdown");
    e.run(&[], &["version"]);
    let pid = e.pid().unwrap();
    let o = e.run(&[], &["shutdown"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    assert!(wait_dead(pid, 10), "server still alive after shutdown");
    assert!(!e.ob.join("server/server.pid.txt").exists());
    assert!(!e.ob.join("server/request_cookie").exists());
    // A following command transparently starts a fresh server.
    let o = e.run(&[], &["version"]);
    assert_eq!(o.status.code(), Some(0));
    assert_ne!(e.pid(), Some(pid));
}

#[test]
fn changed_startup_options_restart_the_server() {
    let e = Env::new("restart");
    e.run(&[], &["version"]);
    let pid = e.pid().unwrap();
    // Volatile options (max_idle_secs, connect_timeout_secs) never restart.
    let o = e.run(&["--max_idle_secs=99999", "--connect_timeout_secs=31"], &["version"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    assert_eq!(e.pid(), Some(pid));
    assert!(!s(&o.stderr).contains("needs to be killed"));
    // A real startup option change does.
    let o = e.run(&["--noidle_server_tasks"], &["version"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    let err = s(&o.stderr);
    assert!(
        err.contains("WARNING: Running Bazel server needs to be killed, because the following startup options are different:"),
        "{err}"
    );
    assert!(err.contains("Only in new server: --idle_server_tasks=false") || err.contains("--noidle_server_tasks"), "{err}");
    let new_pid = e.pid().unwrap();
    assert_ne!(new_pid, pid);
    assert!(wait_dead(pid, 10));
}

#[test]
fn crashed_server_is_detected_and_replaced() {
    let e = Env::new("crash");
    e.run(&[], &["version"]);
    let pid = e.pid().unwrap();
    Command::new("kill").args(["-9", &pid.to_string()]).status().unwrap();
    assert!(wait_dead(pid, 5));
    // Stale pid/status files remain on disk; the client must notice and restart.
    assert!(e.ob.join("server/server.pid.txt").exists());
    let o = e.run(&[], &["version"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    let new_pid = e.pid().unwrap();
    assert_ne!(new_pid, pid);
    assert!(alive(new_pid));
}

#[test]
fn sigterm_stops_server_cleanly() {
    let e = Env::new("sigterm");
    e.run(&[], &["version"]);
    let pid = e.pid().unwrap();
    Command::new("kill").args(["-TERM", &pid.to_string()]).status().unwrap();
    assert!(wait_dead(pid, 10));
    assert!(!e.ob.join("server/server.pid.txt").exists(), "clean shutdown removes status files");
}

#[test]
fn idle_server_shuts_down() {
    let e = Env::new("idle");
    let o = e.run(&["--max_idle_secs=1"], &["version"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    let pid = e.pid().unwrap();
    assert!(wait_dead(pid, 10), "idle server did not exit");
}

#[cfg(unix)]
extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

#[test]
fn output_base_lock_noblock_for_lock_exits_9() {
    use std::os::unix::io::AsRawFd;
    let e = Env::new("lock");
    e.run(&[], &["version"]);
    let lock = fs::OpenOptions::new().read(true).write(true).open(e.ob.join("lock")).unwrap();
    assert_eq!(unsafe { flock(lock.as_raw_fd(), 2) }, 0);
    let o = e.run(&["--noblock_for_lock"], &["version"]);
    assert_eq!(o.status.code(), Some(9), "{}", s(&o.stderr));
    let err = s(&o.stderr);
    assert!(err.contains("Another command holds the output base lock"), "{err}");
    assert!(err.contains("Exiting because the output base lock is held and --noblock_for_lock was given."), "{err}");
    // Blocking client waits, then proceeds once the lock is released.
    let ob = e.ob.clone();
    let ws = e.ws.clone();
    let ib = e.root.join("install");
    let child = std::thread::spawn(move || {
        Command::new(env!("CARGO_BIN_EXE_zel"))
            .current_dir(ws)
            .env("ZEL_INSTALL_MD5_OVERRIDE", "e2etestinstallmd5000000000000000")
            .arg(format!("--output_base={}", ob.display()))
            .arg(format!("--install_base={}", ib.display()))
            .args(["version"])
            .output()
            .unwrap()
    });
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(unsafe { flock(lock.as_raw_fd(), 8) }, 0);
    let o = child.join().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(s(&o.stderr).contains("Waiting for it to complete..."), "{}", s(&o.stderr));
}

#[test]
fn unknown_command_exit_code_2() {
    let e = Env::new("unknown");
    let o = e.run(&[], &["frobnicate"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(s(&o.stderr).contains("Command 'frobnicate' not found. Try 'bazel help'."));
}

#[test]
fn batch_mode_runs_without_a_server() {
    let e = Env::new("batch");
    let o = e.run(&["--batch"], &["version"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    assert!(e.pid().is_none());
    let _ = Path::new("/");
}

#[test]
fn command_panic_leaves_daemon_alive_and_releases_lock() {
    let e = Env::new("panic-isolation");
    let o = e.run(&[], &["version"]);
    assert_eq!(o.status.code(), Some(0));
    let pid = e.pid().expect("daemon pid");
    assert!(alive(pid));

    // Force a panic via the test hook
    let o_panic = e.run(&[], &["version", "--test_force_command_panic"]);
    assert_eq!(o_panic.status.code(), Some(37), "panic must exit 37: {}", s(&o_panic.stderr));
    assert!(s(&o_panic.stderr).contains("command panicked"), "{}", s(&o_panic.stderr));

    // Daemon must still be running with the exact same pid!
    assert!(alive(pid), "daemon must survive command panic");
    assert_eq!(e.pid(), Some(pid));

    // Lock must be released: subsequent command runs immediately without blocking.
    let o_after = e.run(&["--noblock_for_lock"], &["version"]);
    assert_eq!(o_after.status.code(), Some(0), "command lock must be released after panic: {}", s(&o_after.stderr));
}
