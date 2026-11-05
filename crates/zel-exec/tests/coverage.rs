//! Coverage: Bazel's `collect_coverage.sh` (verbatim) driven through `test-setup.sh`, the LCOV
//! merger (merge / filter / manifest / ordering) and `test.cache_status` round trips.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use zel_exec::test_runner::*;
use zel_exec::test_status::TestResultData;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zel-cov-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::canonicalize(&d).unwrap()
}

fn merger(dir: &Path) -> PathBuf {
    let (_, m) = install_coverage_tools(dir).unwrap();
    m
}

fn lcov(sf: &str, da: &[(u32, u32)], fn_: &[(&str, u32, u32)], br: &[(u32, u32, u32, &str)]) -> String {
    let mut s = format!("SF:{sf}\n");
    for (n, l, _) in fn_ {
        s.push_str(&format!("FN:{l},{n}\n"));
    }
    for (n, _, c) in fn_ {
        s.push_str(&format!("FNDA:{c},{n}\n"));
    }
    for (l, b, n, t) in br {
        s.push_str(&format!("BRDA:{l},{b},{n},{t}\n"));
    }
    for (l, c) in da {
        s.push_str(&format!("DA:{l},{c}\n"));
    }
    s.push_str("end_of_record\n");
    s
}

fn run_merger(m: &Path, args: &[String]) -> (i32, String, String) {
    let o = std::process::Command::new(m).args(args).output().unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into(), String::from_utf8_lossy(&o.stderr).into())
}

#[test]
fn lcov_merger_merges_sums_sorts_and_prints_in_lcov_printer_order() {
    let d = tmp("merge");
    let m = merger(&d.join("tools"));
    let cd = d.join("covdir/nested");
    std::fs::create_dir_all(&cd).unwrap();
    std::fs::write(
        cd.join("a.dat"),
        lcov("pkg/b.c", &[(1, 1), (2, 0)], &[("f", 1, 1)], &[(2, 0, 0, "1"), (2, 0, 1, "-")])
            + &lcov("pkg/a.c", &[(10, 2)], &[], &[]),
    )
    .unwrap();
    std::fs::write(
        d.join("covdir/b.dat"),
        lcov("pkg/b.c", &[(1, 3), (2, 5), (4, 0)], &[("f", 1, 2), ("g", 7, 0)], &[(2, 0, 0, "2"), (2, 0, 1, "4")]),
    )
    .unwrap();
    std::fs::write(d.join("covdir/ignored.txt"), "SF:nope\nend_of_record\n").unwrap();
    let out = d.join("out.dat");
    let (code, _, _) = run_merger(&m, &[format!("--coverage_dir={}", d.join("covdir").display()), format!("--output_file={}", out.display())]);
    assert_eq!(code, 0);
    let got = std::fs::read_to_string(&out).unwrap();
    let want = "SF:pkg/a.c\nFNF:0\nFNH:0\nDA:10,2\nLH:1\nLF:1\nend_of_record\n\
SF:pkg/b.c\nFN:1,f\nFN:7,g\nFNDA:3,f\nFNDA:0,g\nFNF:2\nFNH:1\nBRDA:2,0,0,3\nBRDA:2,0,1,4\nBRF:2\nBRH:2\n\
DA:1,4\nDA:2,5\nDA:4,0\nLH:2\nLF:3\nend_of_record\n";
    assert_eq!(got, want);
}

#[test]
fn lcov_merger_filters_manifest_replace_and_empty_cases() {
    let d = tmp("filter");
    let m = merger(&d.join("tools"));
    let rep = d.join("reports.txt");
    let t1 = d.join("t1.dat");
    std::fs::write(&t1, lcov("pkg/keep.c", &[(1, 1)], &[], &[]) + &lcov("/usr/include/x.h", &[(1, 1)], &[], &[]) + &lcov("pkg/gen.c", &[(3, 1)], &[], &[])).unwrap();
    std::fs::write(&rep, format!("{}\n", t1.display())).unwrap();
    let manifest = d.join("manifest");
    std::fs::write(&manifest, "pkg/keep.c\npkg/real.c\npkg/skip.gcno\n").unwrap();
    let replace = d.join("replace");
    std::fs::write(&replace, "pkg/gen.c pkg/real.c\n").unwrap();
    let out = d.join("o.dat");
    let (code, _, _) = run_merger(
        &m,
        &[
            format!("--reports_file={}", rep.display()),
            format!("--output_file={}", out.display()),
            "--filter_sources=/usr/include.+".into(),
            format!("--source_file_manifest={}", manifest.display()),
            format!("--sources_to_replace_file={}", replace.display()),
        ],
    );
    assert_eq!(code, 0);
    let got = std::fs::read_to_string(&out).unwrap();
    assert!(got.contains("SF:pkg/keep.c") && got.contains("SF:pkg/real.c"), "{got}");
    assert!(!got.contains("/usr/include") && !got.contains("gen.c"), "{got}");
    // filter must match the whole path (Pattern.matches)
    let out2 = d.join("o2.dat");
    run_merger(&m, &[format!("--reports_file={}", rep.display()), format!("--output_file={}", out2.display()), "--filter_sources=pkg/k".into()]);
    assert!(std::fs::read_to_string(&out2).unwrap().contains("SF:pkg/keep.c"));
    // No coverage: empty output file, exit 0, warning on stderr.
    let out3 = d.join("o3.dat");
    let (code, _, err) = run_merger(&m, &[format!("--coverage_dir={}", d.join("empty").display()), format!("--output_file={}", out3.display())]);
    assert_eq!(code, 0);
    assert!(err.contains("There was no coverage found."));
    assert_eq!(std::fs::read_to_string(&out3).unwrap(), "");
    // Flag errors.
    assert_eq!(run_merger(&m, &["--coverage_dir=x".into()]).0, 1);
    assert_eq!(run_merger(&m, &["--bogus".into()]).0, 1);
}

fn coverage_spec(d: &Path, script_body: &str, shards: u32) -> TestSpec {
    let er = d.join("execroot/_main");
    std::fs::create_dir_all(er.join("pkg")).unwrap();
    let exe = er.join("bazel-out/bin/pkg/t");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, format!("#!/bin/sh\n{script_body}")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let rf = er.join("bazel-out/bin/pkg/t.runfiles");
    std::fs::create_dir_all(rf.join("_main/pkg")).unwrap();
    std::fs::write(rf.join("_main/pkg/t"), "").unwrap();
    let er_for_logs = er.clone();
    let tools = d.join("install/tools/test");
    let setup = tools.join("test-setup.sh");
    install_test_setup(&setup).unwrap();
    let (collect, lcov_merger) = install_coverage_tools(&tools).unwrap();
    let manifest = er.join("bazel-out/testlogs/pkg/t/coverage_manifest.txt");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    std::fs::write(&manifest, "pkg/lib.c\n").unwrap();
    TestSpec {
        label: "//pkg:t".into(),
        exe_path: exe,
        runfiles_dir: rf,
        exec_root: er,
        workspace_name: "_main".into(),
        test_log_dir: er_for_logs.join("bazel-out/testlogs/pkg/t"),
        size: "small".into(),
        timeout_secs: 60,
        shard_count: shards,
        runs_per_test: 1,
        flaky_attempts: 1,
        runs_per_test_detects_flakes: false,
        run_under: None,
        args: vec![],
        env: BTreeMap::new(),
        base_env: zel_exec::test_runner::TestBaseEnv {
            action_env: zel_util::action_env::EnvConfig::default().action_environment(false),
            ..Default::default()
        },
        test_filter: None,
        coverage: true,
        coverage_script: Some(collect),
        lcov_merger: Some(lcov_merger),
        coverage_manifest: Some(manifest),
        zip_undeclared_outputs: false,
        setup_script: setup,
        kill_delay_ms: None,
        test_tmpdir: None,
        check_sharding_support: false,
    }
}

#[test]
fn collect_coverage_sh_runs_the_test_and_merges_with_manifest_filter() {
    let d = tmp("run");
    // The "instrumented" test writes tracefiles into COVERAGE_DIR and checks the coverage env.
    let body = r#"
set -e
test -n "$COVERAGE_DIR" && test -n "$COVERAGE_MANIFEST" && test -n "$LCOV_MERGER" && test -n "$COVERAGE_OUTPUT_FILE"
test "$RUNTEST_PRESERVE_CWD" = 1 && test "$IS_COVERAGE_SPAWN" = 0 && test "$SPLIT_COVERAGE_POST_PROCESSING" = 0
test -f "$COVERAGE_MANIFEST"
mkdir -p "$COVERAGE_DIR"
printf 'SF:pkg/lib.c\nDA:1,1\nDA:2,0\nend_of_record\nSF:pkg/not_in_manifest.c\nDA:1,1\nend_of_record\n' > "$COVERAGE_DIR/t1.dat"
printf 'SF:pkg/lib.c\nDA:2,4\nDA:3,1\nend_of_record\n' > "$COVERAGE_DIR/t2.dat"
"#;
    let spec = coverage_spec(&d, body, 0);
    let r = run_test(&spec).unwrap();
    assert!(r.status.is_passed(), "{}", String::from_utf8_lossy(&std::fs::read(&r.log_path).unwrap()));
    assert!(r.has_coverage);
    let cov = std::fs::read_to_string(spec.test_log_dir.join("coverage.dat")).unwrap();
    assert_eq!(cov, "SF:pkg/lib.c\nFNF:0\nFNH:0\nDA:1,1\nDA:2,4\nDA:3,1\nLH:3\nLF:3\nend_of_record\n");
    // Failing tests do not collect coverage ("Coverage runner: Not collecting coverage").
    let d2 = tmp("fail");
    let spec = coverage_spec(&d2, "echo nope; exit 3\n", 0);
    let r = run_test(&spec).unwrap();
    assert!(!r.status.is_passed());
    let log = std::fs::read_to_string(&r.log_path).unwrap();
    assert!(log.contains("Coverage runner: Not collecting coverage for failed test."), "{log}");
}

#[test]
fn sharded_coverage_is_merged_across_shards() {
    let d = tmp("shards");
    let body = r#"
mkdir -p "$COVERAGE_DIR"
printf "SF:pkg/lib.c\nDA:$((TEST_SHARD_INDEX+1)),1\nend_of_record\n" > "$COVERAGE_DIR/s.dat"
"#;
    let spec = coverage_spec(&d, body, 3);
    let r = run_test(&spec).unwrap();
    assert!(r.status.is_passed());
    let cov = std::fs::read_to_string(spec.test_log_dir.join("coverage.dat")).unwrap();
    assert!(cov.contains("DA:1,1\nDA:2,1\nDA:3,1\n") && cov.contains("LF:3"), "{cov}");
}

#[test]
fn test_cache_status_is_testresultdata_proto_with_parsed_xml() {
    let d = tmp("cs");
    let spec = {
        let mut s = coverage_spec(&d, "echo hi\n", 0);
        s.coverage = false;
        s
    };
    std::fs::create_dir_all(&spec.test_log_dir).unwrap();
    // A test that writes its own JUnit XML.
    let body = r#"
cat > "$XML_OUTPUT_FILE" <<'EOF'
<testsuites><testsuite name="S" time="0.5"><testcase name="a" classname="C" time="0.1"/><testcase name="b"><failure/></testcase></testsuite></testsuites>
EOF
exit 0
"#;
    std::fs::write(&spec.exe_path, format!("#!/bin/sh\n{body}")).unwrap();
    let r = run_test(&spec).unwrap();
    assert!(r.status.is_passed());
    let bytes = std::fs::read(spec.test_log_dir.join("test.cache_status")).unwrap();
    let data = TestResultData::decode(&bytes).unwrap();
    assert_eq!(data.cachable, Some(true));
    assert_eq!(data.test_passed, Some(true));
    assert_eq!(data.status, Some(1));
    assert_eq!(data.exit_code, Some(0));
    assert_eq!(data.has_coverage, Some(false));
    assert!(data.passed_log.as_deref().unwrap().ends_with("testlogs/pkg/t/test.log"));
    assert_eq!(data.test_times.len(), 1);
    assert_eq!(data.test_times, data.test_process_times);
    assert!(data.start_time_millis_epoch.unwrap() > 1_600_000_000_000);
    // Round trip through the cache reader.
    let cached = read_cache_status(&spec.test_log_dir).unwrap();
    assert!(cached.cached);
    assert_eq!(cached.status, r.status);
    assert_eq!(cached.exit_code, 0);
}
