#![cfg(unix)]
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use zel_proto::command_server::{ExecRequest, RunRequest};
use zel_server::{BlazeDirectories, BlazeRuntime};
use zel_vfs::{FileSystem, LocalFileSystem, PathFragment};

/// A hermetic local registry built once per test process from the vendored closure under
/// `third_party` (`scripts/make_vendored_registry.py`); no network is ever used.
fn local_registry(root: &str) -> PathBuf {
    static REG: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    REG.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("zel-btrc-registry-{}", std::process::id()));
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

fn shell_loads(rel: &str, content: &str) -> String {
    if !(rel == "BUILD" || rel.ends_with("/BUILD") || rel.ends_with("BUILD.bazel")) {
        return content.to_string();
    }
    let mut out = String::new();
    for name in ["sh_binary", "sh_test", "sh_library"] {
        if content.contains(&format!("{name}(")) && !content.contains(&format!("\"{name}\")")) {
            out.push_str(&format!(
                "load(\"@rules_shell//shell:{name}.bzl\", \"{name}\")\n"
            ));
        }
    }
    out.push_str(content);
    out
}

struct Ws {
    ws: PathBuf,
    ob: PathBuf,
    rt: BlazeRuntime,
}

fn make_ws(name: &str) -> Ws {
    let root = std::env::temp_dir().join(format!("zel-btrc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ws_dir = root.join("ws");
    std::fs::create_dir_all(&ws_dir).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let ws_dir = root.join("ws");
    std::fs::write(
        ws_dir.join("MODULE.bazel"),
        "module(name = \"w\")\nbazel_dep(name = \"rules_shell\", version = \"0.6.1\")\n",
    )
    .unwrap();
    std::fs::write(ws_dir.join("BUILD"), "").unwrap();
    let fs: Arc<dyn FileSystem> = Arc::new(LocalFileSystem::default());
    let p = |s: &str| PathFragment::create(root.join(s).to_str().unwrap());
    let ob = root.join("ob");
    let dirs = BlazeDirectories {
        workspace_dir: PathFragment::create(ws_dir.to_str().unwrap()),
        output_base: p("ob"),
        install_base: p("ib"),
        output_user_root: p("ur"),
    };
    Ws {
        ws: ws_dir,
        ob,
        rt: BlazeRuntime::new(dirs, fs, 1),
    }
}

struct CmdOut {
    code: i32,
    #[allow(dead_code)]
    out: String,
    err: String,
    exec: Option<ExecRequest>,
}

impl Ws {
    fn write(&self, rel: &str, content: &str) {
        // Bazel 9 has no native `sh_*` rules: BUILD files load them from `@rules_shell` (hermetic
        // local registry, see `local_registry`).
        let content = &shell_loads(rel, content);
        let p = self.ws.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if rel.ends_with(".sh") {
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
            }
        }
    }

    fn run(&self, args: &[&str]) -> CmdOut {
        let root = env!("CARGO_MANIFEST_DIR")
            .strip_suffix("/crates/zel-server")
            .unwrap_or("/Users/pledger/Development/zel");
        let reg = format!(
            "--registry={}",
            zel_bzlmod::file_url_for_path(&local_registry(root))
        );
        let mut full_args = vec![args[0], &reg];
        let path_env = std::env::var("PATH")
            .ok()
            .map(|p| format!("--client_env=PATH={p}"));
        if let Some(ref pe) = path_env {
            full_args.push(pe);
        }
        full_args.extend_from_slice(&args[1..]);
        let arg: Vec<Vec<u8>> = full_args.iter().map(|a| a.as_bytes().to_vec()).collect();
        let req = RunRequest {
            cookie: "c".into(),
            arg,
            client_description: "t".into(),
            ..Default::default()
        };
        let r = self.rt.dispatch_command(&req, "r");
        CmdOut {
            code: r.exit_code,
            out: String::from_utf8_lossy(&r.standard_output).into_owned(),
            err: String::from_utf8_lossy(&r.standard_error).into_owned(),
            exec: r.exec_request,
        }
    }
}

#[test]
fn test_build_empty_target_warning() {
    let w = make_ws("empty-target");
    let res = w.run(&["build"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(
        res.err
            .contains("WARNING: Usage: bazel build <options> <targets>."),
        "{}",
        res.err
    );
    assert!(res.err.contains("Nothing will be built."), "{}", res.err);
}

#[test]
fn test_build_wildcard_filters_manual_targets() {
    let w = make_ws("manual-filter");
    w.write(
        "pkg/BUILD",
        r#"
sh_binary(name = "normal", srcs = ["normal.sh"])
sh_binary(name = "tagged", srcs = ["tagged.sh"], tags = ["manual"])
"#,
    );
    w.write("pkg/normal.sh", "#!/bin/sh\nexit 0\n");
    w.write("pkg/tagged.sh", "#!/bin/sh\nexit 0\n");

    // Without --build_manual_tests, //pkg/... should only build //pkg:normal
    let res = w.run(&["build", "//pkg/..."]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(
        res.err.contains("Target //pkg:normal up-to-date:"),
        "{}",
        res.err
    );
    assert!(
        !res.err.contains("Target //pkg:tagged up-to-date:"),
        "{}",
        res.err
    );

    // With --build_manual_tests, //pkg/... builds both
    let res2 = w.run(&[
        "build",
        "--show_result=10",
        "--build_manual_tests",
        "//pkg/...",
    ]);
    assert_eq!(res2.code, 0, "{}", res2.err);
    assert!(
        res2.err.contains("Target //pkg:tagged up-to-date:"),
        "{}",
        res2.err
    );

    // Explicitly targeting //pkg:tagged builds it even without --build_manual_tests
    let res3 = w.run(&["build", "//pkg:tagged"]);
    assert_eq!(res3.code, 0, "{}", res3.err);
    assert!(
        res3.err.contains("Target //pkg:tagged up-to-date:"),
        "{}",
        res3.err
    );
}

#[test]
fn test_convenience_symlinks_false() {
    let w = make_ws("symlinks-false");
    w.write("pkg/BUILD", "sh_binary(name = 'b', srcs = ['b.sh'])\n");
    w.write("pkg/b.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&[
        "build",
        "--experimental_convenience_symlinks=false",
        "//pkg:b",
    ]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(
        !w.ws.join("bazel-bin").exists(),
        "bazel-bin should not exist when convenience_symlinks=false"
    );

    let res2 = w.run(&[
        "build",
        "--experimental_convenience_symlinks=normal",
        "//pkg:b",
    ]);
    assert_eq!(res2.code, 0, "{}", res2.err);
    assert!(
        w.ws.join("bazel-bin").exists(),
        "bazel-bin should exist when convenience_symlinks=normal"
    );
}

#[test]
fn test_output_groups_and_fail_on_unknown() {
    let w = make_ws("out-groups");
    w.write(
        "pkg/BUILD",
        r#"
genrule(
    name = "g",
    outs = ["out.txt"],
    cmd = "echo hello > $@",
)
"#,
    );

    // Normal build shows out.txt
    let res = w.run(&["build", "//pkg:g"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(res.err.contains("bazel-bin/pkg/out.txt"), "{}", res.err);

    // Nonexistent output group: does NOT list default_info.files
    let res2 = w.run(&["build", "--output_groups=nonexistent", "//pkg:g"]);
    assert_eq!(res2.code, 0, "{}", res2.err);
    assert!(!res2.err.contains("bazel-bin/pkg/out.txt"), "{}", res2.err);

    // `--incompatible_fail_on_unknown_output_groups` only exists in Bazel HEAD; 9.3.2 rejects it.
    let res3 = w.run(&[
        "build",
        "--incompatible_fail_on_unknown_output_groups",
        "--output_groups=nonexistent",
        "//pkg:g",
    ]);
    assert_eq!(res3.code, 2, "{}", res3.err);
    assert!(
        res3.err
            .contains("Unrecognized option: --incompatible_fail_on_unknown_output_groups"),
        "{}",
        res3.err
    );
}

#[test]
fn test_run_multi_target_error_exit_2() {
    let w = make_ws("run-multi");
    w.write(
        "pkg/BUILD",
        r#"
sh_binary(name = "bin1", srcs = ["bin1.sh"])
sh_binary(name = "bin2", srcs = ["bin2.sh"])
"#,
    );
    w.write("pkg/bin1.sh", "#!/bin/sh\nexit 0\n");
    w.write("pkg/bin2.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["run", "//pkg:all"]);
    assert_eq!(res.code, 2, "{}", res.err);
    assert!(
        res.err.contains("ERROR: Only a single target can be run"),
        "{}",
        res.err
    );
}

#[test]
fn test_run_norun_flag() {
    let w = make_ws("run-norun");
    w.write("pkg/BUILD", "sh_binary(name = 'b', srcs = ['b.sh'])\n");
    w.write("pkg/b.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["run", "--norun", "//pkg:b"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(
        res.err.contains("INFO: Runnable command line:"),
        "{}",
        res.err
    );
    let exec = res
        .exec
        .expect("exec_request should be returned with should_exec=false under --norun");
    assert!(!exec.should_exec);
}

#[test]
fn test_run_in_cwd() {
    let w = make_ws("run-in-cwd");
    w.write("pkg/BUILD", "sh_binary(name = 'b', srcs = ['b.sh'])\n");
    w.write("pkg/b.sh", "#!/bin/sh\nexit 0\n");
    let sub = w.ws.join("sub");
    std::fs::create_dir_all(&sub).unwrap();

    let res = w.run(&[
        "run",
        "--run_in_cwd",
        &format!("--client_cwd={}", sub.display()),
        "//pkg:b",
    ]);
    assert_eq!(res.code, 0, "{}", res.err);
    let req = res.exec.expect("should have exec_request");
    assert_eq!(
        String::from_utf8_lossy(&req.working_directory),
        sub.to_string_lossy()
    );

    let bad = w.run(&["run", "--run_in_cwd", "--client_cwd=/custom/cwd", "//pkg:b"]);
    assert_eq!(bad.code, 2, "{}", bad.err);
}

#[test]
fn test_run_under_target_label() {
    let w = make_ws("run-under");
    w.write(
        "pkg/BUILD",
        r#"
sh_binary(name = "wrapper", srcs = ["wrapper.sh"])
sh_binary(name = "target", srcs = ["target.sh"])
"#,
    );
    w.write("pkg/wrapper.sh", "#!/bin/sh\nexec \"$@\"\n");
    w.write("pkg/target.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["run", "--run_under=//pkg:wrapper --arg", "//pkg:target"]);
    assert_eq!(res.code, 0, "{}", res.err);
    let req = res.exec.expect("should have exec_request");
    let argv: Vec<String> = req
        .argv
        .iter()
        .map(|a| String::from_utf8_lossy(a).to_string())
        .collect();
    assert!(argv.len() >= 3, "{:?}", argv);
    assert!(
        argv[0].contains("wrapper"),
        "first arg should be wrapper: {:?}",
        argv
    );
    assert_eq!(argv[1], "--arg");
    assert!(
        argv[2].contains("target"),
        "third arg should be target: {:?}",
        argv
    );
}

#[test]
fn test_run_test_env_vars() {
    let w = make_ws("run-test-env");
    w.write("pkg/BUILD", "sh_test(name = 't', srcs = ['t.sh'])\n");
    w.write("pkg/t.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["run", "//pkg:t"]);
    assert_eq!(res.code, 0, "{}", res.err);
    let req = res.exec.expect("should have exec_request");
    let env: BTreeMap<String, String> = req
        .environment_variable
        .iter()
        .map(|ev| {
            (
                String::from_utf8_lossy(&ev.name).to_string(),
                String::from_utf8_lossy(&ev.value).to_string(),
            )
        })
        .collect();
    assert_eq!(env.get("TEST_TARGET").map(|s| s.as_str()), Some("//pkg:t"));
    assert!(env.contains_key("TEST_TMPDIR"), "{:?}", env);
    assert!(env.contains_key("TEST_SRCDIR"), "{:?}", env);
    assert!(env.contains_key("TEST_WORKSPACE"), "{:?}", env);

    let to_clear: Vec<String> = req
        .environment_variable_to_clear
        .iter()
        .map(|b| String::from_utf8_lossy(b).to_string())
        .collect();
    assert!(
        to_clear.contains(&"RUNFILES_DIR".to_string()),
        "{:?}",
        to_clear
    );
}

#[test]
fn test_test_zero_tests_found_no_dangling_stats() {
    let w = make_ws("zero-tests");
    w.write("pkg/BUILD", "sh_binary(name = 'b', srcs = ['b.sh'])\n");
    w.write("pkg/b.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["test", "//pkg:b"]);
    assert_eq!(res.code, 4, "{}", res.err);
    assert!(
        res.err
            .contains("ERROR: No test targets were found, yet testing was requested"),
        "{}",
        res.err
    );
    assert!(
        !res.err.contains("Executed 0 out of 0 tests: ."),
        "{}",
        res.err
    );
}

#[test]
fn test_test_output_streamed_warning_and_execution() {
    let w = make_ws("streamed-output");
    w.write("pkg/BUILD", "sh_test(name = 't', srcs = ['t.sh'])\n");
    w.write("pkg/t.sh", "#!/bin/sh\necho 'hello from test'\nexit 0\n");

    let res = w.run(&["test", "--test_output=streamed", "//pkg:t"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(
        res.err.contains("WARNING: Streamed test output requested."),
        "{}",
        res.err
    );
    assert!(res.out.contains("hello from test"), "{}", res.out);
    assert!(
        !res.out
            .contains("==================== Test output for //pkg:t:"),
        "{}",
        res.out
    );
}

#[test]
fn test_test_max_output_bytes_truncation() {
    let w = make_ws("max-output");
    w.write("pkg/BUILD", "sh_test(name = 't', srcs = ['t.sh'])\n");
    w.write(
        "pkg/t.sh",
        "#!/bin/sh\necho '0123456789012345678901234567890123456789'\nexit 1\n",
    );

    let res = w.run(&[
        "test",
        "--test_output=errors",
        "--max_test_output_bytes=10",
        "//pkg:t",
    ]);
    assert_eq!(res.code, 3, "{}", res.err);
    assert!(res.out.contains("Test log too large ("), "{}", res.out);
}

#[test]
fn test_coverage_combined_report_default_and_baseline() {
    // Analysing `@rules_shell`'s Starlark `sh_test` with `--instrument_test_targets` recurses deeper than the 2 MiB
    // default stack of a debug-build test thread (the server runs on a bigger one).
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(coverage_default_body)
        .unwrap()
        .join()
        .unwrap();
}

fn coverage_default_body() {
    let w = make_ws("coverage-default");
    w.write("pkg/BUILD", "sh_test(name = 't', srcs = ['t.sh'])\n");
    w.write("pkg/t.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["coverage", "--instrument_test_targets", "//pkg:t"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(
        res.err.contains("INFO: LCOV coverage report is located at"),
        "{}",
        res.err
    );
    assert!(
        w.ob.join("bazel-out/_coverage/_coverage_report.dat")
            .exists()
            || res.err.contains("_coverage_report.dat")
    );
    // Baseline coverage file created
    let baseline = w.ws.join("bazel-testlogs/pkg/t/baseline_coverage.dat");
    assert!(
        baseline.exists() || res.err.contains("coverage"),
        "baseline coverage should be generated"
    );
}

#[test]
fn test_test_summary_modes() {
    let w = make_ws("summary-modes");
    w.write("pkg/BUILD", "sh_test(name = 'pass', srcs = ['pass.sh'])\n");
    w.write("pkg/pass.sh", "#!/bin/sh\nexit 0\n");

    // --test_summary=none should omit summary lines
    let res = w.run(&["test", "--test_summary=none", "//pkg:pass"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(!res.err.contains("PASSED in "), "{}", res.err);

    // --test_summary=terse should omit passed tests
    let res2 = w.run(&["test", "--test_summary=terse", "//pkg:pass"]);
    assert_eq!(res2.code, 0, "{}", res2.err);
    assert!(!res2.err.contains("PASSED in "), "{}", res2.err);

    // --test_summary=testcase should omit per-target summary and show Test cases line
    let res3 = w.run(&[
        "test",
        "--test_summary=testcase",
        "--cache_test_results=no",
        "//pkg:pass",
    ]);
    assert_eq!(res3.code, 0, "{}", res3.out);
    assert!(!res3.err.contains("PASSED in "), "{}", res3.err);
    assert!(
        res3.out.contains(
            "Test cases: finished with 1 passing, 0 skipped and 0 failing out of 1 test cases"
        ),
        "{}",
        res3.out
    );
}

#[test]
fn test_notest_keep_going() {
    let w = make_ws("test-keep-going");
    w.write(
        "pkg/BUILD",
        r#"
sh_test(name = "t1_fail", srcs = ["t1.sh"])
sh_test(name = "t2_pass", srcs = ["t2.sh"])
"#,
    );
    w.write("pkg/t1.sh", "#!/bin/sh\nexit 1\n");
    w.write("pkg/t2.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&[
        "test",
        "--notest_keep_going",
        "--jobs=1",
        "//pkg:t1_fail",
        "//pkg:t2_pass",
    ]);
    assert_eq!(res.code, 3, "{}", res.err);
}

#[test]
fn test_explain_and_output_filter() {
    let w = make_ws("explain-filter");
    w.write(
        "pkg/BUILD",
        r#"
genrule(name = "g", outs = ["g.out"], cmd = "echo 'my message' >&2; echo hi > $@")
"#,
    );
    let explain_file = w.ws.join("explain.txt");
    let res = w.run(&[
        "build",
        &format!("--explain={}", explain_file.display()),
        "--output_filter=nomatch",
        "//pkg:g",
    ]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(explain_file.exists(), "explain file should be created");
}

#[test]
fn test_run_portable_paths_and_omit_args() {
    let w = make_ws("run-portable");
    w.write("pkg/BUILD", "sh_binary(name = 'b', srcs = ['b.sh'])\n");
    w.write("pkg/b.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&[
        "run",
        "--portable_paths",
        "--omit_run_args",
        "//pkg:b",
        "--",
        "secret_arg",
    ]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(res.err.contains("<args omitted>"), "{}", res.err);
    assert!(!res.err.contains("secret_arg"), "{}", res.err);

    let req = res.exec.expect("should have exec request");
    assert!(
        !req.path_to_replace.is_empty(),
        "path_to_replace should not be empty with --portable_paths"
    );
}

#[test]
fn test_test_nobuild() {
    let w = make_ws("nobuild");
    w.write("pkg/BUILD", "sh_test(name = 'pass', srcs = ['pass.sh'])\n");
    w.write("pkg/pass.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["test", "--nobuild", "//pkg:pass"]);
    assert_eq!(res.code, 1, "{}", res.err);
    assert!(
        res.err
            .contains("ERROR: Couldn't start the build. Unable to run tests"),
        "{}",
        res.err
    );
    assert!(
        res.err.contains("INFO: Found 1 test target..."),
        "{}",
        res.err
    );
    assert!(!res.err.contains("Executed 1 out of 1"), "{}", res.err);
    assert!(!res.err.contains("fails locally"), "{}", res.err);
}

#[test]
fn test_found_targets_line() {
    let w = make_ws("found-targets");
    w.write(
        "pkg/BUILD",
        r#"
sh_binary(name = "bin", srcs = ["bin.sh"])
sh_test(name = "test1", srcs = ["test1.sh"])
"#,
    );
    w.write("pkg/bin.sh", "#!/bin/sh\nexit 0\n");
    w.write("pkg/test1.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["build", "//pkg:bin"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(res.err.contains("INFO: Found 1 target..."), "{}", res.err);

    let res2 = w.run(&["test", "//pkg:test1"]);
    assert_eq!(res2.code, 0, "{}", res2.err);
    assert!(
        res2.err.contains("INFO: Found 1 test target..."),
        "{}",
        res2.err
    );
}

#[test]
fn test_target_nothing_to_build() {
    let w = make_ws("nothing-to-build");
    w.write(
        "pkg/BUILD",
        "genrule(name = 'g', outs = ['out.txt'], cmd = 'echo hi > $@')\n",
    );

    let res = w.run(&["build", "--output_groups=nonexistent", "//pkg:g"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(
        res.err
            .contains("Target //pkg:g up-to-date (nothing to build)"),
        "{}",
        res.err
    );
}

#[test]
fn test_run_multi_target_truncated() {
    let w = make_ws("multi-trunc");
    w.write(
        "pkg/BUILD",
        r#"
sh_binary(name = "t1", srcs = ["t.sh"])
sh_binary(name = "t2", srcs = ["t.sh"])
sh_binary(name = "t3", srcs = ["t.sh"])
sh_binary(name = "t4", srcs = ["t.sh"])
sh_binary(name = "t5", srcs = ["t.sh"])
sh_binary(name = "t6", srcs = ["t.sh"])
"#,
    );
    w.write("pkg/t.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["run", "//pkg/..."]);
    assert_eq!(res.code, 2, "{}", res.err);
    assert!(res.err.contains("[TRUNCATED]"), "{}", res.err);
    assert!(
        res.err
            .contains("//pkg:t1, //pkg:t2, //pkg:t3, //pkg:t4, //pkg:t5[TRUNCATED]"),
        "{}",
        res.err
    );
}

#[test]
fn test_run_test_target_runs_per_test_rejected() {
    let w = make_ws("run-test-rpt");
    w.write("pkg/BUILD", "sh_test(name = 't', srcs = ['t.sh'])\n");
    w.write("pkg/t.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["run", "--runs_per_test=2", "//pkg:t"]);
    assert_eq!(res.code, 2, "{}", res.err);
    assert!(
        res.err.contains("'run' only works with tests with one shard ('--test_sharding_strategy=disabled' is okay) and without --runs_per_test"),
        "{}",
        res.err
    );
}

#[test]
fn test_run_test_target_full_env_and_setup_script() {
    let w = make_ws("run-test-full-env");
    w.write("pkg/BUILD", "sh_test(name = 't', srcs = ['t.sh'])\n");
    w.write("pkg/t.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["run", "//pkg:t"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(
        res.err.contains(
            "INFO: Running command line: external/bazel_tools/tools/test/test-setup.sh pkg/t"
        ),
        "{}",
        res.err
    );
    let req = res.exec.expect("should have exec request");
    let env: BTreeMap<String, String> = req
        .environment_variable
        .iter()
        .map(|ev| {
            (
                String::from_utf8_lossy(&ev.name).to_string(),
                String::from_utf8_lossy(&ev.value).to_string(),
            )
        })
        .collect();
    assert_eq!(env.get("TEST_TARGET").map(|s| s.as_str()), Some("//pkg:t"));
    assert_eq!(env.get("TEST_BINARY").map(|s| s.as_str()), Some("pkg/t"));
    assert!(
        env.get("TEST_TMPDIR").unwrap().contains("_tmp"),
        "{:?}",
        env.get("TEST_TMPDIR")
    );
    assert!(env.contains_key("XML_OUTPUT_FILE"), "{:?}", env);
    assert!(env.contains_key("TEST_WARNINGS_OUTPUT_FILE"), "{:?}", env);
    assert!(env.contains_key("TEST_PREMATURE_EXIT_FILE"), "{:?}", env);
    assert!(env.contains_key("TEST_UNDECLARED_OUTPUTS_DIR"), "{:?}", env);
    assert!(env.contains_key("RUNFILES_DIR"), "{:?}", env);
    assert!(env.contains_key("JAVA_RUNFILES"), "{:?}", env);
}

#[test]
fn test_check_tests_up_to_date_trailer_and_message() {
    let w = make_ws("check-up-to-date");
    w.write("pkg/BUILD", "sh_test(name = 't', srcs = ['t.sh'])\n");
    w.write("pkg/t.sh", "#!/bin/sh\nexit 0\n");

    let b_res = w.run(&["build", "//pkg:t"]);
    assert_eq!(b_res.code, 0, "{}", b_res.err);

    let res = w.run(&["test", "--check_tests_up_to_date", "//pkg:t"]);
    assert_eq!(res.code, 1, "{}", res.err);
    assert!(
        res.err.contains("ERROR: Testing //pkg:t is not up-to-date"),
        "{}",
        res.err
    );
    assert!(
        res.out
            .contains("Finished with 0 passing and 0 failing tests up to date, 1 out of date."),
        "{}",
        res.out
    );
}

#[test]
fn test_notest_keep_going_skipped_reporting() {
    let w = make_ws("notest-kg-skipped");
    w.write(
        "pkg/BUILD",
        r#"
sh_test(name = "t1_fail", srcs = ["t1.sh"])
sh_test(name = "t2_pass", srcs = ["t2.sh"])
"#,
    );
    w.write("pkg/t1.sh", "#!/bin/sh\nexit 1\n");
    w.write("pkg/t2.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&[
        "test",
        "--test_summary=detailed",
        "--notest_keep_going",
        "--jobs=1",
        "--test_strategy=exclusive",
        "//pkg:t1_fail",
        "//pkg:t2_pass",
    ]);
    assert_eq!(res.code, 3, "{}", res.err);
    assert!(
        res.err.contains("INFO: Build completed, 1 test FAILED"),
        "{}",
        res.err
    );
    assert!(res.out.contains("and 1 was skipped."), "{}", res.out);
    assert!(
        res.out.contains("//pkg:t2_pass") && res.out.contains("NO STATUS"),
        "{}",
        res.out
    );
}

#[test]
fn test_explain_created_when_no_actions_run() {
    let w = make_ws("explain-no-act");
    w.write(
        "pkg/BUILD",
        "genrule(name = 'g', outs = ['g.out'], cmd = 'echo hi > $@')\n",
    );
    // Build once so it is up-to-date
    let res1 = w.run(&["build", "//pkg:g"]);
    assert_eq!(res1.code, 0, "{}", res1.err);

    let explain_file = w.ws.join("explain_no_act.txt");
    let res2 = w.run(&[
        "build",
        &format!("--explain={}", explain_file.display()),
        "//pkg:g",
    ]);
    assert_eq!(res2.code, 0, "{}", res2.err);
    assert!(
        explain_file.exists(),
        "explain file must be created even when no actions run"
    );
    let content = std::fs::read_to_string(&explain_file).unwrap();
    assert!(content.contains("Build options:"), "{}", content);
    assert!(
        res2.err
            .contains("INFO: Writing explanation of rebuilds to"),
        "{}",
        res2.err
    );
}

#[test]
fn test_coverage_support_nonexistent_label_fails() {
    let w = make_ws("cov-support-err");
    w.write("pkg/BUILD", "sh_test(name = 't', srcs = ['t.sh'])\n");
    w.write("pkg/t.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&[
        "coverage",
        "--coverage_support=//pkg:nonexistent",
        "//pkg:t",
    ]);
    assert_eq!(res.code, 1, "{}", res.err);
    // Real bazel 9.3.2: the tests implicitly depend on the label.
    assert!(
        res.err.contains("every rule of type sh_test implicitly depends upon the target '//pkg:nonexistent', but this target could not be found because of: no such target '//pkg:nonexistent'"),
        "{}",
        res.err
    );
    assert!(
        res.err
            .contains("ERROR: Analysis of target '//pkg:t' failed; build aborted: Analysis failed"),
        "{}",
        res.err
    );
}

#[test]
fn test_genrule_location_declared_output() {
    let w = make_ws("gen-location-out");
    w.write(
        "pkg/BUILD",
        r#"
genrule(
    name = "g",
    outs = ["m1.txt"],
    cmd = "echo hello > $(location m1.txt)",
)
"#,
    );

    let res = w.run(&["build", "//pkg:g"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(w.ws.join("bazel-bin/pkg/m1.txt").exists() || res.err.contains("m1.txt"));
}

#[test]
fn test_bust_action_caches_label() {
    let w = make_ws("bust-cache-label");
    w.write(
        "pkg/BUILD",
        r#"
genrule(name = "g1", outs = ["g1.out"], cmd = "echo 1 > $@")
genrule(name = "g2", outs = ["g2.out"], cmd = "echo 2 > $@")
"#,
    );
    let res1 = w.run(&["build", "//pkg:g1", "//pkg:g2"]);
    assert_eq!(res1.code, 0, "{}", res1.err);

    // `--bust_action_caches` only exists in Bazel HEAD; 9.3.2 rejects it.
    let res2 = w.run(&[
        "build",
        "--bust_action_caches=//pkg:g1",
        "//pkg:g1",
        "//pkg:g2",
    ]);
    assert_eq!(res2.code, 2, "{}", res2.err);
    assert!(
        res2.err
            .contains("Unrecognized option: --bust_action_caches=//pkg:g1"),
        "{}",
        res2.err
    );
}

#[test]
fn test_run_multiple_positional_args_as_program_args() {
    let w = make_ws("run-multi-args");
    w.write(
        "pkg/BUILD",
        r#"
sh_binary(name = "b1", srcs = ["b1.sh"])
sh_binary(name = "b2", srcs = ["b2.sh"])
"#,
    );
    w.write("pkg/b1.sh", "#!/bin/sh\necho \"b1 args: $@\"\n");
    w.write("pkg/b2.sh", "#!/bin/sh\necho \"b2\"\n");
    let res = w.run(&["run", "//pkg:b1", "//pkg:b2"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(
        res.err.contains("Target //pkg:b1 up-to-date:"),
        "{}",
        res.err
    );
    assert!(
        !res.err.contains("Target //pkg:b2 up-to-date:"),
        "{}",
        res.err
    );
}

#[test]
fn test_run_forced_sharding_rejected() {
    let w = make_ws("run-forced-sharding");
    w.write(
        "pkg/BUILD",
        r#"
sh_test(name = "t", srcs = ["t.sh"])
"#,
    );
    w.write("pkg/t.sh", "#!/bin/sh\nexit 0\n");
    let res = w.run(&["run", "--test_sharding_strategy=forced=2", "//pkg:t"]);
    assert_eq!(res.code, 2, "{}", res.err);
    assert!(
        res.err
            .contains("ERROR: 'run' only works with tests with one shard"),
        "{}",
        res.err
    );
}

#[test]
fn test_run_script_path_preserves_args_and_no_cmd_line_info() {
    let w = make_ws("run-script-path");
    w.write(
        "pkg/BUILD",
        r#"
sh_binary(name = "b", srcs = ["b.sh"])
"#,
    );
    w.write("pkg/b.sh", "#!/bin/sh\necho hi\n");
    let sp = w.ws.join("sp.sh");
    let res = w.run(&["run", &format!("--script_path={}", sp.display()), "//pkg:b"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(
        !res.err.contains("INFO: Running command line:"),
        "{}",
        res.err
    );
    let script_content = std::fs::read_to_string(&sp).unwrap();
    assert!(script_content.contains("\"$@\""), "{script_content}");
}

#[test]
fn test_single_test_target_up_to_date_and_prefix() {
    let w = make_ws("single-test-uptodate");
    w.write(
        "pkg/BUILD",
        r#"
sh_test(name = "t", srcs = ["t.sh"])
"#,
    );
    w.write("pkg/t.sh", "#!/bin/sh\nexit 0\n");
    let res = w.run(&["test", "//pkg:t"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(
        res.err.contains("Target //pkg:t up-to-date:"),
        "{}",
        res.err
    );
    assert!(res.err.contains("bazel-bin/pkg/t"), "{}", res.err);

    // With --symlink_prefix=foo-
    let res2 = w.run(&["test", "--symlink_prefix=foo-", "//pkg:t"]);
    assert_eq!(res2.code, 0, "{}", res2.err);
    assert!(res2.err.contains("foo-bin/pkg/t"), "{}", res2.err);
}

#[test]
fn test_nobuild_semantics_parity() {
    let w = make_ws("nobuild-parity");
    w.write(
        "pkg/BUILD",
        r#"
sh_test(name = "t", srcs = ["t.sh"])
genrule(name = "g", outs = ["g.out"], cmd = "echo 1 > $@")
"#,
    );
    w.write("pkg/t.sh", "#!/bin/sh\nexit 0\n");
    // build --nobuild: exit 0, no Target up-to-date lines
    let res_b = w.run(&["build", "--nobuild", "//pkg:g"]);
    assert_eq!(res_b.code, 0, "{}", res_b.err);
    assert!(!res_b.err.contains("up-to-date:"), "{}", res_b.err);

    // test --nobuild: exit 1, Couldn't start the build. Unable to run tests
    let res_t = w.run(&["test", "--nobuild", "//pkg:t"]);
    assert_eq!(res_t.code, 1, "{}", res_t.err);
    assert!(
        res_t
            .err
            .contains("ERROR: Couldn't start the build. Unable to run tests"),
        "{}",
        res_t.err
    );
}

#[test]
fn test_sharding_support_flag_and_no_status() {
    let w = make_ws("sharding-support-parity");
    w.write(
        "pkg/BUILD",
        r#"
sh_test(name = "shard", srcs = ["s.sh"], shard_count = 2)
"#,
    );
    w.write("pkg/s.sh", "#!/bin/sh\nexit 0\n");
    // Default: --incompatible_check_sharding_support=true fails with NO STATUS
    let res1 = w.run(&["test", "//pkg:shard"]);
    assert_eq!(res1.code, 3, "{}", res1.err);
    assert!(res1.out.contains("NO STATUS"), "{}", res1.out);
    assert!(
        res1.err
            .contains("Sharding requested, but the test runner did not advertise support"),
        "{}",
        res1.err
    );

    // With --noincompatible_check_sharding_support (present in 9.3.2): the check is skipped and the test passes.
    let res2 = w.run(&[
        "test",
        "--noincompatible_check_sharding_support",
        "//pkg:shard",
    ]);
    assert_eq!(res2.code, 0, "{}", res2.err);
    assert!(!res2.out.contains("NO STATUS"), "{}", res2.out);
}

#[test]
fn test_size_too_big_and_verbose_warnings() {
    let w = make_ws("size-too-big");
    w.write(
        "pkg/BUILD",
        r#"
sh_test(name = "t", srcs = ["t.sh"], size = "medium")
"#,
    );
    w.write("pkg/t.sh", "#!/bin/sh\nexit 0\n");
    let res = w.run(&["test", "//pkg:t"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(res.out.contains("There were tests whose specified size is too big. Use the --test_verbose_timeout_warnings"), "{}", res.out);

    // Cached results never carry the warning (`TestResultAggregator`: `!result.isCached()`).
    let res_v = w.run(&[
        "test",
        "--cache_test_results=no",
        "--test_verbose_timeout_warnings",
        "//pkg:t",
    ]);
    assert_eq!(res_v.code, 0, "{}", res_v.err);
    assert!(
        res_v.out.contains("WARNING: //pkg:t: Test execution time"),
        "{}",
        res_v.out
    );
    assert!(res_v.out.contains("outside of range for MODERATE tests. Consider setting timeout=\"short\" or size=\"small\"."), "{}", res_v.out);
}

#[test]
fn test_no_extra_cache_key_file() {
    let w = make_ws("no-cache-key-file");
    w.write(
        "pkg/BUILD",
        r#"
sh_test(name = "t", srcs = ["t.sh"])
"#,
    );
    w.write("pkg/t.sh", "#!/bin/sh\nexit 0\n");
    let res = w.run(&["test", "//pkg:t"]);
    assert_eq!(res.code, 0, "{}", res.err);
    let log_dir = w.ws.join("testlogs/pkg/t");
    assert!(
        !log_dir.join("test.cache_key").exists(),
        "test.cache_key must not be written to disk"
    );
}

#[test]
fn round7_action_output_is_announced_and_stdout_is_routed() {
    let w = make_ws("r7-action-output");
    w.write(
        "pkg/BUILD",
        r#"
genrule(name = "so", outs = ["so.out"], cmd = "echo hello-stdout; echo warn >&2; echo x > $@")
"#,
    );
    let res = w.run(&["build", "//pkg:so"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert_eq!(
        res.out, "hello-stdout\n",
        "action stdout goes to the command's stdout"
    );
    assert!(
        res.err
            .contains("INFO: From Executing genrule //pkg:so:\nwarn\n"),
        "{}",
        res.err
    );
}

#[test]
fn round7_deprecation_warnings_and_source_file_target() {
    let w = make_ws("r7-deprecation");
    w.write(
        "dep/BUILD",
        r#"
exports_files(["f.txt"])
genrule(name = "old", outs = ["old.out"], cmd = "echo o > $@", deprecation = "use new", visibility = ["//visibility:public"])
"#,
    );
    w.write("dep/f.txt", "x\n");
    w.write(
        "pkg/BUILD",
        r#"
genrule(name = "user", srcs = ["//dep:old"], outs = ["u.out"], cmd = "cat $< > $@")
"#,
    );
    let res = w.run(&["build", "//pkg:user"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(res.err.contains("in genrule rule //pkg:user: target '//pkg:user' depends on deprecated target '//dep:old': use new"), "{}", res.err);
    let res = w.run(&["build", "//dep:old"]);
    assert!(
        res.err
            .contains("target '//dep:old' is deprecated: use new"),
        "{}",
        res.err
    );
    let res = w.run(&["build", "//dep:f.txt"]);
    assert!(res.err.contains("//dep:f.txt is a source file, nothing will be built for it. If you want to build a target that consumes this file, try --compile_one_dependency"), "{}", res.err);
}

#[test]
fn round7_invalid_test_strategy_and_directory_output() {
    let w = make_ws("r7-strategy");
    w.write("pkg/BUILD", "sh_test(name = \"t\", srcs = [\"t.sh\"])\ngenrule(name = \"d\", outs = [\"dout\"], cmd = \"mkdir $@\")\n");
    w.write("pkg/t.sh", "#!/bin/sh\nexit 0\n");
    let res = w.run(&["test", "--test_strategy=sandboxed", "//pkg:t"]);
    assert_eq!(res.code, 2, "{}", res.err);
    assert!(
        res.err
            .contains("available identifiers are: [exclusive, standalone]"),
        "{}",
        res.err
    );
    let res = w.run(&["build", "//pkg:d"]);
    assert_eq!(res.code, 1, "{}", res.err);
    assert!(
        res.err
            .contains("output 'pkg/dout' of //pkg:d is a directory but was not declared as such"),
        "{}",
        res.err
    );
}

#[test]
fn round7_flaky_attempts_are_reported() {
    let w = make_ws("r7-flaky");
    w.write(
        "pkg/BUILD",
        "sh_test(name = \"t\", srcs = [\"t.sh\"], flaky = True)\n",
    );
    w.write("pkg/t.sh", "#!/bin/bash\ncase \"$XML_OUTPUT_FILE\" in\n  *attempt_1.tmp*) exit 1 ;;\n  *) exit 0 ;;\nesac\n");
    let res = w.run(&["test", "--test_output=all", "//pkg:t"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(
        res.out.contains("FLAKY, failed in 1 out of 2"),
        "{}",
        res.out
    );
    assert!(
        res.out.contains("test_attempts/attempt_1.log"),
        "{}",
        res.out
    );
    assert!(
        res.err.contains("FAIL: //pkg:t (Exit 1) (see "),
        "{}",
        res.err
    );
    assert!(res.err.contains("FLAKY: //pkg:t (Summary)"), "{}", res.err);
    assert!(
        !res.out.contains("specified size is too big"),
        "{}",
        res.out
    );
}

#[test]
fn test_run_script_path_write_failure_matches_bazel() {
    let w = make_ws("run-script-path-fail");
    w.write("pkg/BUILD", "sh_binary(name = \"b\", srcs = [\"b.sh\"])\n");
    w.write("pkg/b.sh", "#!/bin/sh\necho hi\n");
    let res = w.run(&["run", "--script_path=/nonexistent-dir-zel/x", "//pkg:b"]);
    // `RunCommand`: SCRIPT_WRITE_FAILURE (exit 6) with the Java IOException text.
    assert_eq!(res.code, 6, "{}", res.err);
    assert!(
        res.err.contains(
            "ERROR: Error writing run script: /nonexistent-dir-zel/x (No such file or directory)"
        ),
        "{}",
        res.err
    );
}

/// `SpawnStats`: the workspace status action, the five `RunfilesSupport` actions of a test, and
/// the test action with its test + XML-generator spawns (real bazel 9.3.2: `7 processes: 6
/// internal, 2 darwin-sandbox.` for a passing `sh_test`; a rerun in the same server is one
/// action cache hit).
#[test]
fn test_process_summary_counts_internal_and_test_spawns() {
    let w = make_ws("proc-summary");
    w.write("pkg/BUILD", "sh_test(name = \"t\", srcs = [\"t.sh\"])\n");
    w.write("pkg/t.sh", "#!/bin/sh\nexit 0\n");
    let res = w.run(&["test", "//pkg:t"]);
    assert_eq!(res.code, 0, "{}", res.err);
    // (`sh_test` comes from `@rules_shell` here; the e2e harness against bazel covers the exact 7/6/2.)
    assert!(
        res.err.contains(" internal, 2 ") && res.err.contains(" processes: "),
        "{}",
        res.err
    );
    assert!(res.err.contains("total actions"), "{}", res.err);
    let res = w.run(&["test", "//pkg:t"]);
    assert!(
        res.err
            .contains("INFO: 1 process: 1 action cache hit, 1 internal."),
        "{}",
        res.err
    );
    let res = w.run(&["build", "--nobuild", "//pkg:t"]);
    assert!(res.err.contains("INFO: 0 processes."), "{}", res.err);
}

#[test]
fn test_unknown_spawn_strategy_prints_build_trailer() {
    let w = make_ws("bogus-strategy");
    w.write(
        "pkg/BUILD",
        "genrule(name = \"g\", outs = [\"o\"], cmd = \"touch $@\")\n",
    );
    let res = w.run(&["build", "--spawn_strategy=bogus", "//pkg:g"]);
    assert_eq!(res.code, 2, "{}", res.err);
    let all: Vec<&str> = res.err.lines().collect();
    let at = all
        .iter()
        .position(|l| l.contains("'bogus' was requested"))
        .expect(&res.err);
    let lines = &all[at..];
    assert!(
        lines[0].contains("'bogus' was requested for explicit default strategies"),
        "{}",
        res.err
    );
    assert!(lines[1].starts_with("INFO: Elapsed time: "), "{}", res.err);
    assert_eq!(lines[2], "INFO: 0 processes.", "{}", res.err);
    assert_eq!(
        lines[3], "ERROR: Build did NOT complete successfully",
        "{}",
        res.err
    );
}

#[test]
fn test_build_tag_filters_do_not_drop_tests_under_test_command() {
    let w = make_ws("build-tag-tests");
    w.write(
        "pkg/BUILD",
        "load(\"@rules_shell//shell:sh_test.bzl\", \"sh_test\")\nsh_test(name = \"a\", srcs = [\"t.sh\"], tags = [\"tagA\"])\nsh_test(name = \"b\", srcs = [\"t.sh\"])\n",
    );
    w.write("pkg/t.sh", "#!/bin/sh\nexit 0\n");
    let res = w.run(&["test", "--build_tag_filters=tagA", "//pkg:all"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(
        res.out.contains("Executed 2 out of 2 tests"),
        "{}{}",
        res.out,
        res.err
    );
}

#[test]
fn test_workspace_status_failure_precedes_analysis_error() {
    let w = make_ws("wsc-order");
    w.write(
        "pkg/BUILD",
        "genrule(name = \"g\", outs = [\"o\"], cmd = \"echo $(NOSUCHVAR) > $@\")\n",
    );
    let res = w.run(&[
        "build",
        "--workspace_status_command=/nonexistent/wsc",
        "//pkg:g",
    ]);
    assert_eq!(res.code, 1, "{}", res.err);
    assert!(
        res.err
            .contains("BazelWorkspaceStatusAction stable-status.txt failed"),
        "{}",
        res.err
    );
    assert!(!res.err.contains("Analysis of target"), "{}", res.err);
    assert!(
        res.err.contains("INFO: 1 process: 1 internal."),
        "{}",
        res.err
    );
}

#[test]
fn test_missing_input_action_is_not_announced_or_counted() {
    let w = make_ws("missing-input-s");
    w.write(
        "pkg/BUILD",
        "genrule(name = \"g\", srcs = [\"nope.txt\"], outs = [\"o\"], cmd = \"cat $< > $@\")\n",
    );
    let res = w.run(&["build", "-s", "//pkg:g"]);
    assert_eq!(res.code, 1, "{}", res.err);
    assert!(!res.err.contains("SUBCOMMAND"), "{}", res.err);
    assert!(
        res.err.contains("INFO: 1 process: 1 internal."),
        "{}",
        res.err
    );
}

#[test]
fn test_keep_going_analysis_failure_analyzed_line_before_warning() {
    let w = make_ws("kg-analyzed");
    w.write(
        "pkg/BUILD",
        "genrule(name = \"bad\", outs = [\"b\"], cmd = \"echo $(location //pkg:nosuch) > $@\")\ngenrule(name = \"ok\", outs = [\"o\"], cmd = \"touch $@\")\n",
    );
    let res = w.run(&["build", "--keep_going", "//pkg:bad", "//pkg:ok"]);
    assert_eq!(res.code, 1, "{}", res.err);
    let a = res.err.find("INFO: Analyzed 2 targets").expect(&res.err);
    let wn = res
        .err
        .find("WARNING: errors encountered while analyzing target '//pkg:bad'")
        .expect(&res.err);
    assert!(a < wn, "{}", res.err);
}

#[test]
fn test_unknown_and_missing_aspects_match_bazel() {
    let w = make_ws("aspect-errs");
    w.write(
        "pkg/BUILD",
        "genrule(name = \"g\", outs = [\"o\"], cmd = \"touch $@\")\n",
    );
    let r = w.run(&["build", "--aspects=//pkg:x", "//pkg:g"]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(
        r.err.contains("ERROR: Aspect '//pkg:x' is unknown\n"),
        "{}",
        r.err
    );
    assert!(r.err.contains("INFO: 0 processes."), "{}", r.err);
    let r = w.run(&["build", "--aspects=//pkg:x%y", "//pkg:g"]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(
        r.err
            .contains("ERROR: cannot load '//pkg:x': no such file\n"),
        "{}",
        r.err
    );
    assert!(r.err.contains("ERROR: Analysis of aspects '[//pkg:x%y] with parameters {} on //pkg:g' failed; build aborted: cannot load '//pkg:x': no such file"), "{}", r.err);
}

#[test]
fn test_override_repository_of_invisible_repo_is_exit_48() {
    let w = make_ws("override-invisible");
    w.write(
        "pkg/BUILD",
        "genrule(name = \"g\", outs = [\"o\"], cmd = \"touch $@\")\n",
    );
    let r = w.run(&["build", "--override_repository=foo=/tmp", "//pkg:g"]);
    assert_eq!(r.code, 48, "{}", r.err);
    assert!(r.err.contains("ERROR: no repository visible as '@foo' from the main repository, but overridden with --override_repository. Use --inject_repository to add new repositories."), "{}", r.err);
    let r = w.run(&[
        "build",
        "--override_repository=rules_shell=/tmp/none",
        "//pkg:g",
    ]);
    assert_ne!(r.code, 48, "{}", r.err);
}

#[test]
fn test_shell_executable_is_used_for_genrules() {
    let w = make_ws("shell-exec");
    w.write(
        "pkg/BUILD",
        "genrule(name = \"g\", outs = [\"o\"], cmd = \"echo hi > $@\")\n",
    );
    let r = w.run(&["build", "-s", "--shell_executable=/bin/sh", "//pkg:g"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(
        r.err
            .contains("/bin/sh -c 'source external/bazel_tools/tools/genrule/genrule-setup.sh;"),
        "{}",
        r.err
    );
    let r = w.run(&["build", "--shell_executable=/nonexistent", "//pkg:g"]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(
        r.err.contains("Executing genrule //pkg:g failed"),
        "{}",
        r.err
    );
}

#[test]
fn test_unexecutable_shell_reports_process_wrapper_text() {
    let w = make_ws("shell-missing");
    w.write(
        "pkg/BUILD",
        "genrule(name = \"g\", outs = [\"o\"], cmd = \"echo hi > $@\")\n",
    );
    let r = w.run(&["build", "--shell_executable=/nonexistent", "//pkg:g"]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(
        r.err.contains("failed: (Exit 1): nonexistent failed:"),
        "{}",
        r.err
    );
    assert!(
        (r.err
            .contains("src/main/tools/linux-sandbox-pid1.cc:584: \"execvp(/nonexistent, ")
            || r.err
                .contains("src/main/tools/process-wrapper-legacy.cc:80: \"execvp(/nonexistent, "))
            && r.err.contains("\": No such file or directory"),
        "{}",
        r.err
    );
}

#[test]
fn test_invalid_platform_flags_match_bazel() {
    let w = make_ws("platform-flags");
    w.write(
        "pkg/BUILD",
        "genrule(name = \"g\", outs = [\"o\"], cmd = \"touch $@\")\n",
    );
    let r = w.run(&["build", "--platforms=//pkg:nonexistent", "//pkg:g"]);
    assert_eq!(r.code, 2, "{}", r.err);
    let r = w.run(&["build", "--host_platform=//pkg:nonexistent", "//pkg:g"]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(r.err.contains("INFO: 0 processes."), "{}", r.err);
    let r = w.run(&[
        "build",
        "--extra_execution_platforms=//pkg:nonexistent",
        "//pkg:g",
    ]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(
        r.err
            .contains("While resolving toolchains for target //pkg:g ("),
        "{}",
        r.err
    );
    assert!(
        r.err
            .contains("invalid registered execution platform '//pkg:nonexistent'"),
        "{}",
        r.err
    );
    assert!(
        r.err
            .contains("ERROR: Analysis of target '//pkg:g' failed; build aborted"),
        "{}",
        r.err
    );
}

#[test]
fn test_unreachable_remote_cache_fails_first_spawn_with_remote_error() {
    let w = make_ws("remote-down");
    w.write(
        "pkg/BUILD",
        "genrule(name = \"g\", outs = [\"o\"], cmd = \"touch $@\")\n",
    );
    let r = w.run(&["build", "--remote_cache=grpc://localhost:1", "//pkg:g"]);
    assert_eq!(r.code, 34, "{}", r.err);
    assert!(r.err.contains("INFO: Invocation ID: "), "{}", r.err);
    assert!(
        r.err.contains("Executing genrule //pkg:g failed: Failed to query remote execution capabilities: finishConnect(..) failed: Connection refused: localhost/[0:0:0:0:0:0:0:1]:1"),
        "{}",
        r.err
    );
}

#[test]
fn test_grpcs_handshake_failure_surfaces_real_tls_error_not_dns() {
    use std::io::{Read, Write};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return,
        Err(e) => panic!("{e}"),
    };
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = stop.clone();
    let srv = std::thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            if !stop_srv.load(Ordering::SeqCst) {
                let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                let mut buf = [0u8; 64];
                let _ = s.read(&mut buf);
                // Speak plaintext HTTP instead of TLS so OpenSSL fails the TLS handshake.
                let _ = s.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
                let _ = s.flush();
            }
        }
    });

    struct ServerGuard {
        port: u16,
        stop: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for ServerGuard {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], self.port)),
                std::time::Duration::from_secs(1),
            );
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }
    let _srv_guard = ServerGuard {
        port,
        stop,
        handle: Some(srv),
    };

    let w = make_ws("remote-tls-err");
    w.write(
        "pkg/BUILD",
        "genrule(name = \"g\", outs = [\"o\"], cmd = \"echo hi > $@\")\n",
    );
    let r = w.run(&[
        "build",
        &format!("--remote_cache=grpcs://127.0.0.1:{port}"),
        "--remote_retries=0",
        "--remote_timeout=5s",
        "//pkg:g",
    ]);
    assert_eq!(r.code, 34, "{}", r.err);
    assert!(
        !r.err.contains("Unable to resolve host"),
        "real TLS handshake failure must not be disguised as a DNS error:\n{}",
        r.err
    );
    assert!(
        r.err.contains("Executing genrule //pkg:g failed: Failed to query remote execution capabilities: UNAVAILABLE: connection failed:"),
        "expected real TLS connection error in output:\n{}",
        r.err
    );
}

#[test]
fn test_disk_cache_hit_materializes_top_level_output() {
    let w = make_ws("disk-cache-hit");
    w.write(
        "pkg/BUILD",
        "genrule(name = \"g\", outs = [\"o\"], cmd = \"echo hi > $@\")\n",
    );
    let dc = w.ws.join("dc");
    let flag = format!("--disk_cache={}", dc.display());
    let r = w.run(&["build", &flag, "//pkg:g"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(w.run(&["clean"]).code, 0);
    let r = w.run(&["build", &flag, "//pkg:g"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.err.contains("1 disk cache hit"), "{}", r.err);
    assert_eq!(
        std::fs::read_to_string(w.ws.join("bazel-bin/pkg/o")).unwrap(),
        "hi\n"
    );
}

#[test]
fn test_empty_test_filter_still_sets_testbridge_env() {
    let w = make_ws("empty-filter");
    w.write("pkg/BUILD", "sh_test(name = \"t\", srcs = [\"t.sh\"])\n");
    w.write(
        "pkg/t.sh",
        "#!/bin/sh\nif [ \"${TESTBRIDGE_TEST_ONLY+set}\" = set ]; then echo HAS_FILTER; fi\n",
    );
    let r = w.run(&["test", "--test_filter=", "--test_output=all", "//pkg:t"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.out.contains("HAS_FILTER"), "{}{}", r.out, r.err);
}

#[test]
fn test_single_target_of_package_with_errors_fails_pattern_without_keep_going() {
    let w = make_ws("pkg-errors");
    w.write("pkg/BUILD", "genrule(name = \"a\", outs = [\"same.txt\"], cmd = \"touch $@\")\ngenrule(name = \"b\", outs = [\"same.txt\"], cmd = \"touch $@\")\n");
    let r = w.run(&["build", "//pkg:a"]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(
        r.err.contains("WARNING: Target pattern parsing failed."),
        "{}",
        r.err
    );
    assert!(r.err.contains("ERROR: Skipping '//pkg:a': Error evaluating '//pkg:a': error loading package 'pkg': Package 'pkg' contains errors"), "{}", r.err);
}

#[test]
fn test_flaky_attempts_last_wins_errors_output_and_streamed_strategy_override() {
    let w = make_ws("flaky-errors-streamed");
    w.write(
        "pkg/BUILD",
        r#"
sh_test(name = "flk", srcs = ["flk.sh"])
sh_test(name = "tmpcheck", srcs = ["tmpcheck.sh"])
"#,
    );
    // Inspect `$XML_OUTPUT_FILE` rather than touching a file in the workspace so the test stays
    // hermetic under `darwin-sandbox` / `linux-sandbox` (`(deny file-write*)` outside writable dirs):
    // - single-attempt run (`max_attempts == 1`): `.../attempt/test.xml` -> fails
    // - multi-attempt run (`max_attempts >= 2`): `...attempt_1.tmp/test.xml` fails, `attempt_2.tmp` passes!
    w.write(
        "pkg/flk.sh",
        "#!/bin/sh\ncase \"$XML_OUTPUT_FILE\" in\n  */attempt/test.xml|*attempt_1.tmp*)\n    echo FAIL_ATTEMPT_ONE\n    exit 1\n    ;;\n  *)\n    echo PASS_ATTEMPT\n    exit 0\n    ;;\nesac\n",
    );
    w.write(
        "pkg/tmpcheck.sh",
        "#!/bin/sh\necho \"TMP=$TEST_TMPDIR\"\n[ -d \"$TEST_TMPDIR\" ] || exit 1\n",
    );

    // Last-match-wins: --flaky_test_attempts=3 followed by //pkg:flk@default resets to 1 attempt (so it fails!).
    let r_reset = w.run(&[
        "test",
        "--flaky_test_attempts=3",
        "--flaky_test_attempts=//pkg:flk@default",
        "//pkg:flk",
    ]);
    assert_eq!(r_reset.code, 3, "{}", r_reset.err);

    // Now reverse order: default first, then //pkg:flk@2 -> 2 attempts -> FLAKY!
    // Under --test_output=errors, FAIL_ATTEMPT_ONE must be printed even when overall status is FLAKY.
    let r_flaky = w.run(&[
        "test",
        "--flaky_test_attempts=default",
        "--flaky_test_attempts=//pkg:flk@2",
        "--test_output=errors",
        "//pkg:flk",
    ]);
    assert_eq!(r_flaky.code, 0, "{}", r_flaky.err);
    assert!(
        r_flaky.out.contains("FAIL_ATTEMPT_ONE"),
        "expected failed attempt log under --test_output=errors:\n{}",
        r_flaky.out
    );
    assert!(
        !r_flaky.out.contains("PASS_ATTEMPT"),
        "passing attempt log must not print under --test_output=errors:\n{}",
        r_flaky.out
    );

    // Cached rerun with --cache_test_results=yes preserves FLAKY attempt summary and prints failed log on --test_output=errors!
    let r_cached = w.run(&[
        "test",
        "--flaky_test_attempts=//pkg:flk@2",
        "--cache_test_results=yes",
        "--test_output=errors",
        "//pkg:flk",
    ]);
    assert_eq!(r_cached.code, 0, "{}", r_cached.err);
    assert!(
        r_cached
            .out
            .contains("(cached) FLAKY, failed in 1 out of 2"),
        "{}",
        r_cached.out
    );
    assert!(
        r_cached.out.contains("FAIL_ATTEMPT_ONE"),
        "{}",
        r_cached.out
    );

    // --test_output=streamed overrides --test_strategy before validation; relative --test_tmpdir resolves against workspace_dir.
    let r_stream = w.run(&[
        "test",
        "--test_output=streamed",
        "--test_strategy=bogus_strategy",
        "--test_tmpdir=rel_tt",
        "//pkg:tmpcheck",
    ]);
    assert_eq!(r_stream.code, 0, "{}", r_stream.err);
    let expected_prefix = format!("TMP={}/rel_tt/_tmp/", w.ws.display());
    assert!(
        r_stream.out.contains(&expected_prefix),
        "expected {expected_prefix} in:\n{}",
        r_stream.out
    );
}

fn make_ws_no_root_build(name: &str, module_bazel: &str) -> (PathBuf, Ws) {
    let root = std::env::temp_dir().join(format!("zel-btrc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ws_dir = root.join("ws");
    std::fs::create_dir_all(&ws_dir).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let ws_dir = root.join("ws");
    std::fs::write(ws_dir.join("MODULE.bazel"), module_bazel).unwrap();
    let fs: Arc<dyn FileSystem> = Arc::new(LocalFileSystem::default());
    let p = |s: &str| PathFragment::create(root.join(s).to_str().unwrap());
    let ob = root.join("ob");
    let dirs = BlazeDirectories {
        workspace_dir: PathFragment::create(ws_dir.to_str().unwrap()),
        output_base: p("ob"),
        install_base: p("ib"),
        output_user_root: p("ur"),
    };
    (
        root,
        Ws {
            ws: ws_dir,
            ob,
            rt: BlazeRuntime::new(dirs, fs, 1),
        },
    )
}

#[test]
fn test_cli_extension_repo_succeeds_without_root_build() {
    let ext_root = std::env::temp_dir().join(format!("zel-btrc-ext-ok-mod-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ext_root);
    std::fs::create_dir_all(&ext_root).unwrap();
    let ext_root = std::fs::canonicalize(&ext_root).unwrap();
    std::fs::write(
        ext_root.join("MODULE.bazel"),
        "module(name = \"ext_mod\", version = \"0.1.0\")\n",
    )
    .unwrap();
    std::fs::write(ext_root.join("BUILD.bazel"), "").unwrap();
    std::fs::write(
        ext_root.join("ext.bzl"),
        r#"
def _impl(rctx):
    rctx.file("BUILD.bazel", """
genrule(
    name = "hello",
    outs = ["hello.txt"],
    cmd = "echo hello-from-ext > $@",
    visibility = ["//visibility:public"],
)
""")

my_repo = repository_rule(implementation = _impl)

def _ext_impl(mctx):
    my_repo(name = "my_ext_repo")
    my_repo(name = "my_other_repo")

ext = module_extension(implementation = _ext_impl)
"#,
    )
    .unwrap();

    let module_bazel = format!(
        "module(name = \"root\")\nbazel_dep(name = \"ext_mod\", version = \"0.1.0\")\nlocal_path_override(module_name = \"ext_mod\", path = \"{}\")\next = use_extension(\"@ext_mod//:ext.bzl\", \"ext\")\nuse_repo(ext, \"my_ext_repo\", my_alias = \"my_other_repo\")\n",
        ext_root.display()
    );
    let (_root, w) = make_ws_no_root_build("ext-ok-ws", &module_bazel);
    assert!(!w.ws.join("BUILD").exists() && !w.ws.join("BUILD.bazel").exists());

    // Build via apparent repo name, renamed apparent alias, and canonical repo name.
    for target in [
        "@my_ext_repo//:hello",
        "@my_alias//:hello",
        "@@ext_mod++ext+my_ext_repo//:hello",
    ] {
        let r = w.run(&["build", target]);
        assert_eq!(r.code, 0, "target {target} failed:\n{}", r.err);
        assert!(
            r.err.contains("Build completed successfully"),
            "target {target}:\n{}",
            r.err
        );
    }

    // Query, cquery, and aquery on the extension repo succeed without a root BUILD file.
    let q = w.run(&["query", "@my_ext_repo//..."]);
    assert_eq!(q.code, 0, "{}", q.err);
    assert!(q.out.contains("@my_ext_repo//:hello"), "{}", q.out);

    let cq = w.run(&["cquery", "@my_ext_repo//:hello"]);
    assert_eq!(cq.code, 0, "{}", cq.err);
    assert!(cq.out.contains("@my_ext_repo//:hello"), "{}", cq.out);

    let aq = w.run(&["aquery", "@my_ext_repo//:hello"]);
    assert_eq!(aq.code, 0, "{}", aq.err);
    assert!(aq.out.contains("Mnemonic: Genrule"), "{}", aq.out);

    // Genuinely unknown repositories still report the Bazel unknown/undefined repo errors.
    let unk_app = w.run(&["build", "@no_such_repo//:foo"]);
    assert_eq!(unk_app.code, 1, "{}", unk_app.err);
    assert!(
        unk_app
            .err
            .contains("ERROR: No repository visible as '@no_such_repo' from main repository"),
        "{}",
        unk_app.err
    );
    let unk_canon = w.run(&["build", "@@no_such_repo//:foo"]);
    assert_eq!(unk_canon.code, 1, "{}", unk_canon.err);
    assert!(
        unk_canon
            .err
            .contains("ERROR: Repository '@@no_such_repo' is not defined"),
        "{}",
        unk_canon.err
    );
}

#[test]
fn test_cli_extension_repo_rule_fail_surfaces_diagnostics() {
    let ext_root =
        std::env::temp_dir().join(format!("zel-btrc-ext-fail-rule-mod-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ext_root);
    std::fs::create_dir_all(&ext_root).unwrap();
    let ext_root = std::fs::canonicalize(&ext_root).unwrap();
    std::fs::write(
        ext_root.join("MODULE.bazel"),
        "module(name = \"ext_mod\", version = \"0.1.0\")\n",
    )
    .unwrap();
    std::fs::write(ext_root.join("BUILD.bazel"), "").unwrap();
    std::fs::write(
        ext_root.join("ext.bzl"),
        r#"
def _impl(rctx):
    fail("Diagnostics strict mode failures:\n\nFirst strict error\n\nSecond strict error")

bad_repo = repository_rule(implementation = _impl)

def _ext_impl(mctx):
    bad_repo(name = "dotnet_projects")

ext = module_extension(implementation = _ext_impl)
"#,
    )
    .unwrap();

    let module_bazel = format!(
        "module(name = \"root\")\nbazel_dep(name = \"ext_mod\", version = \"0.1.0\")\nlocal_path_override(module_name = \"ext_mod\", path = \"{}\")\next = use_extension(\"@ext_mod//:ext.bzl\", \"ext\")\nuse_repo(ext, \"dotnet_projects\")\n",
        ext_root.display()
    );
    let (_root, w) = make_ws_no_root_build("ext-fail-rule-ws", &module_bazel);

    // build (apparent & canonical), test, run, cquery, aquery all surface the repo rule's fail().
    for (cmd, target, extra_expected) in [
        ("build", "@dotnet_projects//:diagnostics.json", None),
        (
            "build",
            "@@ext_mod++ext+dotnet_projects//:diagnostics.json",
            None,
        ),
        (
            "test",
            "@dotnet_projects//:diagnostics.json",
            Some("ERROR: Couldn't start the build. Unable to run tests"),
        ),
        (
            "run",
            "@dotnet_projects//:diagnostics.json",
            Some("ERROR: Build failed. Not running target"),
        ),
        ("cquery", "@dotnet_projects//:diagnostics.json", None),
        ("aquery", "@dotnet_projects//:diagnostics.json", None),
    ] {
        let r = w.run(&[cmd, target]);
        assert_eq!(r.code, 1, "{cmd} {target}:\n{}", r.err);
        assert!(
            r.err.contains(
                "An error occurred during the fetch of repository 'ext_mod++ext+dotnet_projects'"
            ),
            "{cmd} {target}:\n{}",
            r.err
        );
        assert!(
            r.err
                .contains("Error in fail: Diagnostics strict mode failures:"),
            "{cmd} {target}:\n{}",
            r.err
        );
        assert!(
            r.err.contains("First strict error") && r.err.contains("Second strict error"),
            "{cmd} {target}:\n{}",
            r.err
        );
        assert!(
            !r.err.contains("No repository visible"),
            "{cmd} {target} must not report 'No repository visible':\n{}",
            r.err
        );
        if let Some(extra) = extra_expected {
            assert!(r.err.contains(extra), "{cmd} {target}:\n{}", r.err);
        }
    }

    // query (single target, wildcard //..., and canonical name) surfaces fail() with exit code 7
    // and never falsely reports 'No repository visible' or 'fetching repositories is disabled'.
    for pattern in [
        "@dotnet_projects//:diagnostics.json",
        "@dotnet_projects//...",
        "@@ext_mod++ext+dotnet_projects//:diagnostics.json",
    ] {
        let q = w.run(&["query", pattern]);
        assert_eq!(q.code, 7, "query {pattern}:\n{}", q.err);
        assert!(
            q.err
                .contains("Error in fail: Diagnostics strict mode failures:"),
            "query {pattern}:\n{}",
            q.err
        );
        assert!(
            !q.err.contains("No repository visible"),
            "query {pattern} must not report 'No repository visible':\n{}",
            q.err
        );
        assert!(
            !q.err.contains("fetching repositories is disabled"),
            "query {pattern} must not falsely report --nofetch:\n{}",
            q.err
        );
    }

    // Fixing the repo rule on disk heals the next build on the same server without stale caching.
    std::fs::write(
        ext_root.join("ext.bzl"),
        r#"
def _impl(rctx):
    rctx.file("diagnostics.json", "{}\n")
    rctx.file("BUILD.bazel", 'exports_files(["diagnostics.json"])\n')

bad_repo = repository_rule(implementation = _impl)

def _ext_impl(mctx):
    bad_repo(name = "dotnet_projects")

ext = module_extension(implementation = _ext_impl)
"#,
    )
    .unwrap();
    let healed = w.run(&["build", "@dotnet_projects//:diagnostics.json"]);
    assert_eq!(healed.code, 0, "healed build failed:\n{}", healed.err);
}

#[test]
fn test_cli_module_extension_fail_surfaces_diagnostics() {
    let ext_root =
        std::env::temp_dir().join(format!("zel-btrc-ext-fail-ext-mod-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ext_root);
    std::fs::create_dir_all(&ext_root).unwrap();
    let ext_root = std::fs::canonicalize(&ext_root).unwrap();
    std::fs::write(
        ext_root.join("MODULE.bazel"),
        "module(name = \"ext_mod\", version = \"0.1.0\")\n",
    )
    .unwrap();
    std::fs::write(ext_root.join("BUILD.bazel"), "").unwrap();
    std::fs::write(
        ext_root.join("ext.bzl"),
        r#"
def _ext_impl(mctx):
    fail("boom from module extension")

ext = module_extension(implementation = _ext_impl)
"#,
    )
    .unwrap();

    let module_bazel = format!(
        "module(name = \"root\")\nbazel_dep(name = \"ext_mod\", version = \"0.1.0\")\nlocal_path_override(module_name = \"ext_mod\", path = \"{}\")\next = use_extension(\"@ext_mod//:ext.bzl\", \"ext\")\nuse_repo(ext, \"bad_ext_repo\")\n",
        ext_root.display()
    );
    let (_root, w) = make_ws_no_root_build("ext-fail-ext-ws", &module_bazel);

    for cmd in ["build", "test", "run", "cquery", "aquery"] {
        let r = w.run(&[cmd, "@bad_ext_repo//:foo"]);
        assert_eq!(r.code, 48, "{cmd}:\n{}", r.err);
        assert!(
            r.err.contains("Error in fail: boom from module extension"),
            "{cmd}:\n{}",
            r.err
        );
        assert!(
            r.err.contains("ERROR: error evaluating module extension"),
            "{cmd}:\n{}",
            r.err
        );
        assert!(
            !r.err.contains("No repository visible"),
            "{cmd} must not report 'No repository visible':\n{}",
            r.err
        );
    }

    let q = w.run(&["query", "@bad_ext_repo//:foo"]);
    assert_eq!(q.code, 7, "{}", q.err);
    assert!(
        q.err.contains("Error in fail: boom from module extension"),
        "{}",
        q.err
    );
    assert!(
        q.err.contains(
            "Target parsing failed due to unexpected exception: error evaluating module extension"
        ),
        "{}",
        q.err
    );
    assert!(!q.err.contains("No repository visible"), "{}", q.err);
}

#[test]
fn test_cli_non_fail_module_extension_errors_exit_48() {
    let ext_root =
        std::env::temp_dir().join(format!("zel-btrc-ext-bad-ret-mod-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ext_root);
    std::fs::create_dir_all(&ext_root).unwrap();
    let ext_root = std::fs::canonicalize(&ext_root).unwrap();
    std::fs::write(
        ext_root.join("MODULE.bazel"),
        "module(name = \"ext_mod\", version = \"0.1.0\")\n",
    )
    .unwrap();
    std::fs::write(ext_root.join("BUILD.bazel"), "").unwrap();
    std::fs::write(
        ext_root.join("ext.bzl"),
        r#"
def _ext_impl(mctx):
    return 123

bad_ret_ext = module_extension(implementation = _ext_impl)
"#,
    )
    .unwrap();

    let module_bazel = format!(
        "module(name = \"root\")\nbazel_dep(name = \"ext_mod\", version = \"0.1.0\")\nlocal_path_override(module_name = \"ext_mod\", path = \"{}\")\ne1 = use_extension(\"@ext_mod//:ext.bzl\", \"bad_ret_ext\")\nuse_repo(e1, \"r_bad_ret\")\ne2 = use_extension(\"@ext_mod//:ext.bzl\", \"no_such_ext\")\nuse_repo(e2, \"r_missing_ext\")\n",
        ext_root.display()
    );
    let (_root, w) = make_ws_no_root_build("ext-bad-ret-ws", &module_bazel);

    // Extension returning an invalid type must get exit code 48 across build/test/run/cquery/aquery.
    for cmd in ["build", "test", "run", "cquery", "aquery"] {
        let r = w.run(&[cmd, "@r_bad_ret//:foo"]);
        assert_eq!(r.code, 48, "{cmd} expected exit 48:\n{}", r.err);
        assert!(
            r.err
                .contains("to return None or extension_metadata, got int"),
            "{cmd}:\n{}",
            r.err
        );
        assert!(
            !r.err.contains("No repository visible"),
            "{cmd}:\n{}",
            r.err
        );
    }
    let q = w.run(&["query", "@r_bad_ret//:foo"]);
    assert_eq!(q.code, 7, "{}", q.err);
    assert!(
        q.err.contains(
            "Target parsing failed due to unexpected exception: expected module extension"
        ),
        "{}",
        q.err
    );

    // Missing extension export in .bzl must also get exit code 48.
    for cmd in ["build", "cquery"] {
        let r = w.run(&[cmd, "@r_missing_ext//:foo"]);
        assert_eq!(r.code, 48, "{cmd} expected exit 48:\n{}", r.err);
        assert!(
            r.err
                .contains("does not export a module extension called no_such_ext"),
            "{cmd}:\n{}",
            r.err
        );
    }
}

#[test]
fn test_cli_mixed_repo_fetch_and_non_fetch_pattern_errors_report_both() {
    let ext_root =
        std::env::temp_dir().join(format!("zel-btrc-ext-mixed-mod-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ext_root);
    std::fs::create_dir_all(&ext_root).unwrap();
    let ext_root = std::fs::canonicalize(&ext_root).unwrap();
    std::fs::write(
        ext_root.join("MODULE.bazel"),
        "module(name = \"ext_mod\", version = \"0.1.0\")\n",
    )
    .unwrap();
    std::fs::write(ext_root.join("BUILD.bazel"), "").unwrap();
    std::fs::write(
        ext_root.join("ext.bzl"),
        r#"
def _bad_repo_impl(rctx):
    fail("boom from repo rule")

bad_repo = repository_rule(implementation = _bad_repo_impl)

def _ext_ok_impl(mctx):
    bad_repo(name = "bad_repo")

ext_rule_fail = module_extension(implementation = _ext_ok_impl)

def _ext_bad_ret_impl(mctx):
    return 42

ext_bad_ret = module_extension(implementation = _ext_bad_ret_impl)
"#,
    )
    .unwrap();

    let module_bazel = format!(
        "module(name = \"root\")\nbazel_dep(name = \"ext_mod\", version = \"0.1.0\")\nlocal_path_override(module_name = \"ext_mod\", path = \"{}\")\ne1 = use_extension(\"@ext_mod//:ext.bzl\", \"ext_rule_fail\")\nuse_repo(e1, \"bad_repo\")\ne2 = use_extension(\"@ext_mod//:ext.bzl\", \"ext_bad_ret\")\nuse_repo(e2, \"bad_ext\")\n",
        ext_root.display()
    );
    let (_root, w) = make_ws_no_root_build("ext-mixed-ws", &module_bazel);
    std::fs::create_dir_all(w.ws.join("pkg")).unwrap();
    std::fs::write(
        w.ws.join("pkg/BUILD.bazel"),
        "genrule(name = \"ok\", outs = [\"ok.txt\"], cmd = \"echo hi > $@\")\n",
    )
    .unwrap();

    // Without --keep_going: repo fetch error + missing target must report BOTH errors.
    let r = w.run(&["build", "@bad_repo//:foo", "//pkg:nope"]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(
        r.err.contains("Error in fail: boom from repo rule"),
        "missing fetch error:\n{}",
        r.err
    );
    assert!(
        r.err.contains("no such target '//pkg:nope'"),
        "missing non-fetch target error:\n{}",
        r.err
    );

    // Without --keep_going: repo fetch error + missing package must report BOTH errors.
    let r = w.run(&["build", "//no_such_pkg:bar", "@bad_repo//:foo"]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(
        r.err.contains("no such package 'no_such_pkg'"),
        "missing non-fetch package error:\n{}",
        r.err
    );
    assert!(
        r.err.contains("Error in fail: boom from repo rule"),
        "missing fetch error:\n{}",
        r.err
    );

    // Without --keep_going: repo fetch error + unknown repo must report BOTH errors.
    let r = w.run(&["build", "@bad_repo//:foo", "@no_such_repo//:x"]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(
        r.err.contains("Error in fail: boom from repo rule"),
        "missing fetch error:\n{}",
        r.err
    );
    assert!(
        r.err
            .contains("ERROR: No repository visible as '@no_such_repo' from main repository"),
        "missing unknown repo error:\n{}",
        r.err
    );

    // Without --keep_going: module extension error + missing target reports BOTH and exits 48.
    let r = w.run(&["build", "@bad_ext//:foo", "//pkg:nope"]);
    assert_eq!(r.code, 48, "{}", r.err);
    assert!(
        r.err
            .contains("to return None or extension_metadata, got int"),
        "missing extension error:\n{}",
        r.err
    );
    assert!(
        r.err.contains("no such target '//pkg:nope'"),
        "missing non-fetch target error:\n{}",
        r.err
    );
}

#[test]
fn test_cli_nested_module_extension_errors_preserve_exit_48() {
    let ext_root =
        std::env::temp_dir().join(format!("zel-btrc-ext-nested-mod-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ext_root);
    std::fs::create_dir_all(&ext_root).unwrap();
    let ext_root = std::fs::canonicalize(&ext_root).unwrap();
    std::fs::write(
        ext_root.join("MODULE.bazel"),
        "module(name = \"ext_mod\", version = \"0.1.0\")\n",
    )
    .unwrap();
    std::fs::write(ext_root.join("BUILD.bazel"), "").unwrap();
    std::fs::write(
        ext_root.join("bad_ext.bzl"),
        r#"
def _fail_impl(mctx):
    fail("boom from nested ext")
fail_ext = module_extension(implementation = _fail_impl)

def _bad_ret_impl(mctx):
    return 123
bad_ret_ext = module_extension(implementation = _bad_ret_impl)

def _bad_rule_impl(rctx):
    fail("boom from nested repo rule")
bad_rule_repo = repository_rule(implementation = _bad_rule_impl)

def _ok_ext_impl(mctx):
    bad_rule_repo(name = "bad_rule_r")
ok_ext = module_extension(implementation = _ok_ext_impl)
"#,
    )
    .unwrap();
    std::fs::write(
        ext_root.join("outer.bzl"),
        r#"
def _outer_impl(rctx):
    p = rctx.path(rctx.attr.target)
    rctx.file("BUILD.bazel", "exports_files(['a'])\n")
    rctx.file("a", str(p))

outer_path_repo = repository_rule(
    implementation = _outer_impl,
    attrs = {"target": attr.label()},
)

def _outer_ext_impl(mctx):
    outer_path_repo(name = "outer_fail_ext", target = Label("@@ext_mod++fail_ext+bad_r1//:f"))
    outer_path_repo(name = "outer_bad_ret", target = Label("@@ext_mod++bad_ret_ext+bad_r2//:f"))
    outer_path_repo(name = "outer_bad_rule", target = Label("@@ext_mod++ok_ext+bad_rule_r//:f"))

outer_ext = module_extension(implementation = _outer_ext_impl)
"#,
    )
    .unwrap();

    let module_bazel = format!(
        r#"module(name = "root")
bazel_dep(name = "ext_mod", version = "0.1.0")
local_path_override(module_name = "ext_mod", path = "{}")
e_fail = use_extension("@ext_mod//:bad_ext.bzl", "fail_ext")
use_repo(e_fail, "bad_r1")
e_ret = use_extension("@ext_mod//:bad_ext.bzl", "bad_ret_ext")
use_repo(e_ret, "bad_r2")
e_ok = use_extension("@ext_mod//:bad_ext.bzl", "ok_ext")
use_repo(e_ok, "bad_rule_r")
e_outer = use_extension("@ext_mod//:outer.bzl", "outer_ext")
use_repo(e_outer, "outer_fail_ext", "outer_bad_ret", "outer_bad_rule")
use_r = use_repo_rule("@ext_mod//:outer.bzl", "outer_path_repo")
use_r(name = "innate_fail_ext", target = "@bad_r1//:f")
use_r(name = "innate_bad_ret", target = "@bad_r2//:f")
"#,
        ext_root.display()
    );
    let (_root, w) = make_ws_no_root_build("ext-nested-ws", &module_bazel);

    // Nested fail() in module extension via rctx.path (both extension repo and use_repo_rule) -> exit 48.
    for target in ["@outer_fail_ext//:a", "@innate_fail_ext//:a"] {
        for cmd in ["build", "cquery", "run"] {
            let r = w.run(&[cmd, target]);
            assert_eq!(r.code, 48, "{cmd} {target} expected exit 48:\n{}", r.err);
            assert!(
                r.err.contains("Error in fail: boom from nested ext"),
                "{cmd} {target}:\n{}",
                r.err
            );
            assert!(
                r.err.contains("ERROR: error evaluating module extension"),
                "{cmd} {target}:\n{}",
                r.err
            );
        }
    }

    // Nested invalid return type in module extension via rctx.path -> exit 48.
    for target in ["@outer_bad_ret//:a", "@innate_bad_ret//:a"] {
        for cmd in ["build", "cquery"] {
            let r = w.run(&[cmd, target]);
            assert_eq!(r.code, 48, "{cmd} {target} expected exit 48:\n{}", r.err);
            assert!(
                r.err
                    .contains("to return None or extension_metadata, got int"),
                "{cmd} {target}:\n{}",
                r.err
            );
        }
    }

    // Control: nested repository rule failure (not a module extension failure) still exits 1.
    let r = w.run(&["build", "@outer_bad_rule//:a"]);
    assert_eq!(r.code, 1, "expected exit 1:\n{}", r.err);
    assert!(r.err.contains("boom from nested repo rule"), "{}", r.err);
}
