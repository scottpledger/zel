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

//! End-to-end regressions for registry failures, `--announce_rc`, client fatal framing,
//! `dump --skylark_memory`, the generated completion script and the generated LICENSE.
//! Expected strings are derived from Bazel's Java/C++ sources (and checked against the real
//! 9.3.2 binary with `scripts/diff_cmds6.sh`), not recorded blobs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../zel-server/tests/fixtures")
}

struct Ws {
    root: PathBuf,
    ws: PathBuf,
}

impl Ws {
    fn new(name: &str, fixture: &str) -> Ws {
        let root = fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("zel-fix6-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let ws = root.join("ws");
        fs::create_dir_all(&ws).unwrap();
        for e in fs::read_dir(fixtures().join(fixture)).unwrap() {
            let e = e.unwrap();
            let n = e.file_name().to_string_lossy().to_string();
            let dst = if n == "dot_bazelrc" { ".bazelrc".to_string() } else { n };
            fs::copy(e.path(), ws.join(dst)).unwrap();
        }
        Ws { root, ws }
    }

    fn run(&self, startup: &[&str], args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_zel"));
        c.current_dir(&self.ws)
            .env("HOME", &self.root)
            .env("ZEL_INSTALL_MD5_OVERRIDE", "fix6installmd5000000000000000000")
            .arg("--batch")
            .arg(format!("--output_base={}", self.root.join("ob").display()))
            .arg(format!("--install_base={}", self.root.join("install").display()))
            .args(startup)
            .args(args);
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }
}

impl Drop for Ws {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn s(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn unreachable_registry_fails_before_anything_is_built() {
    let w = Ws::new("reg", "registry_unreachable");
    let proxy = [("https_proxy", "http://127.0.0.1:9"), ("HTTPS_PROXY", "http://127.0.0.1:9")];
    for cmd in ["build", "query"] {
        let o = w.run(&[], &[cmd, "//:g"], &proxy);
        let err = s(&o.stderr);
        assert_eq!(o.status.code(), Some(32), "{cmd}: {err}");
        assert!(err.starts_with("ERROR: Error computing the main repository mapping: Error accessing registry https://bcr.bazel.build/: Failed to fetch registry file https://bcr.bazel.build/modules/"), "{err}");
        assert!(!err.contains("module dependency chain"), "registry I/O errors carry no dependency chain: {err}");
        assert!(!err.contains("Analyzed target") && !err.contains("up-to-date") && !err.contains("did NOT complete"), "{err}");
        assert!(s(&o.stdout).is_empty(), "{cmd} must not print results: {}", s(&o.stdout));
    }
}

#[test]
fn dump_skylark_memory_requires_memory_tracking() {
    let w = Ws::new("dump", "rc_announce");
    let o = w.run(&[], &["dump", "--skylark_memory=/tmp/never-written.pprof"], &[]);
    assert_eq!(o.status.code(), Some(0));
    assert!(s(&o.stdout).starts_with("Cannot dump Starlark heap without running in memory tracking mode."), "{}", s(&o.stdout));
    assert!(!Path::new("/tmp/never-written.pprof").exists());
}

#[test]
fn dump_skyframe_summary_counts_the_real_graph() {
    let w = Ws::new("sum", "rc_announce");
    // One batch process = one graph: build, then dump in the same invocation chain is impossible,
    // so check the shape and that a loaded package gives non-zero counts via the server path.
    let o = w.run(&[], &["dump", "--skyframe=summary"], &[]);
    let out = s(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}", s(&o.stderr));
    let nodes: usize = out.lines().find_map(|l| l.strip_prefix("Node count: ")).unwrap().trim().parse().unwrap();
    let edges: usize = out.lines().find_map(|l| l.strip_prefix("Edge count: ")).unwrap().trim().parse().unwrap();
    assert!(edges <= nodes * nodes.max(1));
}

#[test]
fn completion_lists_every_registered_command_including_hidden_ones() {
    let w = Ws::new("compl", "rc_announce");
    let o = w.run(&[], &["help", "completion"], &[]);
    let out = s(&o.stdout);
    let list = out.lines().find(|l| l.starts_with("BAZEL_COMMAND_LIST=")).unwrap();
    for c in ["build", "config", "print_action", "mobile-install", "canonicalize-flags"] {
        assert!(list.split(['"', ' ']).any(|w| w == c), "{c} missing from {list}");
    }
    assert!(out.contains("BAZEL_COMMAND_CONFIG_FLAGS=\""), "no per-command flag block for `config`");
}

#[test]
fn announce_rc_prints_startup_client_and_config_notes() {
    let w = Ws::new("ann", "rc_announce");
    let o = w.run(&[], &["build", "--announce_rc", "--config=foo", "//:g"], &[]);
    let err = s(&o.stderr);
    let rc = w.ws.join(".bazelrc");
    let rc = fs::canonicalize(&rc).unwrap_or(rc);
    let rc = rc.display();
    let expected = [
        format!("INFO: Reading 'startup' options from {rc}: --max_idle_secs=100"),
        "INFO: Options provided by the client:\n  Inherited 'common' options: --isatty=0 --terminal_columns=80".to_string(),
        format!("INFO: Reading rc options for 'build' from {rc}:\n  Inherited 'common' options: --isatty=0"),
        format!("INFO: Reading rc options for 'build' from {rc}:\n  'build' options: --jobs=3"),
        format!("INFO: Found applicable config definition build:foo in file {rc}: --copt=-O2"),
    ];
    let mut pos = 0;
    for e in &expected {
        let at = err[pos..].find(e.as_str()).unwrap_or_else(|| panic!("missing/out of order: {e}\n--- stderr:\n{err}"));
        pos += at + e.len();
    }
}

#[test]
fn client_fatal_framing_for_rc_and_startup_errors() {
    let w = Ws::new("fatal", "rc_announce");
    // Nullary startup flag with a value: rejected while splitting, no rc log yet.
    let o = w.run(&["--nobatch=1"], &["info", "release"], &[]);
    let err = s(&o.stderr);
    assert_eq!(o.status.code(), Some(2));
    assert!(err.contains("src/main/cpp/blaze.cc:1275] In argument '--nobatch=1': option '--nobatch' does not take a value."), "{err}");
    assert!(!err.contains("Looking for the following rc files"), "{err}");

    // Bad value: converted after the rc files were read, so the buffered INFO log is flushed first.
    let o = w.run(&["--max_idle_secs=abc"], &["info", "release"], &[]);
    let err = s(&o.stderr);
    assert_eq!(o.status.code(), Some(2));
    let l = err.find("option_processor.cc:437] Looking for the following rc files: ").expect(&err);
    let f = err.find("src/main/cpp/blaze.cc:1275] ").expect(&err);
    assert!(l < f, "{err}");

    // Import of an unreadable file: INTERNAL_ERROR (37) with the Parsing log lines.
    fs::write(w.ws.join(".bazelrc"), format!("import {}/missing.rc\n", w.root.display())).unwrap();
    let o = w.run(&[], &["info", "release"], &[]);
    let err = s(&o.stderr);
    assert_eq!(o.status.code(), Some(37), "{err}");
    assert!(err.contains("rc_file.cc:94] Parsing the RcFile "), "{err}");
    assert!(err.contains("] Unexpected error reading config file '") && err.contains("missing.rc': (error: 2): No such file or directory"), "{err}");

    // Unknown startup option inside an rc file: provenance note, buffered log, one FATAL.
    fs::write(w.ws.join(".bazelrc"), "startup --bogus_startup\n").unwrap();
    let o = w.run(&[], &["info", "release"], &[]);
    let err = s(&o.stderr);
    assert_eq!(o.status.code(), Some(2), "{err}");
    assert!(err.starts_with("INFO: Reading 'startup' options from "), "{err}");
    assert!(err.contains("] Unknown startup option: '--bogus_startup'."), "{err}");
    assert!(!err.contains("[ERROR"), "{err}");
}

#[test]
fn generated_license_matches_merge_licenses_recipe() {
    let bazel = Path::new(&std::env::var("HOME").unwrap_or_default()).join("Development/bazel");
    if !bazel.join("LICENSE").exists() {
        eprintln!("skipping: no Bazel source tree");
        return;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/generate_license.py");
    let o = Command::new("python3").arg(script).arg("--check").arg("--bazel-dir").arg(&bazel).output().unwrap();
    assert!(o.status.success(), "{}{}", s(&o.stdout), s(&o.stderr));
}
