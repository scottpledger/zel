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

use std::collections::HashSet;
use std::time::Duration;
use zel_options::*;
use zel_vfs::{FileSystem, InMemoryFileSystem, PathFragment};

#[test]
fn test_all_flag_tables_populated() {
    assert!(ALL_COMMAND_OPTIONS.len() >= 1000);
    assert_eq!(ALL_STARTUP_OPTIONS.len(), 45);

    let cmd_map = get_command_options_map();
    assert!(cmd_map.contains_key("compilation_mode"));
    assert!(cmd_map.contains_key("subcommands"));
    assert!(cmd_map.contains_key("jobs"));
    assert!(cmd_map.contains_key("keep_going"));
    assert!(cmd_map.contains_key("define"));
    assert!(cmd_map.contains_key("config"));
    assert!(cmd_map.contains_key("copt"));
    assert!(cmd_map.contains_key("javacopt"));
    assert!(cmd_map.contains_key("remote_cache"));
    assert!(cmd_map.contains_key("sandbox_base"));

    let startup_map = get_startup_options_map();
    assert!(startup_map.contains_key("batch"));
    assert!(startup_map.contains_key("output_base"));
    assert!(startup_map.contains_key("output_user_root"));
    assert!(startup_map.contains_key("install_base"));
    assert!(startup_map.contains_key("connect_timeout_secs"));
    assert!(startup_map.contains_key("max_idle_secs"));
    assert!(startup_map.contains_key("ignore_all_rc_files"));
    assert!(startup_map.contains_key("bazelrc"));
}

#[test]
fn test_typed_converters_full_suite() {
    // Boolean
    assert!(convert_boolean("true").unwrap());
    assert!(convert_boolean("1").unwrap());
    assert!(convert_boolean("yes").unwrap());
    assert!(convert_boolean("t").unwrap());
    assert!(convert_boolean("y").unwrap());
    assert!(!convert_boolean("false").unwrap());
    assert!(!convert_boolean("0").unwrap());
    assert!(!convert_boolean("no").unwrap());
    assert!(!convert_boolean("f").unwrap());
    assert!(!convert_boolean("n").unwrap());
    assert_eq!(
        convert_boolean("invalid").unwrap_err().message,
        "'invalid' is not a boolean"
    );

    // TriState
    assert_eq!(convert_tristate("auto").unwrap(), TriState::Auto);
    assert_eq!(convert_tristate("true").unwrap(), TriState::Yes);
    assert_eq!(convert_tristate("no").unwrap(), TriState::No);

    // Integers / Long / Double
    assert_eq!(convert_int("42").unwrap(), 42);
    assert_eq!(convert_long("10000000000").unwrap(), 10000000000);
    assert_eq!(convert_double("3.1415").unwrap(), 3.1415);

    // Duration
    assert_eq!(convert_duration("0").unwrap(), Duration::ZERO);
    assert_eq!(convert_duration("45s").unwrap(), Duration::from_secs(45));
    assert_eq!(convert_duration("10m").unwrap(), Duration::from_secs(600));
    assert_eq!(convert_duration("2h").unwrap(), Duration::from_secs(7200));
    assert_eq!(convert_duration("1d").unwrap(), Duration::from_secs(86400));
    assert_eq!(convert_duration("250ms").unwrap(), Duration::from_millis(250));
    assert_eq!(convert_duration("100ns").unwrap(), Duration::from_nanos(100));
    // Unlike RemoteDurationConverter, `DurationConverter` requires a unit (except for `0`).
    assert_eq!(convert_duration("120").unwrap_err().message, "Illegal duration '120'.");
    assert!(convert_duration("xyz").is_err());

    // ByteSize
    assert_eq!(convert_bytesize("2048").unwrap(), 2048);
    assert_eq!(convert_bytesize("16K").unwrap(), 16 * 1024);
    assert_eq!(convert_bytesize("64M").unwrap(), 64 * 1024 * 1024);
    assert_eq!(convert_bytesize("2G").unwrap(), 2 * 1024 * 1024 * 1024);
    assert_eq!(convert_bytesize("1T").unwrap(), 1024 * 1024 * 1024 * 1024);

    // Assignments
    let (k, v) = convert_assignment("CC=clang").unwrap();
    assert_eq!(k, "CC");
    assert_eq!(v, "clang");

    // Compilation Mode
    assert_eq!(convert_compilation_mode("opt").unwrap(), "opt");
    assert_eq!(convert_compilation_mode("dbg").unwrap(), "dbg");
    assert_eq!(convert_compilation_mode("fastbuild").unwrap(), "fastbuild");
    assert!(convert_compilation_mode("custom").is_err());
}

#[test]
fn test_unknown_option_error_and_spelling_suggestion() {
    let args = vec!["--compilation_mod=opt".to_string()];
    let err = ParsedCommandOptions::parse_for_command("build", &args).unwrap_err();
    assert_eq!(
        err.format_bazel_error(),
        "ERROR: --compilation_mod=opt :: Unrecognized option: --compilation_mod=opt (did you mean '--compilation_mode'?)"
    );

    let bogus_args = vec!["--completely_unknown_flag_123".to_string()];
    let bogus_err = ParsedCommandOptions::parse_for_command("build", &bogus_args).unwrap_err();
    assert_eq!(
        bogus_err.format_bazel_error(),
        "ERROR: --completely_unknown_flag_123 :: Unrecognized option: --completely_unknown_flag_123"
    );
}

#[test]
fn test_residue_and_target_loss_fix() {
    // Ensure `zel build -- //foo:bar //baz:qux` keeps targets
    let args = vec![
        "-c".to_string(),
        "opt".to_string(),
        "--".to_string(),
        "//foo:bar".to_string(),
        "//baz:qux".to_string(),
    ];
    let parsed = ParsedCommandOptions::parse_for_command("build", &args).unwrap();
    assert_eq!(parsed.compilation_mode, "opt");
    assert_eq!(parsed.targets, vec!["//foo:bar", "//baz:qux"]);
    assert!(parsed.passthrough_args.is_empty());

    // Ensure `zel run //binary -- arg1 arg2` separates target and passthrough args
    let run_args = vec![
        "//cmd:binary".to_string(),
        "--".to_string(),
        "--custom_binary_flag".to_string(),
        "pos_arg".to_string(),
    ];
    let run_parsed = ParsedCommandOptions::parse_for_command("run", &run_args).unwrap();
    assert_eq!(run_parsed.targets, vec!["//cmd:binary"]);
    assert_eq!(
        run_parsed.passthrough_args,
        vec!["--custom_binary_flag", "pos_arg"]
    );
}

#[test]
fn test_boolean_flag_syntax_variety() {
    // --flag
    let p1 = ParsedCommandOptions::parse_for_command("build", &["--subcommands".to_string()]).unwrap();
    assert!(p1.subcommands);

    // --noflag
    let p2 = ParsedCommandOptions::parse_for_command("build", &["--nosubcommands".to_string()]).unwrap();
    assert!(!p2.subcommands);

    // --flag=true / 1 / yes
    let p3 = ParsedCommandOptions::parse_for_command("build", &["--subcommands=true".to_string()]).unwrap();
    assert!(p3.subcommands);
    let p4 = ParsedCommandOptions::parse_for_command("build", &["--subcommands=1".to_string()]).unwrap();
    assert!(p4.subcommands);
    let p5 = ParsedCommandOptions::parse_for_command("build", &["--subcommands=yes".to_string()]).unwrap();
    assert!(p5.subcommands);

    // --flag=false / 0 / no
    let p6 = ParsedCommandOptions::parse_for_command("build", &["--subcommands=false".to_string()]).unwrap();
    assert!(!p6.subcommands);
    let p7 = ParsedCommandOptions::parse_for_command("build", &["--subcommands=0".to_string()]).unwrap();
    assert!(!p7.subcommands);
    let p8 = ParsedCommandOptions::parse_for_command("build", &["--subcommands=no".to_string()]).unwrap();
    assert!(!p8.subcommands);

    // Short flag -k and -k-
    let p9 = ParsedCommandOptions::parse_for_command("build", &["-k".to_string()]).unwrap();
    assert!(p9.keep_going);
    let p10 = ParsedCommandOptions::parse_for_command("build", &["-k-".to_string()]).unwrap();
    assert!(!p10.keep_going);
}

#[test]
fn test_starlark_flags_and_aliasing() {
    let args = vec![
        "--//tools/build:custom_flag=opt_value".to_string(),
        "--@rules_cc//cc:linkstatic".to_string(),
        "--no//settings:debug_mode".to_string(),
        "--flag_alias=myalias=//pkg:target_setting".to_string(),
        "--myalias=aliased_val".to_string(),
    ];
    let parsed = ParsedCommandOptions::parse_for_command("build", &args).unwrap();
    assert_eq!(
        parsed.starlark_options.get("//tools/build:custom_flag").unwrap(),
        "opt_value"
    );
    assert_eq!(
        parsed.starlark_options.get("@rules_cc//cc:linkstatic").unwrap(),
        "true"
    );
    assert_eq!(
        parsed.starlark_options.get("//settings:debug_mode").unwrap(),
        "false"
    );
    assert_eq!(
        parsed.starlark_options.get("//pkg:target_setting").unwrap(),
        "aliased_val"
    );
}

#[test]
fn test_rc_command_inheritance() {
    let mut rc = BazelrcConfig::default();
    rc.sections.insert(
        "common".to_string(),
        vec![("--verbose_failures".to_string(), "rc".to_string())],
    );
    rc.sections.insert(
        "build".to_string(),
        vec![("-c".to_string(), "rc".to_string()), ("opt".to_string(), "rc".to_string())],
    );
    rc.sections.insert(
        "test".to_string(),
        vec![("--test_output=all".to_string(), "rc".to_string())],
    );

    // When running `test`, it inherits `common` and `build`
    let (expanded, _) = expand_command_args(&rc, "test", &["//test:t1".to_string()]).unwrap();
    assert_eq!(
        expanded,
        vec!["--verbose_failures", "-c", "opt", "--test_output=all", "//test:t1"]
    );

    let parsed = ParsedCommandOptions::parse_for_command("test", &expanded).unwrap();
    assert!(parsed.verbose_failures);
    assert_eq!(parsed.compilation_mode, "opt");
    assert_eq!(parsed.test_output, "all");
    assert_eq!(parsed.targets, vec!["//test:t1"]);
}

#[test]
fn test_config_expansion_diamond_and_cycles() {
    let mut rc = BazelrcConfig::default();
    rc.sections.insert(
        "common:shared".to_string(),
        vec![("--jobs=32".to_string(), "rc".to_string())],
    );
    rc.sections.insert(
        "build:confA".to_string(),
        vec![("--config=shared".to_string(), "rc".to_string()), ("-c".to_string(), "rc".to_string()), ("opt".to_string(), "rc".to_string())],
    );
    rc.sections.insert(
        "build:confB".to_string(),
        vec![("--config=shared".to_string(), "rc".to_string()), ("--subcommands".to_string(), "rc".to_string())],
    );
    rc.sections.insert(
        "build:diamond".to_string(),
        vec![("--config=confA".to_string(), "rc".to_string()), ("--config=confB".to_string(), "rc".to_string())],
    );

    let (expanded, _) = expand_command_args(&rc, "build", &["--config=diamond".to_string()]).unwrap();
    assert_eq!(
        expanded,
        vec!["--jobs=32", "-c", "opt", "--jobs=32", "--subcommands"]
    );

    // Cycle detection
    rc.sections.insert(
        "build:cycle1".to_string(),
        vec![("--config=cycle2".to_string(), "rc".to_string())],
    );
    rc.sections.insert(
        "build:cycle2".to_string(),
        vec![("--config=cycle1".to_string(), "rc".to_string())],
    );

    let err = expand_command_args(&rc, "build", &["--config=cycle1".to_string()]).unwrap_err();
    assert!(err.message.contains("Config expansion has a cycle"));
}

#[test]
fn test_rc_imports_and_loop_detection() {
    let fs = InMemoryFileSystem::default();
    let ws = PathFragment::create("/workspace");
    fs.create_directory_and_parents(&ws).unwrap();

    let root_rc = ws.get_relative_str(".bazelrc");
    fs.write_file(
        &root_rc,
        b"import %workspace%/configs/base.bazelrc\nbuild -c opt\n",
    )
    .unwrap();

    let configs_dir = ws.get_relative_str("configs");
    fs.create_directory_and_parents(&configs_dir).unwrap();

    fs.write_file(
        &configs_dir.get_relative_str("base.bazelrc"),
        b"import nested.bazelrc\ncommon --jobs=16\n",
    )
    .unwrap();

    // Nested relative import (resolved relative to `configs/` directory)
    fs.write_file(
        &configs_dir.get_relative_str("nested.bazelrc"),
        b"common --subcommands\n",
    )
    .unwrap();

    let mut config = BazelrcConfig::default();
    config.parse_file(&root_rc, Some(&ws), &fs, "7.0.0").unwrap();

    let (expanded, _) = expand_command_args(&config, "build", &[]).unwrap();
    assert_eq!(expanded, vec!["--subcommands", "--jobs=16", "-c", "opt"]);

    // Test loop detection
    fs.write_file(
        &ws.get_relative_str("loop1.bazelrc"),
        b"import %workspace%/loop2.bazelrc\n",
    )
    .unwrap();
    fs.write_file(
        &ws.get_relative_str("loop2.bazelrc"),
        b"import %workspace%/loop1.bazelrc\n",
    )
    .unwrap();

    let mut loop_config = BazelrcConfig::default();
    let err = loop_config
        .parse_file(&ws.get_relative_str("loop1.bazelrc"), Some(&ws), &fs, "7.0.0")
        .unwrap_err();
    assert!(err.message.contains("Import loop detected:"));
}

#[test]
fn test_rc_try_import_and_version_conditional() {
    let fs = InMemoryFileSystem::default();
    let ws = PathFragment::create("/workspace");
    fs.create_directory_and_parents(&ws).unwrap();

    fs.write_file(
        &ws.get_relative_str("extra.bazelrc"),
        b"build --jobs=64\n",
    )
    .unwrap();

    let rc_content = "\
try-import %workspace%/does_not_exist.bazelrc
try-import-if-bazel-version >=7.0.0 %workspace%/extra.bazelrc
try-import-if-bazel-version <6.0.0 %workspace%/old.bazelrc
build -c opt
";
    let mut config = BazelrcConfig::default();
    config
        .parse_str(rc_content, "/workspace/.bazelrc", &ws, &fs, &mut HashSet::new())
        .unwrap();

    let (expanded, _) = expand_command_args(&config, "build", &[]).unwrap();
    assert_eq!(expanded, vec!["--jobs=64", "-c", "opt"]);
}

#[test]
fn test_rc_discovery_order_and_ignore_all_rc() {
    let fs = InMemoryFileSystem::default();
    let ws = PathFragment::create("/workspace");
    fs.create_directory_and_parents(&ws).unwrap();

    fs.write_file(&ws.get_relative_str(".bazelrc"), b"build --jobs=8\n").unwrap();
    fs.write_file(&ws.get_relative_str("custom.bazelrc"), b"build --jobs=16\n").unwrap();

    let mut startup = StartupOptions::default();
    startup.bazelrc_files.push("/workspace/custom.bazelrc".to_string());

    let cwd = PathFragment::create("/workspace");
    let cfg = discover_and_load_rc_files(&mut startup, Some(&ws), None, &cwd, &fs, "7.0.0").unwrap();

    let (expanded, _) = expand_command_args(&cfg, "build", &[]).unwrap();
    assert_eq!(expanded, vec!["--jobs=8", "--jobs=16"]);

    // Test --ignore_all_rc_files ignores all rc files (including --bazelrc) matching Bazel
    let mut ignore_startup = StartupOptions::default();
    ignore_startup.ignore_all_rc_files = true;
    ignore_startup.bazelrc_files.push("/workspace/custom.bazelrc".to_string());

    let ignore_cfg = discover_and_load_rc_files(&mut ignore_startup, Some(&ws), None, &cwd, &fs, "7.0.0").unwrap();
    let (ignore_expanded, _) = expand_command_args(&ignore_cfg, "build", &[]).unwrap();
    assert_eq!(ignore_expanded, Vec::<String>::new());
}

#[test]
fn test_invocation_policy_enforcement() {
    let text_policy = r#"
flag_policies {
  flag_name: "test_output"
  commands: "test"
  set_value {
    flag_value: "all"
    behavior: FINAL_VALUE_IGNORE_OVERRIDES
  }
}
flag_policies {
  flag_name: "compilation_mode"
  commands: "build"
  disallow_values {
    disallowed_values: "dbg"
    new_value: "opt"
  }
}
"#;
    let policy = InvocationPolicy::parse(text_policy).unwrap();

    let mut parsed = ParsedCommandOptions::parse_for_command(
        "test",
        &["--test_output=summary".to_string(), "-c".to_string(), "dbg".to_string()],
    )
    .unwrap();

    parsed.apply_invocation_policy(&policy, "test").unwrap();

    // FINAL_VALUE_IGNORE_OVERRIDES forced test_output to "all"
    assert_eq!(parsed.test_output, "all");
    // Disallowed "dbg" replaced by "opt"
    assert_eq!(parsed.compilation_mode, "opt");
}

#[test]
fn test_invocation_policy_throw_on_override() {
    let text_policy = r#"
flag_policies {
  flag_name: "subcommands"
  commands: "build"
  set_value {
    flag_value: "false"
    behavior: FINAL_VALUE_THROW_ON_OVERRIDE
  }
  custom_error_message: "Subcommands are strictly disallowed by company policy"
}
"#;
    let policy = InvocationPolicy::parse(text_policy).unwrap();

    let mut parsed = ParsedCommandOptions::parse_for_command(
        "build",
        &["--subcommands".to_string()],
    )
    .unwrap();

    let err = parsed.apply_invocation_policy(&policy, "build").unwrap_err();
    assert_eq!(err.message, "Subcommands are strictly disallowed by company policy");
}

#[test]
fn test_canonicalize_flags_command() {
    let args = vec![
        "--for_command=build".to_string(),
        "--".to_string(),
        "-c".to_string(),
        "opt".to_string(),
        "--subcommands=true".to_string(),
        "-k".to_string(),
    ];
    let canon = canonicalize_flags(&args).unwrap();
    assert!(canon.contains(&"--compilation_mode=opt".to_string()));
    // `subcommands` is a BoolOrEnum (not a `boolean`), so Bazel keeps `--subcommands=<value>`.
    assert!(canon.contains(&"--subcommands=true".to_string()));
    assert!(canon.contains(&"--keep_going=1".to_string()));
}

#[test]
fn test_rc_relative_bazelrc_against_cwd() {
    let fs = InMemoryFileSystem::default();
    let ws = PathFragment::create("/workspace");
    let cwd = PathFragment::create("/workspace/sub");
    fs.create_directory_and_parents(&cwd).unwrap();
    fs.write_file(&cwd.get_relative_str("my.rc"), b"build --verbose_failures\n").unwrap();

    let mut startup = StartupOptions::default();
    startup.bazelrc_files.push("my.rc".to_string());
    let config = discover_and_load_rc_files(&mut startup, Some(&ws), None, &cwd, &fs, "0.0.1").unwrap();
    assert!(config.sections.get("build").unwrap().iter().any(|(flag, _)| flag == "--verbose_failures"));
}

#[test]
fn test_rc_unknown_startup_option_fatal() {
    let fs = InMemoryFileSystem::default();
    let ws = PathFragment::create("/workspace");
    let cwd = PathFragment::create("/workspace");
    fs.create_directory_and_parents(&ws).unwrap();
    fs.write_file(&ws.get_relative_str(".bazelrc"), b"startup --nosuchstartupflag\n").unwrap();

    let mut startup = StartupOptions::default();
    let err = discover_and_load_rc_files(&mut startup, Some(&ws), None, &cwd, &fs, "0.0.1").unwrap_err();
    assert!(err.message.contains("FATAL: Unknown startup option: '--nosuchstartupflag'"), "{}", err.message);
}

#[test]
fn test_rc_only_bazelrc_read_no_zelrc() {
    let fs = InMemoryFileSystem::default();
    let ws = PathFragment::create("/workspace");
    let cwd = PathFragment::create("/workspace");
    fs.create_directory_and_parents(&ws).unwrap();
    fs.write_file(&ws.get_relative_str(".zelrc"), b"build --verbose_failures\n").unwrap();

    let mut startup = StartupOptions::default();
    let config = discover_and_load_rc_files(&mut startup, Some(&ws), None, &cwd, &fs, "0.0.1").unwrap();
    assert!(config.sections.get("build").is_none());
}

#[test]
fn test_canonicalize_flags_invocation_policy_malformed() {
    let args = vec!["--invocation_policy=malformed".to_string()];
    let err = canonicalize_flags(&args).unwrap_err();
    assert!(err.message.contains("Malformed value of --invocation_policy: malformed"), "{}", err.message);
}

#[test]
fn test_canonicalize_flags_options_before_double_dash_not_canonicalized() {
    let args = vec!["--keep_going".to_string()];
    let canon = canonicalize_flags(&args).unwrap();
    assert!(canon.is_empty(), "Options before double-dash must not be canonicalized: {canon:?}");
}


