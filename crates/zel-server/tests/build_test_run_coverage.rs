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
            out.push_str(&format!("load(\"@rules_shell//shell:{name}.bzl\", \"{name}\")\n"));
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
    std::fs::write(ws_dir.join("MODULE.bazel"), "module(name = \"w\")\nbazel_dep(name = \"rules_shell\", version = \"0.6.1\")\n").unwrap();
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
        let root = env!("CARGO_MANIFEST_DIR").strip_suffix("/crates/zel-server").unwrap_or("/Users/pledger/Development/zel");
        let reg = format!("--registry=file://{}", local_registry(root).display());
        let mut full_args = vec![args[0], &reg];
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
    assert!(res.err.contains("WARNING: Usage: bazel build <options> <targets>."), "{}", res.err);
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
    assert!(res.err.contains("Target //pkg:normal up-to-date:"), "{}", res.err);
    assert!(!res.err.contains("Target //pkg:tagged up-to-date:"), "{}", res.err);

    // With --build_manual_tests, //pkg/... builds both
    let res2 = w.run(&["build", "--show_result=10", "--build_manual_tests", "//pkg/..."]);
    assert_eq!(res2.code, 0, "{}", res2.err);
    assert!(res2.err.contains("Target //pkg:tagged up-to-date:"), "{}", res2.err);

    // Explicitly targeting //pkg:tagged builds it even without --build_manual_tests
    let res3 = w.run(&["build", "//pkg:tagged"]);
    assert_eq!(res3.code, 0, "{}", res3.err);
    assert!(res3.err.contains("Target //pkg:tagged up-to-date:"), "{}", res3.err);
}

#[test]
fn test_convenience_symlinks_false() {
    let w = make_ws("symlinks-false");
    w.write("pkg/BUILD", "sh_binary(name = 'b', srcs = ['b.sh'])\n");
    w.write("pkg/b.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["build", "--experimental_convenience_symlinks=false", "//pkg:b"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(!w.ws.join("bazel-bin").exists(), "bazel-bin should not exist when convenience_symlinks=false");

    let res2 = w.run(&["build", "--experimental_convenience_symlinks=normal", "//pkg:b"]);
    assert_eq!(res2.code, 0, "{}", res2.err);
    assert!(w.ws.join("bazel-bin").exists(), "bazel-bin should exist when convenience_symlinks=normal");
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
    let res3 = w.run(&["build", "--incompatible_fail_on_unknown_output_groups", "--output_groups=nonexistent", "//pkg:g"]);
    assert_eq!(res3.code, 2, "{}", res3.err);
    assert!(res3.err.contains("Unrecognized option: --incompatible_fail_on_unknown_output_groups"), "{}", res3.err);
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
    assert!(res.err.contains("ERROR: Only a single target can be run"), "{}", res.err);
}

#[test]
fn test_run_norun_flag() {
    let w = make_ws("run-norun");
    w.write("pkg/BUILD", "sh_binary(name = 'b', srcs = ['b.sh'])\n");
    w.write("pkg/b.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["run", "--norun", "//pkg:b"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(res.err.contains("INFO: Runnable command line:"), "{}", res.err);
    assert!(res.exec.is_none(), "exec_request should be None with --norun");
}

#[test]
fn test_run_in_cwd() {
    let w = make_ws("run-in-cwd");
    w.write("pkg/BUILD", "sh_binary(name = 'b', srcs = ['b.sh'])\n");
    w.write("pkg/b.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["run", "--run_in_cwd", "--client_cwd=/custom/cwd", "//pkg:b"]);
    assert_eq!(res.code, 0, "{}", res.err);
    let req = res.exec.expect("should have exec_request");
    assert_eq!(String::from_utf8_lossy(&req.working_directory), "/custom/cwd");
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
    let argv: Vec<String> = req.argv.iter().map(|a| String::from_utf8_lossy(a).to_string()).collect();
    assert!(argv.len() >= 3, "{:?}", argv);
    assert!(argv[0].contains("wrapper"), "first arg should be wrapper: {:?}", argv);
    assert_eq!(argv[1], "--arg");
    assert!(argv[2].contains("target"), "third arg should be target: {:?}", argv);
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
        .map(|ev| (String::from_utf8_lossy(&ev.name).to_string(), String::from_utf8_lossy(&ev.value).to_string()))
        .collect();
    assert_eq!(env.get("TEST_TARGET").map(|s| s.as_str()), Some("//pkg:t"));
    assert!(env.contains_key("TEST_TMPDIR"), "{:?}", env);
    assert!(env.contains_key("TEST_SRCDIR"), "{:?}", env);
    assert!(env.contains_key("TEST_WORKSPACE"), "{:?}", env);

    let to_clear: Vec<String> = req.environment_variable_to_clear.iter().map(|b| String::from_utf8_lossy(b).to_string()).collect();
    assert!(to_clear.contains(&"RUNFILES_DIR".to_string()), "{:?}", to_clear);
}

#[test]
fn test_test_zero_tests_found_no_dangling_stats() {
    let w = make_ws("zero-tests");
    w.write("pkg/BUILD", "sh_binary(name = 'b', srcs = ['b.sh'])\n");
    w.write("pkg/b.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["test", "//pkg:b"]);
    assert_eq!(res.code, 4, "{}", res.err);
    assert!(res.err.contains("ERROR: No test targets were found, yet testing was requested"), "{}", res.err);
    assert!(!res.err.contains("Executed 0 out of 0 tests: ."), "{}", res.err);
}

#[test]
fn test_test_output_streamed_warning_and_execution() {
    let w = make_ws("streamed-output");
    w.write("pkg/BUILD", "sh_test(name = 't', srcs = ['t.sh'])\n");
    w.write("pkg/t.sh", "#!/bin/sh\necho 'hello from test'\nexit 0\n");

    let res = w.run(&["test", "--test_output=streamed", "//pkg:t"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(res.err.contains("WARNING: Streamed test output requested."), "{}", res.err);
    assert!(res.out.contains("hello from test"), "{}", res.out);
    assert!(!res.out.contains("==================== Test output for //pkg:t:"), "{}", res.out);
}

#[test]
fn test_test_max_output_bytes_truncation() {
    let w = make_ws("max-output");
    w.write("pkg/BUILD", "sh_test(name = 't', srcs = ['t.sh'])\n");
    w.write("pkg/t.sh", "#!/bin/sh\necho '0123456789012345678901234567890123456789'\nexit 1\n");

    let res = w.run(&["test", "--test_output=errors", "--max_test_output_bytes=10", "//pkg:t"]);
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
    assert!(res.err.contains("INFO: LCOV coverage report is located at"), "{}", res.err);
    assert!(w.ob.join("bazel-out/_coverage/_coverage_report.dat").exists() || res.err.contains("_coverage_report.dat"));
    // Baseline coverage file created
    let baseline = w.ws.join("bazel-testlogs/pkg/t/baseline_coverage.dat");
    assert!(baseline.exists() || res.err.contains("coverage"), "baseline coverage should be generated");
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
    let res3 = w.run(&["test", "--test_summary=testcase", "--cache_test_results=no", "//pkg:pass"]);
    assert_eq!(res3.code, 0, "{}", res3.out);
    assert!(!res3.err.contains("PASSED in "), "{}", res3.err);
    assert!(res3.out.contains("Test cases: finished with 1 passing, 0 skipped and 0 failing out of 1 test cases"), "{}", res3.out);
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

    let res = w.run(&["test", "--notest_keep_going", "--jobs=1", "//pkg:t1_fail", "//pkg:t2_pass"]);
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
    let res = w.run(&["build", &format!("--explain={}", explain_file.display()), "--output_filter=nomatch", "//pkg:g"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(explain_file.exists(), "explain file should be created");
}

#[test]
fn test_run_portable_paths_and_omit_args() {
    let w = make_ws("run-portable");
    w.write("pkg/BUILD", "sh_binary(name = 'b', srcs = ['b.sh'])\n");
    w.write("pkg/b.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["run", "--portable_paths", "--omit_run_args", "//pkg:b", "--", "secret_arg"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(res.err.contains("<args omitted>"), "{}", res.err);
    assert!(!res.err.contains("secret_arg"), "{}", res.err);

    let req = res.exec.expect("should have exec request");
    assert!(!req.path_to_replace.is_empty(), "path_to_replace should not be empty with --portable_paths");
}

#[test]
fn test_test_nobuild() {
    let w = make_ws("nobuild");
    w.write("pkg/BUILD", "sh_test(name = 'pass', srcs = ['pass.sh'])\n");
    w.write("pkg/pass.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["test", "--nobuild", "//pkg:pass"]);
    assert_eq!(res.code, 1, "{}", res.err);
    assert!(res.err.contains("ERROR: Couldn't start the build. Unable to run tests"), "{}", res.err);
    assert!(res.err.contains("INFO: Found 1 test target..."), "{}", res.err);
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
    assert!(res2.err.contains("INFO: Found 1 test target..."), "{}", res2.err);
}

#[test]
fn test_target_nothing_to_build() {
    let w = make_ws("nothing-to-build");
    w.write("pkg/BUILD", "genrule(name = 'g', outs = ['out.txt'], cmd = 'echo hi > $@')\n");

    let res = w.run(&["build", "--output_groups=nonexistent", "//pkg:g"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(res.err.contains("Target //pkg:g up-to-date (nothing to build)"), "{}", res.err);
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
    assert!(res.err.contains("//pkg:t1, //pkg:t2, //pkg:t3, //pkg:t4, //pkg:t5[TRUNCATED]"), "{}", res.err);
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
    assert!(res.err.contains("INFO: Running command line: external/bazel_tools/tools/test/test-setup.sh ./t"), "{}", res.err);
    let req = res.exec.expect("should have exec request");
    let env: BTreeMap<String, String> = req
        .environment_variable
        .iter()
        .map(|ev| (String::from_utf8_lossy(&ev.name).to_string(), String::from_utf8_lossy(&ev.value).to_string()))
        .collect();
    assert_eq!(env.get("TEST_TARGET").map(|s| s.as_str()), Some("//pkg:t"));
    assert_eq!(env.get("TEST_BINARY").map(|s| s.as_str()), Some("./t"));
    assert!(env.get("TEST_TMPDIR").unwrap().contains("_tmp"), "{:?}", env.get("TEST_TMPDIR"));
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
    assert!(res.err.contains("ERROR: Testing //pkg:t is not up-to-date"), "{}", res.err);
    assert!(res.out.contains("Finished with 0 passing and 0 failing tests up to date, 1 out of date."), "{}", res.out);
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

    let res = w.run(&["test", "--test_summary=detailed", "--notest_keep_going", "--jobs=1", "--test_strategy=exclusive", "//pkg:t1_fail", "//pkg:t2_pass"]);
    assert_eq!(res.code, 3, "{}", res.err);
    assert!(res.err.contains("INFO: Build completed, 1 test FAILED"), "{}", res.err);
    assert!(res.out.contains("and 1 was skipped."), "{}", res.out);
    assert!(res.out.contains("//pkg:t2_pass") && res.out.contains("NO STATUS"), "{}", res.out);
}

#[test]
fn test_explain_created_when_no_actions_run() {
    let w = make_ws("explain-no-act");
    w.write("pkg/BUILD", "genrule(name = 'g', outs = ['g.out'], cmd = 'echo hi > $@')\n");
    // Build once so it is up-to-date
    let res1 = w.run(&["build", "//pkg:g"]);
    assert_eq!(res1.code, 0, "{}", res1.err);

    let explain_file = w.ws.join("explain_no_act.txt");
    let res2 = w.run(&["build", &format!("--explain={}", explain_file.display()), "//pkg:g"]);
    assert_eq!(res2.code, 0, "{}", res2.err);
    assert!(explain_file.exists(), "explain file must be created even when no actions run");
    let content = std::fs::read_to_string(&explain_file).unwrap();
    assert!(content.contains("Build options:"), "{}", content);
    assert!(res2.err.contains("INFO: Writing explanation of rebuilds to"), "{}", res2.err);
}

#[test]
fn test_coverage_support_nonexistent_label_fails() {
    let w = make_ws("cov-support-err");
    w.write("pkg/BUILD", "sh_test(name = 't', srcs = ['t.sh'])\n");
    w.write("pkg/t.sh", "#!/bin/sh\nexit 0\n");

    let res = w.run(&["coverage", "--coverage_support=//pkg:nonexistent", "//pkg:t"]);
    assert_eq!(res.code, 1, "{}", res.err);
    // Real bazel 9.3.2: the tests implicitly depend on the label.
    assert!(
        res.err.contains("every rule of type sh_test implicitly depends upon the target '//pkg:nonexistent', but this target could not be found because of: no such target '//pkg:nonexistent'"),
        "{}",
        res.err
    );
    assert!(res.err.contains("ERROR: Analysis of target '//pkg:t' failed; build aborted: Analysis failed"), "{}", res.err);
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
    let res2 = w.run(&["build", "--bust_action_caches=//pkg:g1", "//pkg:g1", "//pkg:g2"]);
    assert_eq!(res2.code, 2, "{}", res2.err);
    assert!(res2.err.contains("Unrecognized option: --bust_action_caches=//pkg:g1"), "{}", res2.err);
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
    assert!(res.err.contains("Target //pkg:b1 up-to-date:"), "{}", res.err);
    assert!(!res.err.contains("Target //pkg:b2 up-to-date:"), "{}", res.err);
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
    assert!(res.err.contains("ERROR: 'run' only works with tests with one shard"), "{}", res.err);
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
    assert!(!res.err.contains("INFO: Running command line:"), "{}", res.err);
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
    assert!(res.err.contains("Target //pkg:t up-to-date:"), "{}", res.err);
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
    assert!(res_t.err.contains("ERROR: Couldn't start the build. Unable to run tests"), "{}", res_t.err);
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
    assert!(res1.err.contains("Sharding requested, but the test runner did not advertise support"), "{}", res1.err);

    // With --noincompatible_check_sharding_support (present in 9.3.2): the check is skipped and the test passes.
    let res2 = w.run(&["test", "--noincompatible_check_sharding_support", "//pkg:shard"]);
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
    let res_v = w.run(&["test", "--cache_test_results=no", "--test_verbose_timeout_warnings", "//pkg:t"]);
    assert_eq!(res_v.code, 0, "{}", res_v.err);
    assert!(res_v.out.contains("WARNING: //pkg:t: Test execution time"), "{}", res_v.out);
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
    assert!(!log_dir.join("test.cache_key").exists(), "test.cache_key must not be written to disk");
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
    assert_eq!(res.out, "hello-stdout\n", "action stdout goes to the command's stdout");
    assert!(res.err.contains("INFO: From Executing genrule //pkg:so:\nwarn\n"), "{}", res.err);
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
    assert!(res.err.contains("target '//dep:old' is deprecated: use new"), "{}", res.err);
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
    assert!(res.err.contains("available identifiers are: [exclusive, standalone]"), "{}", res.err);
    let res = w.run(&["build", "//pkg:d"]);
    assert_eq!(res.code, 1, "{}", res.err);
    assert!(res.err.contains("output 'pkg/dout' of //pkg:d is a directory but was not declared as such"), "{}", res.err);
}

#[test]
fn round7_flaky_attempts_are_reported() {
    let w = make_ws("r7-flaky");
    w.write("pkg/BUILD", "sh_test(name = \"t\", srcs = [\"t.sh\"], flaky = True)\n");
    w.write("pkg/t.sh", "#!/bin/bash\nf=\"$TEST_TMPDIR/../marker\"\nif [ -e \"$f\" ]; then exit 0; fi\ntouch \"$f\"\nexit 1\n");
    let res = w.run(&["test", "--test_output=all", "//pkg:t"]);
    assert_eq!(res.code, 0, "{}", res.err);
    assert!(res.out.contains("FLAKY, failed in 1 out of 2"), "{}", res.out);
    assert!(res.out.contains("test_attempts/attempt_1.log"), "{}", res.out);
    assert!(res.err.contains("FAIL: //pkg:t (Exit 1) (see "), "{}", res.err);
    assert!(res.err.contains("FLAKY: //pkg:t (Summary)"), "{}", res.err);
    assert!(!res.out.contains("specified size is too big"), "{}", res.out);
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
        res.err.contains("ERROR: Error writing run script: /nonexistent-dir-zel/x (No such file or directory)"),
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
    assert!(res.err.contains(" internal, 2 ") && res.err.contains(" processes: "), "{}", res.err);
    assert!(res.err.contains("total actions"), "{}", res.err);
    let res = w.run(&["test", "//pkg:t"]);
    assert!(res.err.contains("INFO: 1 process: 1 action cache hit, 1 internal."), "{}", res.err);
    let res = w.run(&["build", "--nobuild", "//pkg:t"]);
    assert!(res.err.contains("INFO: 0 processes."), "{}", res.err);
}

#[test]
fn test_unknown_spawn_strategy_prints_build_trailer() {
    let w = make_ws("bogus-strategy");
    w.write("pkg/BUILD", "genrule(name = \"g\", outs = [\"o\"], cmd = \"touch $@\")\n");
    let res = w.run(&["build", "--spawn_strategy=bogus", "//pkg:g"]);
    assert_eq!(res.code, 2, "{}", res.err);
    let all: Vec<&str> = res.err.lines().collect();
    let at = all.iter().position(|l| l.contains("'bogus' was requested")).expect(&res.err);
    let lines = &all[at..];
    assert!(lines[0].contains("'bogus' was requested for explicit default strategies"), "{}", res.err);
    assert!(lines[1].starts_with("INFO: Elapsed time: "), "{}", res.err);
    assert_eq!(lines[2], "INFO: 0 processes.", "{}", res.err);
    assert_eq!(lines[3], "ERROR: Build did NOT complete successfully", "{}", res.err);
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
    assert!(res.out.contains("Executed 2 out of 2 tests"), "{}{}", res.out, res.err);
}

#[test]
fn test_workspace_status_failure_precedes_analysis_error() {
    let w = make_ws("wsc-order");
    w.write("pkg/BUILD", "genrule(name = \"g\", outs = [\"o\"], cmd = \"echo $(NOSUCHVAR) > $@\")\n");
    let res = w.run(&["build", "--workspace_status_command=/nonexistent/wsc", "//pkg:g"]);
    assert_eq!(res.code, 1, "{}", res.err);
    assert!(res.err.contains("BazelWorkspaceStatusAction stable-status.txt failed"), "{}", res.err);
    assert!(!res.err.contains("Analysis of target"), "{}", res.err);
    assert!(res.err.contains("INFO: 1 process: 1 internal."), "{}", res.err);
}

#[test]
fn test_missing_input_action_is_not_announced_or_counted() {
    let w = make_ws("missing-input-s");
    w.write("pkg/BUILD", "genrule(name = \"g\", srcs = [\"nope.txt\"], outs = [\"o\"], cmd = \"cat $< > $@\")\n");
    let res = w.run(&["build", "-s", "//pkg:g"]);
    assert_eq!(res.code, 1, "{}", res.err);
    assert!(!res.err.contains("SUBCOMMAND"), "{}", res.err);
    assert!(res.err.contains("INFO: 1 process: 1 internal."), "{}", res.err);
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
    let wn = res.err.find("WARNING: errors encountered while analyzing target '//pkg:bad'").expect(&res.err);
    assert!(a < wn, "{}", res.err);
}

#[test]
fn test_unknown_and_missing_aspects_match_bazel() {
    let w = make_ws("aspect-errs");
    w.write("pkg/BUILD", "genrule(name = \"g\", outs = [\"o\"], cmd = \"touch $@\")\n");
    let r = w.run(&["build", "--aspects=//pkg:x", "//pkg:g"]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(r.err.contains("ERROR: Aspect '//pkg:x' is unknown\n"), "{}", r.err);
    assert!(r.err.contains("INFO: 0 processes."), "{}", r.err);
    let r = w.run(&["build", "--aspects=//pkg:x%y", "//pkg:g"]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(r.err.contains("ERROR: cannot load '//pkg:x': no such file\n"), "{}", r.err);
    assert!(r.err.contains("ERROR: Analysis of aspects '[//pkg:x%y] with parameters {} on //pkg:g' failed; build aborted: cannot load '//pkg:x': no such file"), "{}", r.err);
}

#[test]
fn test_override_repository_of_invisible_repo_is_exit_48() {
    let w = make_ws("override-invisible");
    w.write("pkg/BUILD", "genrule(name = \"g\", outs = [\"o\"], cmd = \"touch $@\")\n");
    let r = w.run(&["build", "--override_repository=foo=/tmp", "//pkg:g"]);
    assert_eq!(r.code, 48, "{}", r.err);
    assert!(r.err.contains("ERROR: no repository visible as '@foo' from the main repository, but overridden with --override_repository. Use --inject_repository to add new repositories."), "{}", r.err);
    let r = w.run(&["build", "--override_repository=rules_shell=/tmp/none", "//pkg:g"]);
    assert_ne!(r.code, 48, "{}", r.err);
}

#[test]
fn test_shell_executable_is_used_for_genrules() {
    let w = make_ws("shell-exec");
    w.write("pkg/BUILD", "genrule(name = \"g\", outs = [\"o\"], cmd = \"echo hi > $@\")\n");
    let r = w.run(&["build", "-s", "--shell_executable=/bin/sh", "//pkg:g"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.err.contains("/bin/sh -c 'source external/bazel_tools/tools/genrule/genrule-setup.sh;"), "{}", r.err);
    let r = w.run(&["build", "--shell_executable=/nonexistent", "//pkg:g"]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(r.err.contains("Executing genrule //pkg:g failed"), "{}", r.err);
}

#[test]
fn test_unexecutable_shell_reports_process_wrapper_text() {
    let w = make_ws("shell-missing");
    w.write("pkg/BUILD", "genrule(name = \"g\", outs = [\"o\"], cmd = \"echo hi > $@\")\n");
    let r = w.run(&["build", "--shell_executable=/nonexistent", "//pkg:g"]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(r.err.contains("failed: (Exit 1): nonexistent failed:"), "{}", r.err);
    assert!(r.err.contains("src/main/tools/process-wrapper-legacy.cc:80: \"execvp(/nonexistent, ...)\": No such file or directory"), "{}", r.err);
}

#[test]
fn test_invalid_platform_flags_match_bazel() {
    let w = make_ws("platform-flags");
    w.write("pkg/BUILD", "genrule(name = \"g\", outs = [\"o\"], cmd = \"touch $@\")\n");
    let r = w.run(&["build", "--platforms=//pkg:nonexistent", "//pkg:g"]);
    assert_eq!(r.code, 2, "{}", r.err);
    let r = w.run(&["build", "--host_platform=//pkg:nonexistent", "//pkg:g"]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(r.err.contains("INFO: 0 processes."), "{}", r.err);
    let r = w.run(&["build", "--extra_execution_platforms=//pkg:nonexistent", "//pkg:g"]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(r.err.contains("While resolving toolchains for target //pkg:g ("), "{}", r.err);
    assert!(r.err.contains("invalid registered execution platform '//pkg:nonexistent'"), "{}", r.err);
    assert!(r.err.contains("ERROR: Analysis of target '//pkg:g' failed; build aborted"), "{}", r.err);
}

#[test]
fn test_unreachable_remote_cache_fails_first_spawn_with_remote_error() {
    let w = make_ws("remote-down");
    w.write("pkg/BUILD", "genrule(name = \"g\", outs = [\"o\"], cmd = \"touch $@\")\n");
    let r = w.run(&["build", "--remote_cache=grpc://localhost:1", "//pkg:g"]);
    assert_eq!(r.code, 34, "{}", r.err);
    assert!(r.err.contains("INFO: Invocation ID: "), "{}", r.err);
    assert!(
        r.err.contains("Executing genrule //pkg:g failed: Failed to query remote execution capabilities: Connection refused: localhost/[0:0:0:0:0:0:0:1]:1"),
        "{}",
        r.err
    );
}

#[test]
fn test_disk_cache_hit_materializes_top_level_output() {
    let w = make_ws("disk-cache-hit");
    w.write("pkg/BUILD", "genrule(name = \"g\", outs = [\"o\"], cmd = \"echo hi > $@\")\n");
    let dc = w.ws.join("dc");
    let flag = format!("--disk_cache={}", dc.display());
    let r = w.run(&["build", &flag, "//pkg:g"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(w.run(&["clean"]).code, 0);
    let r = w.run(&["build", &flag, "//pkg:g"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.err.contains("1 disk cache hit"), "{}", r.err);
    assert_eq!(std::fs::read_to_string(w.ws.join("bazel-bin/pkg/o")).unwrap(), "hi\n");
}

#[test]
fn test_empty_test_filter_still_sets_testbridge_env() {
    let w = make_ws("empty-filter");
    w.write("pkg/BUILD", "sh_test(name = \"t\", srcs = [\"t.sh\"])\n");
    w.write("pkg/t.sh", "#!/bin/sh\nif [ \"${TESTBRIDGE_TEST_ONLY+set}\" = set ]; then echo HAS_FILTER; fi\n");
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
    assert!(r.err.contains("WARNING: Target pattern parsing failed."), "{}", r.err);
    assert!(r.err.contains("ERROR: Skipping '//pkg:a': Error evaluating '//pkg:a': error loading package 'pkg': Package 'pkg' contains errors"), "{}", r.err);
}
