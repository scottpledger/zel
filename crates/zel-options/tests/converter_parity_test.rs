//! Parse-level tests of the option converters (`--jobs=auto` and friends, enums, ranges, ...)
//! asserting Bazel's exact `While parsing option ...` messages.

use zel_options::ParsedCommandOptions;

fn parse(cmd: &str, args: &[&str]) -> Result<ParsedCommandOptions, String> {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    ParsedCommandOptions::parse_for_command(cmd, &args).map_err(|e| e.message)
}

fn err(args: &[&str]) -> String {
    parse("build", args).unwrap_err()
}

fn raw(p: &ParsedCommandOptions, name: &str) -> Vec<String> {
    p.parsed_options.get(name).map(|d| d.raw_values.clone()).unwrap_or_default()
}

const SYNTAX: &str = "an integer, or a keyword (\"auto\", \"HOST_CPUS\", \"HOST_RAM\"), optionally followed by an operation ([-|*]<float>) eg. \"auto\", \"HOST_CPUS*.5\"";

fn host_cpus() -> i64 {
    std::thread::available_parallelism().map(|n| n.get() as i64).unwrap_or(1)
}

#[test]
fn jobs_forms_are_accepted_and_raw_string_is_preserved() {
    for raw_value in ["auto", "HOST_CPUS", "HOST_CPUS*.5", "HOST_CPUS*0.5", "HOST_RAM*.0001", "7", "HOST_CPUS-0"] {
        let p = parse("build", &[&format!("--jobs={raw_value}")]).unwrap();
        assert_eq!(raw(&p, "jobs"), vec![raw_value.to_string()], "{raw_value}");
        assert!(p.jobs >= 1 && p.jobs <= 5000, "{raw_value}: {}", p.jobs);
    }
    let p = parse("build", &["--jobs=auto"]).unwrap();
    assert_eq!(p.jobs as i64, host_cpus().min(5000));
    let p = parse("build", &["--jobs=HOST_CPUS*.5"]).unwrap();
    assert_eq!(p.jobs as i64, ((host_cpus() as f64) * 0.5f32 as f64 + 0.5).floor() as i64);
    let p = parse("build", &["-j", "3"]).unwrap();
    assert_eq!(p.jobs, 3);
    assert_eq!(raw(&p, "jobs"), vec!["3".to_string()]);
    // MAX_JOBS caps instead of failing.
    assert_eq!(parse("build", &["--jobs=99999"]).unwrap().jobs, 5000);
    // Default (no flag).
    assert_eq!(parse("build", &[]).unwrap().jobs as i64, host_cpus().min(5000));
}

#[test]
fn jobs_errors() {
    assert_eq!(
        err(&["--jobs=lots"]),
        format!("While parsing option --jobs=lots: Parameter 'lots' does not follow correct syntax. This flag takes {SYNTAX}.")
    );
    assert_eq!(err(&["--jobs=0"]), "While parsing option --jobs=0: Value '(0)' must be at least 1.");
    assert_eq!(err(&["--jobs=-2"]), "While parsing option --jobs=-2: Value '(-2)' must be at least 1.");
    assert_eq!(
        err(&["-j", "1.5"]),
        format!("While parsing option -j 1.5: Parameter '1.5' does not follow correct syntax. This flag takes {SYNTAX}.")
    );
    assert_eq!(err(&["--jobs=HOST_CPUS*"]), "While parsing option --jobs=HOST_CPUS*: ' is not a float");
    assert_eq!(err(&["--jobs"]), "Expected value after --jobs");
    assert_eq!(err(&["--nojobs"]), "Illegal use of 'no' prefix on non-boolean option: --nojobs");
}

#[test]
fn local_test_jobs_and_local_resources() {
    let p = parse("test", &["--local_test_jobs=auto"]).unwrap();
    assert_eq!(raw(&p, "local_test_jobs"), vec!["auto".to_string()]);
    assert!(parse("test", &["--local_test_jobs=HOST_CPUS*.5", "--local_test_jobs=0"]).is_ok());
    assert_eq!(
        parse("test", &["--local_test_jobs=-1"]).unwrap_err(),
        "While parsing option --local_test_jobs=-1: Value '(-1.000000)' must be at least 0.000000."
    );

    let p = parse(
        "build",
        &["--local_resources=cpu=HOST_CPUS-1", "--local_resources=memory=HOST_RAM*.5", "--local_resources=gpu=1.5"],
    )
    .unwrap();
    assert_eq!(
        raw(&p, "local_resources"),
        vec!["cpu=HOST_CPUS-1".to_string(), "memory=HOST_RAM*.5".to_string(), "gpu=1.5".to_string()]
    );
    assert_eq!(
        err(&["--local_resources=cpu"]),
        "While parsing option --local_resources=cpu: Variable definitions must be in the form of a 'name=value' assignment"
    );
    assert_eq!(
        err(&["--local_resources=cpu=-1"]),
        "While parsing option --local_resources=cpu=-1: Value '(-1.000000)' must be at least 0.000000."
    );
    assert!(err(&["--local_resources=cpu=auto"]).starts_with(
        "While parsing option --local_resources=cpu=auto: Parameter 'auto' does not follow correct syntax. This flag takes an integer, or a keyword (\"HOST_CPUS\", \"HOST_RAM\"), optionally"
    ));
    // `--local_cpu_resources` / `--local_ram_resources` do not exist in this Bazel version.
    assert_eq!(
        err(&["--local_cpu_resources=2"]),
        "Unrecognized option: --local_cpu_resources=2"
    );
    assert!(err(&["--local_ram_resources=HOST_RAM*.5"]).starts_with("Unrecognized option: --local_ram_resources=HOST_RAM*.5"));
}

#[test]
fn other_resource_converters() {
    assert!(parse("build", &["--loading_phase_threads=HOST_CPUS*.5", "--experimental_fsvc_threads=auto"]).is_ok());
    assert_eq!(
        err(&["--loading_phase_threads=0"]),
        "While parsing option --loading_phase_threads=0: Value '(0.000000)' must be at least 1.000000."
    );
    assert!(parse("build", &["--local_ram_resources=1"]).is_err());
    assert!(parse("build", &["--experimental_worker_memory_limit_mb=HOST_RAM*.5", "--worker_max_instances=Javac=auto", "--worker_max_instances=4"]).is_ok());
    assert_eq!(
        err(&["--worker_max_instances=Javac=-1"]),
        "While parsing option --worker_max_instances=Javac=-1: Value '(-1.000000)' must be at least 0.000000."
    );
}

#[test]
fn enums_and_bool_or_enum() {
    assert!(parse("build", &["--color=auto", "--curses=no", "--compilation_mode=OPT"]).is_ok());
    assert_eq!(
        err(&["--color=sometimes"]),
        "While parsing option --color=sometimes: Not a valid --color setting: 'sometimes' (should be yes, no or auto)"
    );
    assert_eq!(
        err(&["-c", "release"]),
        "While parsing option -c release: Not a valid compilation mode: 'release' (should be fastbuild, dbg or opt)"
    );
    let p = parse("build", &["--subcommands=pretty_print"]).unwrap();
    assert!(p.subcommands);
    assert!(!parse("build", &["--subcommands=false"]).unwrap().subcommands);
    assert!(parse("build", &["-s"]).unwrap().subcommands);
    assert!(!parse("build", &["--nosubcommands"]).unwrap().subcommands);
    assert_eq!(
        err(&["--subcommands=maybe"]),
        "While parsing option --subcommands=maybe: Not a valid subcommand option: 'maybe' (should be true, pretty_print or false)"
    );
    assert_eq!(
        err(&["--test_output=loud"]),
        "While parsing option --test_output=loud: Not a valid test output: 'loud' (should be summary, errors, all or streamed)"
    );
    assert_eq!(parse("test", &["--test_output=ALL"]).unwrap().test_output, "all");
}

#[test]
fn numbers_booleans_durations_sizes() {
    assert_eq!(
        err(&["--keep_going=maybe"]),
        "While parsing option --keep_going=maybe: 'maybe' is not a boolean"
    );
    assert_eq!(err(&["--keep_going=MAYBE"]), "While parsing option --keep_going=MAYBE: 'maybe' is not a boolean");
    // 9.3.2: a plain `long` option (no range converter).
    assert!(parse("build", &["--cache_computed_file_digests=-1"]).is_ok());
    assert_eq!(
        err(&["--cache_computed_file_digests=abc"]),
        "While parsing option --cache_computed_file_digests=abc: 'abc' is not a long"
    );
    assert_eq!(
        err(&["--experimental_cpu_load_scheduling_window_size=5"]),
        "While parsing option --experimental_cpu_load_scheduling_window_size=5: Illegal duration '5'."
    );
    assert!(parse("build", &["--experimental_cpu_load_scheduling_window_size=5ms"]).is_ok());
    assert_eq!(
        err(&["--experimental_disk_cache_gc_max_size=1.5G"]),
        "While parsing option --experimental_disk_cache_gc_max_size=1.5G: Invalid size: 1.5G"
    );
    assert!(parse("build", &["--experimental_disk_cache_gc_max_size=10G"]).is_ok());
    // RemoteDurationConverter takes unit-less seconds.
    assert!(parse("build", &["--remote_timeout=60"]).is_ok());
    assert_eq!(
        err(&["--remote_timeout=abc"]),
        "While parsing option --remote_timeout=abc: Illegal duration 'abc'."
    );
    assert_eq!(
        err(&["--progress_report_interval=4000"]),
        "While parsing option --progress_report_interval=4000: '4000' should be <= 3600"
    );
    assert_eq!(
        err(&["--gc_thrashing_threshold=101"]),
        "While parsing option --gc_thrashing_threshold=101: '101' should be <= 100"
    );
    // TriState: `auto` is accepted and canonicalizes to the raw value (not `--no...`).
    assert!(parse("build", &["--cache_test_results=auto"]).is_ok());
    assert!(err(&["--cache_test_results=maybe"]).contains("'maybe' is not a boolean"));
}

#[test]
fn lists_assignments_and_timeouts() {
    let p = parse("build", &["--spawn_strategy=worker,sandboxed,local"]).unwrap();
    assert_eq!(raw(&p, "spawn_strategy"), vec!["worker,sandboxed,local".to_string()]);
    assert_eq!(
        err(&["--spawn_strategy=a,,b"]),
        "While parsing option --spawn_strategy=a,,b: Empty values are not allowed as part of this comma-separated list of options"
    );
    assert!(parse("build", &["--spawn_strategy="]).is_ok());
    assert!(parse("build", &["--strategy=Genrule=local", "--strategy=remote"]).is_ok());
    assert_eq!(
        err(&["--strategy=Genrule=a,,b"]),
        "While parsing option --strategy=Genrule=a,,b: Variable definitions must not contain empty strings or leading / trailing commas"
    );
    let p = parse("build", &["--define=a=b,c=d", "--define", "x=y"]).unwrap();
    assert_eq!(raw(&p, "define"), vec!["a=b,c=d".to_string(), "x=y".to_string()]);
    assert_eq!(p.user_defines.get("a").map(|s| s.as_str()), Some("b,c=d"));
    assert_eq!(p.user_defines.get("x").map(|s| s.as_str()), Some("y"));
    assert_eq!(
        err(&["--define=nonsense"]),
        "While parsing option --define=nonsense: Variable definitions must be in the form of a 'name=value' assignment"
    );
    assert!(parse("test", &["--test_timeout=30", "--test_timeout=1,2,3,4"]).is_ok());
    assert_eq!(
        parse("test", &["--test_timeout=1,2"]).unwrap_err(),
        "While parsing option --test_timeout=1,2: Invalid number of comma-separated entries"
    );
    assert_eq!(
        parse("test", &["--test_timeout=1m"]).unwrap_err(),
        "While parsing option --test_timeout=1m: '1m' is not an int"
    );
    // 9.3.2: `--sandbox_writable_path` is a plain string list (no absolute-path converter).
    assert!(parse("build", &["--sandbox_writable_path=rel"]).is_ok());
}

#[test]
fn argument_shapes() {
    assert_eq!(err(&["-j8"]), "Invalid options syntax: -j8");
    assert_eq!(err(&["--=x"]), "Invalid options syntax: --=x");
    assert_eq!(err(&["--nokeep_going=1"]), "Unexpected value after boolean option: --nokeep_going=1");
    assert_eq!(err(&["--nojobs=1"]), "Illegal use of 'no' prefix on non-boolean option: --nojobs=1");
    assert!(parse("build", &["--keep_going", "--nokeep_going", "-k-", "-k"]).is_ok());
    assert!(err(&["-q"]).starts_with("Unrecognized option: -q"));
}
