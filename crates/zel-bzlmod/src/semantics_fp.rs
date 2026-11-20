//! `BuildLanguageOptions.stableFingerprint`: SHA-256 over the enabled boolean Starlark semantics
//! flags and the string forms of the other semantics values.
//!
//! The boolean defaults are the `+`/`-` prefix of the flag's `StarlarkSemantics` key (the declared
//! default); a flag given on the command line overrides it.

use crate::hash::Fingerprint;
use std::collections::BTreeMap;

/// `(key name, default)` of the boolean keys of `BuildLanguageOptions.setFlags` / `StarlarkSemantics`.
const BOOLS: &[(&str, bool)] = &[
    ("incompatible_stop_exporting_language_modules", false),
    ("incompatible_allow_tags_propagation", true),
    ("incompatible_disable_autoloads_in_main_repo", true),
    ("experimental_builtins_dummy", false),
    ("experimental_bzl_visibility", true),
    ("allow_experimental_loads", false),
    ("check_bzl_visibility", true),
    ("experimental_enable_android_migration_apis", false),
    ("experimental_single_package_toolchain_binding", false),
    ("experimental_enable_first_class_macros", true),
    ("experimental_enable_scl_dialect", true),
    ("experimental_isolated_extension_usages", false),
    ("incompatible_no_implicit_watch_label", true),
    ("experimental_google_legacy_api", false),
    ("experimental_platforms_api", false),
    ("experimental_cc_shared_library", false),
    ("experimental_repo_remote_exec", false),
    ("experimental_disable_external_package", false),
    ("experimental_sibling_repository_layout", false),
    ("incompatible_always_check_depset_elements", true),
    (
        "incompatible_check_external_repo_source_dir_package_boundary",
        false,
    ),
    ("incompatible_disallow_empty_glob", true),
    ("incompatible_package_group_has_public_syntax", true),
    ("incompatible_fix_package_group_reporoot_syntax", true),
    ("incompatible_java_info_merge_runtime_module_flags", false),
    ("incompatible_no_attr_license", true),
    ("incompatible_no_implicit_file_export", false),
    ("incompatible_no_rule_outputs_param", false),
    ("incompatible_run_shell_command_string", true),
    ("print_test_marker", false),
    ("incompatible_do_not_split_linking_cmdline", true),
    ("incompatible_use_cc_configure_from_rules", false),
    ("incompatible_unambiguous_label_stringification", true),
    ("incompatible_disable_starlark_host_transitions", false),
    ("incompatible_disable_objc_library_transition", true),
    ("add_go_exec_groups_to_binary_rules", false),
    ("incompatible_fail_on_unknown_attributes", true),
    ("incompatible_enable_proto_toolchain_resolution", true),
    ("incompatible_disable_non_executable_java_binary", false),
    ("incompatible_disable_target_default_provider_fields", false),
    ("experimental_rule_extension_api", true),
    ("experimental_dormant_deps", false),
    ("experimental_starlark_type_syntax", true),
    ("experimental_starlark_type_checking", false),
    ("incompatible_enable_deprecated_label_apis", true),
    ("incompatible_stop_exporting_build_file_path", false),
    ("incompatible_disallow_ctx_resolve_tools", true),
    (
        "incompatible_simplify_unconditional_selects_in_rule_attrs",
        true,
    ),
    ("incompatible_locations_prefers_executable", true),
    ("incompatible_resolve_select_keys_eagerly", false),
    ("experimental_enable_starlark_set", true),
    ("internal_bazel_only_utf_8_byte_strings", true),
    ("experimental_repository_ctx_execute_wasm", false),
    ("force_starlark_stack_trace", false),
];

/// Non-boolean keys with their default string forms (`Object.toString()` of the value).
const OTHERS: &[(&str, &str)] = &[
    ("experimental_builtins_bzl_path", "%bundled%"),
    ("experimental_builtins_injection_override", "[]"),
    ("experimental_starlark_types", "[]"),
    ("incompatible_autoload_externally", "[]"),
    ("incompatible_disable_transitions_on", "[]"),
    ("incompatible_enforce_starlark_utf8", "WARNING"),
    ("max_computation_steps", "0"),
    ("nested_set_depth_limit", "3500"),
    ("repositories_without_autoloads", "[]"),
];

/// `overrides`: boolean flag name -> value given on the command line.
pub fn stable_fingerprint(overrides: &BTreeMap<String, bool>) -> Vec<u8> {
    stable_fingerprint_with_others(overrides, &BTreeMap::new())
}

/// Like [`stable_fingerprint`], also incorporating non-boolean `StarlarkSemantics` key overrides
/// (e.g. `incompatible_autoload_externally`, `repositories_without_autoloads`).
pub fn stable_fingerprint_with_others(
    overrides: &BTreeMap<String, bool>,
    other_overrides: &BTreeMap<String, String>,
) -> Vec<u8> {
    let mut enabled: Vec<&str> = BOOLS
        .iter()
        .filter(|(k, d)| overrides.get(*k).copied().unwrap_or(*d))
        .map(|(k, _)| *k)
        .collect();
    enabled.sort();
    let mut fp = Fingerprint::new();
    fp.add_strings(&enabled);
    let mut others: Vec<(&str, &str)> = OTHERS
        .iter()
        .map(|(k, d)| {
            (
                *k,
                other_overrides.get(*k).map(String::as_str).unwrap_or(*d),
            )
        })
        .collect();
    others.sort_by_key(|(k, _)| *k);
    fp.add_int(others.len() as i32);
    for (k, v) in others {
        fp.add_string(k);
        fp.add_string(v);
    }
    fp.digest().to_vec()
}

/// The boolean semantics flags present in `parsed`.
pub fn bool_overrides(parsed: &zel_options::ParsedCommandOptions) -> BTreeMap<String, bool> {
    BOOLS
        .iter()
        .filter_map(|(k, _)| parsed.get_bool(k).map(|v| (k.to_string(), v)))
        .collect()
}

/// Non-boolean `StarlarkSemantics` flags present in `parsed`, formatted like Java's
/// `Object.toString()` (`ImmutableList.toString()` -> `[a, b]`).
pub fn other_overrides(parsed: &zel_options::ParsedCommandOptions) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for key in [
        "incompatible_autoload_externally",
        "repositories_without_autoloads",
        "incompatible_disable_transitions_on",
    ] {
        if let Some(d) = parsed.parsed_options.get(key) {
            let mut items: Vec<String> = match &d.value {
                zel_options::OptionValue::List(list) => list
                    .iter()
                    .filter_map(|v| match v {
                        zel_options::OptionValue::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect(),
                zel_options::OptionValue::String(s) => {
                    if s.is_empty() {
                        Vec::new()
                    } else {
                        s.split(',').map(str::to_string).collect()
                    }
                }
                _ => Vec::new(),
            };
            items.sort();
            items.dedup();
            out.insert(key.to_string(), format!("[{}]", items.join(", ")));
        }
    }
    if let Some(v) = parsed.get_str("experimental_builtins_bzl_path") {
        out.insert("experimental_builtins_bzl_path".to_string(), v.to_string());
    }
    if let Some(v) = parsed.get_int("max_computation_steps") {
        out.insert("max_computation_steps".to_string(), v.to_string());
    }
    if let Some(v) = parsed.get_int("nested_set_depth_limit") {
        out.insert("nested_set_depth_limit".to_string(), v.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autoload_flags_affect_semantics_fingerprint_and_reset_clears() {
        let default_fp = stable_fingerprint(&BTreeMap::new());

        let parsed_on = zel_options::ParsedCommandOptions::parse_for_command(
            "build",
            &[
                "--incompatible_autoload_externally=".to_string(),
                "--incompatible_autoload_externally=+@rules_cc".to_string(),
            ],
        )
        .unwrap();
        let others_on = other_overrides(&parsed_on);
        assert_eq!(
            others_on
                .get("incompatible_autoload_externally")
                .map(String::as_str),
            Some("[+@rules_cc]")
        );
        let fp_on = stable_fingerprint_with_others(&bool_overrides(&parsed_on), &others_on);
        assert_ne!(fp_on, default_fp);

        // Resetting with a trailing empty value returns to the default fingerprint.
        let parsed_reset = zel_options::ParsedCommandOptions::parse_for_command(
            "build",
            &[
                "--incompatible_autoload_externally=+@rules_cc".to_string(),
                "--incompatible_autoload_externally=".to_string(),
            ],
        )
        .unwrap();
        let others_reset = other_overrides(&parsed_reset);
        assert_eq!(
            others_reset
                .get("incompatible_autoload_externally")
                .map(String::as_str),
            Some("[]")
        );
        let fp_reset =
            stable_fingerprint_with_others(&bool_overrides(&parsed_reset), &others_reset);
        assert_eq!(fp_reset, default_fp);

        // --repositories_without_autoloads also changes the fingerprint.
        let parsed_rwa = zel_options::ParsedCommandOptions::parse_for_command(
            "build",
            &["--repositories_without_autoloads=foo,bar".to_string()],
        )
        .unwrap();
        let others_rwa = other_overrides(&parsed_rwa);
        assert_eq!(
            others_rwa
                .get("repositories_without_autoloads")
                .map(String::as_str),
            Some("[bar, foo]")
        );
        let fp_rwa = stable_fingerprint_with_others(&bool_overrides(&parsed_rwa), &others_rwa);
        assert_ne!(fp_rwa, default_fp);
    }
}
