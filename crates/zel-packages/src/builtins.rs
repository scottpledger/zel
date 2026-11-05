//! Bazel's Starlark builtins (`@_builtins`): the real `builtins_bzl` tree embedded in
//! `zel-bazel-tools`, evaluated by this loader. Its `exports.bzl` yields `exported_toplevels`
//! (`cc_common`, `java_common`, `apple_common`, `py_internal`, ...), which are injected as
//! predeclared symbols of every `.bzl` file (mirrors `StarlarkBuiltinsFunction.java`).

use crate::lookup::RepoResolver;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use zel_cmdline::{RepositoryMapping, RepositoryName};
use zel_starlark::{StructValue, Value};
use zel_vfs::PathFragment;

pub(crate) const BUILTINS_REPO: &str = "_builtins";

/// Methods of `_builtins.internal.cc_common` (`CcModule.java`), not yet ported.
const CC_COMMON_NATIVES: &[&str] = &[
    "get_tool_for_action",
    "get_execution_requirements",
    "action_is_enabled",
    "get_memory_inefficient_command_line",
    "get_environment_variables",
    "empty_variables",
    "legacy_cc_flags_make_variable_do_not_use",
    "check_experimental_cc_shared_library",
    "incompatible_disable_objc_library_transition",
    "add_go_exec_groups_to_binary_rules",
    "get_tool_requirement_for_action",
    "implementation_deps_allowed_by_allowlist",
    "do_not_use_tools_cpp_compiler_present",
];

/// Methods of `_builtins.internal.py_builtins` (`PyBuiltins.java`), not yet ported.
const PY_BUILTINS_NATIVES: &[&str] = &[
    "add_py_extra_pseudo_action",
    "are_action_listeners_enabled",
    "copy_without_caching",
    "create_repo_mapping_manifest",
    "create_sources_only_manifest",
    "declare_constant_metadata_file",
    "expand_location_and_make_variables",
    "get_current_os_name",
    "get_label_repo_runfiles_path",
    "get_rule_name",
    "is_bzlmod_enabled",
    "is_singleton_depset",
    "merge_runfiles_with_generated_inits_empty_files_supplier",
    "regex_match",
];

/// Methods of `_builtins.internal.java_common_internal_do_not_use` (`JavaCommonInternal`),
/// not yet ported.
const JAVA_COMMON_INTERNAL_NATIVES: &[&str] = &[
    "create_compilation_action",
    "create_header_compilation_action",
    "create_jvm_compile_action",
    "_check_java_toolchain_is_declared_on_rule",
    "check_provider_instances",
    "collect_native_deps_dirs",
    "expand_java_opts",
    "get_runtime_classpath_for_archive",
    "_google_legacy_api_enabled",
    "incompatible_disable_non_executable_java_binary",
    "_incompatible_java_info_merge_runtime_module_flags",
    "target_kind",
];

fn mk_struct(fields: Vec<(&str, Value)>) -> Value {
    Value::Struct(Arc::new(StructValue::new(
        fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    )))
}

fn stubs(ns: &str, names: &[&str]) -> Value {
    mk_struct(
        names
            .iter()
            .map(|n| (*n, Value::BuiltinFunction(format!("host:builtins.{ns}.{n}"))))
            .collect(),
    )
}

/// The `_builtins` module visible to builtins files: `internal` natives, `toplevel` (the
/// predeclared symbols before overriding) and `native`.
pub(crate) fn builtins_module(predeclared: &BTreeMap<String, Value>) -> Value {
    let internal = mk_struct(vec![
        ("cc_internal", zel_builtins::cc_internal::internal_struct()),
        ("cc_common", stubs("cc_common", CC_COMMON_NATIVES)),
        (
            "apple_common",
            mk_struct(vec![
                ("DottedVersion", Value::Provider { name: "DottedVersion".into() }),
                ("dotted_version", Value::BuiltinFunction("host:builtins.apple_common.dotted_version".into())),
            ]),
        ),
        ("py_builtins", stubs("py_builtins", PY_BUILTINS_NATIVES)),
        ("java_common_internal_do_not_use", stubs("java_common_internal_do_not_use", JAVA_COMMON_INTERNAL_NATIVES)),
    ]);
    let toplevel = Value::Struct(Arc::new(StructValue::new(predeclared.clone())));
    let native = predeclared.get("native").cloned().unwrap_or(Value::None);
    mk_struct(vec![("internal", internal), ("toplevel", toplevel), ("native", native)])
}

/// Directory holding the materialised `@_builtins` tree (content-addressed under the temp dir).
pub(crate) fn materialize() -> Result<PathFragment, String> {
    let mut h: u64 = 0xcbf29ce484222325;
    for (p, b) in zel_bazel_tools::builtins_files() {
        for byte in p.bytes().chain(b.iter().copied()) {
            h ^= byte as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    let dir = std::env::temp_dir().join(format!("zel-builtins-{h:016x}"));
    if !dir.join("exports.bzl").exists() {
        let tmp = std::env::temp_dir().join(format!("zel-builtins-{h:016x}.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        zel_bazel_tools::materialize_builtins(&tmp)?;
        if std::fs::rename(&tmp, &dir).is_err() {
            let _ = std::fs::remove_dir_all(&tmp);
        }
    }
    Ok(PathFragment::create(&dir.to_string_lossy()))
}

/// Resolver decorator that serves the `@_builtins` repository.
pub(crate) struct BuiltinsResolver {
    pub inner: Arc<dyn RepoResolver>,
    pub root: PathFragment,
}

impl RepoResolver for BuiltinsResolver {
    fn repo_root(&self, repo: &RepositoryName) -> Option<PathFragment> {
        if repo.name() == BUILTINS_REPO {
            return Some(self.root.clone());
        }
        self.inner.repo_root(repo)
    }

    fn mapping(&self, owner: &RepositoryName) -> RepositoryMapping {
        if owner.name() == BUILTINS_REPO {
            let mut m: HashMap<String, RepositoryName> = HashMap::new();
            m.insert(BUILTINS_REPO.to_string(), owner.clone());
            return RepositoryMapping::new(m, owner.clone());
        }
        self.inner.mapping(owner)
    }
}
