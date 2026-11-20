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

/// Methods of `_builtins.internal.cc_common` (`CcModule.java`).
pub(crate) const CC_COMMON_NATIVES: &[&str] = &[
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
    "create_compile_action",
    "implementation_deps_allowed_by_allowlist",
    "register_swig_action",
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
        fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )))
}

fn stubs(ns: &str, names: &[&str]) -> Value {
    mk_struct(
        names
            .iter()
            .map(|n| {
                (
                    *n,
                    Value::BuiltinFunction(format!("host:builtins.{ns}.{n}")),
                )
            })
            .collect(),
    )
}

/// The `_builtins` module visible to builtins files: `internal` natives, `toplevel` (the
/// predeclared symbols before overriding) and `native`.
pub(crate) fn builtins_module(predeclared: &BTreeMap<String, Value>) -> Value {
    let mut cc_common_fields: BTreeMap<String, Value> = CC_COMMON_NATIVES
        .iter()
        .map(|n| {
            (
                (*n).to_string(),
                Value::BuiltinFunction(format!("host:builtins.cc_common.{n}")),
            )
        })
        .collect();
    cc_common_fields.insert(
        "do_not_use_tools_cpp_compiler_present".to_string(),
        Value::None,
    );
    let internal = mk_struct(vec![
        ("cc_internal", zel_builtins::cc_internal::internal_struct()),
        (
            "cc_common",
            Value::Struct(Arc::new(StructValue::new(cc_common_fields))),
        ),
        (
            "apple_common",
            mk_struct(vec![
                (
                    "DottedVersion",
                    Value::Provider {
                        name: "DottedVersion".into(),
                    },
                ),
                (
                    "dotted_version",
                    Value::BuiltinFunction("host:builtins.apple_common.dotted_version".into()),
                ),
            ]),
        ),
        ("py_builtins", stubs("py_builtins", PY_BUILTINS_NATIVES)),
        (
            "java_common_internal_do_not_use",
            stubs(
                "java_common_internal_do_not_use",
                JAVA_COMMON_INTERNAL_NATIVES,
            ),
        ),
    ]);
    let toplevel = Value::Struct(Arc::new(StructValue::new(predeclared.clone())));
    let native = predeclared.get("native").cloned().unwrap_or(Value::None);
    mk_struct(vec![
        ("internal", internal),
        ("toplevel", toplevel),
        ("native", native),
    ])
}

fn builtins_hash() -> u64 {
    static HASH: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *HASH.get_or_init(|| {
        let mut h: u64 = 0xcbf29ce484222325;
        for (p, b) in zel_bazel_tools::builtins_files() {
            for byte in p.bytes().chain(b.iter().copied()) {
                h ^= byte as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
        }
        h
    })
}

/// Directory holding the materialised `@_builtins` tree (content-addressed under the temp dir).
pub(crate) fn materialize() -> Result<PathFragment, String> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let h = builtins_hash();
    let dir = std::env::temp_dir().join(format!("zel-builtins-{h:016x}"));
    if dir.join("exports.bzl").exists() {
        return Ok(PathFragment::create(&dir.to_string_lossy()));
    }
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !dir.join("exports.bzl").exists() {
        let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "zel-builtins-{h:016x}.{}.{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        zel_bazel_tools::materialize_builtins(&tmp)?;
        if std::fs::rename(&tmp, &dir).is_err() {
            let _ = std::fs::remove_dir_all(&tmp);
            if !dir.join("exports.bzl").exists() {
                return Err(format!(
                    "failed to materialize builtins at {}",
                    dir.display()
                ));
            }
        }
    }
    Ok(PathFragment::create(&dir.to_string_lossy()))
}

/// Resolver decorator that serves the `@_builtins` repository.
pub(crate) struct BuiltinsResolver {
    pub inner: Arc<dyn RepoResolver>,
    pub root: PathFragment,
}

/// Apparent repository dependencies declared in `@bazel_tools`'s `MODULE.tools`.
const BAZEL_TOOLS_DEFAULT_REPOS: &[(&str, &str)] = &[
    ("bazel_tools", "bazel_tools"),
    ("rules_license", "rules_license"),
    ("buildozer", "buildozer"),
    ("platforms", "platforms"),
    ("zlib", "zlib"),
    ("local_config_xcode", "local_config_xcode"),
    ("local_jdk", "local_jdk"),
    ("remote_java_tools", "remote_java_tools"),
    ("remote_coverage_tools", "remote_coverage_tools"),
    ("buildozer_binary", "buildozer_binary"),
    ("com_google_protobuf", "protobuf"),
    ("rules_java", "rules_java"),
    ("rules_cc", "rules_cc"),
    ("rules_python", "rules_python"),
    ("rules_shell", "rules_shell"),
    ("apple_support", "apple_support"),
    ("local_config_winsdk", "local_config_winsdk"),
];

impl RepoResolver for BuiltinsResolver {
    fn repo_root(&self, repo: &RepositoryName) -> Option<PathFragment> {
        if !repo.is_visible() {
            return None;
        }
        if repo.name() == BUILTINS_REPO {
            return Some(self.root.clone());
        }
        self.inner.repo_root(repo)
    }

    fn mapping(&self, owner: &RepositoryName) -> RepositoryMapping {
        if owner.name() == BUILTINS_REPO {
            let mut m: HashMap<String, RepositoryName> = BAZEL_TOOLS_DEFAULT_REPOS
                .iter()
                .map(|(apparent, canon)| {
                    (
                        (*apparent).to_string(),
                        RepositoryName::create_unvalidated(*canon),
                    )
                })
                .collect();
            for (apparent, canon) in self.inner.mapping(&RepositoryName::bazel_tools()).entries() {
                m.insert(apparent.clone(), canon.clone());
            }
            m.insert(String::new(), RepositoryName::main());
            m.insert(BUILTINS_REPO.to_string(), owner.clone());
            return RepositoryMapping::new(m, owner.clone());
        }
        if owner.name() == "bazel_tools" {
            let inner_map = self.inner.mapping(owner);
            let has_bazel_tools = inner_map.entries().any(|(k, _)| k == "bazel_tools");
            if !has_bazel_tools && !inner_map.is_fallback() {
                let mut m: HashMap<String, RepositoryName> = BAZEL_TOOLS_DEFAULT_REPOS
                    .iter()
                    .map(|(apparent, canon)| {
                        (
                            (*apparent).to_string(),
                            RepositoryName::create_unvalidated(*canon),
                        )
                    })
                    .collect();
                for (apparent, canon) in inner_map.entries() {
                    m.insert(apparent.clone(), canon.clone());
                }
                return RepositoryMapping::new(m, owner.clone());
            }
            return inner_map;
        }
        self.inner.mapping(owner)
    }

    fn module_name_and_version(&self, repo: &RepositoryName) -> Option<(String, String)> {
        self.inner.module_name_and_version(repo)
    }

    fn autoload_module_repo(&self, module_name: &str, repo_name: &str) -> Option<RepositoryName> {
        self.inner.autoload_module_repo(module_name, repo_name)
    }
}
