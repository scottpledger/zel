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

//! End-to-end tests of the non-build commands and startup options, through the real `zel`
//! binary and its persistent server (plus `--batch` for the in-process path).

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Env {
    root: PathBuf,
    ws: PathBuf,
    ob: PathBuf,
}

impl Env {
    fn new(name: &str) -> Env {
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("zel-cmd-e2e-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let ws = root.join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("MODULE.bazel"), "module(name = \"t\")\n").unwrap();
        let ob = root.join("ob");
        Env { root, ws, ob }
    }

    fn cmd(&self, startup: &[&str], args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_zel"));
        c.current_dir(&self.ws)
            .env("HOME", &self.root)
            .env("ZEL_INSTALL_MD5_OVERRIDE", "cmdtestinstallmd50000000000000000")
            .arg(format!("--output_base={}", self.ob.display()))
            .arg(format!("--install_base={}", self.root.join("install").display()))
            .args(startup)
            .args(args);
        c
    }

    fn run(&self, startup: &[&str], args: &[&str]) -> Output {
        self.cmd(startup, args).output().unwrap()
    }

    fn pid(&self) -> Option<i32> {
        fs::read_to_string(self.ob.join("server/server.pid.txt")).ok()?.trim().parse().ok()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Some(p) = self.pid() {
            let _ = Command::new("kill").args(["-9", &p.to_string()]).status();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn s(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
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

#[test]
fn version_prints_all_build_data_fields_and_gnu_format() {
    let e = Env::new("version");
    let o = e.run(&["--batch"], &["version"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    let out = s(&o.stdout);
    let keys: Vec<&str> = out.lines().map(|l| l.split(": ").next().unwrap()).collect();
    assert_eq!(
        keys,
        ["Build label", "Build target", "Build time", "Build timestamp", "Build timestamp as int"],
        "{out}"
    );
    let ts_line = out.lines().find(|l| l.starts_with("Build timestamp as int: ")).unwrap();
    let ts: i64 = ts_line.rsplit(": ").next().unwrap().parse().unwrap();
    assert!(out.contains(&format!("({ts})")), "Build time carries the timestamp: {out}");
    let g = e.run(&["--batch"], &["version", "--gnu_format"]);
    assert_eq!(g.status.code(), Some(0));
    let gs = s(&g.stdout);
    assert!(gs == "bazel 9.3.2\n" || gs == "zel 9.3.2\n", "{gs}");
    let bad = e.run(&["--batch"], &["version", "extra"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(s(&bad.stderr).contains("Unrecognized arguments: extra"));
}

#[test]
fn info_keys_ordering_hidden_keys_and_errors() {
    let e = Env::new("info");
    let o = e.run(&[], &["info"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    let out = s(&o.stdout);
    let keys: Vec<&str> = out.lines().map(|l| l.split(": ").next().unwrap()).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "info keys are printed sorted: {out}");
    for k in ["bazel-bin", "bazel-genfiles", "bazel-testlogs", "character-encoding", "command_log", "execution_root",
        "install_base", "output_base", "output_path", "package_path", "release", "repository_cache", "server_log",
        "server_pid", "workspace", "local_resources", "java-home"] {
        assert!(keys.contains(&k), "missing key {k}: {out}");
    }
    for hidden in ["build-language", "client-env", "install_md5", "worker_metrics", "starlark-semantics", "peak-heap-size"] {
        assert!(!keys.contains(&hidden), "hidden key {hidden} listed");
    }
    // Single key: value only; several keys: `key: value`.
    let one = e.run(&[], &["info", "output_base"]);
    assert_eq!(s(&one.stdout).trim(), e.ob.display().to_string());
    let two = e.run(&[], &["info", "release", "workspace"]);
    let t = s(&two.stdout);
    assert!(t.contains("release: release 9.3.2") && t.contains(&format!("workspace: {}\n", e.ws.display())), "{t}");
    // Hidden keys can be asked for explicitly.
    let hid = e.run(&[], &["info", "worker_metrics"]);
    assert_eq!(s(&hid.stdout), "No persistent workers active.\n");
    let md5 = e.run(&[], &["info", "install_md5"]);
    assert_eq!(s(&md5.stdout).trim(), "cmdtestinstallmd50000000000000000");
    // Unknown key.
    let bad = e.run(&[], &["info", "nokey", "release"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(s(&bad.stderr).contains("ERROR: unknown key(s): 'nokey'"), "{}", s(&bad.stderr));
    let pid = e.run(&[], &["info", "server_pid"]);
    assert_eq!(s(&pid.stdout).trim(), e.pid().unwrap().to_string());
}

#[test]
fn info_and_commands_outside_a_workspace() {
    let e = Env::new("nows");
    let nows = e.root.join("nows");
    fs::create_dir_all(&nows).unwrap();
    let run = |args: &[&str]| {
        let mut c = e.cmd(&["--batch"], args);
        c.current_dir(&nows);
        c.output().unwrap()
    };
    let b = run(&["build", "//x"]);
    assert_eq!(b.status.code(), Some(2));
    let err = s(&b.stderr);
    assert!(
        err.starts_with("ERROR: Bazel 'build' must be invoked from within a workspace (below a directory having a MODULE.bazel file).\nSee documentation at https://bazel.build/concepts/build-ref#workspace"),
        "{err}"
    );
    let i = run(&["info", "workspace"]);
    assert_eq!(i.status.code(), Some(2));
    assert!(s(&i.stderr).contains("must be invoked from within a workspace"), "{}", s(&i.stderr));
    let all = run(&["info"]);
    assert_eq!(all.status.code(), Some(2));
    assert!(s(&all.stderr).contains("must be invoked from within a workspace"), "{}", s(&all.stderr));
    let v = run(&["version"]);
    assert_eq!(v.status.code(), Some(0));
}

#[test]
fn unknown_and_removed_commands_and_help_aliases() {
    let e = Env::new("unknown");
    for cmd in ["frobnicate", "sync", "b", "analyze-profile"] {
        let o = e.run(&["--batch"], &[cmd]);
        assert_eq!(o.status.code(), Some(2), "{cmd}");
        assert_eq!(s(&o.stderr), format!("Command '{cmd}' not found. Try 'bazel help'.\n"), "{cmd}");
    }
    let plain = e.run(&["--batch"], &["help"]);
    for alias in ["-h", "--help"] {
        let o = e.run(&["--batch"], &[alias]);
        assert_eq!(s(&o.stdout), s(&plain.stdout), "{alias}");
    }
}

#[test]
fn help_topics() {
    let e = Env::new("help");
    let h = s(&e.run(&["--batch"], &["help"]).stdout);
    assert!(h.lines().next().unwrap().ends_with("[bazel release 9.3.2]"), "{h}");
    assert_eq!(h.lines().next().unwrap().len(), 80);
    assert!(h.contains("Usage: bazel <command> <options> ...\n\nAvailable commands:\n"));
    assert!(h.contains("  build               Builds the specified targets.\n"));
    assert!(!h.contains("  config "), "hidden commands are not listed");
    assert!(h.contains("Getting more help:\n  bazel help <command>\n"));

    let b = e.run(&["--batch"], &["help", "build"]);
    let bs = s(&b.stdout);
    assert!(bs.contains("Usage: bazel build <options> <targets>"), "{bs}");
    assert!(bs.contains("(Use 'help --long' for full details or --short to just enumerate options.)"));
    assert!(bs.contains("  --[no]keep_going [-k] (a boolean; default: \"false\")"), "{bs}");
    assert!(!bs.contains("    Continue as much as possible"), "medium verbosity has no descriptions");
    let long = s(&e.run(&["--batch"], &["help", "build", "--long"]).stdout);
    assert!(long.contains("    Continue as much as possible after an error."), "{long}");
    assert!(long.contains("Tags: "), "long help lists tags");
    let short = s(&e.run(&["--batch"], &["help", "build", "--short"]).stdout);
    assert!(short.contains("\n  --[no]keep_going\n"), "{short}");

    let hd = s(&e.run(&["--batch"], &["help", "dump"]).stdout);
    assert!(hd.contains("Usage: bazel dump <options>"), "{hd}");
    assert!(hd.contains("--[no]packages"), "{hd}");
    assert!(hd.contains("--[no]action_cache"), "{hd}");

    let cl_long = s(&e.run(&["--batch"], &["help", "clean", "--long"]).stdout);
    assert!(cl_long.contains("If specified, clean asynchronously removes the entire working tree"), "{cl_long}");

    let so = s(&e.run(&["--batch"], &["help", "startup_options"]).stdout);
    assert!(so.contains("Startup options\n===============\n"), "{so}");
    assert!(so.contains("--output_base"), "{so}");
    let ts = s(&e.run(&["--batch"], &["help", "target-syntax"]).stdout);
    assert!(ts.contains("Target pattern syntax\n=====================\n"));
    let ik = s(&e.run(&["--batch"], &["help", "info-keys"]).stdout);
    assert!(ik.contains("bazel-bin               Configuration dependent directory for binaries.\n"), "{ik}");
    let comp = s(&e.run(&["--batch"], &["help", "completion"]).stdout);
    assert!(comp.starts_with("BAZEL_COMMAND_LIST=\""), "{comp}");
    let compb = s(&e.run(&["--batch"], &["help", "completion", "bash"]).stdout);
    assert!(compb.contains("BAZEL_COMMAND_BUILD_FLAGS=\""));
    let bad = e.run(&["--batch"], &["help", "completion", "zsh"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(s(&bad.stderr).contains("The completion command only supports 'bash' as an argument, got 'zsh'"));
    let nc = e.run(&["--batch"], &["help", "nosuch"]);
    assert_eq!(nc.status.code(), Some(2));
    assert!(s(&nc.stderr).contains("ERROR: 'nosuch' is not a known command"));
    let two = e.run(&["--batch"], &["help", "build", "test"]);
    assert_eq!(two.status.code(), Some(2));
    assert!(s(&two.stderr).contains("You must specify exactly one command"));
    let proto = s(&e.run(&["--batch"], &["help", "flags-as-proto"]).stdout);
    assert!(proto.len() > 1000 && proto.chars().all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c)));
    let html = s(&e.run(&["--batch"], &["help", "everything-as-html"]).stdout);
    assert!(html.starts_with("<h2>Commands</h2>\n<table>\n"), "{}", &html[..80]);
    assert!(html.contains("<h3>Option Metadata Tags</h3>"));
}

#[test]
fn clean_removes_outputs_symlinks_and_expunge_stops_server() {
    let e = Env::new("clean");
    // Populate the output base like a build would.
    let mk = |e: &Env| {
        e.run(&[], &["version"]);
        let ex = e.ob.join("execroot/_main/bazel-out/k8-fastbuild/bin/pkg");
        fs::create_dir_all(&ex).unwrap();
        fs::write(ex.join("out.txt"), "x").unwrap();
        // Read-only artifacts must not prevent cleaning.
        let mut perm = fs::metadata(ex.join("out.txt")).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o444);
        fs::set_permissions(ex.join("out.txt"), perm).unwrap();
        for (l, t) in [("bazel-ws", "execroot/_main"), ("bazel-out", "execroot/_main/bazel-out"), ("bazel-bin", "execroot/_main/bazel-out/k8-fastbuild/bin")] {
            let _ = std::os::unix::fs::symlink(e.ob.join(t), e.ws.join(l));
        }
        let _ = std::os::unix::fs::symlink(e.ob.join("execroot"), e.ws.join(format!("bazel-{}", e.ws.file_name().unwrap().to_string_lossy())));
        ex
    };
    let ex = mk(&e);
    let pid = e.pid().unwrap();
    let o = e.run(&[], &["clean"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    assert!(s(&o.stderr).contains("INFO: Starting clean (this may take a while). Use --async if the clean takes more than several minutes."), "{}", s(&o.stderr));
    assert!(!ex.exists(), "execroot content removed");
    assert!(e.ob.join("execroot").exists(), "execroot dir itself stays");
    assert!(fs::symlink_metadata(e.ws.join("bazel-out")).is_err(), "convenience symlinks removed");
    assert!(fs::symlink_metadata(e.ws.join("bazel-bin")).is_err());
    assert_eq!(e.pid(), Some(pid), "plain clean keeps the server");
    assert!(alive(pid));

    // --async moves the tree away and deletes in the background.
    let ex = mk(&e);
    let o = e.run(&[], &["clean", "--async"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    let err = s(&o.stderr);
    assert!(err.contains("INFO: Starting clean.\n") && err.contains("INFO: Output tree moved to ") && err.contains("for deletion"), "{err}");
    assert!(!ex.exists());

    // --expunge deletes the whole output base and stops the server.
    mk(&e);
    let o = e.run(&[], &["clean", "--expunge"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    assert!(wait_dead(pid, 10), "server exits after clean --expunge");
    assert!(!e.ob.exists(), "output base deleted");

    // --expunge_async.
    let e2 = Env::new("clean2");
    e2.run(&[], &["version"]);
    let pid2 = e2.pid().unwrap();
    let o = e2.run(&[], &["clean", "--expunge_async"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    assert!(s(&o.stderr).contains("INFO: Output base moved to "), "{}", s(&o.stderr));
    assert!(wait_dead(pid2, 10));
    assert!(!e2.ob.exists());

    let bad = e2.run(&["--batch"], &["clean", "foo"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(s(&bad.stderr).contains("Unrecognized arguments: foo"));
}

#[test]
fn shutdown_iff_heap_size_and_plain() {
    let e = Env::new("shutdown");
    e.run(&[], &["version"]);
    let pid = e.pid().unwrap();
    // A huge threshold keeps the server alive.
    let o = e.run(&[], &["shutdown", "--iff_heap_size_greater_than=900000000"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    assert!(alive(pid));
    let o = e.run(&[], &["shutdown"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(wait_dead(pid, 10));
    // No server is running: the client returns before parsing the command (blaze.cc), exit 0.
    let extra = e.run(&["--batch"], &["shutdown", "extra"]);
    assert_eq!(extra.status.code(), Some(0));
}

#[test]
fn canonicalize_flags_and_license_and_dump() {
    let e = Env::new("misc");
    let c = e.run(&["--batch"], &["canonicalize-flags", "--", "-c", "opt", "--keep_going"]);
    assert_eq!(c.status.code(), Some(0), "{}", s(&c.stderr));
    let out = s(&c.stdout);
    assert!(out.contains("--compilation_mode=opt\n") && out.contains("--keep_going=1\n"), "{out}");
    let bad = e.run(&["--batch"], &["canonicalize-flags", "--for_command=nosuch"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(
        s(&bad.stderr).contains("Not a valid command: 'nosuch' (should be one of mobile-install, fetch,")
            || s(&bad.stderr).contains("Not a valid command: 'nosuch' (should be one of aquery, build,"),
        "{}",
        s(&bad.stderr)
    );
    let l = e.run(&["--batch"], &["license"]);
    assert_eq!(l.status.code(), Some(0));
    assert!(s(&l.stdout).starts_with("Licenses of all components included in this binary:\n\n"), "{}", s(&l.stdout));
    assert!(s(&l.stdout).contains("Apache License"));
    // dump without output flags prints its usage and fails with exit code 7.
    let d = e.run(&["--batch"], &["dump"]);
    assert_eq!(d.status.code(), Some(7));
    assert!(s(&d.stderr).contains("Usage: bazel dump <options>"), "{}", s(&d.stderr));
    assert!(s(&d.stderr).contains("--[no]packages"), "{}", s(&d.stderr));
    assert!(s(&d.stderr).contains("--[no]action_cache"), "{}", s(&d.stderr));
    assert!(s(&d.stderr).contains("--[no]rule_classes"), "{}", s(&d.stderr));
    let rc = e.run(&["--batch"], &["dump", "--rule_classes"]);
    assert_eq!(rc.status.code(), Some(0));
    assert!(s(&rc.stderr).contains("WARNING: This information is intended for consumption by developers only"));
    assert!(s(&rc.stdout).contains("\nfilegroup(") || s(&rc.stdout).starts_with("filegroup("), "{}", s(&rc.stdout));
    let sk = e.run(&["--batch"], &["dump", "--skyframe=summary"]);
    assert!(s(&sk.stdout).starts_with("Node count: "), "{}", s(&sk.stdout));
    let ac = e.run(&["--batch"], &["dump", "--action_cache"]);
    assert!(s(&ac.stdout).starts_with("Action cache (0 records):"), "{}", s(&ac.stdout));
    let cfg = e.run(&["--batch"], &["config"]);
    assert_eq!(cfg.status.code(), Some(2));
    assert!(s(&cfg.stderr).contains("No configurations found."));
}

#[test]
fn startup_options_failure_detail_out_command_port_and_friends() {
    let e = Env::new("startup");
    // --failure_detail_out: serialized FailureDetail of a failed command.
    let fd = e.root.join("failure_detail.rawproto");
    let o = e.run(&[&format!("--failure_detail_out={}", fd.display())], &["frobnicate"]);
    assert_eq!(o.status.code(), Some(2));
    let bytes = fs::read(&fd).expect("failure detail written");
    assert!(!bytes.is_empty() && String::from_utf8_lossy(&bytes).contains("Command 'frobnicate' not found."));
    // --server_jvm_out: where the daemon's stdout/stderr goes.
    let jvm = e.root.join("my-jvm.out");
    e.run(&[&format!("--server_jvm_out={}", jvm.display())], &["version"]);
    assert!(jvm.exists(), "--server_jvm_out file created");
    // --command_port: bind to a fixed loopback port (restart because the option changed).
    let port = {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        l.local_addr().unwrap().port()
    };
    let o = e.run(&[&format!("--server_jvm_out={}", jvm.display()), &format!("--command_port={port}")], &["info", "server_pid"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    let addr = fs::read_to_string(e.ob.join("server/command_port")).unwrap();
    assert_eq!(addr.trim(), format!("127.0.0.1:{port}"));
    // --client_debug prints connection details; --preemptible / --unlimit_coredumps accepted.
    let o = e.run(
        &[&format!("--server_jvm_out={}", jvm.display()), &format!("--command_port={port}"), "--client_debug", "--preemptible"],
        &["version"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    assert!(s(&o.stderr).contains("INFO: connected to server pid="), "{}", s(&o.stderr));
    // Volatile options (--client_debug, --preemptible) did not restart the server.
    // --nowrite_command_log: command.log is not (re)written.
    let _ = fs::remove_file(e.ob.join("command.log"));
    let o = e.run(
        &[&format!("--server_jvm_out={}", jvm.display()), &format!("--command_port={port}"), "--nowrite_command_log"],
        &["version"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    assert!(!e.ob.join("command.log").exists(), "--nowrite_command_log");

    // --bazelrc with nonexistent file fails with code 2
    let bad_rc = e.run(&["--bazelrc=/nonexistent/path/file"], &["version"]);
    assert_eq!(bad_rc.status.code(), Some(2));
    assert!(s(&bad_rc.stderr).contains("Unable to read .bazelrc file '/nonexistent/path/file'"), "{}", s(&bad_rc.stderr));

    // --ignore_all_rc_files warns about --bazelrc and ignores it
    let ign_rc = e.run(&["--ignore_all_rc_files", "--bazelrc=/nonexistent/path/file"], &["version"]);
    assert_eq!(ign_rc.status.code(), Some(0));
    assert!(s(&ign_rc.stderr).contains("WARNING: Value of --bazelrc is ignored, since --ignore_all_rc_files is on."), "{}", s(&ign_rc.stderr));
}

#[test]
fn unlimit_coredumps_and_shutdown() {
    // `--delete_output_base_on_shutdown` only exists in Bazel HEAD; 9.3.2 rejects it as an unknown startup option.
    let e = Env::new("delob");
    let bad = e.run(&["--delete_output_base_on_shutdown"], &["version"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(s(&bad.stderr).contains("Unknown startup option: '--delete_output_base_on_shutdown'."), "{}", s(&bad.stderr));
    let o = e.run(&["--unlimit_coredumps"], &["version"]);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    let pid = e.pid().unwrap();
    let o = e.run(&["--unlimit_coredumps"], &["shutdown"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(wait_dead(pid, 10));
}

fn slow_workspace(e: &Env) {
    fs::write(
        e.ws.join("BUILD"),
        "genrule(name = \"slow\", outs = [\"slow.txt\"], cmd = \"sleep 120; echo done > $@\")\n",
    )
    .unwrap();
}

#[test]
fn live_output_streaming_and_sigint_cancel() {
    let e = Env::new("stream");
    slow_workspace(&e);
    e.run(&[], &["version"]);
    let mut child = e
        .cmd(&[], &["build", "//:slow"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = child.stderr.take().unwrap();
    // Output must arrive while the 30s action is still running.
    let start = Instant::now();
    let mut seen = String::new();
    let mut buf = [0u8; 1024];
    while !seen.contains("Analyzed target //:slow") {
        let n = stderr.read(&mut buf).unwrap();
        assert!(n > 0, "stream closed early: {seen}");
        seen.push_str(&String::from_utf8_lossy(&buf[..n]));
        assert!(start.elapsed() < Duration::from_secs(100), "no live output: {seen}");
    }
    assert!(child.try_wait().unwrap().is_none(), "command still running when its output was seen");
    // Ctrl-C: the client sends a CancelRequest and exits with INTERRUPTED (8).
    std::thread::sleep(Duration::from_millis(500));
    Command::new("kill").args(["-INT", &child.id().to_string()]).status().unwrap();
    let t = Instant::now();
    let st = child.wait().unwrap();
    assert_eq!(st.code(), Some(8), "interrupted exit code");
    assert!(t.elapsed() < Duration::from_secs(20), "cancel did not wait for the action");
    let mut rest = String::new();
    let _ = stderr.read_to_string(&mut rest);
    assert!(rest.contains("ERROR: build interrupted") || seen.contains("ERROR: build interrupted"), "{rest}");
}

#[test]
fn test_print_action_and_mobile_install_parity() {
    let e = Env::new("pami");
    let pa = e.run(&["--batch"], &["print_action"]);
    assert_eq!(pa.status.code(), Some(1));
    assert!(s(&pa.stderr).contains("ERROR: no actions to print were found"));

    let mi = e.run(&["--batch"], &["mobile-install"]);
    assert_eq!(mi.status.code(), Some(2));
    assert!(s(&mi.stderr).contains("ERROR: Must specify a target to run"));

    let mi_multi = e.run(&["--batch"], &["mobile-install", "//a:a", "//b:b"]);
    // Only the first residue is the target (the rest are deployer arguments): the build of `//a:a` runs.
    assert_eq!(mi_multi.status.code(), Some(1));
    assert!(s(&mi_multi.stderr).contains("Not running mobile-install on target"), "{}", s(&mi_multi.stderr));

    let pa_nonexistent = e.run(&["--batch"], &["print_action", "//nonexistent:target"]);
    assert_eq!(pa_nonexistent.status.code(), Some(1));
    assert!(s(&pa_nonexistent.stderr).contains("Build failed when printing actions"), "{}", s(&pa_nonexistent.stderr));

    let mi_nonexistent = e.run(&["--batch"], &["mobile-install", "//nonexistent:target"]);
    assert_eq!(mi_nonexistent.status.code(), Some(1));
    assert!(s(&mi_nonexistent.stderr).contains("Build failed. Not running mobile-install on target."), "{}", s(&mi_nonexistent.stderr));
}
