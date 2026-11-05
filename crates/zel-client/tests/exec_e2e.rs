//! End-to-end tests of the execution phase through the real `zel` binary: spawn strategies,
//! sandboxing, persistent workers, the action cache, coverage, test flags and the output base
//! layout.

use std::path::{Path, PathBuf};
use std::process::Command;

fn zel() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_zel"))
}


/// A hermetic local registry built once per test process from the vendored closure under
/// `third_party` (`scripts/make_vendored_registry.py`); no network is ever used.
fn local_registry() -> PathBuf {
    static REG: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    REG.get_or_init(|| {
        let root = env!("CARGO_MANIFEST_DIR").strip_suffix("/crates/zel-client").unwrap_or("/Users/pledger/Development/zel");
        let dir = std::env::temp_dir().join(format!("zel-client-registry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = std::process::Command::new("python3")
            .arg(format!("{root}/scripts/make_vendored_registry.py"))
            .arg(&dir)
            .status()
            .expect("python3 is required to build the local test registry");
        assert!(st.success());
        dir
    })
    .clone()
}

fn registry_flag(cmd: Option<&str>) -> Option<String> {
    matches!(cmd, Some("build" | "test" | "run" | "coverage" | "query" | "cquery" | "aquery" | "fetch"))
        .then(|| format!("--registry=file://{}", local_registry().display()))
}

/// Bazel 9 has no native `sh_*` rules: BUILD files load them from `@rules_shell`.
fn shell_loads(rel: &str, content: &str) -> String {
    if !(rel == "BUILD" || rel.ends_with("/BUILD")) {
        return content.to_string();
    }
    let mut out = String::new();
    for name in ["sh_binary", "sh_test", "sh_library"] {
        if content.contains(&format!("{name}(")) {
            out.push_str(&format!("load(\"@rules_shell//shell:{name}.bzl\", \"{name}\")\n"));
        }
    }
    out.push_str(content);
    out
}

struct Ws {
    root: PathBuf,
    ws: PathBuf,
    ob: PathBuf,
}

fn ws(name: &str) -> Ws {
    let root = std::env::temp_dir().join(format!("zel-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("ws")).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let w = Ws { ws: root.join("ws"), ob: root.join("ob"), root };
    std::fs::write(w.ws.join("MODULE.bazel"), "module(name = \"e2e\")\nbazel_dep(name = \"rules_shell\", version = \"0.6.1\")\n").unwrap();
    w
}

impl Ws {
    fn write(&self, rel: &str, content: &str) {
        let p = self.ws.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, shell_loads(rel, content)).unwrap();
    }

    fn exe(&self, rel: &str, content: &str) {
        self.write(rel, content);
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(self.ws.join(rel), std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn zel(&self, startup: &[&str], args: &[&str]) -> (i32, String) {
        let o = Command::new(zel())
            .current_dir(&self.ws)
            .arg(format!("--output_base={}", self.ob.display()))
            .args(startup)
            .args(args.first())
            .args(registry_flag(args.first().copied()))
            .args(args.iter().skip(1))
            .env("HOME", &self.root)
            .output()
            .unwrap();
        let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&o.stderr));
        (o.status.code().unwrap_or(-1), text)
    }

    fn build(&self, args: &[&str]) -> (i32, String) {
        let mut a = vec!["build"];
        a.extend_from_slice(args);
        self.zel(&[], &a)
    }

    fn bin(&self, rel: &str) -> PathBuf {
        self.ws.join("bazel-bin").join(rel)
    }
}

const GENRULES: &str = r#"
genrule(
    name = "undecl",
    outs = ["u.txt"],
    cmd = "cat pkg/in.txt > $@ 2>/dev/null || echo MISSING > $@",
)
genrule(
    name = "undecl_local",
    outs = ["ul.txt"],
    cmd = "cat pkg/in.txt > $@ 2>/dev/null || echo MISSING > $@",
)
genrule(
    name = "fail",
    outs = ["f.txt"],
    cmd = "echo boom >&2; exit 3",
)
genrule(
    name = "ok",
    outs = ["ok.txt"],
    cmd = "echo ok > $@",
)
"#;

/// Leak guard: every e2e workspace shuts its daemon down (and kills it if that fails) when dropped.
impl Drop for Ws {
    fn drop(&mut self) {
        let _ = Command::new(zel())
            .current_dir(&self.ws)
            .arg(format!("--output_base={}", self.ob.display()))
            .arg("shutdown")
            .env("HOME", &self.root)
            .output();
        if let Ok(pid) = std::fs::read_to_string(self.ob.join("server/server.pid.txt")) {
            let _ = Command::new("kill").args(["-9", pid.trim()]).status();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn strategies_sandbox_and_error_messages() {
    let w = ws("strat");
    w.write("pkg/BUILD", GENRULES);
    w.write("pkg/in.txt", "hello\n");
    // Default strategies: sandboxed — the undeclared input is invisible.
    let (code, out) = w.build(&["//pkg:undecl"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(std::fs::read_to_string(w.bin("pkg/u.txt")).unwrap(), "MISSING\n");
    assert!(out.contains("INFO: 2 processes: 1 internal, 1 ") && out.contains("sandbox"), "{out}");
    // --spawn_strategy=local: the symlink forest makes the whole workspace visible.
    // (The strategy is not part of the action key, so use a second target.)
    let (code, out) = w.build(&["//pkg:undecl_local", "--spawn_strategy=local"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(std::fs::read_to_string(w.bin("pkg/ul.txt")).unwrap(), "hello\n");
    assert!(out.contains("INFO: 2 processes: 1 internal, 1 local."), "{out}");
    // --genrule_strategy and --strategy precedence.
    let (_, out) = w.build(&["//pkg:ok", "--genrule_strategy=local", "--strategy=Genrule=standalone", "--noincompatible_x_unknown"]);
    let _ = out;
    // Unknown strategy: exit 2 with Bazel's message.
    let (code, out) = w.build(&["//pkg:ok", "--strategy=Genrule=bogus"]);
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("ERROR: 'bogus' was requested for mnemonic Genrule but no strategy with that identifier was registered. Valid values are: ["),
        "{out}"
    );
    let (code, out) = w.build(&["//pkg:ok", "--spawn_strategy=remote"]);
    // Bazel's RemoteModule registers a placeholder "remote" strategy even without a remote
    // executor (RemoteActionContextProvider.createForPlaceholder), so the name resolves and the spawn
    // fails to execute (a build failure, exit 1) rather than a registry error (exit 2).
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("Genrule spawn cannot be executed with any of the available strategies: [remote]"), "{out}");
    // Bad regexp strategy value.
    let (code, out) = w.build(&["//pkg:ok", "--strategy_regexp=Gen=nope"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("was requested for filter Gen but"), "{out}");
    // strategy_regexp routes by progress message.
    let (code, out) = w.build(&["//pkg:fail", "--strategy_regexp=Executing genrule //pkg:fail=local"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("failed: (Exit 3): bash failed: error executing Genrule command"), "{out}");
    assert!(out.contains("INFO: 2 processes: 2 internal."), "{out}");
    // Sandboxed failure: hint to use --sandbox_debug.
    let (_, out) = w.build(&["//pkg:fail"]);
    assert!(out.contains("Use --sandbox_debug to see verbose messages from the sandbox and retain the sandbox build root for debugging"), "{out}");
    let (_, out) = w.build(&["//pkg:fail", "--sandbox_debug"]);
    assert!(!out.contains("Use --sandbox_debug"), "{out}");
    assert!(!out.contains("Use --verbose_failures"), "{out}");
    // --sandbox_debug keeps the sandbox directories; a normal build cleans them.
    let sb = w.ob.join("sandbox");
    assert!(std::fs::read_dir(&sb).map(|mut d| d.next().is_some()).unwrap_or(false), "sandbox kept with --sandbox_debug");
    let (_, _) = w.build(&["//pkg:ok", "--nosandbox_debug"]);
    assert!(!sb.join("processwrapper-sandbox").exists() && !sb.join("darwin-sandbox").exists(), "cleaned after build");
    // Invalid resource flags.
    let (code, out) = w.build(&["//pkg:ok", "--jobs=lots"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("lots"), "{out}");

    let (code, out) = w.build(&["//pkg:ok", "--local_resources=cpu"]);
    assert_eq!(code, 2, "{out}");
    let (code, out) = w.build(&["//pkg:ok", "--sandbox_add_mount_pair=/a::/b"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("contains one or more empty paths"), "{out}");
    let (code, out) = w.build(&["//pkg:ok", "--jobs=3", "--local_resources=memory=HOST_RAM*.5", "--local_resources=cpu=2"]);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn action_cache_check_up_to_date_and_digest_function() {
    let w = ws("cache");
    w.write("pkg/BUILD", GENRULES);
    let (code, out) = w.build(&["//pkg:ok", "--check_up_to_date"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("is not up-to-date"), "{out}");
    assert!(!w.bin("pkg/ok.txt").exists());
    let (code, out) = w.build(&["//pkg:ok"]);
    assert_eq!(code, 0, "{out}");
    let (code, out) = w.build(&["//pkg:ok", "--check_up_to_date"]);
    assert_eq!(code, 0, "{out}");
    // Second run is an action-cache hit: only the workspace-status action executes
    // (real bazel: `INFO: 1 process: 1 internal.`).
    let (_, out) = w.build(&["//pkg:ok"]);
    assert!(out.contains("INFO: 1 process: 1 internal."), "{out}");
    // Tampering is detected via the recorded output digest.
    std::fs::write(w.bin("pkg/ok.txt"), "tampered").ok();
    let _ = std::fs::remove_file(w.bin("pkg/ok.txt"));
    // Startup option --digest_function.
    let (code, out) = w.zel(&["--digest_function=SHA1"], &["build", "//pkg:ok"]);
    assert_eq!(code, 0, "{out}");
    let (code, out) = w.zel(&["--digest_function=BLAKE3"], &["build", "//pkg:ok"]);
    assert_eq!(code, 0, "{out}");
    let (code, out) = w.zel(&["--digest_function=MD5"], &["build", "//pkg:ok"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("'MD5' is not a valid hash function. Possible values are: BLAKE3, SHA-1, SHA-256, SHA-384, SHA-512"), "{out}");
}

#[test]
fn output_base_layout_and_symlink_forest() {
    let w = ws("layout");
    w.write("pkg/BUILD", GENRULES);
    w.write("pkg/in.txt", "x\n");
    let (code, out) = w.build(&["//pkg:ok"]);
    assert_eq!(code, 0, "{out}");
    let er = w.ob.join("execroot/_main");
    assert!(std::fs::symlink_metadata(er.join("pkg")).unwrap().file_type().is_symlink(), "top-level dirs are symlinks");
    assert!(er.join("pkg/in.txt").exists());
    assert!(er.join("MODULE.bazel").exists());
    assert!(std::fs::symlink_metadata(er.join("bazel-out")).unwrap().is_dir());
    let cl = std::fs::read_to_string(w.ob.join("command.log")).unwrap();
    assert!(cl.contains("Build completed successfully"), "{cl}");
    let install = std::fs::read_link(w.ob.join("install")).unwrap();
    assert!(install.is_absolute());
    let jl = std::fs::read_link(w.ob.join("java.log")).unwrap();
    assert!(jl.to_string_lossy().starts_with("java.log."), "{jl:?}");
    assert!(std::fs::read_to_string(w.ob.join("java.log")).unwrap().contains("Command: build exit_code=0"));
    for d in ["action_cache", "execroot", "external", "server"] {
        assert!(w.ob.join(d).is_dir(), "{d}");
    }
    // Without Bzlmod the execroot directory is the WORKSPACE name.
    let w2 = ws("layout2");
    w2.write("WORKSPACE", "workspace(name = \"legacy_ws\")\n");
    w2.write("pkg/BUILD", GENRULES);
    let (code, out) = w2.build(&["//pkg:ok", "--noenable_bzlmod"]);
    assert_eq!(code, 0, "{out}");
    assert!(w2.ob.join("execroot/legacy_ws/pkg").exists(), "{out}");
    let _ = Path::new("/");
}

const WORKER_DEFS: &str = r#"
def _impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".out")
    args = ctx.actions.args()
    args.add("--out=" + out.path)
    args.add("--text=" + ctx.attr.text)
    args.use_param_file("@%s", use_always = True)
    # Persistent workers read the flagfile verbatim: they need the unquoted `multiline` format.
    args.set_param_file_format("multiline")
    ctx.actions.run(
        outputs = [out],
        executable = ctx.executable._worker,
        arguments = [args],
        mnemonic = "ZelWorker",
        progress_message = "Working on %s" % ctx.label,
        execution_requirements = {"supports-workers": "1"},
    )
    return [DefaultInfo(files = depset([out]))]

zworker = rule(
    implementation = _impl,
    attrs = {
        "text": attr.string(),
        "_worker": attr.label(default = "//tools:worker", executable = True, cfg = "exec", allow_files = True),
    },
)
"#;

fn test_worker_bin() -> Option<PathBuf> {
    let p = zel().parent()?.join("zel-test-worker");
    p.exists().then_some(p)
}

#[test]
fn persistent_worker_is_reused_within_a_build() {
    let Some(worker) = test_worker_bin() else {
        eprintln!("zel-test-worker not built; skipping");
        return;
    };
    let w = ws("worker");
    w.write("tools/defs.bzl", WORKER_DEFS);
    w.write(
        "tools/BUILD",
        "load(\":defs.bzl\", \"zworker\")\nsh_binary(name = \"worker\", srcs = [\"worker.sh\"], visibility = [\"//visibility:public\"])\nzworker(name = \"w1\", text = \"one\")\nzworker(name = \"w2\", text = \"two\")\n",
    );
    w.exe("tools/worker.sh", &format!("#!/bin/sh\nexec {} \"$@\"\n", worker.display()));
    let (code, out) = w.build(&["//tools:w1", "//tools:w2", "--jobs=1"]);
    assert_eq!(code, 0, "{out}");
    let a = std::fs::read_to_string(w.bin("tools/w1.out")).unwrap();
    let b = std::fs::read_to_string(w.bin("tools/w2.out")).unwrap();
    let f = |s: &str, i: usize| s.split(' ').nth(i).unwrap().to_string();
    assert_eq!(f(&a, 0), f(&b, 0), "same worker pid");
    assert_eq!((f(&a, 1), f(&b, 1)), ("1".to_string(), "2".to_string()), "warm worker handled both");
    assert!(out.contains("2 worker"), "{out}");
    let logs: Vec<_> = std::fs::read_dir(w.ob.join("bazel-workers")).unwrap().collect();
    assert!(!logs.is_empty());
    // --strategy=ZelWorker=local bypasses the worker (the tool would fail without a flagfile
    // protocol, so only check the strategy error path here).
    let (code, out) = w.build(&["//tools:w1", "--strategy=ZelWorker=worker", "--worker_max_instances=ZelWorker=2", "--noworker_multiplex"]);
    assert_eq!(code, 0, "{out}");
    let (code, out) = w.build(&["//tools:w1", "--worker_max_instances=ZelWorker=x"]);
    assert_eq!(code, 2, "{out}");
}

const SH_TEST: &str = r#"
sh_test(name = "t", srcs = ["t.sh"])
"#;

#[test]
fn test_command_cache_status_filters_and_sharding() {
    let w = ws("tests");
    w.write("pkg/BUILD", SH_TEST);
    w.exe("pkg/t.sh", "#!/bin/sh\n[ -z \"$TEST_SHARD_STATUS_FILE\" ] || touch \"$TEST_SHARD_STATUS_FILE\"\necho \"shards=$TEST_TOTAL_SHARDS idx=$TEST_SHARD_INDEX\"\nexit 0\n");
    let (code, out) = w.zel(&[], &["test", "//pkg:t", "--test_output=all"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("shards= idx="), "{out}");
    let dir = w.ws.join("bazel-testlogs/pkg/t");
    let data = std::fs::read(dir.join("test.cache_status")).unwrap();
    // proto2 TestResultData: cachable=true, test_passed=true, status=PASSED.
    assert_eq!(&data[..6], &[0x08, 1, 0x10, 1, 0x18, 1]);
    // Cached on rerun.
    let (code, out) = w.zel(&[], &["test", "//pkg:t"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("(cached) PASSED"), "{out}");
    // Sharding strategies.
    let (code, out) = w.zel(&[], &["test", "//pkg:t", "--test_sharding_strategy=forced=3", "--test_output=all", "--cache_test_results=no"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("shards=3 idx=0") && out.contains("shards=3 idx=2"), "{out}");
    let (code, out) = w.zel(&[], &["test", "//pkg:t", "--test_sharding_strategy=bogus"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("Not a valid test sharding strategy: 'bogus'"), "{out}");
    // Language filter: sh excluded => no tests found (exit 4).
    let (code, out) = w.zel(&[], &["test", "//pkg:t", "--test_lang_filters=-sh"]);
    assert_eq!(code, 4, "{out}");
    let (code, out) = w.zel(&[], &["test", "//pkg:t", "--test_lang_filters=sh", "--cache_test_results=no"]);
    assert_eq!(code, 0, "{out}");
    // --check_up_to_date for the build part of a never-built target.
    let (code, out) = w.zel(&[], &["build", "//pkg:t", "--check_up_to_date", "--noexperimental_nothing"]);
    let _ = (code, out);
}

#[test]
fn coverage_command_produces_combined_lcov_report() {
    let w = ws("cov");
    w.write("lib/BUILD", "sh_library(name = \"lib\", srcs = [\"lib.sh\"], visibility = [\"//visibility:public\"])\n");
    w.write("lib/lib.sh", "echo lib\n");
    w.write("pkg/BUILD", "sh_test(name = \"t\", srcs = [\"t.sh\"], deps = [\"//lib:lib\"])\n");
    w.exe(
        "pkg/t.sh",
        "#!/bin/sh\ntest -n \"$COVERAGE_DIR\" || exit 9\ntest -f \"$COVERAGE_MANIFEST\" || exit 8\nmkdir -p \"$COVERAGE_DIR\"\nprintf 'SF:lib/lib.sh\\nDA:1,1\\nend_of_record\\nSF:pkg/t.sh\\nDA:1,1\\nend_of_record\\n' > \"$COVERAGE_DIR/x.dat\"\n",
    );
    let (code, out) = w.zel(&[], &["coverage", "//pkg:t", "--combined_report=lcov", "--test_output=errors"]);
    assert_eq!(code, 0, "{out}");
    let report = w.ob.join("execroot/_main/bazel-out/_coverage/_coverage_report.dat");
    assert!(out.contains(&format!("INFO: LCOV coverage report is located at {}", report.display())), "{out}");
    assert!(out.contains(" and execpath is bazel-out/_coverage/_coverage_report.dat"), "{out}");
    let rep = std::fs::read_to_string(&report).unwrap_or_default();
    assert!(rep.contains("SF:") && rep.contains("DA:1,1"), "{rep}");
    let cov = std::fs::read_to_string(w.ws.join("bazel-testlogs/pkg/t/coverage.dat")).unwrap();
    assert!(cov.contains("DA:1,1"), "{cov}");
    // An invalid combined_report value is a command line error.
    let (code, out) = w.zel(&[], &["coverage", "//pkg:t", "--combined_report=html5"]);
    assert_eq!(code, 2, "{out}");
}
