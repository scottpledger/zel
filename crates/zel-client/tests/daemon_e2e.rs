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
        let root =
            std::env::temp_dir().join(format!("zel-daemon-e2e-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let ws = root.join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("MODULE.bazel"), "module(name = \"t\")\n").unwrap();
        let ob = root.join("ob");
        Env { root, ws, ob }
    }

    fn cmd(&self, startup: &[&str], cmd: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_zel"));
        for var in [
            "USE_ZEL_VERSION",
            "USE_BAZEL_VERSION",
            "USE_ZEL_FALLBACK_VERSION",
            "USE_BAZEL_FALLBACK_VERSION",
            "ZEL_SKIP_WRAPPER",
            "BAZELISK_SKIP_WRAPPER",
            "ZEL_WRAPPER",
            "BAZEL_WRAPPER",
            "ZEL_HOME",
            "BAZELISK_HOME",
            "ZEL_BASE_URL",
            "BAZELISK_BASE_URL",
            "ZEL_NO_DOWNLOAD",
            "BAZELISK_NO_DOWNLOAD",
            "ZEL_VERIFY_SHA256",
            "BAZELISK_VERIFY_SHA256",
            "ZEL_PRODUCT_PREFIX",
            "ZEL_EMBED_LABEL",
            "BAZEL_EMBED_LABEL",
            "XDG_CACHE_HOME",
        ] {
            c.env_remove(var);
        }
        c.current_dir(&self.ws)
            .env("HOME", &self.root)
            .env(
                "ZEL_INSTALL_MD5_OVERRIDE",
                "e2etestinstallmd5000000000000000",
            )
            .arg(format!("--output_base={}", self.ob.display()))
            .arg(format!(
                "--install_base={}",
                self.root.join("install").display()
            ))
            .args(startup)
            .args(cmd);
        c
    }

    fn run(&self, startup: &[&str], cmd: &[&str]) -> Output {
        self.cmd(startup, cmd).output().unwrap()
    }

    fn pid(&self) -> Option<i32> {
        fs::read_to_string(self.ob.join("server/server.pid.txt"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.run(&[], &["shutdown"]);
        if let Some(p) = self.pid() {
            let _ = zel_server::sys::send_signal(p, zel_server::sys::SIGKILL);
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn alive(pid: i32) -> bool {
    zel_server::sys::process_alive(pid)
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
    for f in [
        "request_cookie",
        "response_cookie",
        "command_port",
        "server_info.rawproto",
        "cmdline",
        "jvm.out",
    ] {
        assert!(e.ob.join("server").join(f).exists(), "missing server/{f}");
    }
    let o2 = e.run(&[], &["info", "output_base"]);
    assert_eq!(o2.status.code(), Some(0), "{}", s(&o2.stderr));
    assert_eq!(
        e.pid(),
        Some(pid),
        "second command must reuse the same server"
    );
    assert!(alive(pid));
    // Detached: reparented away from the (finished) client.
    #[cfg(unix)]
    {
        let ppid = Command::new("ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        assert_ne!(s(&ppid.stdout).trim(), std::process::id().to_string());
    }
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
    let o = e.run(
        &["--max_idle_secs=99999", "--connect_timeout_secs=31"],
        &["version"],
    );
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
    assert!(
        err.contains("Only in new server: --idle_server_tasks=false")
            || err.contains("--noidle_server_tasks"),
        "{err}"
    );
    let new_pid = e.pid().unwrap();
    assert_ne!(new_pid, pid);
    assert!(wait_dead(pid, 10));

    // `shutdown` with differing startup options kills the running server immediately without
    // spawning a throwaway daemon or stalling on `POST_SHUTDOWN_GRACE_SECS`.
    let shut_start = Instant::now();
    let shut = e.run(&[], &["shutdown"]);
    assert_eq!(shut.status.code(), Some(0), "{}", s(&shut.stderr));
    assert!(
        shut_start.elapsed() < Duration::from_secs(15),
        "shutdown with mismatched startup options stalled for {:?}",
        shut_start.elapsed()
    );
    assert!(wait_dead(new_pid, 10));
    assert!(e.pid().is_none());

    // Cold `clean --expunge` starts a daemon as a child of the client and terminates it in the
    // same client invocation (`termination_expected = true`); `await_server_exit` must reap the
    // exited child immediately instead of stalling on a `<defunct>` zombie.
    let expunge_start = Instant::now();
    let expunge = e.run(&[], &["clean", "--expunge"]);
    assert_eq!(expunge.status.code(), Some(0), "{}", s(&expunge.stderr));
    assert!(
        expunge_start.elapsed() < Duration::from_secs(15),
        "cold clean --expunge stalled on daemon child zombie for {:?}",
        expunge_start.elapsed()
    );
}

#[test]
fn crashed_server_is_detected_and_replaced() {
    let e = Env::new("crash");
    e.run(&[], &["version"]);
    let pid = e.pid().unwrap();
    assert!(zel_server::sys::send_signal(pid, zel_server::sys::SIGKILL));
    assert!(wait_dead(pid, 5));
    // Stale pid/status files remain on disk; the client must notice and restart.
    assert!(e.ob.join("server/server.pid.txt").exists());
    let o = e.run(&[], &["version"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    let new_pid = e.pid().unwrap();
    assert_ne!(new_pid, pid);
    assert!(alive(new_pid));
}

#[cfg(unix)]
#[test]
fn sigterm_stops_server_cleanly() {
    let e = Env::new("sigterm");
    e.run(&[], &["version"]);
    let pid = e.pid().unwrap();
    Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap();
    assert!(wait_dead(pid, 10));
    assert!(
        !e.ob.join("server/server.pid.txt").exists(),
        "clean shutdown removes status files"
    );
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

#[cfg(unix)]
#[test]
fn output_base_lock_noblock_for_lock_exits_9() {
    use std::os::unix::io::AsRawFd;
    let e = Env::new("lock");
    e.run(&[], &["version"]);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(e.ob.join("lock"))
        .unwrap();
    assert_eq!(unsafe { flock(lock.as_raw_fd(), 2) }, 0);
    let o = e.run(&["--noblock_for_lock"], &["version"]);
    assert_eq!(o.status.code(), Some(9), "{}", s(&o.stderr));
    let err = s(&o.stderr);
    assert!(
        err.contains("Another command holds the output base lock"),
        "{err}"
    );
    assert!(
        err.contains(
            "Exiting because the output base lock is held and --noblock_for_lock was given."
        ),
        "{err}"
    );
    // Blocking client waits, then proceeds once the lock is released.
    let mut child_cmd = e.cmd(&[], &["version"]);
    let child = std::thread::spawn(move || child_cmd.output().unwrap());
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(unsafe { flock(lock.as_raw_fd(), 8) }, 0);
    let o = child.join().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    assert!(
        s(&o.stderr).contains("Waiting for it to complete..."),
        "{}",
        s(&o.stderr)
    );
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
    assert_eq!(
        o_panic.status.code(),
        Some(37),
        "panic must exit 37: {}",
        s(&o_panic.stderr)
    );
    assert!(
        s(&o_panic.stderr).contains("command panicked"),
        "{}",
        s(&o_panic.stderr)
    );

    // Daemon must still be running with the exact same pid!
    assert!(alive(pid), "daemon must survive command panic");
    assert_eq!(e.pid(), Some(pid));

    // Lock must be released: subsequent command runs immediately without blocking.
    let o_after = e.run(&["--noblock_for_lock"], &["version"]);
    assert_eq!(
        o_after.status.code(),
        Some(0),
        "command lock must be released after panic: {}",
        s(&o_after.stderr)
    );
}

#[test]
fn missing_or_stale_install_symlink_restarts_server_and_batch_kills_daemon() {
    let e = Env::new("install-symlink");
    let o = e.run(&[], &["version"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    let _pid1 = e.pid().expect("daemon pid");
    #[cfg(unix)]
    {
        let install_link = e.ob.join("install");
        assert!(
            install_link.symlink_metadata().is_ok(),
            "install symlink must be created"
        );

        // Removing the install symlink forces a server restart and recreates the symlink.
        fs::remove_file(&install_link).unwrap();
        let o2 = e.run(&[], &["version"]);
        assert_eq!(o2.status.code(), Some(0), "{}", s(&o2.stderr));
        let pid2 = e.pid().expect("new daemon pid");
        assert_ne!(pid2, _pid1, "missing install symlink must restart server");
        assert!(
            install_link.symlink_metadata().is_ok(),
            "install symlink must be recreated"
        );
        assert!(wait_dead(_pid1, 10));

        // --batch takes the output base lock (--noblock_for_lock exits 9 when held).
        {
            use std::os::unix::io::AsRawFd;
            let lock = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(e.ob.join("lock"))
                .unwrap();
            assert_eq!(unsafe { flock(lock.as_raw_fd(), 2) }, 0);
            let ob_blocked = e.run(&["--batch", "--noblock_for_lock"], &["version"]);
            assert_eq!(
                ob_blocked.status.code(),
                Some(9),
                "{}",
                s(&ob_blocked.stderr)
            );
            assert_eq!(unsafe { flock(lock.as_raw_fd(), 8) }, 0);
        }
    }
    #[cfg(windows)]
    {
        let install_link = e.ob.join("install");
        assert!(
            zel_vfs::junction_or_symlink_points_to(&install_link, &e.root.join("install")),
            "install junction must point to install_base"
        );
        fs::remove_dir(&install_link)
            .or_else(|_| fs::remove_file(&install_link))
            .unwrap();
        let o2 = e.run(&[], &["version"]);
        assert_eq!(o2.status.code(), Some(0), "{}", s(&o2.stderr));
        let pid2 = e.pid().expect("new daemon pid");
        assert_ne!(pid2, _pid1, "missing install junction must restart server");
        assert!(
            zel_vfs::junction_or_symlink_points_to(&install_link, &e.root.join("install")),
            "install junction must be recreated"
        );
        assert!(wait_dead(_pid1, 10));
    }
    let pid_before_batch = e.pid().expect("daemon pid before batch");

    // --batch kills any running daemon on this output_base.
    let ob_run = e.run(&["--batch"], &["version"]);
    assert_eq!(ob_run.status.code(), Some(0), "{}", s(&ob_run.stderr));
    assert!(
        wait_dead(pid_before_batch, 10),
        "--batch must kill existing daemon"
    );

    // exec-server refuses --batch (exit 2) and refuses when a server is already running (exit 36).
    let es_batch = e.run(&["--batch"], &["exec-server"]);
    assert_eq!(es_batch.status.code(), Some(2), "{}", s(&es_batch.stderr));
    assert!(
        s(&es_batch.stderr).contains("not compatible with --batch"),
        "{}",
        s(&es_batch.stderr)
    );
    let o3 = e.run(&[], &["version"]);
    assert_eq!(o3.status.code(), Some(0), "{}", s(&o3.stderr));
    let es_running = e.run(&[], &["exec-server"]);
    assert_eq!(
        es_running.status.code(),
        Some(36),
        "{}",
        s(&es_running.stderr)
    );
    assert!(
        s(&es_running.stderr).contains("please shut down existing server"),
        "{}",
        s(&es_running.stderr)
    );
}

#[test]
fn update_terminal_size_rpc_validates_cookie_and_returns_response_cookie() {
    use zel_client::daemon_client::update_terminal_size;
    use zel_proto::command_server::ServerInfo;

    let e = Env::new("term-size-rpc");
    let o = e.run(&[], &["version"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));

    let raw = fs::read(e.ob.join("server/server_info.rawproto")).unwrap();
    let info = ServerInfo::decode(&raw).unwrap();

    let mut bad_info = info.clone();
    bad_info.request_cookie = "wrong-cookie".to_string();
    let err = update_terminal_size(&bad_info, "1-1", 120, 40, Duration::from_secs(5))
        .expect_err("mismatched cookie must fail");
    assert_eq!(err.code, 7, "{err:?}");

    let ok = update_terminal_size(&info, "1-1", 120, 40, Duration::from_secs(5))
        .expect("valid cookie must succeed");
    assert_eq!(ok.cookie, info.response_cookie);
}

#[cfg(unix)]
fn local_registry() -> PathBuf {
    static REG: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    REG.get_or_init(|| {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let dir =
            std::env::temp_dir().join(format!("zel-daemon-e2e-registry-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let st = Command::new("python3")
            .arg(repo_root.join("scripts/make_vendored_registry.py"))
            .arg(&dir)
            .status()
            .expect("python3 is required to build the local test registry");
        assert!(st.success(), "make_vendored_registry.py failed");
        dir
    })
    .clone()
}

#[cfg(unix)]
#[test]
fn sigwinch_forwards_terminal_size_to_running_daemon_command() {
    use std::net::TcpListener;
    use std::sync::Arc;
    use zel_client::daemon_client::run_command;
    use zel_proto::command_server::{RunRequest, ServerInfo};
    use zel_server::create_default_runtime;
    use zel_server::daemon::{serve_grpc, DaemonConfig};
    use zel_vfs::PathFragment;

    let root = std::env::temp_dir().join(format!("zel-sigwinch-e2e-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let ws = root.join("ws");
    let ob = root.join("ob");
    let ib = root.join("ib");
    let our = root.join("our");
    fs::create_dir_all(ws.join("pkg")).unwrap();
    fs::create_dir_all(&ib).unwrap();
    fs::write(ws.join("WORKSPACE"), "workspace(name = \"sigwinch_ws\")\n").unwrap();

    let started_marker = root.join("action_started");
    let release_gate = root.join("release_gate");
    fs::write(
        ws.join("pkg/BUILD"),
        format!(
            "genrule(\n    name = \"wait_gate\",\n    outs = [\"out.txt\"],\n    cmd = \"echo 1 > '{started}'; while [ ! -f '{gate}' ]; do sleep 0.05; done; echo ok > $@\",\n    local = True,\n)\n",
            started = started_marker.display(),
            gate = release_gate.display(),
        ),
    )
    .unwrap();

    let rt = create_default_runtime(
        PathFragment::create(&ws.to_string_lossy()),
        PathFragment::create(&ob.to_string_lossy()),
        PathFragment::create(&ib.to_string_lossy()),
        PathFragment::create(&our.to_string_lossy()),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let req_cookie = "req-cookie-sigwinch".to_string();
    let resp_cookie = "resp-cookie-sigwinch".to_string();

    let rt_srv = Arc::clone(&rt);
    let req_c = req_cookie.clone();
    let resp_c = resp_cookie.clone();
    let srv_thread = std::thread::spawn(move || {
        serve_grpc(
            listener,
            rt_srv,
            DaemonConfig {
                request_cookie: req_c,
                response_cookie: resp_c,
                max_idle_secs: 0,
                shutdown_on_low_sys_mem: false,
                idle_server_tasks: false,
            },
        )
    });

    let prev_cols = std::env::var("COLUMNS").ok();
    let prev_lines = std::env::var("LINES").ok();
    std::env::set_var("COLUMNS", "137");
    std::env::set_var("LINES", "49");
    let fd = if zel_server::sys::is_tty(2) { 2 } else { 1 };
    let (expected_cols, expected_rows) = zel_server::sys::terminal_size(fd);

    let info = ServerInfo {
        pid: std::process::id() as i32,
        address: addr,
        request_cookie: req_cookie,
        response_cookie: resp_cookie,
    };

    let registry_arg = format!("--registry=file://{}", local_registry().display()).into_bytes();
    let info_run = info.clone();
    let client_thread = std::thread::spawn(move || {
        let req = RunRequest {
            arg: vec![
                b"build".to_vec(),
                registry_arg,
                b"--noenable_bzlmod".to_vec(),
                b"--spawn_strategy=local".to_vec(),
                b"--terminal_columns=61".to_vec(),
                b"//pkg:wait_gate".to_vec(),
            ],
            block_for_lock: true,
            client_description: format!("pid={}", std::process::id()),
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let outcome = run_command(
            &info_run,
            &req,
            &mut out,
            &mut err,
            Duration::from_secs(5),
            None,
        );
        (outcome, out, err)
    });

    let deadline = Instant::now() + Duration::from_secs(60);
    while !started_marker.exists() {
        if client_thread.is_finished() {
            let (outcome, out, err) = client_thread.join().unwrap();
            panic!(
                "build exited before genrule action started: exit_code={} abrupt={:?}\nstdout:\n{}\nstderr:\n{}",
                outcome.response.exit_code,
                outcome.abrupt,
                s(&out),
                s(&err)
            );
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for genrule action to start"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    // Deliver a real OS SIGWINCH to this process while `run_command` is streaming `Run`.
    assert!(zel_server::sys::send_signal(
        std::process::id() as i32,
        zel_server::sys::SIGWINCH
    ));

    let resize_deadline = Instant::now() + Duration::from_secs(5);
    let recorded = loop {
        if let Some(rec) = rt.last_terminal_size.lock().unwrap().clone() {
            break rec;
        }
        assert!(
            Instant::now() < resize_deadline,
            "timed out waiting for SIGWINCH to forward UpdateTerminalSize to daemon"
        );
        std::thread::sleep(Duration::from_millis(25));
    };

    assert!(
        !recorded.command_id.is_empty(),
        "command_id must be non-empty"
    );
    assert_eq!(
        recorded.columns, expected_cols,
        "columns forwarded via UpdateTerminalSize"
    );
    assert_eq!(
        recorded.rows, expected_rows,
        "rows forwarded via UpdateTerminalSize"
    );
    assert_eq!(
        recorded.ui_width,
        Some(expected_cols),
        "in-flight UiEventHandler width must update from 61 to {expected_cols}"
    );

    fs::write(&release_gate, b"go\n").unwrap();
    let (outcome, _out, err) = client_thread.join().unwrap();
    assert!(outcome.abrupt.is_none(), "{:?}", outcome.abrupt);
    assert_eq!(outcome.response.exit_code, 0, "{}", s(&err));

    // Shut down the in-process daemon cleanly.
    let mut out = Vec::new();
    let mut err = Vec::new();
    let shut_req = RunRequest {
        arg: vec![b"shutdown".to_vec()],
        block_for_lock: true,
        client_description: "shutdown".into(),
        ..Default::default()
    };
    let _ = run_command(
        &info,
        &shut_req,
        &mut out,
        &mut err,
        Duration::from_secs(5),
        None,
    );
    let _ = srv_thread.join();

    match prev_cols {
        Some(v) => std::env::set_var("COLUMNS", v),
        None => std::env::remove_var("COLUMNS"),
    }
    match prev_lines {
        Some(v) => std::env::set_var("LINES", v),
        None => std::env::remove_var("LINES"),
    }
    let _ = fs::remove_dir_all(&root);
}
