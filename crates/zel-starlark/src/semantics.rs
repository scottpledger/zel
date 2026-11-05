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

//! Starlark language semantics flags mirroring Bazel's `BuildLanguageOptions`
//! and `StarlarkSemantics`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StarlarkSemantics {
    // Core Language, Types, and Syntax Flags
    pub experimental_starlark_type_syntax: bool,
    pub experimental_starlark_types: bool,
    pub experimental_starlark_static_type_checking: bool,
    pub experimental_starlark_dynamic_type_checking: bool,
    pub experimental_starlark_type_checking: String,
    pub experimental_starlark_types_allowed_paths: Vec<String>,
    pub experimental_enable_starlark_set: bool,
    pub incompatible_enforce_starlark_utf8: bool,
    pub internal_starlark_utf_8_byte_strings: bool,
    pub max_computation_steps: u64,
    pub force_starlark_stack_trace: bool,

    // Depset and Collection Semantics Flags
    pub incompatible_always_check_depset_elements: bool,
    pub nested_set_depth_limit: u32,
    pub nested_set_count_limit: u32,
    pub nested_set_count_limit_allowlist: Vec<String>,

    // Build & Macro Dialect Semantics Flags
    pub experimental_enable_first_class_macros: bool,
    pub incompatible_symbolic_macro_strict_attrs: bool,
    pub incompatible_existing_rules_default_omit_macro_internals: bool,
    pub experimental_bzl_visibility: bool,
    pub check_bzl_visibility: bool,
    pub allow_experimental_loads: bool,
    pub incompatible_stop_exporting_build_file_path: bool,
    pub incompatible_no_implicit_file_export: bool,
    pub incompatible_no_rule_outputs_param: bool,
    pub incompatible_disallow_empty_glob: bool,
    pub incompatible_fail_on_unknown_attributes: bool,
    pub incompatible_resolve_select_keys_eagerly: bool,
    pub incompatible_simplify_unconditional_selects_in_rule_attrs: bool,
    pub incompatible_unambiguous_label_stringification: bool,

    // Builtins Injection & Migration Flags
    pub experimental_builtins_bzl_path: String,
    pub experimental_builtins_injection_override: Vec<String>,
    pub experimental_builtins_dummy: bool,
    pub experimental_google_legacy_api: bool,
    pub experimental_platforms_api: bool,
    pub incompatible_enable_deprecated_label_apis: bool,
    pub incompatible_disable_target_default_provider_fields: bool,
    pub incompatible_run_shell_command_string: bool,
    pub incompatible_require_mnemonic_for_run_actions: bool,
    pub incompatible_no_attr_license: bool,
    pub incompatible_package_group_has_public_syntax: bool,
    pub incompatible_fix_package_group_reporoot_syntax: bool,
    pub incompatible_allow_tags_propagation: bool,
    pub incompatible_require_matching_aspect_hints_providers: bool,
    pub incompatible_disable_starlark_host_transitions: bool,
    pub incompatible_disable_objc_library_transition: bool,
    pub incompatible_disable_transitions_on: Vec<String>,
    pub add_go_exec_groups_to_binary_rules: bool,
    pub incompatible_enable_proto_toolchain_resolution: bool,
    pub incompatible_disable_non_executable_java_binary: bool,
    pub experimental_rule_extension_api: bool,
    pub experimental_dormant_deps: bool,
    pub incompatible_disallow_ctx_resolve_tools: bool,
    pub incompatible_check_external_repo_source_dir_package_boundary: bool,
    pub incompatible_locations_prefers_executable: bool,
    pub experimental_repository_ctx_execute_wasm: bool,
    pub experimental_repository_ctx_wasm_compilation: bool,
    pub max_bzl_file_size: i64,
    pub soft_max_bzl_file_size: i64,
    pub bzl_file_size_limit_allowlist: Vec<String>,
    pub experimental_cc_skylark_api_enabled_packages: Vec<String>,
    pub experimental_enable_android_migration_apis: bool,
    pub experimental_isolated_extension_usages: bool,
    pub experimental_cc_shared_library: bool,
    pub experimental_repo_remote_exec: bool,
    pub experimental_sibling_repository_layout: bool,
    pub internal_starlark_flag_test_canary: bool,
    pub incompatible_java_info_merge_runtime_module_flags: bool,
    pub experimental_single_package_toolchain_binding: bool,

    // Runtime flags
    pub print_test_marker: bool,
    pub allow_recursion: bool,
}

impl Default for StarlarkSemantics {
    fn default() -> Self {
        Self {
            experimental_starlark_type_syntax: false,
            experimental_starlark_types: false,
            experimental_starlark_static_type_checking: false,
            experimental_starlark_dynamic_type_checking: false,
            experimental_starlark_type_checking: "off".to_string(),
            experimental_starlark_types_allowed_paths: Vec::new(),
            experimental_enable_starlark_set: true,
            incompatible_enforce_starlark_utf8: true,
            internal_starlark_utf_8_byte_strings: false,
            max_computation_steps: 0,
            force_starlark_stack_trace: false,

            incompatible_always_check_depset_elements: true,
            nested_set_depth_limit: 3500,
            nested_set_count_limit: 0,
            nested_set_count_limit_allowlist: Vec::new(),

            experimental_enable_first_class_macros: false,
            incompatible_symbolic_macro_strict_attrs: true,
            incompatible_existing_rules_default_omit_macro_internals: true,
            experimental_bzl_visibility: true,
            check_bzl_visibility: true,
            allow_experimental_loads: false,
            incompatible_stop_exporting_build_file_path: true,
            incompatible_no_implicit_file_export: true,
            incompatible_no_rule_outputs_param: true,
            incompatible_disallow_empty_glob: false,
            incompatible_fail_on_unknown_attributes: true,
            incompatible_resolve_select_keys_eagerly: true,
            incompatible_simplify_unconditional_selects_in_rule_attrs: true,
            incompatible_unambiguous_label_stringification: false,

            experimental_builtins_bzl_path: String::new(),
            experimental_builtins_injection_override: Vec::new(),
            experimental_builtins_dummy: false,
            experimental_google_legacy_api: false,
            experimental_platforms_api: false,
            incompatible_enable_deprecated_label_apis: false,
            incompatible_disable_target_default_provider_fields: true,
            incompatible_run_shell_command_string: false,
            incompatible_require_mnemonic_for_run_actions: false,
            incompatible_no_attr_license: true,
            incompatible_package_group_has_public_syntax: true,
            incompatible_fix_package_group_reporoot_syntax: true,
            incompatible_allow_tags_propagation: false,
            incompatible_require_matching_aspect_hints_providers: false,
            incompatible_disable_starlark_host_transitions: true,
            incompatible_disable_objc_library_transition: true,
            incompatible_disable_transitions_on: Vec::new(),
            add_go_exec_groups_to_binary_rules: false,
            incompatible_enable_proto_toolchain_resolution: false,
            incompatible_disable_non_executable_java_binary: false,
            experimental_rule_extension_api: false,
            experimental_dormant_deps: false,
            incompatible_disallow_ctx_resolve_tools: false,
            incompatible_check_external_repo_source_dir_package_boundary: false,
            incompatible_locations_prefers_executable: false,
            experimental_repository_ctx_execute_wasm: false,
            experimental_repository_ctx_wasm_compilation: false,
            max_bzl_file_size: -1,
            soft_max_bzl_file_size: -1,
            bzl_file_size_limit_allowlist: Vec::new(),
            experimental_cc_skylark_api_enabled_packages: Vec::new(),
            experimental_enable_android_migration_apis: false,
            experimental_isolated_extension_usages: false,
            experimental_cc_shared_library: false,
            experimental_repo_remote_exec: false,
            experimental_sibling_repository_layout: false,
            internal_starlark_flag_test_canary: false,
            incompatible_java_info_merge_runtime_module_flags: false,
            experimental_single_package_toolchain_binding: false,

            print_test_marker: false,
            allow_recursion: false,
        }
    }
}
