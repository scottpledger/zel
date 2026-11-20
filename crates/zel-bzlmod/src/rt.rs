//! Runtime for external repositories: `.bzl` loading, `repository_rule()`, `module_extension()`,
//! `tag_class()`, `attr.*`, repository fetching and module extension evaluation.
//! Mirrors `lib/bazel/repository/starlark` and `SingleExtensionEvalFunction`.

use crate::model::*;
use crate::resolve::{ExtensionId, Resolution};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use zel_starlark::{
    BigInt, EvalContext, StarlarkDict, StarlarkError, StarlarkList, StructValue, Value,
};

/// Command-line / environment configuration of the external repository subsystem.
#[derive(Clone, Debug)]
pub struct ExternalOptions {
    pub output_base: PathBuf,
    pub workspace_root: PathBuf,
    pub distdirs: Vec<PathBuf>,
    pub repository_cache: Option<PathBuf>,
    pub repo_contents_cache: Option<PathBuf>,
    pub disable_download: bool,
    /// `--override_repository=name=path` (apparent or canonical name -> local path).
    pub override_repository: BTreeMap<String, PathBuf>,
    pub downloader_rewriter: Option<crate::download::UrlRewriter>,
    /// `BuildLanguageOptions.stableFingerprint` of the command's Starlark semantics.
    pub semantics_fingerprint: Vec<u8>,
    pub download_retries: u32,
    pub client_env: BTreeMap<String, String>,
    pub vendor_dir: Option<PathBuf>,
    pub force: bool,
    /// `--nofetch`: only use repositories that are already in the output base.
    pub nofetch: bool,
    pub verbose: bool,
    pub experimental_repository_ctx_execute_wasm: bool,
    pub experimental_sibling_repository_layout: bool,
    pub incompatible_no_implicit_watch_label: bool,
    pub incompatible_use_cc_configure_from_rules_cc: bool,
    pub credential_helpers: Vec<(String, PathBuf)>,
}

impl Default for ExternalOptions {
    fn default() -> Self {
        Self {
            output_base: PathBuf::new(),
            workspace_root: PathBuf::new(),
            distdirs: Vec::new(),
            repository_cache: None,
            repo_contents_cache: None,
            disable_download: false,
            override_repository: BTreeMap::new(),
            downloader_rewriter: None,
            semantics_fingerprint: Vec::new(),
            download_retries: 5,
            client_env: BTreeMap::new(),
            vendor_dir: None,
            force: false,
            nofetch: false,
            verbose: false,
            experimental_repository_ctx_execute_wasm: false,
            experimental_sibling_repository_layout: false,
            incompatible_no_implicit_watch_label: true,
            incompatible_use_cc_configure_from_rules_cc: false,
            credential_helpers: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct AttrDef {
    pub kind: String,
    pub default: Option<AttrVal>,
    pub mandatory: bool,
    pub values: Vec<AttrVal>,
    pub allow_empty: bool,
}

#[derive(Clone)]
pub struct RuleDef {
    pub implementation: Value,
    pub attrs: Vec<(String, AttrDef)>,
    pub user_has_repo_mapping: bool,
    pub local: bool,
    pub environ: Vec<String>,
    pub configure: bool,
    /// Canonical label of the defining bzl file and exported symbol name.
    pub bzl_file: String,
    pub name: String,
}

#[derive(Clone)]
pub struct ExtensionDef {
    pub implementation: Value,
    pub tag_classes: Vec<(String, Vec<(String, AttrDef)>)>,
    pub environ: Vec<String>,
    pub os_dependent: bool,
    pub arch_dependent: bool,
    pub facts_version: u32,
    pub bzl_file: String,
    pub name: String,
    /// `file:line:col` of the `module_extension(...)` call.
    pub location: String,
}

/// Result of evaluating a module extension.
#[derive(Clone, Debug, Default)]
pub struct ExtensionResult {
    /// Generated repos: exported name -> spec (insertion ordered).
    pub generated: Vec<(String, RepoSpec)>,
    pub metadata: Option<ExtensionMetadata>,
    pub env_variables: BTreeMap<String, Option<String>>,
    pub recorded_file_inputs: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default)]
pub struct ExtensionMetadata {
    pub explicit_root_direct_deps: Option<Vec<String>>,
    pub explicit_root_direct_dev_deps: Option<Vec<String>>,
    pub use_all_repos: bool,
    /// `NO`, `REGULAR` or `DEV` (`UseAllRepos`).
    pub use_all: String,
    pub reproducible: bool,
    pub facts: Option<crate::json::Json>,
}

#[derive(Clone, Debug)]
pub struct UsedLockExt {
    pub key: String,
    pub factors: String,
    pub entry: crate::lockfile::LockExt,
    pub facts: Option<crate::json::Json>,
    pub facts_version: i64,
}

#[derive(Default)]
pub struct LockState {
    pub mode: Option<crate::lockfile::LockfileMode>,
    pub workspace: crate::lockfile::Lockfile,
    pub hidden: crate::lockfile::Lockfile,
    /// Entries (extension key, factors, entry, facts) produced or reused in this session.
    pub used: Vec<UsedLockExt>,
}

pub struct Rt {
    pub opts: ExternalOptions,
    pub lock: Mutex<LockState>,
    pub resolution: Arc<Resolution>,
    pub(crate) state: Mutex<RtState>,
    pub(crate) log: Mutex<Vec<String>>,
}

#[derive(Clone)]
pub(crate) struct DeferredRepoCall {
    pub name: String,
    pub bzl_file: String,
    pub rule_name: String,
    pub attr_defs: Vec<(String, AttrDef)>,
    pub kw: BTreeMap<String, AttrVal>,
}

#[derive(Default)]
pub(crate) struct RepoCollector {
    pub calls: Vec<DeferredRepoCall>,
    /// Call-site location (`file:line:col`) of each repository rule call in this extension.
    pub locs: BTreeMap<String, String>,
}

/// Execution phase active on the current Starlark evaluation call stack of [`Rt`].
///
/// Re-entrant `.bzl` loading triggered while evaluating an active `module_extension` or
/// `repository_rule` pushes a fresh `BzlInit` frame on top of the outer `ExtensionImpl` /
/// `RepoRuleImpl` frame and pops it on return (or error/unwind) via [`EvalPhaseGuard`].
pub(crate) enum EvalPhase {
    BzlInit(Arc<zel_starlark::ModuleEnv>),
    ExtensionImpl,
    RepoRuleImpl,
}

thread_local! {
    static EVAL_PHASE_STACK: std::cell::RefCell<Vec<EvalPhase>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

pub(crate) struct EvalPhaseGuard {
    active: bool,
}

impl EvalPhaseGuard {
    pub(crate) fn enter(phase: EvalPhase) -> Self {
        EVAL_PHASE_STACK.with(|stack| stack.borrow_mut().push(phase));
        Self { active: true }
    }
}

impl Drop for EvalPhaseGuard {
    fn drop(&mut self) {
        if self.active {
            self.active = false;
            EVAL_PHASE_STACK.with(|stack| {
                stack.borrow_mut().pop();
            });
        }
    }
}

#[derive(Default)]
pub(crate) struct RtState {
    pub rule_defs: Vec<RuleDef>,
    pub ext_defs: Vec<ExtensionDef>,
    pub ext_metadatas: Vec<ExtensionMetadata>,
    pub tag_class_defs: Vec<Vec<(String, AttrDef)>>,
    pub bzl_cache: HashMap<String, BTreeMap<String, Value>>,
    pub loading: Vec<String>,
    pub ext_results: HashMap<ExtensionId, ExtensionResult>,
    pub evaluating_exts: Vec<ExtensionId>,
    pub fetched: HashSet<String>,
    pub fetching: Vec<String>,
    /// Active collectors for repos created by calling repository rules inside an extension.
    pub collectors: Vec<RepoCollector>,
    pub ctxs: Vec<Option<crate::ctx::CtxData>>,
    pub exec_results: Vec<(i32, Vec<u8>, Vec<u8>)>,
    /// bzl key -> (sha256 of source, keys of directly loaded bzl files)
    pub bzl_deps: HashMap<String, (String, Vec<String>)>,
    /// Memoized transitive `.bzl` digests, parsed load lists, load-graph repo mappings, and
    /// repository rule metadata `(local, configure, environ)` for the lifetime of this `Rt`.
    pub bzl_transitive_digests: HashMap<String, Vec<u8>>,
    pub bzl_load_mappings: HashMap<String, Vec<(String, String, String)>>,
    pub bzl_loads_cache: HashMap<String, Vec<String>>,
    pub rule_meta_cache: HashMap<(String, String), (bool, bool, Vec<String>)>,
    pub force_configure: bool,
    /// `Label()` apparent-name lookups recorded while `.bzl` files load in this runtime.
    pub load_label_mappings: Vec<(String, String, String)>,
    /// In-flight background downloads (`block = False`) owned by this runtime.
    pub(crate) pending_downloads: Vec<Option<crate::ctx::PendingDownload>>,
    /// Package loader used for `.bzl` files that do not define repository rules / extensions
    /// (rule definitions such as rules_cc, which need the full rule-building builtins).
    pub loader: Option<Arc<zel_packages::PackageLoader>>,
    pub me: Weak<Rt>,
}

pub type RtResult<T> = Result<T, String>;

fn err<T>(m: impl Into<String>) -> Result<T, StarlarkError> {
    Err(StarlarkError(m.into()))
}

pub fn list_value(items: Vec<Value>) -> Value {
    Value::List(Arc::new(Mutex::new(StarlarkList::new(items))))
}

pub fn dict_value(items: Vec<(Value, Value)>) -> Value {
    let mut d = StarlarkDict::new();
    for (k, v) in items {
        let _ = d.insert(k, v);
    }
    Value::Dict(Arc::new(Mutex::new(d)))
}

pub fn str_value(s: impl Into<String>) -> Value {
    Value::String(s.into())
}

pub fn int_value(i: i64) -> Value {
    Value::Int(BigInt::from_i64(i))
}

pub fn struct_value(fields: Vec<(&str, Value)>) -> Value {
    let mut m = BTreeMap::new();
    for (k, v) in fields {
        m.insert(k.to_string(), v);
    }
    Value::Struct(Arc::new(StructValue::new(m)))
}

/// A struct-like value Bazel prints as `<unknown object CLASS>`.
pub fn opaque_struct_value(fields: Vec<(&str, Value)>, class: &str) -> Value {
    let mut m = BTreeMap::new();
    for (k, v) in fields {
        m.insert(k.to_string(), v);
    }
    Value::Struct(Arc::new(StructValue::new_opaque(m, class)))
}

fn attr_kinds() -> &'static [&'static str] {
    &[
        "string",
        "int",
        "bool",
        "string_list",
        "int_list",
        "string_dict",
        "string_list_dict",
        "label",
        "label_list",
        "label_keyed_string_dict",
        "output",
        "output_list",
        "string_keyed_label_dict",
        "label_list_dict",
    ]
}

fn host_globals() -> Vec<&'static str> {
    vec![
        "repository_rule",
        "module_extension",
        "tag_class",
        "extension_metadata",
        "Label",
        "Label_",
        "exec_result",
        "watch_marker",
        "rule",
        "__do_not_use_fail_with_incompatible_use_cc_configure_from_rules_cc",
    ]
}

/// Prefix of an [`Rt::eval_bzl_fresh`] error that is a static (compilation) failure; the rest is the
/// list of messages, one per `ERROR: ` line.
pub(crate) const STATIC_FAILURE_MARK: &str = "<<zel-static-failure>>";
pub(crate) const NESTED_STATIC_FAILURE_MARK: &str = "<<zel-nested-static-failure>>";
pub(crate) const NESTED_STATIC_TRAILER_MARK: &str = "\n<<zel-nested-static-trailer>>";

/// `.bzl` builtins besides the Starlark core, which the resolver accepts as predeclared names.
const BZL_BUILTIN_NAMES: &[&str] = &[
    "rule",
    "aspect",
    "provider",
    "macro",
    "Label",
    "select",
    "depset",
    "struct",
    "native",
    "config",
    "config_common",
    "platform_common",
    "cc_common",
    "java_common",
    "apple_common",
    "android_common",
    "proto_common",
    "testing",
    "json",
    "proto",
    "exec_group",
    "subrule",
    "visibility",
    "__do_not_use_fail_with_incompatible_use_cc_configure_from_rules_cc",
    "DefaultInfo",
    "OutputGroupInfo",
    "RunEnvironmentInfo",
    "InstrumentedFilesInfo",
    "CcInfo",
    "JavaInfo",
    "ProtoInfo",
    "PyInfo",
    "coverage_common",
    "analysis_test_transition",
    "transition",
    "module_extension",
    "tag_class",
    "repository_rule",
    "attr",
    "extension_metadata",
    "Args",
    "PackageSpecificationInfo",
    "FilesToRunProvider",
    "ToolchainInfo",
    "TemplateVariableInfo",
    "ProguardSpecProvider",
    "CcSharedLibraryInfo",
    "CcToolchainConfigInfo",
    "IncompatiblePlatformProvider",
    "ConstraintValueInfo",
    "ConstraintSettingInfo",
    "PlatformInfo",
    "ToolchainTypeInfo",
    "StarlarkAspectInfo",
    "OutputGroups",
    "DebugPackageInfo",
    "JavaPluginInfo",
    "JavaRuntimeInfo",
    "JavaToolchainInfo",
    "ObjcProvider",
    "AppleExecutableBinaryInfo",
    "bazel_features",
    "existing_rule",
    "package_name",
    "repository_name",
    "glob",
    "package",
    "licenses",
    "exports_files",
    "fail",
    "print",
    "__do_not_use_fail_with_incompatible_use_cc_configure_from_rules_cc",
];

impl Rt {
    pub fn new(opts: ExternalOptions, resolution: Arc<Resolution>) -> Arc<Rt> {
        Arc::new_cyclic(|me| Rt {
            opts,
            lock: Mutex::new(LockState::default()),
            resolution,
            state: Mutex::new(RtState {
                me: me.clone(),
                ..Default::default()
            }),
            log: Mutex::new(vec![]),
        })
    }

    pub fn external_dir(&self) -> PathBuf {
        self.opts.output_base.join("external")
    }

    pub fn repo_dir(&self, canonical: &str) -> PathBuf {
        if canonical.is_empty() {
            return self.opts.workspace_root.clone();
        }
        self.external_dir().join(canonical)
    }

    pub fn take_log(&self) -> Vec<String> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }

    pub(crate) fn info(&self, msg: impl Into<String>) {
        self.log.lock().unwrap().push(msg.into());
    }

    // ---------------------------------------------------------------- repo mapping & labels

    /// Repo mapping (apparent -> canonical) visible from inside repo `canonical`.
    pub fn repo_mapping_of(&self, canonical: &str) -> BTreeMap<String, String> {
        self.repo_mapping_ordered_of(canonical)
            .into_iter()
            .collect()
    }

    /// Looks up the repo mapping of `canonical` if `canonical` is a known module or extension
    /// repository (`None` if `canonical` no longer exists).
    pub fn try_repo_mapping_of(&self, canonical: &str) -> Option<BTreeMap<String, String>> {
        let res = &self.resolution;
        if canonical.is_empty() || canonical == "bazel_tools" {
            return Some(
                res.repo_mapping_ordered(&ModuleKey::root())
                    .into_iter()
                    .collect(),
            );
        }
        if let Some(k) = res.canonical_repo_names.get(&canonical.to_string()) {
            return Some(res.repo_mapping_ordered(k).into_iter().collect());
        }
        let (id, exported) = self.owning_extension_for_canonical(canonical)?;
        let exists = {
            let st = self.state.lock().unwrap();
            st.evaluating_exts.contains(&id)
                || st
                    .ext_results
                    .get(&id)
                    .is_some_and(|r| r.generated.iter().any(|(n, _)| n == exported))
        };
        if !exists {
            return None;
        }
        Some(self.repo_mapping_of(canonical))
    }

    /// The repo mapping in `ImmutableMap` insertion order (module repos: extension imports, own
    /// names, then deps; extension-generated repos: the host repo's mapping plus siblings).
    pub fn repo_mapping_ordered_of(&self, canonical: &str) -> Vec<(String, String)> {
        let res = &self.resolution;
        if canonical.is_empty() {
            return res.repo_mapping_ordered(&ModuleKey::root());
        }
        if let Some(k) = res.canonical_repo_names.get(&canonical.to_string()) {
            return res.repo_mapping_ordered(k);
        }
        fn put(m: &mut Vec<(String, String)>, k: String, v: String) {
            if let Some(e) = m.iter_mut().find(|(x, _)| *x == k) {
                e.1 = v;
            } else {
                m.push((k, v));
            }
        }
        // Repo generated by an extension: mapping of the extension's defining repo, then generated
        // repos, then root overrides (`ModuleExtensionRepoMappingEntriesFunction`).
        if let Some((ref id, _)) = self.owning_extension_for_canonical(canonical) {
            if let Some(unique) = res.extension_unique_names.get(id) {
                let host_repo = id.bzl_repo();
                let mut m = if host_repo.is_empty() && id.is_innate() {
                    if let Some((_, mk, _)) =
                        res.extension_usages.iter().find(|(eid, _, _)| eid == id)
                    {
                        res.repo_mapping_ordered(mk)
                    } else {
                        res.repo_mapping_ordered(&ModuleKey::root())
                    }
                } else if let Some(k) = res.canonical_repo_names.get(&host_repo) {
                    res.repo_mapping_ordered(k)
                } else {
                    res.repo_mapping_ordered(&ModuleKey::root())
                };
                let (already, me) = {
                    let st = self.state.lock().unwrap();
                    (
                        st.ext_results.contains_key(id) || st.evaluating_exts.contains(id),
                        st.me.upgrade(),
                    )
                };
                if !already {
                    if let Some(rt) = me {
                        let _ = rt.eval_extension(id);
                    }
                }
                if let Some(r) = self.state.lock().unwrap().ext_results.get(id) {
                    for (n, _) in &r.generated {
                        put(&mut m, n.clone(), format!("{unique}+{n}"));
                    }
                }
                // Repos injected via inject_repo()/override_repo() by the root module apply last.
                if let Some(ov) = res.repo_overrides.get(id) {
                    for (exported, canon) in ov {
                        put(&mut m, exported.clone(), canon.clone());
                    }
                }
                return m;
            }
        }
        vec![]
    }

    /// Resolves `raw` relative to `(repo, pkg)` into canonical `(repo, package, name)`.
    pub fn resolve_label(
        &self,
        raw: &str,
        ctx_repo: &str,
        ctx_pkg: &str,
    ) -> RtResult<(String, String, String)> {
        self.resolve_label_with_mapping(raw, ctx_repo, ctx_pkg, None)
    }

    /// Resolves `raw` relative to `(ctx_repo, ctx_pkg)` using `explicit_mapping` if provided,
    /// otherwise `self.repo_mapping_of(ctx_repo)`.
    pub fn resolve_label_with_mapping(
        &self,
        raw: &str,
        ctx_repo: &str,
        ctx_pkg: &str,
        explicit_mapping: Option<&BTreeMap<String, String>>,
    ) -> RtResult<(String, String, String)> {
        // Default target of the `@repo` shorthand (`@a` is `@a//:a`).
        let short_name: String = {
            let r = raw.trim_start_matches('@');
            let r = r.split_once("//").map(|(a, _)| a).unwrap_or(r);
            r.rsplit('+')
                .find(|p| !p.is_empty())
                .unwrap_or(r)
                .to_string()
        };
        let (repo, rest): (String, String) = if let Some(r) = raw.strip_prefix("@@") {
            let (a, b) = r
                .split_once("//")
                .map(|(a, b)| (a.to_string(), format!("//{b}")))
                .unwrap_or((r.to_string(), String::new()));
            (a, b)
        } else if let Some(r) = raw.strip_prefix('@') {
            let (a, b) = r
                .split_once("//")
                .map(|(a, b)| (a.to_string(), format!("//{b}")))
                .unwrap_or((r.to_string(), String::new()));
            let owned_map;
            let mapping = match explicit_mapping {
                Some(m) => m,
                None => {
                    owned_map = self.repo_mapping_of(ctx_repo);
                    &owned_map
                }
            };
            let canon = mapping.get(&a).cloned();
            match canon {
                Some(c) => (c, b),
                None => {
                    if self.opts.override_repository.contains_key(&a) {
                        (a, b)
                    } else {
                        let from_repo = format!("@@{ctx_repo}");
                        (
                            format!("[unknown repo '{a}' requested from {from_repo}]"),
                            b,
                        )
                    }
                }
            }
        } else if raw.starts_with("//") {
            let body = raw.trim_start_matches("//");
            let p = body.split_once(':').map(|(p, _)| p).unwrap_or(body);
            let r = if p == "conditions" || p == "visibility" {
                ""
            } else {
                ctx_repo
            };
            (r.to_string(), raw.to_string())
        } else if raw.starts_with(':') {
            let r = if ctx_pkg == "conditions" || ctx_pkg == "visibility" {
                ""
            } else {
                ctx_repo
            };
            (r.to_string(), format!("//{ctx_pkg}{raw}"))
        } else {
            return Err(format!(
                "invalid label '{raw}': absolute label must begin with '@' or '//'"
            ));
        };
        let rest = if rest.is_empty() {
            format!("//:{short_name}")
        } else {
            rest
        };
        let body = rest.trim_start_matches("//");
        let (pkg, name) = match body.split_once(':') {
            Some((p, n)) => (p.to_string(), n.to_string()),
            None => (
                body.to_string(),
                body.rsplit('/').next().unwrap_or("").to_string(),
            ),
        };
        Ok((repo, pkg, name))
    }

    pub fn label_value(&self, repo: &str, pkg: &str, name: &str) -> Value {
        let s = format!("@@{repo}//{pkg}:{name}");
        match zel_cmdline::Label::parse_canonical(&s) {
            Ok(l) => Value::Label(l),
            Err(_) => Value::String(s),
        }
    }

    pub fn register_exec_result(&self, code: i32, stdout: Vec<u8>, stderr: Vec<u8>) -> usize {
        let mut st = self.state.lock().unwrap();
        st.exec_results.push((code, stdout, stderr));
        st.exec_results.len() - 1
    }

    pub fn get_exec_result(&self, idx: usize) -> Option<(i32, Vec<u8>, Vec<u8>)> {
        let st = self.state.lock().unwrap();
        st.exec_results.get(idx).cloned()
    }

    // ---------------------------------------------------------------- bzl loading

    fn repo_mapping_for_owner(
        &self,
        owner: &zel_cmdline::RepositoryName,
    ) -> zel_cmdline::RepositoryMapping {
        let entries: Vec<(String, zel_cmdline::RepositoryName)> = self
            .repo_mapping_ordered_of(owner.name())
            .into_iter()
            .map(|(a, c)| (a, zel_cmdline::RepositoryName::create_unvalidated(c)))
            .collect();
        zel_cmdline::RepositoryMapping::new_ordered(entries, owner.clone())
    }

    fn new_context(
        self: &Arc<Self>,
        file_name: &str,
        ctx_repo: &str,
        ctx_pkg: &str,
    ) -> EvalContext {
        // Undefined builtins (e.g. `macro`, `cc_common`) evaluate leniently; set for diagnostics.
        let mut ctx = EvalContext {
            file_name: file_name.to_string(),
            strict_names: std::env::var("ZEL_STRICT_BZLMOD").is_ok(),
            main_repo_mapping: Some(Arc::new(self.repo_mapping_of(""))),
            ..Default::default()
        };
        ctx.semantics.experimental_sibling_repository_layout =
            self.opts.experimental_sibling_repository_layout;
        ctx.thread.semantics.experimental_sibling_repository_layout =
            self.opts.experimental_sibling_repository_layout;
        let owner = zel_cmdline::RepositoryName::create_unvalidated(ctx_repo);
        if let Ok(pkg_id) = zel_cmdline::PackageIdentifier::create(
            owner.clone(),
            zel_vfs::PathFragment::create(ctx_pkg),
        ) {
            *ctx.module_env.label_ctx.lock().unwrap() =
                Some((pkg_id, self.repo_mapping_for_owner(&owner)));
        }
        let rt = self.clone();
        ctx.host = Some(Arc::new(move |c, name, args, kw| {
            rt.host_dispatch(c, name, args, kw)
        }));
        let rt_print = self.clone();
        ctx.print_handler = Some(Arc::new(move |loc, msg| {
            rt_print.log.lock().unwrap().push(format!(
                "DEBUG: {}:{}:{}: {msg}",
                loc.file, loc.line, loc.column
            ));
        }));
        for n in host_globals() {
            ctx.globals
                .insert(n.to_string(), Value::BuiltinFunction(format!("host:{n}")));
        }
        let mut attr_fields = BTreeMap::new();
        for k in attr_kinds() {
            attr_fields.insert(
                k.to_string(),
                Value::BuiltinFunction(format!("host:attr.{k}")),
            );
        }
        ctx.globals.insert(
            "attr".into(),
            Value::Struct(Arc::new(StructValue::new(attr_fields))),
        );
        // `native` in repo-rule / extension .bzl files: only `bazel_version` is available outside
        // BUILD files (like Bazel, `native.existing_rule` etc. are not usable there).
        let mut native_fields = BTreeMap::new();
        native_fields.insert(
            "bazel_version".to_string(),
            Value::String(crate::resolve::default_bazel_version()),
        );
        // `StarlarkNativeModule.ExistingRulesShouldBeNoOp`: in module extension / repo rule
        // evaluation `native.existing_rule[s]` just return nothing (used by `maybe()`).
        native_fields.insert(
            "existing_rule".to_string(),
            Value::BuiltinFunction("host:native.existing_rule".into()),
        );
        native_fields.insert(
            "existing_rules".to_string(),
            Value::BuiltinFunction("host:native.existing_rules".into()),
        );
        native_fields.insert(
            "package_name".to_string(),
            Value::BuiltinFunction("host:native.package_name".into()),
        );
        native_fields.insert(
            "glob".to_string(),
            Value::BuiltinFunction("host:native.glob".into()),
        );
        for name in [
            "action_listener",
            "alias",
            "cc_binary",
            "cc_import",
            "cc_libc_top_alias",
            "cc_library",
            "cc_shared_library",
            "cc_static_library",
            "cc_test",
            "cc_toolchain",
            "cc_toolchain_alias",
            "cc_toolchain_suite",
            "config_feature_flag",
            "config_setting",
            "constraint_setting",
            "constraint_value",
            "environment",
            "exports_files",
            "extra_action",
            "fdo_prefetch_hints",
            "fdo_profile",
            "filegroup",
            "genquery",
            "genrule",
            "java_binary",
            "java_import",
            "java_library",
            "java_package_configuration",
            "java_plugin",
            "java_plugins_flag_alias",
            "java_runtime",
            "java_test",
            "java_toolchain",
            "label_flag",
            "label_setting",
            "memprof_profile",
            "module_name",
            "module_version",
            "objc_import",
            "objc_library",
            "package_default_visibility",
            "package_group",
            "package_relative_label",
            "platform",
            "propeller_optimize",
            "repo_name",
            "repository_name",
            "starlark_doc_extract",
            "subpackages",
            "test_suite",
            "toolchain",
            "toolchain_type",
        ] {
            native_fields.insert(
                name.to_string(),
                Value::BuiltinFunction(format!("host:native.{name}")),
            );
        }
        ctx.globals.insert(
            "native".into(),
            Value::Struct(Arc::new(StructValue::new_native(native_fields))),
        );
        let rt2 = self.clone();
        let (cr, cp) = (ctx_repo.to_string(), ctx_pkg.to_string());
        ctx.load_hook = Some(Arc::new(move |label: &str| rt2.load_bzl(label, &cr, &cp)));
        ctx
    }

    /// Loads (and caches) the exports of a `.bzl` file given a label relative to `(ctx_repo, ctx_pkg)`.
    pub fn load_bzl(
        self: &Arc<Self>,
        label: &str,
        ctx_repo: &str,
        ctx_pkg: &str,
    ) -> Result<BTreeMap<String, Value>, String> {
        let (repo, pkg, name) = self.resolve_label(label, ctx_repo, ctx_pkg)?;
        let key = format!("@@{repo}//{pkg}:{name}");
        if let Some(c) = self.state.lock().unwrap().bzl_cache.get(&key) {
            return Ok(c.clone());
        }
        {
            let mut st = self.state.lock().unwrap();
            if st.loading.contains(&key) {
                let mut chain = st.loading.clone();
                chain.push(key.clone());
                return Err(format!("cycle in load graph: {}", chain.join(" -> ")));
            }
            st.loading.push(key.clone());
        }
        let r = self.load_bzl_uncached(&key, &repo, &pkg, &name);
        self.state.lock().unwrap().loading.retain(|k| *k != key);
        let exports = r?;
        self.state
            .lock()
            .unwrap()
            .bzl_cache
            .insert(key, exports.clone());
        Ok(exports)
    }

    /// Evaluates a bzl file in a fresh context (functions resolve globals dynamically, so callers
    /// that want to invoke a function from this file should call it on the returned context).
    pub(crate) fn eval_bzl_fresh(
        self: &Arc<Self>,
        key: &str,
        repo: &str,
        pkg: &str,
        name: &str,
    ) -> Result<EvalContext, String> {
        let dir = match self.fetch_repo_detailed(repo) {
            Ok(dir) => dir,
            Err(crate::fetch::FetchRepoError::ModuleExtension(e)) => {
                crate::fetch::record_nested_extension_error(&e);
                return Err(format!("Unable to load package for {key}: {e}"));
            }
            Err(e) => {
                return Err(format!(
                    "Unable to load package for {key}: {}",
                    e.into_message()
                ))
            }
        };
        let path = if pkg.is_empty() {
            dir.join(name)
        } else {
            dir.join(pkg).join(name)
        };
        let src = std::fs::read_to_string(&path).map_err(|e| {
            format!(
                "cannot load '{key}': no such file ({}: {e})",
                path.display()
            )
        })?;
        let mut ctx = self.new_context(&path.display().to_string(), repo, pkg);
        // Bazel compiles (parses and resolves) the whole file before executing it: syntax errors and
        // undefined names are rejected up front.
        *ctx.module_env.label_string.lock().unwrap() = Some(
            key.strip_prefix("@@")
                .filter(|_| repo.is_empty())
                .unwrap_or(key)
                .to_string(),
        );
        ctx.static_names = true;
        ctx.static_universe = BZL_BUILTIN_NAMES.iter().map(|s| s.to_string()).collect();
        let eval_res = {
            let _phase = EvalPhaseGuard::enter(EvalPhase::BzlInit(Arc::clone(&ctx.module_env)));
            ctx.eval_file_checked(&src, None)
        };
        if let Err(errs) = eval_res {
            if ctx.static_failure {
                return Err(format!("{STATIC_FAILURE_MARK}{}", errs.join("\nERROR: ")));
            }
            let joined = errs.join("\n");
            // A compilation failure in a transitively loaded file is still a compilation failure
            // of this file. Preserve the child's diagnostics and add Bazel's load-site context.
            if let Some(i) = joined.find(STATIC_FAILURE_MARK) {
                let child_messages = &joined[i + STATIC_FAILURE_MARK.len()..];
                let load = ctx.loads.last();
                let child_name = load
                    .map(|l| {
                        l.module_label
                            .rsplit_once(':')
                            .map(|(_, name)| name)
                            .or_else(|| l.module_label.rsplit_once('/').map(|(_, name)| name))
                            .unwrap_or(&l.module_label)
                            .to_string()
                    })
                    .unwrap_or_else(|| "loaded file".to_string());
                let (line, column) = load
                    .and_then(|l| {
                        let needle = format!("\"{}\"", l.module_label);
                        src.lines().enumerate().find_map(|(line, text)| {
                            text.find(&needle).map(|column| (line + 1, column + 1))
                        })
                    })
                    .unwrap_or((1, 1));
                return Err(format!(
                    "{NESTED_STATIC_FAILURE_MARK}{child_messages}{NESTED_STATIC_TRAILER_MARK}at {}:{line}:{column}: compilation of module '{child_name}' failed",
                    path.display()
                ));
            }
            return Err(joined);
        }
        let mut st = self.state.lock().unwrap();
        {
            let children: Vec<String> = ctx
                .loads
                .iter()
                .filter_map(|l| self.resolve_label(&l.module_label, repo, pkg).ok())
                .map(|(r, p, n)| format!("@@{r}//{p}:{n}"))
                .collect();
            st.bzl_deps.insert(
                key.to_string(),
                (crate::hash::sha256_hex(src.as_bytes()), children),
            );
        }
        for pass_private in [false, true] {
            for (n, v) in &ctx.exported_assignments {
                if n.starts_with('_') != pass_private {
                    continue;
                }
                if let Value::Host(kind, id) = v {
                    let idx: usize = id.parse().unwrap_or(usize::MAX);
                    match kind.as_str() {
                        "repository_rule" => {
                            if let Some(d) = st.rule_defs.get_mut(idx) {
                                if d.name.is_empty() {
                                    d.name = n.clone();
                                    if d.bzl_file.is_empty() {
                                        d.bzl_file = key.to_string();
                                    }
                                }
                            }
                        }
                        "module_extension" if !pass_private => {
                            if let Some(d) = st.ext_defs.get_mut(idx) {
                                if d.name.is_empty() {
                                    d.name = n.clone();
                                }
                                if d.bzl_file.is_empty() {
                                    d.bzl_file = key.to_string();
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(ctx)
    }

    /// Whether a `.bzl` source defines repository rules / module extensions (evaluated by this
    /// runtime's own host) rather than ordinary rules/macros/functions (evaluated by the package
    /// loader, which provides `rule`, `provider`, `attr`, ...).
    fn defines_external_machinery(src: &str) -> bool {
        [
            "repository_rule(",
            "module_extension(",
            "tag_class(",
            "tools/build_defs/repo",
        ]
        .iter()
        .any(|m| src.contains(m))
    }

    pub(crate) fn package_loader(self: &Arc<Self>) -> Arc<zel_packages::PackageLoader> {
        let mut st = self.state.lock().unwrap();
        if let Some(l) = &st.loader {
            return l.clone();
        }
        let fs = Arc::new(zel_vfs::LocalFileSystem::new(
            zel_vfs::DigestHashFunction::Sha256,
        ));
        let mut cfg = zel_packages::LoaderConfig::new(
            fs,
            Arc::new(crate::resolver::BzlmodRepoResolver::new(self.clone())),
        );
        cfg.sibling_repository_layout = self.opts.experimental_sibling_repository_layout;
        // Helper .bzl files evaluated by the loader call repository rules and `repository_ctx` /
        // `module_ctx` methods; those belong to this runtime.
        let rt_o = Arc::downgrade(self);
        cfg.bzl_override = Some(Arc::new(move |label: &zel_cmdline::Label| {
            let rt = rt_o.upgrade()?;
            let pkg = label.package_identifier();
            let repo = pkg.repository().name().to_string();
            let dir = rt.fetch_repo(&repo).ok()?;
            let pf = pkg.package_fragment().path_string().to_string();
            let path = if pf.is_empty() {
                dir.join(label.name())
            } else {
                dir.join(&pf).join(label.name())
            };
            let src = std::fs::read_to_string(path).ok()?;
            if !Self::defines_external_machinery(&src) {
                return None;
            }
            Some(rt.load_bzl(&label.canonical_form(), "", ""))
        }));
        let rt = Arc::downgrade(self);
        cfg.external_host = Some(Arc::new(move |ctx, name, pos, kw| match rt.upgrade() {
            Some(rt) => rt.host_dispatch(ctx, name, pos, kw),
            None => Err(StarlarkError("external repository runtime is gone".into())),
        }));
        let l = zel_packages::PackageLoader::new(cfg);
        st.loader = Some(l.clone());
        l
    }

    fn load_bzl_uncached(
        self: &Arc<Self>,
        key: &str,
        repo: &str,
        pkg: &str,
        name: &str,
    ) -> Result<BTreeMap<String, Value>, String> {
        let dir = match self.fetch_repo_detailed(repo) {
            Ok(dir) => dir,
            Err(crate::fetch::FetchRepoError::ModuleExtension(e)) => {
                crate::fetch::record_nested_extension_error(&e);
                return Err(format!("Unable to load package for {key}: {e}"));
            }
            Err(e) => {
                return Err(format!(
                    "Unable to load package for {key}: {}",
                    e.into_message()
                ))
            }
        };
        {
            let path = if pkg.is_empty() {
                dir.join(name)
            } else {
                dir.join(pkg).join(name)
            };
            if let Ok(src) = std::fs::read_to_string(&path) {
                if !Self::defines_external_machinery(&src) {
                    let label =
                        zel_cmdline::Label::parse_canonical(key).map_err(|e| e.to_string())?;
                    if let Ok(exports) = self.package_loader().load_bzl_exports(&label) {
                        // Record the dependency edge for lockfile digests like `eval_bzl_fresh` does.
                        self.state.lock().unwrap().bzl_deps.insert(
                            key.to_string(),
                            (crate::hash::sha256_hex(src.as_bytes()), vec![]),
                        );
                        return Ok(exports);
                    }
                    // Re-evaluate failures through the Bzlmod Starlark context below so static
                    // failures retain their compilation marker and load-site context.
                }
            }
        }
        let ctx = self.eval_bzl_fresh(key, repo, pkg, name)?;
        // Predeclared names (`json`, `proto`, `attr`, ...) are not exports of the module unless the module
        // rebinds them (`json = struct(...)` in rules_jvm_external's `specs.bzl` is a real export).
        let pristine = self.new_context("", repo, pkg).globals;
        let mut exports = BTreeMap::new();
        for (k, v) in ctx.globals {
            let predeclared = matches!(
                k.as_str(),
                "attr"
                    | "repository_rule"
                    | "module_extension"
                    | "tag_class"
                    | "extension_metadata"
                    | "Label"
                    | "json"
                    | "proto"
                    | "rule"
            ) && matches!(v, Value::BuiltinFunction(_) | Value::Struct(_));
            if predeclared
                && pristine
                    .get(&k)
                    .is_none_or(|p| format!("{p:?}") == format!("{v:?}"))
            {
                continue;
            }
            exports.insert(k, v);
        }
        Ok(exports)
    }

    // ---------------------------------------------------------------- host dispatch

    /// Returns the label string of the `.bzl` file currently being initialized on this thread,
    /// if and only if the innermost active evaluation phase is `.bzl` initialization for `ctx`.
    fn current_bzl_init_label(&self, ctx: &EvalContext) -> Option<String> {
        let is_active_bzl = EVAL_PHASE_STACK.with(|stack| {
            matches!(
                stack.borrow().last(),
                Some(EvalPhase::BzlInit(env)) if Arc::ptr_eq(env, &ctx.module_env)
            )
        });
        if !is_active_bzl {
            return None;
        }
        ctx.module_env.label_string.lock().unwrap().clone()
    }

    /// Returns whether the innermost active evaluation phase on this thread is executing a
    /// `module_extension` implementation function.
    fn in_extension_impl(&self) -> bool {
        EVAL_PHASE_STACK
            .with(|stack| matches!(stack.borrow().last(), Some(EvalPhase::ExtensionImpl)))
    }

    pub(crate) fn host_dispatch(
        self: &Arc<Self>,
        ctx: &mut EvalContext,
        name: &str,
        pos: Vec<Value>,
        kw: BTreeMap<String, Value>,
    ) -> Result<Value, StarlarkError> {
        if let Some(kind) = name.strip_prefix("attr.") {
            return self.make_attr(kind, pos, kw);
        }
        if let Some(rest) = name.strip_prefix("call:") {
            return self.call_host_object(ctx, rest, pos, kw);
        }
        if let Some(kind) = name
            .strip_prefix("index:")
            .or_else(|| name.strip_prefix("contains:"))
            .or_else(|| name.strip_prefix("debugprint:"))
        {
            if kind == "Facts" {
                return crate::ctx::facts_op(name, &pos);
            }
        }
        if name.starts_with("method:") || name.starts_with("attr:") {
            return crate::ctx::dispatch_object(self, ctx, name, pos, kw);
        }
        match name {
            "native.package_name" => Err(StarlarkError(
                "package_name() can only be used while evaluating a BUILD file or a legacy or symbolic macro".into(),
            )),
            "native.glob" => Err(StarlarkError("glob() can only be used while evaluating a BUILD file or a legacy macro".into())),
            "native.existing_rule"
                if self.in_extension_impl() && !self.state.lock().unwrap().collectors.is_empty() =>
            {
                Ok(Value::None)
            }
            "native.existing_rules"
                if self.in_extension_impl() && !self.state.lock().unwrap().collectors.is_empty() =>
            {
                Ok(dict_value(Vec::new()))
            }
            "native.existing_rule" => Err(StarlarkError(
                "existing_rule() can only be used while evaluating a BUILD file, a legacy macro, or a rule finalizer".into(),
            )),
            "native.existing_rules" => Err(StarlarkError(
                "existing_rules() can only be used while evaluating a BUILD file, a legacy macro, or a rule finalizer".into(),
            )),
            "repository_rule" => {
                if self.current_bzl_init_label(ctx).is_none() {
                    return err(
                        "repository_rule() can only be used while initializing a .bzl file",
                    );
                }
                match self.make_repository_rule(pos, kw) {
                    Ok(v) => Ok(v),
                    Err(e) if !ctx.last_call_loc.file.is_empty() && !e.0.contains("Traceback (most recent call last)") => {
                        let at = ctx.assign_stmt_location.clone().unwrap_or_else(|| ctx.last_call_loc.clone());
                        let trace = ctx.thread.format_stack_trace_at(&at);
                        Err(StarlarkError(format!("ERROR: {trace}Error in repository_rule: {}", e.0)))
                    }
                    other => other,
                }
            }
            "module_extension" => self.make_module_extension(ctx, pos, kw, ctx.call_location.clone()),
            "rule" => {
                if self.current_bzl_init_label(ctx).is_none() {
                    return err("rule() can only be used while initializing a .bzl file");
                }
                Ok(Value::Host("bzlmod_rule".into(), String::new()))
            }
            "__do_not_use_fail_with_incompatible_use_cc_configure_from_rules_cc" => {
                if !pos.is_empty() || !kw.is_empty() {
                    return err("__do_not_use_fail_with_incompatible_use_cc_configure_from_rules_cc() accepts no arguments");
                }
                if self.opts.incompatible_use_cc_configure_from_rules_cc {
                    return err("Incompatible flag --incompatible_use_cc_configure_from_rules_cc has been flipped. Please use cc_configure and related logic from https://github.com/bazelbuild/rules_cc. See https://github.com/bazelbuild/bazel/issues/10134 for details and migration instructions.");
                }
                Ok(Value::None)
            }
            "tag_class" => {
                if pos.len() > 1 {
                    return err(format!(
                        "tag_class() accepts no more than 1 positional argument but got {}",
                        pos.len()
                    ));
                }
                if pos.len() == 1 && kw.contains_key("attrs") {
                    return err("tag_class() got multiple values for parameter 'attrs'");
                }
                for k in kw.keys() {
                    if k != "attrs" && k != "doc" {
                        return err(format!("tag_class() got an unexpected keyword argument '{k}'"));
                    }
                }
                if let Some(doc) = kw.get("doc") {
                    if !matches!(doc, Value::None | Value::String(_)) {
                        return err(format!(
                            "in call to tag_class(), parameter 'doc' got value of type '{}', want 'string or NoneType'",
                            doc.type_name()
                        ));
                    }
                }
                let attrs = match pos.first().or_else(|| kw.get("attrs")) {
                    None => vec![],
                    Some(v) => self.attrs_from_dict(v, "tag_class")?,
                };
                let mut st = self.state.lock().unwrap();
                st.tag_class_defs.push(attrs);
                Ok(Value::Host("tag_class".into(), (st.tag_class_defs.len() - 1).to_string()))
            }
            "extension_metadata" => {
                for k in kw.keys() {
                    if !matches!(
                        k.as_str(),
                        "root_module_direct_deps"
                            | "root_module_direct_dev_deps"
                            | "reproducible"
                            | "facts"
                    ) {
                        return err(format!(
                            "extension_metadata() got unexpected keyword argument '{k}'"
                        ));
                    }
                }
                if pos.len() > 2 {
                    return err(format!(
                        "extension_metadata() accepts no more than 2 positional arguments but got {}",
                        pos.len()
                    ));
                }
                if !pos.is_empty() && kw.contains_key("root_module_direct_deps") {
                    return err(
                        "extension_metadata() got multiple values for parameter 'root_module_direct_deps'",
                    );
                }
                if pos.get(1).is_some() && kw.contains_key("root_module_direct_dev_deps") {
                    return err(
                        "extension_metadata() got multiple values for parameter 'root_module_direct_dev_deps'",
                    );
                }
                let deps_val = pos.first().or_else(|| kw.get("root_module_direct_deps"));
                let dev_val = pos
                    .get(1)
                    .or_else(|| kw.get("root_module_direct_dev_deps"));
                let deps_is_none = deps_val.is_none() || matches!(deps_val, Some(Value::None));
                let dev_is_none = dev_val.is_none() || matches!(dev_val, Some(Value::None));
                let is_all = |v: Option<&Value>| matches!(v, Some(Value::String(s)) if s == "all");
                let is_empty_seq = |v: Option<&Value>| match v {
                    Some(Value::List(l)) => l.lock().unwrap().elements.is_empty(),
                    Some(Value::Tuple(t)) => t.is_empty(),
                    _ => false,
                };
                let (deps_is_all, dev_is_all) = (is_all(deps_val), is_all(dev_val));
                let extract_strict_string_seq =
                    |label: &str, v: Option<&Value>| -> Result<Vec<String>, StarlarkError> {
                        let Some(val) = v else {
                            return Ok(vec![]);
                        };
                        let elems: Vec<Value> = match val {
                            Value::List(l) => l.lock().unwrap().elements.clone(),
                            Value::Tuple(t) => t.clone(),
                            other => {
                                return err(format!(
                                    "in call to extension_metadata(), parameter '{label}' got value of type '{}', want 'NoneType, string, or sequence'",
                                    other.type_name()
                                ))
                            }
                        };
                        let mut out = Vec::with_capacity(elems.len());
                        for (idx, elem) in elems.into_iter().enumerate() {
                            match elem {
                                Value::String(s) => out.push(s),
                                other => {
                                    return err(format!(
                                        "at index {idx} of {label}, got element of type {}, want string",
                                        other.type_name()
                                    ))
                                }
                            }
                        }
                        Ok(out)
                    };
                // `ModuleExtensionMetadata.create` checks, in this order.
                #[allow(clippy::nonminimal_bool)]
                if !(deps_is_none && dev_is_none)
                    && !(deps_is_all && is_empty_seq(dev_val))
                    && !(dev_is_all && is_empty_seq(deps_val))
                {
                    if deps_is_all || dev_is_all {
                        return err("if one of root_module_direct_deps and root_module_direct_dev_deps is \"all\", the other must be an empty list");
                    }
                    if matches!(deps_val, Some(Value::String(_)))
                        || matches!(dev_val, Some(Value::String(_)))
                    {
                        return err("root_module_direct_deps and root_module_direct_dev_deps must be None, \"all\", or a list of strings");
                    }
                    if deps_is_none != dev_is_none {
                        return err("root_module_direct_deps and root_module_direct_dev_deps must both be specified or both be unspecified");
                    }
                    let mut seen: Vec<String> = vec![];
                    for (label, v, check_overlap) in [
                        ("root_module_direct_deps", deps_val, false),
                        ("root_module_direct_dev_deps", dev_val, true),
                    ] {
                        let items = extract_strict_string_seq(label, v)?;
                        let mut own: Vec<String> = vec![];
                        for dep in items {
                            let valid = !dep.is_empty()
                                && dep.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
                                && dep
                                    .chars()
                                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
                            if !valid {
                                return err(format!("in {label}: invalid user-provided repo name '{dep}': valid names may contain only A-Z, a-z, 0-9, '-', '_', '.', and must start with a letter or a number"));
                            }
                            if check_overlap && seen.contains(&dep) {
                                return err(format!(
                                    "in {label}: entry '{dep}' is also in root_module_direct_deps"
                                ));
                            }
                            if own.contains(&dep) {
                                return err(format!("in {label}: duplicate entry '{dep}'"));
                            }
                            own.push(dep);
                        }
                        if !check_overlap {
                            seen = own;
                        }
                    }
                }
                let reproducible = match kw.get("reproducible") {
                    None => false,
                    Some(Value::Bool(b)) => *b,
                    Some(other) => {
                        return err(format!(
                            "in call to extension_metadata(), parameter 'reproducible' got value of type '{}', want 'bool'",
                            other.type_name()
                        ))
                    }
                };
                let facts = match kw.get("facts") {
                    None | Some(Value::None) => None,
                    Some(v) => Some(crate::json::validate_and_encode_facts(v)?),
                };
                let list_val = |label: &str, v: Option<&Value>| -> Result<Option<Vec<String>>, StarlarkError> {
                    match v {
                        None | Some(Value::None) => Ok(None),
                        Some(Value::String(s)) if s == "all" => Ok(None),
                        Some(_) => Ok(Some(extract_strict_string_seq(label, v)?)),
                    }
                };
                let use_all = if deps_is_all {
                    "REGULAR"
                } else if dev_is_all {
                    "DEV"
                } else {
                    "NO"
                };
                let m = ExtensionMetadata {
                    use_all: use_all.to_string(),
                    use_all_repos: use_all != "NO",
                    // `ModuleExtensionMetadata.create(null, null, UseAllRepos.*)`: the explicit lists are dropped with "all".
                    explicit_root_direct_deps: if use_all != "NO" {
                        None
                    } else {
                        list_val("root_module_direct_deps", deps_val)?
                    },
                    explicit_root_direct_dev_deps: if use_all != "NO" {
                        None
                    } else {
                        list_val("root_module_direct_dev_deps", dev_val)?
                    },
                    reproducible,
                    facts,
                };
                let mut st = self.state.lock().unwrap();
                st.ext_metadatas.push(m);
                Ok(Value::Host(
                    "extension_metadata".into(),
                    (st.ext_metadatas.len() - 1).to_string(),
                ))
            }
            "Label" | "Label_" => {
                let raw = match pos.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Label(l)) => return Ok(Value::Label(l.clone())),
                    _ => return err("Label() takes a string argument"),
                };
                let (pkg_id, mapping) = match ctx.current_label_ctx() {
                    Some((p, m)) => (p, m),
                    None => {
                        let (repo, pkg) = self.current_label_context(ctx);
                        let owner = zel_cmdline::RepositoryName::create_unvalidated(&repo);
                        let p = zel_cmdline::PackageIdentifier::create(
                            owner.clone(),
                            zel_vfs::PathFragment::create(&pkg),
                        )
                        .map_err(|e| StarlarkError(e.to_string()))?;
                        let m = self.repo_mapping_for_owner(&owner);
                        (p, m)
                    }
                };
                let repo = pkg_id.repository().name().to_string();
                let parsed =
                    zel_cmdline::Label::parse_with_package_context(&raw, &pkg_id, &mapping);
                // `Label()` records the apparent-name lookups it performs (`RecordedRepoMapping`).
                if let Some(rest) = raw.strip_prefix('@').filter(|r| !r.starts_with('@')) {
                    let apparent = rest.split_once("//").map(|x| x.0).unwrap_or(rest);
                    let resolved_canon = parsed
                        .as_ref()
                        .ok()
                        .filter(|l| l.repository().is_visible())
                        .map(|l| l.repository().name().to_string());
                    let active: Vec<crate::ctx::CtxData> = self
                        .state
                        .lock()
                        .unwrap()
                        .ctxs
                        .iter()
                        .flatten()
                        .filter(|c| c.kind == crate::ctx::CtxKind::Module)
                        .cloned()
                        .collect();
                    if active.is_empty() || self.current_bzl_init_label(ctx).is_some() {
                        if let Some(r) = &resolved_canon {
                            let mut st = self.state.lock().unwrap();
                            let e = (repo.clone(), apparent.to_string(), r.clone());
                            if !st.load_label_mappings.contains(&e) {
                                st.load_label_mappings.push(e);
                            }
                        }
                    }
                    for c in active {
                        c.record(
                            format!(
                                "REPO_MAPPING:{}",
                                crate::ctx::escape_input(&format!("{repo},{apparent}"))
                            ),
                            resolved_canon.clone(),
                        );
                    }
                }
                let lbl = parsed.map_err(|e| StarlarkError(e.to_string()))?;
                Ok(Value::Label(lbl))
            }
            other => err(format!("unknown host function '{other}'")),
        }
    }

    fn current_label_context(&self, ctx: &EvalContext) -> (String, String) {
        // The file name of the evaluating context is the absolute path of the bzl file; map back
        // to (canonical repo, package).
        let ext = self.external_dir();
        let p = Path::new(&ctx.file_name);
        if let Ok(rel) = p.strip_prefix(&ext) {
            let mut comps = rel.components();
            if let Some(first) = comps.next() {
                let repo = first.as_os_str().to_string_lossy().into_owned();
                let rest: PathBuf = comps.collect();
                let pkg = rest
                    .parent()
                    .map(|x| x.to_string_lossy().into_owned())
                    .unwrap_or_default();
                return (repo, pkg);
            }
        }
        if let Ok(rel) = p.strip_prefix(&self.opts.workspace_root) {
            let pkg = rel
                .parent()
                .map(|x| x.to_string_lossy().into_owned())
                .unwrap_or_default();
            return (String::new(), pkg);
        }
        (String::new(), String::new())
    }

    fn attrs_from_dict(
        &self,
        v: &Value,
        fn_name: &str,
    ) -> Result<Vec<(String, AttrDef)>, StarlarkError> {
        let Value::Dict(d) = v else {
            return err(format!(
                "in call to {fn_name}(), parameter 'attrs' got value of type '{}', want 'dict'",
                v.type_name()
            ));
        };
        let mut out = vec![];
        for (k, v) in &d.lock().unwrap().entries {
            let Value::String(k) = k else {
                return err(format!(
                    "in call to {fn_name}(), got dict<{}, {}> for 'attrs', want dict<string, attr>",
                    k.type_name(),
                    v.type_name()
                ));
            };
            let Value::Struct(s) = v else {
                return err("attrs must be created with attr.*()");
            };
            if !s.fields.contains_key("_attr_kind") && !s.fields.contains_key("_attr_type") {
                return err("attrs must be created with attr.*()");
            }
            // `attr.*()` values made by the package loader (`_attr_type`) are accepted too.
            let kind = s
                .fields
                .get("_attr_kind")
                .or_else(|| s.fields.get("_attr_type"))
                .and_then(|k| k.as_str())
                .unwrap_or("string")
                .to_string();
            let def = AttrDef {
                kind,
                default: match s.fields.get("default") {
                    Some(Value::None) | None => None,
                    Some(x) => Some(AttrVal::from_value(x)),
                },
                mandatory: s
                    .fields
                    .get("mandatory")
                    .map(|x| x.is_truthy())
                    .unwrap_or(false),
                values: s
                    .fields
                    .get("values")
                    .map(|x| match AttrVal::from_value(x) {
                        AttrVal::List(l) => l,
                        _ => vec![],
                    })
                    .unwrap_or_default(),
                allow_empty: s
                    .fields
                    .get("allow_empty")
                    .map(|x| x.is_truthy())
                    .unwrap_or(true),
            };
            out.push((k.clone(), def));
        }
        Ok(out)
    }

    fn attrs_from_kw(&self, v: Option<&Value>) -> Result<Vec<(String, AttrDef)>, StarlarkError> {
        let Some(v @ Value::Dict(_)) = v else {
            return Ok(vec![]);
        };
        self.attrs_from_dict(v, "repository_rule")
    }

    fn make_attr(
        &self,
        kind: &str,
        pos: Vec<Value>,
        kw: BTreeMap<String, Value>,
    ) -> Result<Value, StarlarkError> {
        fn check_i32_attr_val(v: &Value, what: &str) -> Result<(), StarlarkError> {
            if let Value::Int(i) = v {
                if i.to_i64().and_then(|n| i32::try_from(n).ok()).is_none() {
                    return err(format!(
                        "for {what}, got {i}, want value in signed 32-bit range"
                    ));
                }
            }
            Ok(())
        }
        let mut default = kw.get("default").cloned().unwrap_or(Value::None);
        if default == Value::None && !pos.is_empty() && !matches!(kind, "label" | "label_list") {
            default = pos[0].clone();
        }
        let values = kw
            .get("values")
            .cloned()
            .unwrap_or_else(|| list_value(vec![]));
        if kind == "int" {
            check_i32_attr_val(&default, "default")?;
            if let Value::List(l) = &values {
                for (idx, item) in l.lock().unwrap().elements.iter().enumerate() {
                    check_i32_attr_val(item, &format!("element {idx} of values"))?;
                }
            }
        } else if kind == "int_list" {
            match &default {
                Value::List(l) => {
                    for (idx, item) in l.lock().unwrap().elements.iter().enumerate() {
                        check_i32_attr_val(item, &format!("element {idx} of default"))?;
                    }
                }
                Value::Tuple(t) => {
                    for (idx, item) in t.iter().enumerate() {
                        check_i32_attr_val(item, &format!("element {idx} of default"))?;
                    }
                }
                _ => {}
            }
        }
        Ok(struct_value(vec![
            ("_attr_kind", str_value(kind)),
            ("default", default),
            (
                "mandatory",
                Value::Bool(kw.get("mandatory").map(|v| v.is_truthy()).unwrap_or(false)),
            ),
            ("values", values),
            (
                "allow_empty",
                Value::Bool(kw.get("allow_empty").map(|v| v.is_truthy()).unwrap_or(true)),
            ),
            ("doc", kw.get("doc").cloned().unwrap_or(Value::None)),
        ]))
    }

    fn make_repository_rule(
        &self,
        pos: Vec<Value>,
        kw: BTreeMap<String, Value>,
    ) -> Result<Value, StarlarkError> {
        let implementation = kw
            .get("implementation")
            .cloned()
            .or_else(|| pos.first().cloned());
        let Some(implementation) = implementation else {
            return err("repository_rule() missing 1 required positional argument: implementation");
        };
        // `@Param(name = "remotable", enableOnlyWithFlag = EXPERIMENTAL_REPO_REMOTE_EXEC)`; the flag is off.
        if kw.contains_key("remotable") {
            return err("in call to repository_rule(), parameter 'remotable' is experimental and thus unavailable with the current flags. It may be enabled by setting --experimental_repo_remote_exec");
        }
        let mut attrs = self.attrs_from_kw(kw.get("attrs"))?;
        for (n, _) in &attrs {
            if matches!(n.as_str(), "name" | "tags" | "deprecation" | "visibility") {
                return err(format!(
                    "There is already a built-in attribute '{n}' which cannot be overridden"
                ));
            }
        }
        let user_has_repo_mapping = attrs.iter().any(|(n, _)| n == "repo_mapping");
        if !user_has_repo_mapping {
            attrs.push((
                "repo_mapping".into(),
                AttrDef {
                    kind: "string_dict".into(),
                    default: None,
                    mandatory: false,
                    values: vec![],
                    allow_empty: true,
                },
            ));
        }
        let local = match kw.get("local") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(other) => {
                return err(format!(
                    "in call to repository_rule(), parameter 'local' got value of type '{}', want 'bool'",
                    other.type_name()
                ))
            }
        };
        let configure = match kw.get("configure") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(other) => {
                return err(format!(
                    "in call to repository_rule(), parameter 'configure' got value of type '{}', want 'bool'",
                    other.type_name()
                ))
            }
        };
        let def = RuleDef {
            implementation,
            attrs,
            user_has_repo_mapping,
            local,
            environ: kw
                .get("environ")
                .map(|v| v.to_string_list())
                .unwrap_or_default(),
            configure,
            bzl_file: String::new(),
            name: String::new(),
        };
        let mut st = self.state.lock().unwrap();
        st.rule_defs.push(def);
        Ok(Value::Host(
            "repository_rule".into(),
            (st.rule_defs.len() - 1).to_string(),
        ))
    }

    fn make_module_extension(
        &self,
        ctx: &EvalContext,
        pos: Vec<Value>,
        kw: BTreeMap<String, Value>,
        location: String,
    ) -> Result<Value, StarlarkError> {
        let Some(raw_label) = self.current_bzl_init_label(ctx) else {
            return err("module_extension() can only be used while initializing a .bzl file");
        };
        let bzl_file = if raw_label.starts_with("//") {
            format!("@@{raw_label}")
        } else {
            raw_label
        };
        let implementation = kw
            .get("implementation")
            .cloned()
            .or_else(|| pos.first().cloned());
        let Some(implementation) = implementation else {
            return err(
                "module_extension() missing 1 required positional argument: implementation",
            );
        };
        let os_dependent = match kw.get("os_dependent") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(other) => {
                return err(format!(
                    "in call to module_extension(), parameter 'os_dependent' got value of type '{}', want 'bool'",
                    other.type_name()
                ))
            }
        };
        let arch_dependent = match kw.get("arch_dependent") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(other) => {
                return err(format!(
                    "in call to module_extension(), parameter 'arch_dependent' got value of type '{}', want 'bool'",
                    other.type_name()
                ))
            }
        };
        let facts_version = match kw.get("facts_version") {
            None => 0u32,
            Some(Value::Int(i)) => {
                let v = i
                    .to_i64()
                    .and_then(|x| i32::try_from(x).ok())
                    .ok_or_else(|| {
                        StarlarkError(format!(
                            "in call to module_extension(), parameter 'facts_version' got {i}, want value in signed 32-bit int range"
                        ))
                    })?;
                if v < 0 {
                    return err("facts_version must be non-negative");
                }
                v as u32
            }
            Some(other) => {
                return err(format!(
                    "in call to module_extension(), parameter 'facts_version' got value of type '{}', want 'int'",
                    other.type_name()
                ))
            }
        };
        let mut tag_classes = vec![];
        if let Some(Value::Dict(d)) = kw.get("tag_classes") {
            for (k, v) in &d.lock().unwrap().entries {
                let Value::Host(kind, id) = v else {
                    return err("tag_classes values must be created with tag_class()");
                };
                if kind != "tag_class" {
                    return err("tag_classes values must be created with tag_class()");
                }
                let idx: usize = id.parse().unwrap_or(0);
                tag_classes.push((
                    k.to_string(),
                    self.state.lock().unwrap().tag_class_defs[idx].clone(),
                ));
            }
        }
        let location = location
            .strip_suffix(":0")
            .map(String::from)
            .unwrap_or(location);
        let def = ExtensionDef {
            implementation,
            tag_classes,
            environ: kw
                .get("environ")
                .map(|v| v.to_string_list())
                .unwrap_or_default(),
            os_dependent,
            arch_dependent,
            facts_version,
            bzl_file,
            name: String::new(),
            location,
        };
        let mut st = self.state.lock().unwrap();
        st.ext_defs.push(def);
        Ok(Value::Host(
            "module_extension".into(),
            (st.ext_defs.len() - 1).to_string(),
        ))
    }

    /// `call:<kind>` on Host objects: invoking a repository rule instantiates a repo.
    fn call_host_object(
        self: &Arc<Self>,
        _ctx: &mut EvalContext,
        rest: &str,
        pos: Vec<Value>,
        kw: BTreeMap<String, Value>,
    ) -> Result<Value, StarlarkError> {
        let Some(Value::Host(_, id)) = pos.first().cloned() else {
            return err("bad callable");
        };
        match rest {
            "repository_rule" => {
                let idx: usize = id.parse().unwrap_or(0);
                let def = self.state.lock().unwrap().rule_defs[idx].clone();
                if def.name.is_empty() {
                    return err("repository rule must be assigned to a top-level variable of a .bzl file before it can be called");
                }
                let builtin_err = |ctx: &mut EvalContext, m: String| -> Result<Value, StarlarkError> {
                    ctx.builtin_error = Some(("repository_rule".to_string(), m.clone()));
                    Err(StarlarkError(m))
                };
                if pos.len() > 1 {
                    return builtin_err(_ctx, "unexpected positional arguments".into());
                }
                let name = match kw.get("name") {
                    Some(Value::String(s)) => s.clone(),
                    other => {
                        let t = other.map(|v| v.type_name()).unwrap_or("NoneType");
                        return builtin_err(_ctx, format!("expected string for attribute 'name', got '{t}'"));
                    }
                };
                if let Err(e) = crate::module_file::validate_user_repo_name(&name) {
                    return builtin_err(_ctx, e.0);
                }
                crate::ctx::validate_repo_rule_call(&def.name, &def.attrs, &kw, &_ctx.call_kw_order.clone())?;
                crate::ctx::check_repo_rule_labels(self, &def.name, &def.attrs, &kw, None)?;
                let mut attrs: BTreeMap<String, AttrVal> = BTreeMap::new();
                for (k, v) in &kw {
                    if matches!(
                        k.as_str(),
                        "name" | "tags" | "deprecation" | "visibility"
                    ) || matches!(v, Value::None)
                    {
                        continue;
                    }
                    attrs.insert(k.clone(), AttrVal::from_value(v));
                }
                let call = DeferredRepoCall {
                    name: name.clone(),
                    bzl_file: def.bzl_file.clone(),
                    rule_name: def.name.clone(),
                    attr_defs: def.attrs.clone(),
                    kw: attrs,
                };
                let call_loc = _ctx.call_location.clone();
                if !self.in_extension_impl() {
                    return err("repository rules can only be called from a module extension or MODULE.bazel (use_repo_rule)");
                }
                let mut st = self.state.lock().unwrap();
                let dup = st
                    .collectors
                    .last()
                    .is_some_and(|c| c.calls.iter().any(|d| d.name == name));
                if dup {
                    let at = st
                        .collectors
                        .last()
                        .and_then(|c| c.locs.get(&name))
                        .cloned()
                        .unwrap_or_default();
                    drop(st);
                    return builtin_err(_ctx, format!("A repo named {name} is already generated by this module extension at {at}"));
                }
                match st.collectors.last_mut() {
                    Some(c) => {
                        c.calls.push(call);
                        c.locs.insert(name, call_loc);
                        Ok(Value::None)
                    }
                    None => err("repository rules can only be called from a module extension or MODULE.bazel (use_repo_rule)"),
                }
            }
            "module_extension" => err("module extensions are not callable"),
            "bzlmod_rule" => err("a rule can only be instantiated while evaluating a BUILD file or a legacy or symbolic macro"),
            other => err(format!("'{other}' object is not callable")),
        }
    }
}

impl Rt {
    pub(crate) fn bzl_loads_of(&self, key: &str, text: &str) -> Vec<String> {
        if let Some(loads) = self.state.lock().unwrap().bzl_loads_cache.get(key) {
            return loads.clone();
        }
        let loads: Vec<String> = zel_starlark::Lexer::new(key, text)
            .lex_all()
            .ok()
            .and_then(|t| zel_starlark::Parser::new(t).parse_module().ok())
            .map(|stmts| {
                stmts
                    .iter()
                    .filter_map(|s| match s {
                        zel_starlark::Stmt::Load(l, _) => Some(l.module_label.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.state
            .lock()
            .unwrap()
            .bzl_loads_cache
            .insert(key.to_string(), loads.clone());
        loads
    }

    /// `BzlLoadFunction`'s transitive digest of a `.bzl` file (module-extension dialect): the
    /// SHA-256 of the concatenation of the file's own SHA-256 and the transitive digests of its
    /// `load()`s in source order (duplicates included). Predeclared bzlmod builtins are not
    /// fingerprinted by Bazel for MODULE-loaded files.
    pub fn bzl_transitive_digest(self: &Arc<Self>, key: &str) -> Vec<u8> {
        if let Some(d) = self.state.lock().unwrap().bzl_transitive_digests.get(key) {
            return d.clone();
        }
        let mut memo: HashMap<String, Vec<u8>> = HashMap::new();
        let d = self.transitive_digest_of(key, &mut memo, &mut vec![]);
        let mut st = self.state.lock().unwrap();
        for (k, v) in memo {
            st.bzl_transitive_digests.insert(k, v);
        }
        d
    }

    fn transitive_digest_of(
        self: &Arc<Self>,
        key: &str,
        memo: &mut HashMap<String, Vec<u8>>,
        stack: &mut Vec<String>,
    ) -> Vec<u8> {
        if let Some(d) = memo.get(key) {
            return d.clone();
        }
        if let Some(d) = self.state.lock().unwrap().bzl_transitive_digests.get(key) {
            return d.clone();
        }
        let split = || -> Option<(String, String, String)> {
            let r = key.strip_prefix("@@")?;
            let (repo, rest) = r.split_once("//")?;
            let (pkg, name) = rest.split_once(':')?;
            Some((repo.to_string(), pkg.to_string(), name.to_string()))
        };
        let src = split().and_then(|(repo, pkg, name)| {
            let dir = self.fetch_repo(&repo).ok()?;
            let path = if pkg.is_empty() {
                dir.join(&name)
            } else {
                dir.join(&pkg).join(&name)
            };
            std::fs::read(path).ok().map(|b| (repo, pkg, b))
        });
        let Some((repo, pkg, bytes)) = src else {
            // Not readable (builtin exports without a source file): fall back to the recorded hash.
            let st = self.state.lock().unwrap();
            let h = st
                .bzl_deps
                .get(key)
                .map(|(h, _)| h.clone())
                .unwrap_or_else(|| "native".into());
            return crate::hash::sha256(h.as_bytes()).to_vec();
        };
        if stack.iter().any(|k| k == key) {
            return vec![];
        }
        stack.push(key.to_string());
        let mut data: Vec<u8> = crate::hash::sha256(&bytes).to_vec();
        let text = String::from_utf8_lossy(&bytes).to_string();
        let loads = self.bzl_loads_of(key, &text);
        for l in loads {
            if let Ok((r, p, n)) = self.resolve_label(&l, &repo, &pkg) {
                let child = format!("@@{r}//{p}:{n}");
                data.extend(self.transitive_digest_of(&child, memo, stack));
            }
        }
        stack.pop();
        let d = crate::hash::sha256(&data).to_vec();
        memo.insert(key.to_string(), d.clone());
        d
    }
}

/// Normalizes a Windows filesystem path string to Bazel's `PathFragment` / `StarlarkPath`
/// display form: strips extended-length `\\?\` prefixes, converts `\` to `/`, and uppercases
/// a leading drive letter (`c:\foo\bar` -> `C:/foo/bar`).
pub fn normalize_windows_starlark_path(raw: &str) -> String {
    let stripped = if let Some(rest) = raw
        .strip_prefix(r"\\?\UNC\")
        .or_else(|| raw.strip_prefix("//?/UNC/"))
    {
        format!("//{rest}")
    } else if let Some(rest) = raw
        .strip_prefix(r"\\?\")
        .or_else(|| raw.strip_prefix("//?/"))
    {
        rest.to_string()
    } else {
        raw.to_string()
    };
    let mut s = stripped.replace('\\', "/");
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes.len() == 2 || bytes[2] == b'/')
    {
        let upper = (bytes[0] as char).to_ascii_uppercase();
        s.replace_range(0..1, &upper.to_string());
    }
    while s.len() > 3
        && s.as_bytes()
            .get(0..3)
            .is_some_and(|p| p[0].is_ascii_alphabetic() && p[1] == b':' && p[2] == b'/')
        && s.ends_with('/')
    {
        s.pop();
    }
    s
}

pub fn path_value(p: &Path) -> Value {
    #[cfg(windows)]
    let formatted = normalize_windows_starlark_path(&p.display().to_string());
    #[cfg(not(windows))]
    let formatted = p.display().to_string();
    Value::Host("path".into(), formatted)
}

thread_local! {
    /// Set while the tag attributes of a module extension are built: label values resolved through a
    /// module's repo mapping are recorded (`StarlarkBazelModule`'s `staticRepoMappingRecorder`).
    pub static RECORD_TAG_MAPPINGS: std::cell::RefCell<Option<Vec<(String, String, String)>>> = const { std::cell::RefCell::new(None) };
}

impl Rt {
    pub fn set_lockfile(
        &self,
        mode: crate::lockfile::LockfileMode,
        workspace: crate::lockfile::Lockfile,
        hidden: crate::lockfile::Lockfile,
    ) {
        let mut l = self.lock.lock().unwrap();
        l.mode = Some(mode);
        l.workspace = workspace;
        l.hidden = hidden;
        l.used.clear();
    }

    /// Computes the lockfiles (workspace, hidden) to write after the session, per
    /// `BazelLockFileModule.combineModuleExtensions`.
    pub fn compute_lockfiles(&self) -> (crate::lockfile::Lockfile, crate::lockfile::Lockfile) {
        use crate::lockfile::*;
        let same_factor_deps = |f1: &str, f2: &str| -> bool {
            let shape = |f: &str| {
                (
                    f.split(',').any(|p| p.starts_with("os:")),
                    f.split(',').any(|p| p.starts_with("arch:")),
                )
            };
            shape(f1) == shape(f2)
        };
        let is_non_empty_facts = |j: &crate::json::Json| -> bool {
            !matches!(j, crate::json::Json::Null)
                && !matches!(j, crate::json::Json::Obj(o) if o.is_empty())
        };
        let l = self.lock.lock().unwrap();
        let res = &self.resolution;
        let live: std::collections::HashSet<String> = res
            .extension_ids()
            .iter()
            .filter(|i| !i.is_innate())
            .map(extension_key)
            .collect();
        let mut ws = Lockfile::default();
        let mut hid = Lockfile::default();
        // BazelLockFileModule drops `file:` registry hashes (commit 9d961b96f5).
        // Hidden lockfile never records registryFileHashes or selectedYankedVersions.
        ws.registry_file_hashes = res
            .registry_file_hashes
            .iter()
            .filter(|(u, _)| !u.starts_with("file:"))
            .map(|(u, h)| (u.clone(), h.clone()))
            .collect();
        ws.selected_yanked_versions = res.selected_yanked_versions.clone();
        for (id, f) in &l.workspace.facts {
            if live.contains(id) && is_non_empty_facts(f) {
                ws.facts.insert(id.clone(), f.clone());
            }
        }
        for (id, ver) in &l.workspace.facts_versions {
            if ws.facts.contains_key(id) && *ver != 0 {
                ws.facts_versions.insert(id.clone(), *ver);
            }
        }
        for (id, f) in &l.hidden.facts {
            if live.contains(id) && is_non_empty_facts(f) {
                hid.facts.insert(id.clone(), f.clone());
            }
        }
        for (id, ver) in &l.hidden.facts_versions {
            if hid.facts.contains_key(id) && *ver != 0 {
                hid.facts_versions.insert(id.clone(), *ver);
            }
        }
        for (id, m) in &l.workspace.module_extensions {
            if live.contains(id) {
                let kept: BTreeMap<String, LockExt> = m
                    .iter()
                    .filter(|(_, e)| !e.reproducible())
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if !kept.is_empty() {
                    ws.module_extensions.insert(id.clone(), kept);
                }
            }
        }
        for (id, m) in &l.hidden.module_extensions {
            if live.contains(id) {
                let kept: BTreeMap<String, LockExt> = m
                    .iter()
                    .filter(|(_, e)| e.reproducible())
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if !kept.is_empty() {
                    hid.module_extensions.insert(id.clone(), kept);
                }
            }
        }
        for u in &l.used {
            let id = &u.key;
            if !live.contains(id) {
                continue;
            }
            // Drop stale factor entries whose os/arch dependency shape no longer matches.
            for map in [&mut ws.module_extensions, &mut hid.module_extensions] {
                if let Some(m) = map.get_mut(id) {
                    m.retain(|old_f, _| same_factor_deps(old_f, &u.factors));
                    if m.is_empty() {
                        map.remove(id);
                    }
                }
            }
            let (target, other) = if u.entry.reproducible() {
                (&mut hid, &mut ws)
            } else {
                (&mut ws, &mut hid)
            };
            if let Some(m) = other.module_extensions.get_mut(id) {
                m.remove(&u.factors);
                if m.is_empty() {
                    other.module_extensions.remove(id);
                }
            }
            target
                .module_extensions
                .entry(id.clone())
                .or_default()
                .insert(u.factors.clone(), u.entry.clone());

            match u.facts.as_ref().filter(|f| is_non_empty_facts(f)) {
                Some(f) => {
                    ws.facts.insert(id.clone(), f.clone());
                    if u.facts_version != 0 {
                        ws.facts_versions.insert(id.clone(), u.facts_version);
                    } else {
                        ws.facts_versions.remove(id);
                    }
                    if u.entry.reproducible() {
                        hid.facts.insert(id.clone(), f.clone());
                        if u.facts_version != 0 {
                            hid.facts_versions.insert(id.clone(), u.facts_version);
                        } else {
                            hid.facts_versions.remove(id);
                        }
                    } else {
                        hid.facts.remove(id);
                        hid.facts_versions.remove(id);
                    }
                }
                None => {
                    ws.facts.remove(id);
                    ws.facts_versions.remove(id);
                    hid.facts.remove(id);
                    hid.facts_versions.remove(id);
                }
            }
        }
        ws.facts_versions
            .retain(|k, v| ws.facts.contains_key(k) && *v != 0);
        hid.facts_versions
            .retain(|k, v| hid.facts.contains_key(k) && *v != 0);
        (ws, hid)
    }
}
