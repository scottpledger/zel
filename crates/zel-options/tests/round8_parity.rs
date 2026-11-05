//! Round-8 regressions: label converter, converters ported from HEAD, invocation-policy text parser.
use zel_options::{canonicalize_flags, convert_enum, InvocationPolicy, ALL_COMMAND_OPTIONS, OptionType};

fn canon(args: &[&str]) -> Result<Vec<String>, String> {
    let v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    canonicalize_flags(&v).map_err(|e| e.message)
}

#[test]
fn label_flags_use_options_label_converter() {
    let e = canon(&["--for_command=build", "--", "--proto_compiler=/abs/p"]).unwrap_err();
    assert!(e.contains("invalid target name '/abs/p': target names may not start with '/'"), "{e}");
    let e = canon(&["--for_command=build", "--", "--platforms=/abs/p"]).unwrap_err();
    assert!(e.contains("invalid target name '/abs/p': target names may not start with '/'"), "{e}");
    assert_eq!(canon(&["--for_command=build", "--", "--proto_compiler=foo/bar:baz"]).unwrap(), vec!["--proto_compiler=foo/bar:baz"]);
    assert!(canon(&["--for_command=build", "--", "--proto_compiler=//foo/..."]).is_err());
}

#[test]
fn package_name_flags_are_validated() {
    let e = canon(&["--for_command=build", "--", "--deleted_packages=/abs/p"]).unwrap_err();
    assert!(e.contains("invalid package name '/abs/p': package names may not start with '/'"), "{e}");
    assert!(canon(&["--for_command=build", "--", "--experimental_lazy_macro_expansion_packages=*"]).is_ok());
}

#[test]
fn residue_is_rejected() {
    let e = canon(&["--for_command=build", "--", "//foo:bar"]).unwrap_err();
    assert!(e.contains("Unrecognized arguments: //foo:bar"), "{e}");
}

#[test]
fn scanning_parallelism_minimum_is_zero() {
    assert!(canon(&["--for_command=build", "--", "--experimental_include_scanning_parallelism=0"]).is_ok());
    let e = canon(&["--for_command=build", "--", "--experimental_include_scanning_parallelism=-5"]).unwrap_err();
    assert!(e.contains("must be at least 0"), "{e}");
}

#[test]
fn tool_command_line_must_be_a_valid_proto_when_base64() {
    assert!(canon(&["--for_command=build", "--", "--experimental_tool_command_line=hello"]).is_ok());
    let e = canon(&["--for_command=build", "--", "--experimental_tool_command_line=9999999999"]).unwrap_err();
    assert!(e.contains("Malformed value of --experimental_tool_command_line: 9999999999"), "{e}");
}

#[test]
fn skyframe_enum_values_come_from_9_3_2() {
    let spec = ALL_COMMAND_OPTIONS
        .iter()
        .find_map(|d| match (&d.option_type, d.name) {
            (OptionType::Enum(s), "skyframe") => Some(*s),
            _ => None,
        })
        .unwrap();
    assert_eq!(spec.values, &["off", "summary", "count", "value", "deps", "rdeps", "function_graph", "active_directories", "active_directories_frontier_deps"]);
    let e = convert_enum(spec, "bogus").unwrap_err().message;
    assert_eq!(
        e,
        "Not a valid Skyframe Dump option: 'bogus' (should be off, summary, count, value, deps, rdeps, function_graph, active_directories or active_directories_frontier_deps)"
    );
}

#[test]
fn policy_text_parser_merges_like_textformat() {
    // singular overwrite + message merge + oneof last-wins, as TextFormat.merge with ALLOW_SINGULAR_OVERWRITES
    let p = InvocationPolicy::parse(
        r#"flag_policies{flag_name:"a" flag_name:"jobs" set_value{flag_value:"5"} set_value{flag_value:"6" behavior:APPEND}}"#,
    )
    .unwrap();
    assert_eq!(p.flag_policies[0].flag_name, "jobs");
    let p = InvocationPolicy::parse(r#"flag_policies{flag_name:"jobs" set_value{flag_value:"5"} use_default{}}"#).unwrap();
    assert!(matches!(p.flag_policies[0].operation, Some(zel_options::PolicyOperation::UseDefault(_))));
}
