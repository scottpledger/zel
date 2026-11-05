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

//! The per-command option sets of the generated flag tables follow
//! `BlazeCommandUtils.getOptions(command, suppliers, ruleClassProvider)`.

use zel_options::flag_tables::{is_command_allowed, COMMAND_INHERITANCE, COMMAND_OPTION_CLASSES};
use zel_options::*;

fn parse(command: &str, args: &[&str]) -> Result<ParsedCommandOptions, OptionsParsingError> {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    ParsedCommandOptions::parse_for_command(command, &args)
}

fn accepted_by(name: &str) -> Vec<&'static str> {
    let mut v: Vec<&'static str> =
        ALL_COMMAND_OPTIONS.iter().filter(|d| d.name == name).flat_map(|d| d.commands.iter().copied()).collect();
    v.sort_unstable();
    v.dedup();
    v
}

#[test]
fn startup_and_command_options_of_the_same_name_live_in_separate_namespaces() {
    // `--write_command_log` is a startup option (`BlazeServerStartupOptions`) *and* a command option
    // (`CommonCommandOptions`).
    assert!(ALL_STARTUP_OPTIONS.iter().any(|d| d.name == "write_command_log"));
    let cmd = ALL_COMMAND_OPTIONS.iter().find(|d| d.name == "write_command_log").expect("command option");
    assert!(cmd.commands.contains(&"build") && cmd.commands.contains(&"query"));
    assert!(!cmd.commands.contains(&"startup"));
    assert!(parse("build", &["--write_command_log"]).is_ok());
    assert!(parse("build", &["--nowrite_command_log"]).is_ok());
    assert!(parse("version", &["--nowrite_command_log"]).is_ok());
}

#[test]
fn info_clean_fetch_vendor_inherit_the_build_options() {
    for cmd in ["info", "clean", "fetch", "vendor", "print_action", "mobile-install", "aquery", "cquery", "canonicalize-flags"] {
        assert!(parse(cmd, &["-c", "opt"]).is_ok(), "{cmd} -c opt");
        assert!(parse(cmd, &["--cpu=k8"]).is_ok(), "{cmd} --cpu");
        assert!(parse(cmd, &["--define=a=b"]).is_ok(), "{cmd} --define");
        assert!(parse(cmd, &["--jobs=3"]).is_ok(), "{cmd} --jobs");
    }
    let inherits = |c: &str| COMMAND_INHERITANCE.iter().find(|e| e.0 == c).unwrap().1.to_vec();
    assert_eq!(inherits("info"), vec!["build"]);
    assert_eq!(inherits("clean"), vec!["build"]);
    assert_eq!(inherits("fetch"), vec!["test"]);
    assert_eq!(inherits("vendor"), vec!["test"]);
    assert_eq!(inherits("coverage"), vec!["test"]);
    assert_eq!(inherits("cquery"), vec!["test"]);
    assert_eq!(inherits("query"), Vec::<&str>::new());
}

#[test]
fn rc_ancestry_is_derived_from_inherits_options_from() {
    assert_eq!(get_command_ancestry("info"), ["always", "common", "build", "info"]);
    assert_eq!(get_command_ancestry("clean"), ["always", "common", "build", "clean"]);
    assert_eq!(get_command_ancestry("fetch"), ["always", "common", "build", "test", "fetch"]);
    assert_eq!(get_command_ancestry("coverage"), ["always", "common", "build", "test", "coverage"]);
    assert_eq!(get_command_ancestry("mobile-install"), ["always", "common", "build", "mobile-install"]);
    assert_eq!(get_command_ancestry("print_action"), ["always", "common", "build", "print_action"]);
    assert_eq!(get_command_ancestry("query"), ["always", "common", "query"]);
    assert_eq!(get_command_ancestry("version"), ["always", "common", "version"]);
}

#[test]
fn options_of_other_commands_do_not_leak() {
    // query-only flags
    for flag in ["--order_output=no", "--keep_going", "--null"] {
        let name = flag.trim_start_matches("--").split('=').next().unwrap();
        let accepted = accepted_by(name);
        assert_eq!(parse("build", &[flag]).is_ok(), accepted.contains(&"build"), "build {flag}");
    }
    assert!(parse("build", &["--order_output=no"]).is_err());
    assert!(parse("query", &["--order_output=no"]).is_ok());
    // run/mobile-install/start flags are not build flags
    assert!(parse("build", &["--run_in_cwd"]).is_err());
    assert!(parse("build", &["--script_path=x"]).is_err());
    assert!(parse("run", &["--script_path=x"]).is_ok());
    // clean-only
    assert!(parse("build", &["--expunge"]).is_err());
    assert!(parse("clean", &["--expunge"]).is_ok());
    // no ghost `--help`/`--start`/`--run` flag
    for cmd in ["build", "test", "query", "info", "clean", "help", "version"] {
        assert!(parse(cmd, &["--help"]).is_err(), "{cmd} --help must be rejected");
    }
    assert!(!ALL_COMMAND_OPTIONS.iter().any(|d| d.name == "help"));
    // `--run` (RunOptions) and `--start` (AdbOptions) belong to their own commands only
    assert_eq!(accepted_by("run"), vec!["run"]);
    assert_eq!(accepted_by("start"), vec!["mobile-install"]);
    assert!(parse("build", &["--run"]).is_err());
    assert!(parse("build", &["--start=WARM"]).is_err());
}

#[test]
fn every_command_option_belongs_to_a_class_of_each_of_its_commands() {
    for d in ALL_COMMAND_OPTIONS {
        assert!(!d.commands.is_empty(), "{} belongs to no command", d.name);
        for c in d.commands {
            let (_, _, all) = COMMAND_OPTION_CLASSES.iter().find(|e| e.0 == *c).unwrap_or_else(|| panic!("command {c}"));
            assert!(all.contains(&d.java_class), "{}: class {} not in {c}", d.name, d.java_class);
            assert!(is_command_allowed(c, d));
        }
    }
    // the converse: a command accepts exactly the options of its classes
    for (cmd, _, all) in COMMAND_OPTION_CLASSES {
        for d in ALL_COMMAND_OPTIONS {
            assert_eq!(all.contains(&d.java_class), d.commands.contains(cmd), "{} for {cmd}", d.name);
        }
    }
}

#[test]
fn common_rc_options_unsupported_by_the_command_are_ignored_but_unknown_ones_are_not() {
    let args: Vec<String> = ["--expunge", "--jobs=3", "--order_output", "no", "--color=no", "--totally_bogus"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (kept, ignored) = split_unsupported_common_args("info", &args);
    assert_eq!(ignored, ["--expunge", "--order_output", "no"]);
    assert_eq!(kept, ["--jobs=3", "--color=no", "--totally_bogus"]);
    let (kept, ignored) = split_unsupported_common_args("clean", &args);
    assert_eq!(ignored, ["--order_output", "no"]);
    assert!(kept.contains(&"--expunge".to_string()));
}
