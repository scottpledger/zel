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

//! The package loader: evaluates `BUILD` files (and the `.bzl` files they `load()`) into
//! [`Package`]s, mirroring `PackageFunction`, `PackageFactory`, `BzlLoadFunction`,
//! `StarlarkNativeModule`, `RuleFactory` and `TargetRecorder`.

use crate::attr::{
    convert_label, convert_plain, convert_value, convert_value_opt, infer_attribute, AttrType,
    AttrValue, Attribute, ConvertCtx, SelectPart,
};
use crate::glob::{self, GlobContext, GlobOp};
use crate::lookup::{
    EventKind, PackageEvent, PackageLookup, PackageLookupConfig, RepoFile, RepoResolver,
};
use crate::pkg_spec::PackageSpec;
use crate::rule_class::{native_rule_classes, starlark_rule_class, RuleClass};
use crate::{Package, PackageArgs, RuleExtra, RuleInstance, Target};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use zel_cmdline::{Label, PackageIdentifier, RepositoryMapping, RepositoryName};
use zel_starlark::{
    BigInt, EvalContext, Location, StarlarkDict, StarlarkError, StarlarkList, StructValue, Value,
};
use zel_vfs::{FileSystem, PathFragment};

/// Loader configuration (the relevant subset of Bazel's loading options).
pub struct LoaderConfig {
    pub lookup: PackageLookupConfig,
    /// Optional memoizing front for [`PackageLoader::load_package`] (Skyframe-backed in the
    /// server): consulted instead of loading from disk on every call.
    pub package_cache: Option<Arc<dyn PackageCache>>,
    /// Default of `glob(allow_empty = ...)` (`!--incompatible_disallow_empty_glob`).
    pub allow_empty_glob: bool,
    /// `--experimental_bzl_visibility` (default `true`): enables `visibility()` in `.bzl` files.
    pub experimental_bzl_visibility: bool,
    /// `--check_bzl_visibility` (default `true`): when `false`, demotes `.bzl` load visibility
    /// violations to warnings.
    pub check_bzl_visibility: bool,
    /// Whether to apply `//tools/build_rules:prelude_bazel` (main repo only).
    pub use_prelude: bool,
    /// Handler for host calls that belong to an embedding runtime (Bzlmod repository rules and
    /// `repository_ctx`/`module_ctx` methods called from helper `.bzl` files evaluated here).
    /// Consulted first for names accepted by [`is_external_host_call`].
    pub external_host: Option<zel_starlark::HostFn>,
    /// Lets an embedding runtime evaluate selected `.bzl` files itself (files defining repository
    /// rules / module extensions): returns `None` to let the loader evaluate the file.
    pub bzl_override: Option<BzlOverride>,
    /// Value of `--experimental_sibling_repository_layout` for Starlark `Label.workspace_root`.
    pub sibling_repository_layout: bool,
}

/// See [`LoaderConfig::bzl_override`].
pub type BzlOverride =
    Arc<dyn Fn(&Label) -> Option<Result<BTreeMap<String, Value>, String>> + Send + Sync>;

/// Whether a host call name (`call:repository_rule`, `method:module_ctx.file`, ...) is routed to
/// [`LoaderConfig::external_host`].
pub fn is_external_host_call(name: &str) -> bool {
    name.starts_with("method:")
        || name.starts_with("attr:")
        || name.starts_with("native_fn:")
        || matches!(
            name,
            "call:repository_rule"
                | "call:native_repo_rule"
                | "call:module_extension"
                | "repository_rule"
                | "module_extension"
                | "tag_class"
                | "extension_metadata"
        )
}

impl LoaderConfig {
    pub fn new(fs: Arc<dyn FileSystem>, resolver: Arc<dyn RepoResolver>) -> Self {
        Self {
            lookup: PackageLookupConfig {
                fs,
                resolver,
                deleted_packages: BTreeSet::new(),
            },
            package_cache: None,
            allow_empty_glob: true,
            experimental_bzl_visibility: true,
            check_bzl_visibility: true,
            use_prelude: true,
            external_host: None,
            bzl_override: None,
            sibling_repository_layout: false,
        }
    }
}

/// A memoizing cache of loaded packages. Implementations call
/// [`PackageLoader::load_package_uncached`] on a miss.
pub trait PackageCache: Send + Sync {
    fn load(
        &self,
        loader: &Arc<PackageLoader>,
        id: &PackageIdentifier,
    ) -> Result<Package, PackageLoadError>;
}

/// A package that could not be loaded at all (as opposed to one containing errors).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageLoadError {
    /// Bazel's message, e.g. `no such package 'x': BUILD file not found ...`.
    pub message: String,
    pub events: Vec<PackageEvent>,
}

impl std::fmt::Display for PackageLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Load visibility recorded by `visibility(...)` in a `.bzl` or `.scl` module (`BzlVisibility`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BzlVisibility {
    Public,
    Restricted(Vec<PackageSpec>),
}

impl BzlVisibility {
    pub(crate) fn allows(
        &self,
        defining_pkg: &PackageIdentifier,
        demanding_pkg: &PackageIdentifier,
    ) -> bool {
        if defining_pkg == demanding_pkg {
            return true;
        }
        match self {
            Self::Public => true,
            Self::Restricted(specs) => specs.iter().any(|s| s.matches_positive(demanding_pkg)),
        }
    }
}

/// Active `.bzl` initialization state passed to `visibility(...)` during top-level `.bzl` eval.
pub(crate) struct BzlInitSlot {
    pub(crate) initializing: bool,
    pub(crate) visibility: Option<BzlVisibility>,
}

/// An evaluated `.bzl` module.
struct BzlModule {
    exports: BTreeMap<String, Value>,
    visibility: BzlVisibility,
    /// `BzlLoadValue.getTransitiveDigest()` stand-in: SHA-256 over the file's digest and the
    /// transitive digests of its direct loads (the real one also covers the `@_builtins` digest of
    /// the Bazel binary, which has no counterpart here).
    digest: [u8; 32],
    observed_files: Vec<(PathFragment, [u8; 32])>,
}

/// Failure to load a `.bzl` file.
#[derive(Clone)]
struct BzlError {
    events: Vec<PackageEvent>,
    message: String,
    observed_files: Vec<(PathFragment, Option<[u8; 32]>)>,
}

type BzlResult = Result<Arc<BzlModule>, BzlError>;

#[derive(Clone)]
struct RuleDef {
    name: Option<String>,
    attrs: Vec<Attribute>,
    executable: bool,
    test: bool,
    outputs: Vec<(String, String)>,
    outputs_fn: Option<Value>,
    build_setting: bool,
    build_setting_flag: bool,
    extendable: bool,
    initializers: Vec<Value>,
    class: Option<Arc<RuleClass>>,
    implementation: Value,
    kwargs: BTreeMap<String, Value>,
    definition_stack: Vec<(String, String)>,
}

#[derive(Clone)]
struct MacroDef {
    name: Option<String>,
    attrs: Vec<Attribute>,
    implementation: Value,
    finalizer: bool,
    /// Package of the `.bzl` file that defined the macro.
    def_pkg: PackageIdentifier,
    /// `file:line:col` of the statement that defined the macro (`macro(...)` call).
    def_location: String,
}

#[derive(Clone)]
struct CachedBuiltins {
    toplevels: BTreeMap<String, Value>,
    bzl_cache: HashMap<String, BzlResult>,
    rules: Vec<RuleDef>,
    macros: Vec<MacroDef>,
}

#[derive(Default)]
struct Registry {
    rules: Vec<RuleDef>,
    macros: Vec<MacroDef>,
}

/// A queued symbolic macro invocation (`MacroInstance`).
struct MacroCall {
    macro_id: usize,
    name: String,
    /// Converted attribute values incl. defaults (`name` excluded).
    values: BTreeMap<String, AttrValue>,
    parent: Option<String>,
    location: String,
}

/// The package loader.
pub struct PackageLoader {
    cfg: LoaderConfig,
    rule_classes: BTreeMap<String, Arc<RuleClass>>,
    registry: Mutex<Registry>,
    bzl_cache: Mutex<HashMap<String, BzlResult>>,
    /// `exported_toplevels` of `@_builtins//:exports.bzl` (computed on first use).
    builtin_toplevels: Mutex<BuiltinToplevels>,
}

/// Key under which `exported_rules` are stored in the builtins map (not a valid identifier).
const EXPORTED_RULES_KEY: &str = "<exported_rules>";

#[derive(Default)]
enum BuiltinToplevels {
    #[default]
    Unloaded,
    Loading,
    Ready(BTreeMap<String, Value>),
    Failed(String),
}

/// Mutable state while a `BUILD` file is being evaluated (`Package.Builder`).
struct PackageState {
    id: PackageIdentifier,
    #[allow(dead_code)]
    build_file: PathFragment,
    pkg_dir: PathFragment,
    mapping: RepositoryMapping,
    targets: BTreeMap<String, Target>,
    /// Rule names in instantiation order (for `existing_rules()`).
    rule_order: Vec<String>,
    /// Location (`path:line:col`) at which each target was declared.
    target_locations: BTreeMap<String, String>,
    /// Name → location for symbolic macro instances.
    macro_names: BTreeMap<String, String>,
    output_prefixes: BTreeMap<String, String>,
    args: PackageArgs,
    package_called: bool,
    any_target_declared: bool,
    exported_files: BTreeMap<String, Vec<Label>>,
    exported_licenses: BTreeMap<String, Vec<String>>,
    group_includes: BTreeMap<String, Vec<Label>>,
    environment_groups: BTreeMap<String, (Vec<Label>, Vec<Label>)>,
    events: Vec<PackageEvent>,
    contains_errors: bool,
    glob_io_error: Option<String>,
    macro_queue: std::collections::VecDeque<MacroCall>,
    /// Stack of `(macro instance name, defining package, is_finalizer)` for the macro being expanded.
    macro_stack: Vec<(String, PackageIdentifier, bool)>,
    /// Active `macro_id` chain for direct/indirect symbolic-macro recursion detection.
    macro_call_ids: Vec<usize>,
    /// Parallel to `macro_stack`: (BUILD/caller call site, macro symbol name, macro definition location, main_submacro_defined).
    macro_frames: Vec<(String, String, String, bool)>,
    /// Explicit label values passed to top-level symbolic macro instances (for assumed input files).
    top_level_macro_labels: Vec<(Label, String)>,
    macro_violations: BTreeMap<String, String>,
    loads: Vec<zel_starlark::LoadStatement>,
}

impl PackageState {
    fn error(&mut self, loc: Option<String>, msg: impl Into<String>) {
        self.contains_errors = true;
        self.events.push(PackageEvent::error(loc, msg));
    }
}

fn fmt_loc(l: &Location) -> String {
    format!("{}:{}:{}", l.file, l.line, l.column)
}

/// Analysis-time API names that exist as predeclared symbols in `.bzl` files. They are bound to
/// inert values so that `.bzl` files defining rule implementations resolve; they are not
/// functional at load time.
const ANALYSIS_ONLY_GLOBALS: &[&str] = &[
    "DefaultInfo",
    "OutputGroupInfo",
    "RunEnvironmentInfo",
    "InstrumentedFilesInfo",
    "CcInfo",
    "JavaInfo",
    "PyInfo",
    "ProtoInfo",
    "DebugPackageInfo",
    "ToolchainInfo",
    "TemplateVariableInfo",
    "FilesToRunProvider",
    "analysis_test_transition",
    "cc_common",
    "java_common",
    "proto_common",
    "platform_common",
    "config_common",
    "apple_common",
    "testing",
    "coverage_common",
    "config",
    "transition",
    "exec_group",
    "subrule",
    "aspect",
    "module_extension",
    "repository_rule",
    "tag_class",
    "visibility",
    "Actions",
    "native_common",
    "AnalysisTestResultInfo",
    "AnalysisFailureInfo",
    "configuration_field",
    "PackageSpecificationInfo",
];

fn provider(name: &str) -> Value {
    Value::Provider {
        name: name.to_string(),
    }
}

fn mk_struct(fields: Vec<(&str, Value)>) -> Value {
    Value::Struct(Arc::new(StructValue::new(
        fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )))
}

/// Starlark API modules that are plain data in this implementation: `platform_common`,
/// `config_common`, `testing`, `config`, `transition`, `exec_group` (overriding the empty
/// placeholders from `ANALYSIS_ONLY_GLOBALS`).
fn install_analysis_modules(ctx: &mut EvalContext) {
    let host = |n: &str| Value::BuiltinFunction(format!("host:{n}"));
    ctx.globals.insert(
        "platform_common".into(),
        mk_struct(vec![
            ("ToolchainInfo", provider("ToolchainInfo")),
            ("PlatformInfo", provider("PlatformInfo")),
            ("ConstraintValueInfo", provider("ConstraintValueInfo")),
            ("ConstraintSettingInfo", provider("ConstraintSettingInfo")),
            ("TemplateVariableInfo", provider("TemplateVariableInfo")),
        ]),
    );
    // `apple_common.apple_toolchain()` is called while loading rules_cc (`objc_common.bzl`).
    ctx.globals.entry("apple_common".into()).and_modify(|v| {
        if let Value::Struct(st) = v {
            if st.fields.is_empty() {
                *v = mk_struct(vec![(
                    "apple_toolchain",
                    host("apple_common.apple_toolchain"),
                )]);
            }
        }
    });
    ctx.globals.insert(
        "config_common".into(),
        mk_struct(vec![
            ("FeatureFlagInfo", provider("FeatureFlagInfo")),
            ("toolchain_type", host("config_common.toolchain_type")),
        ]),
    );
    ctx.globals.insert(
        "testing".into(),
        mk_struct(vec![
            ("ExecutionInfo", provider("ExecutionInfo")),
            ("TestEnvironment", provider("TestEnvironment")),
        ]),
    );
    ctx.globals.insert(
        "config".into(),
        mk_struct(vec![
            ("int", host("config.int")),
            ("bool", host("config.bool")),
            ("string", host("config.string")),
            ("string_list", host("config.string_list")),
            ("string_set", host("config.string_set")),
            ("exec", host("config.exec")),
            ("target", host("config.target")),
            ("none", host("config.none")),
        ]),
    );
    ctx.globals.insert("aspect".into(), host("aspect"));
    ctx.globals.insert("subrule".into(), host("subrule"));
    ctx.globals.insert(
        "analysis_test_transition".into(),
        host("analysis_test_transition"),
    );
    ctx.globals
        .insert("configuration_field".into(), host("configuration_field"));
    ctx.globals.insert(
        "coverage_common".into(),
        mk_struct(vec![(
            "instrumented_files_info",
            host("coverage_common.instrumented_files_info"),
        )]),
    );
    ctx.globals.insert("transition".into(), host("transition"));
    ctx.globals.insert("exec_group".into(), host("exec_group"));
    ctx.globals
        .insert("repository_rule".into(), host("repository_rule"));
    ctx.globals
        .insert("module_extension".into(), host("module_extension"));
    ctx.globals.insert("tag_class".into(), host("tag_class"));
}

fn mk_list(v: Vec<Value>) -> Value {
    let val = Value::List(Arc::new(Mutex::new(StarlarkList::new(v))));
    val.freeze();
    val
}

fn mk_dict(items: Vec<(Value, Value)>) -> Value {
    let val = Value::Dict(Arc::new(Mutex::new(
        StarlarkDict::from_entries(items).unwrap_or_default(),
    )));
    val.freeze();
    val
}

fn int(n: i64) -> Value {
    Value::Int(BigInt::from_i64(n))
}

fn sl_values(v: &Value) -> Result<Vec<String>, String> {
    match v {
        Value::List(l) => l
            .lock()
            .unwrap()
            .elements
            .iter()
            .map(|e| match e {
                Value::String(s) => Ok(s.clone()),
                other => Err(format!(
                    "expected value of type 'string' but got {} ({})",
                    other.repr(),
                    other.type_name()
                )),
            })
            .collect(),
        Value::Tuple(t) => t
            .iter()
            .map(|e| match e {
                Value::String(s) => Ok(s.clone()),
                other => Err(format!(
                    "expected value of type 'string' but got {} ({})",
                    other.repr(),
                    other.type_name()
                )),
            })
            .collect(),
        other => Err(format!(
            "expected value of type 'sequence' but got {} ({})",
            other.repr(),
            other.type_name()
        )),
    }
}

fn arg<'a>(
    pos: &'a [Value],
    kw: &'a BTreeMap<String, Value>,
    idx: usize,
    name: &str,
) -> Option<&'a Value> {
    kw.get(name).or_else(|| pos.get(idx))
}

fn check_kwargs(func: &str, kw: &BTreeMap<String, Value>, allowed: &[&str]) -> Result<(), String> {
    for k in kw.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(format!("{func}() got unexpected keyword argument '{k}'"));
        }
    }
    Ok(())
}

impl PackageLoader {
    pub fn new(mut cfg: LoaderConfig) -> Arc<Self> {
        // Serve `@_builtins` (Bazel's embedded Starlark builtins) next to the configured repos.
        if let Ok(root) = crate::builtins::materialize() {
            cfg.lookup.resolver = Arc::new(crate::builtins::BuiltinsResolver {
                inner: cfg.lookup.resolver.clone(),
                root,
            });
        }
        Arc::new(Self {
            builtin_toplevels: Mutex::new(BuiltinToplevels::Unloaded),
            cfg,
            rule_classes: native_rule_classes(),
            registry: Mutex::new(Registry::default()),
            bzl_cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn config(&self) -> &LoaderConfig {
        &self.cfg
    }

    /// Drops cached `.bzl` modules (call when files changed).
    /// Loads the `.bzl` file `label` and returns its public global `name` (e.g. the aspect
    /// behind `--aspects=//pkg:defs.bzl%my_aspect`).
    pub fn load_bzl_export(self: &Arc<Self>, label: &Label, name: &str) -> Result<Value, String> {
        let m = self.load_bzl(label, &[]).map_err(|e| {
            let mut lines: Vec<String> = e.events.iter().map(|ev| ev.render()).collect();
            lines.push(e.message.clone());
            lines.join("\n")
        })?;
        m.exports.get(name).cloned().ok_or_else(|| {
            format!(
                "file '{}' does not contain a symbol '{name}'",
                label.canonical_form()
            )
        })
    }

    /// Loads the `.bzl` file `label` and returns all its public globals (what `load()` sees).
    /// Errors carry the rendered events of the failing module followed by Bazel's message.
    pub fn load_bzl_exports(
        self: &Arc<Self>,
        label: &Label,
    ) -> Result<BTreeMap<String, Value>, String> {
        let m = self.load_bzl(label, &[]).map_err(|e| {
            let mut lines: Vec<String> = e.events.iter().map(|ev| ev.render()).collect();
            lines.push(e.message.clone());
            lines.join("\n")
        })?;
        Ok(m.exports.clone())
    }

    pub fn invalidate_bzl_cache(&self) {
        self.bzl_cache.lock().unwrap().clear();
    }

    fn fs(&self) -> &dyn FileSystem {
        self.cfg.lookup.fs.as_ref()
    }

    // ------------------------------------------------------------------ package loading

    /// Looks up and evaluates the package `id` (through `LoaderConfig::package_cache` if set).
    pub fn load_package(
        self: &Arc<Self>,
        id: &PackageIdentifier,
    ) -> Result<Package, PackageLoadError> {
        if let Some(cache) = &self.cfg.package_cache {
            return cache.load(self, id);
        }
        self.load_package_uncached(id)
    }

    /// Looks up and evaluates the package `id`, bypassing the package cache.
    pub fn load_package_uncached(
        self: &Arc<Self>,
        id: &PackageIdentifier,
    ) -> Result<Package, PackageLoadError> {
        let lookup = self.cfg.lookup.lookup(id);
        match lookup {
            PackageLookup::Found { build_file } => {
                let source = match self.fs().read_file(&build_file) {
                    Ok(b) => String::from_utf8_lossy(&b).to_string(),
                    Err(e) => {
                        return Err(PackageLoadError {
                            message: format!(
                                "error loading package '{}': Encountered error while reading BUILD file '{}': {e}",
                                id.package_fragment(),
                                build_file
                            ),
                            events: Vec::new(),
                        })
                    }
                };
                let (pkg, fatal) = self.evaluate_source_impl(id, &build_file, &source);
                match fatal {
                    None => Ok(pkg),
                    Some(msg) => Err(PackageLoadError {
                        message: format!(
                            "error loading package '{}': {msg}",
                            id.package_fragment()
                        ),
                        events: pkg.events,
                    }),
                }
            }
            other => {
                let pkg_display = if id.repository().is_main() {
                    id.package_fragment().to_string()
                } else if matches!(other, crate::lookup::PackageLookup::RepoNotFound(_))
                    && id.repository().is_visible()
                {
                    format!(
                        "@@[unknown repo '{}' requested from @@]//{}",
                        id.repository().name(),
                        id.package_fragment()
                    )
                } else {
                    // The repository exists or already carries non-visible owner context.
                    format!(
                        "{}//{}",
                        id.repository().canonical_form(),
                        id.package_fragment()
                    )
                };
                Err(PackageLoadError {
                    message: format!(
                        "no such package '{pkg_display}': {}",
                        self.cfg.lookup.error_message(id, &other)
                    ),
                    events: Vec::new(),
                })
            }
        }
    }

    /// Evaluates `source` as the BUILD file of `id`.
    pub fn evaluate_source(
        self: &Arc<Self>,
        id: &PackageIdentifier,
        build_file: &PathFragment,
        source: &str,
    ) -> Package {
        let (mut pkg, fatal) = self.evaluate_source_impl(id, build_file, source);
        if let Some(msg) = fatal {
            pkg.contains_errors = true;
            pkg.events.push(PackageEvent::error(
                None,
                format!("error loading package '{}': {msg}", id.package_fragment()),
            ));
        }
        pkg
    }

    /// Evaluates the BUILD file; the second result is the message of a *fatal* load failure
    /// (a `.bzl` dependency that cannot be loaded, malformed `load()` labels): Bazel throws
    /// `error loading package '<pkg>': <message>` instead of reporting a package with errors.
    fn evaluate_source_impl(
        self: &Arc<Self>,
        id: &PackageIdentifier,
        build_file: &PathFragment,
        source: &str,
    ) -> (Package, Option<String>) {
        let repo_root = self
            .cfg
            .lookup
            .resolver
            .repo_root(id.repository())
            .unwrap_or_else(PathFragment::empty);
        let pkg_dir = repo_root.get_relative(id.package_fragment());
        let mapping = self.cfg.lookup.resolver.mapping(id.repository());
        let mut st = PackageState {
            id: id.clone(),
            build_file: build_file.clone(),
            pkg_dir: pkg_dir.clone(),
            mapping,
            targets: BTreeMap::new(),
            rule_order: Vec::new(),
            target_locations: BTreeMap::new(),
            macro_names: BTreeMap::new(),
            output_prefixes: BTreeMap::new(),
            args: PackageArgs::default(),
            package_called: false,
            any_target_declared: false,
            exported_files: BTreeMap::new(),
            exported_licenses: BTreeMap::new(),
            group_includes: BTreeMap::new(),
            environment_groups: BTreeMap::new(),
            events: Vec::new(),
            contains_errors: false,
            glob_io_error: None,
            macro_queue: Default::default(),
            macro_stack: Vec::new(),
            macro_call_ids: Vec::new(),
            macro_frames: Vec::new(),
            top_level_macro_labels: Vec::new(),
            macro_violations: BTreeMap::new(),
            loads: Vec::new(),
        };
        // Pre-insert the BUILD file InputFile target before evaluation so a later target of the
        // same name conflicts (`Package.java`, `InputFile.java`).
        let bf_name = build_file.base_name();
        match Label::create(id.clone(), bf_name) {
            Ok(label) => {
                st.targets
                    .insert(bf_name.to_string(), Target::InputFile { label });
            }
            Err(e) => {
                let pkg = Package {
                    id: id.clone(),
                    build_file: build_file.clone(),
                    targets: BTreeMap::new(),
                    loads: Vec::new(),
                    default_visibility: Vec::new(),
                    args: st.args,
                    exported_files: BTreeMap::new(),
                    group_includes: BTreeMap::new(),
                    target_locations: BTreeMap::new(),
                    environment_groups: BTreeMap::new(),
                    macro_namespace_violating_targets: BTreeMap::new(),
                    events: Vec::new(),
                    contains_errors: true,
                };
                return (pkg, Some(format!("illegal BUILD file name: {e}")));
            }
        }
        // `javatests/` packages default to `default_testonly = True` (`Package.java`).
        if id
            .package_fragment()
            .path_string()
            .starts_with("javatests/")
        {
            st.args.default_testonly = Some(true);
        }
        // repo() defaults from REPO.bazel apply to every package of the repository.
        match eval_repo_file_checked(self.fs(), &repo_root, id.repository(), &st.mapping) {
            Ok(Some((_, repo_args))) => {
                st.args.merge_from(&repo_args);
            }
            Ok(None) => {}
            Err(msg) => {
                let pkg = Package {
                    id: id.clone(),
                    build_file: build_file.clone(),
                    targets: st.targets,
                    loads: Vec::new(),
                    default_visibility: Vec::new(),
                    args: st.args,
                    exported_files: BTreeMap::new(),
                    group_includes: BTreeMap::new(),
                    target_locations: BTreeMap::new(),
                    environment_groups: BTreeMap::new(),
                    macro_namespace_violating_targets: BTreeMap::new(),
                    events: vec![PackageEvent::error(None, msg.clone())],
                    contains_errors: true,
                };
                return (pkg, Some(msg));
            }
        }
        let state = Arc::new(Mutex::new(st));

        let mut ctx = EvalContext::new(pkg_dir.clone(), Some(Arc::clone(&self.cfg.lookup.fs)));
        ctx.semantics.experimental_sibling_repository_layout = self.cfg.sibling_repository_layout;
        ctx.thread.semantics.experimental_sibling_repository_layout =
            self.cfg.sibling_repository_layout;
        ctx.file_name = build_file.path_string().to_string();
        ctx.strict_names = true;
        ctx.captured_prints = Some(Vec::new());
        self.install_build_globals(&mut ctx);
        self.inject_builtin_toplevels(&mut ctx, true);
        self.install_host(&mut ctx, Some(Arc::clone(&state)), None, id.clone());
        let sink: Arc<Mutex<Vec<PackageEvent>>> = Arc::new(Mutex::new(Vec::new()));
        self.install_load_hook(
            &mut ctx,
            id.repository().clone(),
            id.clone(),
            Arc::clone(&sink),
            Vec::new(),
            Arc::new(Mutex::new(Vec::new())),
            None,
        );
        let build_label_display = Label::create(id.clone(), bf_name)
            .map(|l| l.canonical_form())
            .unwrap_or_else(|_| format!("{}:{bf_name}", id.canonical_form()));
        self.install_preload_hook(
            &mut ctx,
            id.repository().clone(),
            id.clone(),
            build_label_display,
            Arc::clone(&sink),
        );

        // Prelude (`//tools/build_rules:prelude_bazel` in the package's own repository).
        let mut prelude_errors: Vec<PackageEvent> = Vec::new();
        if self.cfg.use_prelude {
            let prelude = repo_root.get_relative_str("tools/build_rules/prelude_bazel");
            if self
                .fs()
                .stat(&prelude, true)
                .map(|s| s.is_file)
                .unwrap_or(false)
            {
                match self.load_prelude(id.repository(), &prelude) {
                    Ok(exports) => {
                        for (k, v) in exports {
                            ctx.globals.insert(k, v);
                        }
                    }
                    Err(e) => {
                        prelude_errors.extend(e.events);
                        prelude_errors.push(PackageEvent::error(None, e.message));
                    }
                }
            }
        }

        let result = if prelude_errors.is_empty() {
            Some(ctx.eval_file_checked(source, Some(("BUILD files", true))))
        } else {
            None
        };
        let fatal = ctx.fatal_error.take();

        // Drain captured prints and flush into events (in order of occurrence).
        let mut early_events: Vec<PackageEvent> = Vec::new();
        early_events.extend(prelude_errors);
        let nested: Vec<PackageEvent> = std::mem::take(&mut *sink.lock().unwrap());
        {
            let mut s = state.lock().unwrap();
            if nested.iter().any(|e| e.kind == EventKind::Error) {
                s.contains_errors = true;
            }
            s.events.extend(nested);
        }

        let mut run_macros = true;
        match result {
            Some(Ok(())) => {}
            Some(Err(errs)) => {
                run_macros = false;
                let mut s = state.lock().unwrap();
                for line in errs {
                    let (loc, msg) = split_location(&dedup_location(&line));
                    // Bazel reports Starlark evaluation failures (they carry their own
                    // traceback) without a location prefix.
                    let loc = if msg.starts_with("Traceback (most recent call last):") {
                        None
                    } else {
                        loc
                    };
                    s.error(loc, msg);
                }
            }
            None => {
                run_macros = false;
                state.lock().unwrap().contains_errors = true;
            }
        }
        // Prints emitted during evaluation.
        {
            let mut s = state.lock().unwrap();
            for e in early_events {
                s.contains_errors = true;
                s.events.push(e);
            }
            if let Some(prints) = ctx.captured_prints.take() {
                for (loc, msg) in prints {
                    s.events.push(PackageEvent {
                        kind: EventKind::Debug,
                        location: Some(fmt_loc(&loc)),
                        message: msg,
                    });
                }
            }
        }
        let run_finalizers = run_macros && !state.lock().unwrap().contains_errors;
        let macro_ok = if run_finalizers {
            ctx.captured_prints = Some(Vec::new());
            let ok = self.run_macro_queue(&mut ctx, &state);
            if let Some(prints) = ctx.captured_prints.take() {
                let mut s = state.lock().unwrap();
                for (loc, msg) in prints {
                    s.events.push(PackageEvent {
                        kind: EventKind::Debug,
                        location: Some(fmt_loc(&loc)),
                        message: msg,
                    });
                }
            }
            ok
        } else {
            run_macros && !state.lock().unwrap().contains_errors
        };
        // Materialize assumed input files even on partial packages (`Package.java`).
        self.finish_package(&state, run_macros && macro_ok);

        let mut s = state.lock().unwrap();
        let fatal = fatal.or_else(|| s.glob_io_error.take());
        let targets = std::mem::take(&mut s.targets);
        let default_vis = s
            .args
            .default_visibility
            .as_ref()
            .map(|v| v.iter().map(|l| l.canonical_form()).collect())
            .unwrap_or_default();
        let package = Package {
            id: id.clone(),
            build_file: build_file.clone(),
            targets,
            loads: std::mem::take(&mut s.loads)
                .into_iter()
                .chain(ctx.loads.clone())
                .collect(),
            default_visibility: default_vis,
            args: s.args.clone(),
            exported_files: std::mem::take(&mut s.exported_files),
            group_includes: std::mem::take(&mut s.group_includes),
            target_locations: std::mem::take(&mut s.target_locations),
            environment_groups: std::mem::take(&mut s.environment_groups),
            macro_namespace_violating_targets: std::mem::take(&mut s.macro_violations),
            events: std::mem::take(&mut s.events),
            contains_errors: s.contains_errors,
        };
        (package, fatal)
    }

    // ------------------------------------------------------------------ globals & host

    /// Predeclared symbols of `BUILD` files: native rules, `glob`, `package`, `select`,
    /// `depset` (no `native`, `struct`, `Label`, `provider`, or `rule_placeholder`).
    fn install_build_globals(&self, ctx: &mut EvalContext) {
        for name in self.rule_classes.keys() {
            ctx.globals.insert(
                name.clone(),
                Value::BuiltinFunction(format!("host:rule.{name}")),
            );
        }
        for f in BUILD_FUNCTIONS {
            ctx.globals.insert(
                (*f).to_string(),
                Value::BuiltinFunction(format!("host:{f}")),
            );
        }
        for n in ["select", "depset"] {
            ctx.globals
                .entry(n.to_string())
                .or_insert_with(|| Value::BuiltinFunction(n.to_string()));
        }
    }

    /// Symbols shared by `.bzl` files.
    fn install_common_globals(&self, ctx: &mut EvalContext) {
        for n in [
            "select",
            "depset",
            "struct",
            "Label",
            "provider",
            "rule_placeholder",
        ] {
            ctx.globals
                .entry(n.to_string())
                .or_insert_with(|| Value::BuiltinFunction(n.to_string()));
        }
        let mut native = BTreeMap::new();
        for name in self.rule_classes.keys() {
            native.insert(
                name.clone(),
                Value::BuiltinFunction(format!("host:rule.{name}")),
            );
        }
        for f in BUILD_FUNCTIONS {
            native.insert(
                (*f).to_string(),
                Value::BuiltinFunction(format!("host:{f}")),
            );
        }
        ctx.globals.insert(
            "native".to_string(),
            Value::Struct(Arc::new(StructValue::new(native))),
        );
    }

    fn install_bzl_globals(&self, ctx: &mut EvalContext) {
        self.install_common_globals(ctx);
        for (n, h) in [
            ("rule", "host:rule"),
            ("macro", "host:macro"),
            ("provider", "host:provider"),
            // `Label()` in a .bzl resolves relative to the file's own repository.
            ("Label", "host:Label"),
        ] {
            ctx.globals
                .insert(n.to_string(), Value::BuiltinFunction(h.to_string()));
        }
        let mut attr = BTreeMap::new();
        for t in ATTR_FUNCTIONS {
            attr.insert(
                (*t).to_string(),
                Value::BuiltinFunction(format!("host:attr.{t}")),
            );
        }
        ctx.globals.insert(
            "attr".to_string(),
            Value::Struct(Arc::new(StructValue::new(attr))),
        );
        for n in ANALYSIS_ONLY_GLOBALS {
            ctx.globals.entry((*n).to_string()).or_insert_with(|| {
                if n.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                    Value::Provider {
                        name: (*n).to_string(),
                    }
                } else {
                    Value::Struct(Arc::new(StructValue::default()))
                }
            });
        }
        install_analysis_modules(ctx);
        // Natives that Bazel's builtins reference through `_builtins.toplevel`.
        let mut pcdnu = BTreeMap::new();
        pcdnu.insert(
            "incompatible_enable_proto_toolchain_resolution".to_string(),
            Value::BuiltinFunction(
                "host:proto_common_do_not_use.incompatible_enable_proto_toolchain_resolution"
                    .into(),
            ),
        );
        pcdnu.insert(
            "external_proto_infos".to_string(),
            Value::BuiltinFunction("host:proto_common_do_not_use.external_proto_infos".into()),
        );
        ctx.globals.insert(
            "proto_common_do_not_use".to_string(),
            Value::Struct(Arc::new(StructValue::new(pcdnu))),
        );
        ctx.globals
            .entry("exec_transition".to_string())
            .or_insert_with(|| Value::BuiltinFunction("host:exec_transition".to_string()));
        // `visibility([...])` in .bzl files restricts who may load the file; load visibility is not
        // enforced yet, the call itself is valid (and returns None).
        ctx.globals.insert(
            "visibility".to_string(),
            Value::BuiltinFunction("host:visibility".to_string()),
        );
        // Native `cc_common`: rules_cc's Starlark is layered on `cc_common.internal_DO_NOT_USE()`.
        let mut cc_common = BTreeMap::new();
        cc_common.insert(
            "internal_DO_NOT_USE".to_string(),
            Value::BuiltinFunction("host:cc_common.internal_DO_NOT_USE".to_string()),
        );
        ctx.globals.insert(
            "cc_common".to_string(),
            Value::Struct(Arc::new(StructValue::new(cc_common))),
        );
        // `rule`, `aspect`, etc. are handled by name for bzlmod; ours override via host.
        ctx.globals.remove("rule_placeholder");
    }

    fn install_host(
        self: &Arc<Self>,
        ctx: &mut EvalContext,
        state: Option<Arc<Mutex<PackageState>>>,
        bzl_init: Option<Arc<Mutex<BzlInitSlot>>>,
        owner: PackageIdentifier,
    ) {
        let this = Arc::clone(self);
        ctx.host = Some(Arc::new(move |ctx, name, pos, kw| {
            this.dispatch_host(
                ctx,
                state.as_ref(),
                bzl_init.as_ref(),
                &owner,
                name,
                pos,
                kw,
            )
        }));
    }

    #[allow(clippy::type_complexity, clippy::too_many_arguments)]
    fn install_load_hook(
        self: &Arc<Self>,
        ctx: &mut EvalContext,
        repo: RepositoryName,
        owner_pkg: PackageIdentifier,
        sink: Arc<Mutex<Vec<PackageEvent>>>,
        chain: Vec<String>,
        child_digests: Arc<Mutex<Vec<([u8; 32], Vec<(PathFragment, [u8; 32])>)>>>,
        bzl_vis_failed: Option<Arc<Mutex<bool>>>,
    ) {
        let this = Arc::clone(self);
        ctx.load_hook = Some(Arc::new(move |module_label: &str| {
            let mapping = this.cfg.lookup.resolver.mapping(&repo);
            let label = parse_load_label(module_label, &owner_pkg, &mapping)?;
            match this.load_bzl(&label, &chain) {
                Ok(m) => {
                    if let Some(ref vis_failed) = bzl_vis_failed {
                        if !m.visibility.allows(label.package_identifier(), &owner_pkg) {
                            let msg = format!(
                                "Starlark file {} is not visible for loading from package {}. Check the file's `visibility()` declaration, or override this check by passing --nocheck_bzl_visibility.",
                                label.canonical_form(),
                                owner_pkg.canonical_form(),
                            );
                            if this.cfg.check_bzl_visibility {
                                sink.lock()
                                    .unwrap()
                                    .push(PackageEvent::error(None, msg.clone()));
                                *vis_failed.lock().unwrap() = true;
                                return Err(msg);
                            } else {
                                let mut s = sink.lock().unwrap();
                                s.push(PackageEvent {
                                    kind: EventKind::Warning,
                                    location: None,
                                    message: msg,
                                });
                                s.push(PackageEvent {
                                    kind: EventKind::Warning,
                                    location: None,
                                    message:
                                        "Continuing because --nocheck_bzl_visibility is active"
                                            .to_string(),
                                });
                            }
                        }
                    }
                    child_digests
                        .lock()
                        .unwrap()
                        .push((m.digest, m.observed_files.clone()));
                    Ok(m.exports.clone())
                }
                Err(e) => {
                    // Events of the failed module are reported before the importer's own error.
                    sink.lock().unwrap().extend(e.events.clone());
                    Err(e.message)
                }
            }
        }));
    }

    /// Resolves all top-level loads of a BUILD file up front, like Bazel: malformed labels are
    /// reported at their location (`malformed load statements`), then each `.bzl` dependency is
    /// loaded in order and the first failure aborts loading the package.
    fn install_preload_hook(
        self: &Arc<Self>,
        ctx: &mut EvalContext,
        repo: RepositoryName,
        owner_pkg: PackageIdentifier,
        build_label_display: String,
        sink: Arc<Mutex<Vec<PackageEvent>>>,
    ) {
        let this = Arc::clone(self);
        let file = ctx.file_name.clone();
        ctx.preload_hook = Some(Arc::new(move |loads: &[zel_starlark::PreloadItem]| {
            let mapping = this.cfg.lookup.resolver.mapping(&repo);
            let mut events = Vec::new();
            let mut labels = Vec::new();
            for (raw, (line, col)) in loads {
                let parsed = parse_load_label(raw, &owner_pkg, &mapping).and_then(|l| {
                    if l.name().ends_with(".bzl") || l.name().ends_with(".scl") {
                        Ok(l)
                    } else {
                        Err(
                            "The label must reference a file with extension \".bzl\" or \".scl\""
                                .to_string(),
                        )
                    }
                });
                match parsed {
                    Ok(l) => labels.push((l, (*line, *col))),
                    Err(e) => {
                        let e = e.strip_prefix("in load statement: ").unwrap_or(&e);
                        events.push(format!("{file}:{line}:{col}: in load statement: {e}"));
                    }
                }
            }
            if !events.is_empty() {
                return Err(zel_starlark::PreloadFailure {
                    events,
                    fatal: "malformed load statements".to_string(),
                });
            }
            let mut canonical = Vec::new();
            let mut has_visibility_violation = false;
            for (label, (line, col)) in labels {
                match this.load_bzl(&label, &[]) {
                    Ok(m) => {
                        if !m.visibility.allows(label.package_identifier(), &owner_pkg) {
                            let loc = Some(format!("{file}:{line}:{col}"));
                            let msg = format!(
                                "Starlark file {} is not visible for loading from package {}. Check the file's `visibility()` declaration, or override this check by passing --nocheck_bzl_visibility.",
                                label.canonical_form(),
                                owner_pkg.canonical_form(),
                            );
                            if this.cfg.check_bzl_visibility {
                                sink.lock().unwrap().push(PackageEvent::error(loc, msg));
                                has_visibility_violation = true;
                            } else {
                                let mut s = sink.lock().unwrap();
                                s.push(PackageEvent {
                                    kind: EventKind::Warning,
                                    location: loc,
                                    message: msg,
                                });
                                s.push(PackageEvent {
                                    kind: EventKind::Warning,
                                    location: None,
                                    message:
                                        "Continuing because --nocheck_bzl_visibility is active"
                                            .to_string(),
                                });
                            }
                        }
                    }
                    Err(e) => {
                        sink.lock().unwrap().extend(e.events.clone());
                        return Err(zel_starlark::PreloadFailure {
                            events: Vec::new(),
                            fatal: e.message,
                        });
                    }
                }
                canonical.push(label.canonical_form());
            }
            if has_visibility_violation {
                return Err(zel_starlark::PreloadFailure {
                    events: Vec::new(),
                    fatal: format!(
                        "file {build_label_display} contains .bzl load visibility violations"
                    ),
                });
            }
            Ok(canonical)
        }));
    }

    // ------------------------------------------------------------------ .bzl loading

    fn load_prelude(
        self: &Arc<Self>,
        repo: &RepositoryName,
        path: &PathFragment,
    ) -> Result<BTreeMap<String, Value>, BzlError> {
        let pkg =
            PackageIdentifier::create(repo.clone(), PathFragment::create("tools/build_rules"))
                .map_err(|e| BzlError {
                    events: Vec::new(),
                    message: e.to_string(),
                    observed_files: Vec::new(),
                })?;
        let label = Label::create(pkg, "prelude_bazel").map_err(|e| BzlError {
            events: Vec::new(),
            message: e.to_string(),
            observed_files: Vec::new(),
        })?;
        self.eval_bzl_module(&label, path, &[], true)
            .map(|(m, all_globals)| {
                let _ = m;
                all_globals
            })
    }

    fn bzl_entry_fresh(&self, entry: &BzlResult) -> bool {
        match entry {
            Ok(m) => m.observed_files.iter().all(|(p, h)| {
                self.fs()
                    .read_file(p)
                    .map(|b| zel_util::sha256::digest(&b) == *h)
                    .unwrap_or(false)
            }),
            Err(e) => {
                !e.observed_files.is_empty()
                    && e.observed_files.iter().all(|(p, h)| {
                        let cur = self
                            .fs()
                            .read_file(p)
                            .ok()
                            .map(|b| zel_util::sha256::digest(&b));
                        cur == *h
                    })
            }
        }
    }

    fn load_bzl(self: &Arc<Self>, label: &Label, chain: &[String]) -> BzlResult {
        let key = label.canonical_form();
        if chain.contains(&key) {
            let mut cyc = chain.to_vec();
            cyc.push(key.clone());
            return Err(BzlError {
                events: Vec::new(),
                message: format!("Starlark load cycle detected: {}", cyc.join(" -> ")),
                observed_files: Vec::new(),
            });
        }
        if let Some(r) = self.bzl_cache.lock().unwrap().get(&key).cloned() {
            if self.bzl_entry_fresh(&r) {
                return r;
            }
        }
        let result = self.load_bzl_uncached(label, chain);
        self.bzl_cache.lock().unwrap().insert(key, result.clone());
        result
    }

    fn load_bzl_uncached(self: &Arc<Self>, label: &Label, chain: &[String]) -> BzlResult {
        if let Some(h) = &self.cfg.bzl_override {
            if let Some(r) = h(label) {
                return r
                    .map(|exports| {
                        Arc::new(BzlModule {
                            exports,
                            visibility: BzlVisibility::Public,
                            digest: [0u8; 32],
                            observed_files: Vec::new(),
                        })
                    })
                    .map_err(|message| BzlError {
                        events: Vec::new(),
                        message,
                        observed_files: Vec::new(),
                    });
            }
        }
        let pkg = label.package_identifier();
        let fail = |message: String| BzlError {
            events: Vec::new(),
            message,
            observed_files: Vec::new(),
        };
        let lookup = self.cfg.lookup.lookup(pkg);
        let root = match &lookup {
            PackageLookup::Found { .. } => self
                .cfg
                .lookup
                .resolver
                .repo_root(pkg.repository())
                .unwrap(),
            PackageLookup::NoBuildFile => {
                // `BzlLoadFunction.noBuildFile` without a reason.
                return Err(fail(format!(
                    "Every .bzl file must have a corresponding package, but '{}' does not have one. Please create a BUILD file in the same or any parent directory. Note that this BUILD file does not need to do anything except exist.",
                    label.canonical_form()
                )));
            }
            other => {
                return Err(fail(format!(
                    "Unable to find package for {}: {}.",
                    label.canonical_form(),
                    self.cfg.lookup.error_message(pkg, other)
                )))
            }
        };
        if let Some(msg) = self.cfg.lookup.label_crossing_subpackage_error(label) {
            return Err(fail(msg));
        }
        let path = root
            .get_relative(pkg.package_fragment())
            .get_relative_str(label.name());
        match self.fs().stat(&path, true) {
            Ok(st) if st.is_file => {}
            _ => {
                return Err(fail(format!(
                    "cannot load '{}': no such file",
                    label.canonical_form()
                )))
            }
        }
        self.eval_bzl_file(label, &path, chain)
    }

    fn eval_builtin_toplevels_uncached(
        self: &Arc<Self>,
    ) -> Result<BTreeMap<String, Value>, String> {
        let label =
            Label::parse_canonical("@@_builtins//:exports.bzl").map_err(|e| e.to_string())?;
        let exports = self.load_bzl_exports(&label)?;
        let Some(Value::Dict(d)) = exports.get("exported_toplevels") else {
            return Err("exports.bzl does not define exported_toplevels".into());
        };
        let mut out = BTreeMap::new();
        for (k, v) in d.lock().unwrap().entries.iter() {
            if let Value::String(name) = k {
                out.insert(name.clone(), v.clone());
            }
        }
        // Version skew (see scripts/version_skew_allowlist.txt, `builtins_cc_common`): rules_apple 3.x,
        // part of the module graph of every Bazel 9.3.x workspace, reads `cc_common.CcToolchainInfo`,
        // which the 9.3.2 binary still has and HEAD's builtins `cc_common` no longer defines.
        if let Some(Value::Struct(cc)) = out.get("cc_common").cloned() {
            if !cc.fields.contains_key("CcToolchainInfo") {
                let mut fields = cc.fields.clone();
                fields.insert(
                    "CcToolchainInfo".to_string(),
                    Value::Provider {
                        name: "CcToolchainInfo".into(),
                    },
                );
                out.insert(
                    "cc_common".to_string(),
                    Value::Struct(Arc::new(StructValue::new(fields))),
                );
            }
        }
        // `exported_rules` become members of `native` (and BUILD-file symbols).
        if let Some(r @ Value::Dict(_)) = exports.get("exported_rules") {
            out.insert(EXPORTED_RULES_KEY.to_string(), r.clone());
        }
        Ok(out)
    }

    fn cached_embedded_builtins() -> &'static Result<CachedBuiltins, String> {
        static CACHE: std::sync::OnceLock<Result<CachedBuiltins, String>> =
            std::sync::OnceLock::new();
        CACHE.get_or_init(|| {
            let root = crate::builtins::materialize()?;
            let mut cfg = LoaderConfig::new(
                Arc::new(zel_vfs::LocalFileSystem::default()),
                Arc::new(crate::lookup::SimpleRepoResolver::new(root.clone())),
            );
            cfg.lookup.resolver = Arc::new(crate::builtins::BuiltinsResolver {
                inner: cfg.lookup.resolver.clone(),
                root,
            });
            let template = Arc::new(Self {
                builtin_toplevels: Mutex::new(BuiltinToplevels::Loading),
                cfg,
                rule_classes: native_rule_classes(),
                registry: Mutex::new(Registry::default()),
                bzl_cache: Mutex::new(HashMap::new()),
            });
            let toplevels = template.eval_builtin_toplevels_uncached()?;
            let reg = template.registry.lock().unwrap();
            let bzl_cache = template.bzl_cache.lock().unwrap().clone();
            Ok(CachedBuiltins {
                toplevels,
                bzl_cache,
                rules: reg.rules.clone(),
                macros: reg.macros.clone(),
            })
        })
    }

    /// Overrides predeclared symbols with `exported_toplevels` of Bazel's builtins (`cc_common`,
    /// `java_common`, `apple_common`, ...). If the builtins fail to initialise the placeholders
    /// stay in place; the error is available from [`PackageLoader::builtins_error`].
    fn inject_builtin_toplevels(self: &Arc<Self>, ctx: &mut EvalContext, is_build: bool) {
        {
            let mut st = self.builtin_toplevels.lock().unwrap();
            match &*st {
                BuiltinToplevels::Ready(m) => {
                    Self::apply_builtin_map(ctx, m, is_build);
                    return;
                }
                BuiltinToplevels::Failed(_) | BuiltinToplevels::Loading => return,
                BuiltinToplevels::Unloaded => *st = BuiltinToplevels::Loading,
            }
        }
        let can_use_cached = self.cfg.bzl_override.is_none()
            && self
                .cfg
                .lookup
                .resolver
                .repo_root(&RepositoryName::create_unvalidated(
                    crate::builtins::BUILTINS_REPO,
                ))
                .is_some_and(|root| {
                    self.fs()
                        .stat(&root.get_relative_str("exports.bzl"), true)
                        .map(|s| s.is_file)
                        .unwrap_or(false)
                });
        let result = if can_use_cached {
            let reg_empty = {
                let reg = self.registry.lock().unwrap();
                reg.rules.is_empty() && reg.macros.is_empty()
            };
            if reg_empty {
                match Self::cached_embedded_builtins() {
                    Ok(cached) => {
                        {
                            let mut reg = self.registry.lock().unwrap();
                            if reg.rules.is_empty() && reg.macros.is_empty() {
                                reg.rules = cached.rules.clone();
                                reg.macros = cached.macros.clone();
                            }
                        }
                        {
                            let mut bc = self.bzl_cache.lock().unwrap();
                            for (k, v) in &cached.bzl_cache {
                                bc.entry(k.clone()).or_insert_with(|| v.clone());
                            }
                        }
                        Ok(cached.toplevels.clone())
                    }
                    Err(e) => Err(e.clone()),
                }
            } else {
                self.eval_builtin_toplevels_uncached()
            }
        } else {
            self.eval_builtin_toplevels_uncached()
        };
        let mut st = self.builtin_toplevels.lock().unwrap();
        match result {
            Ok(m) => {
                Self::apply_builtin_map(ctx, &m, is_build);
                *st = BuiltinToplevels::Ready(m);
            }
            Err(e) => *st = BuiltinToplevels::Failed(e),
        }
    }

    fn apply_builtin_map(ctx: &mut EvalContext, m: &BTreeMap<String, Value>, is_build: bool) {
        for (k, v) in m {
            if k == EXPORTED_RULES_KEY {
                Self::apply_exported_rules(ctx, v, is_build);
            } else if !is_build {
                // `exported_toplevels` are `.bzl` predeclareds, not BUILD globals.
                ctx.globals.insert(k.clone(), v.clone());
            }
        }
    }

    /// Adds Starlarkified native rules (`exported_rules`: `cc_toolchain_suite`, the "removed rule"
    /// failure stubs, ...) to `native` (in `.bzl`) and as predeclared symbols (in `BUILD`).
    fn apply_exported_rules(ctx: &mut EvalContext, rules: &Value, is_build: bool) {
        let Value::Dict(d) = rules else { return };
        let mut fields = if is_build {
            BTreeMap::new()
        } else {
            match ctx.globals.get("native") {
                Some(Value::Struct(s)) => s.fields.clone(),
                _ => BTreeMap::new(),
            }
        };
        for (k, v) in d.lock().unwrap().entries.iter() {
            if let Value::String(name) = k {
                // Exactly one `+`/`-` prefix is stripped; `-` means disabled by default.
                let n = if let Some(rest) = name.strip_prefix('+') {
                    rest
                } else if name.starts_with('-') {
                    continue;
                } else {
                    name.as_str()
                };
                if is_build {
                    ctx.globals.insert(n.to_string(), v.clone());
                } else {
                    fields.insert(n.to_string(), v.clone());
                }
            }
        }
        if !is_build {
            ctx.globals.insert(
                "native".to_string(),
                Value::Struct(Arc::new(StructValue::new(fields))),
            );
        }
    }

    /// Why Bazel's Starlark builtins could not be initialised, if they could not.
    pub fn builtins_error(self: &Arc<Self>) -> Option<String> {
        let mut c = EvalContext::default();
        self.inject_builtin_toplevels(&mut c, false);
        match &*self.builtin_toplevels.lock().unwrap() {
            BuiltinToplevels::Failed(e) => Some(e.clone()),
            _ => None,
        }
    }

    fn eval_bzl_file(
        self: &Arc<Self>,
        label: &Label,
        path: &PathFragment,
        chain: &[String],
    ) -> BzlResult {
        self.eval_bzl_module(label, path, chain, false)
            .map(|(m, _)| m)
    }

    fn eval_bzl_module(
        self: &Arc<Self>,
        label: &Label,
        path: &PathFragment,
        chain: &[String],
        is_prelude: bool,
    ) -> Result<(Arc<BzlModule>, BTreeMap<String, Value>), BzlError> {
        let is_builtins =
            label.package_identifier().repository().name() == crate::builtins::BUILTINS_REPO;
        let bytes = self.fs().read_file(path).map_err(|e| BzlError {
            events: Vec::new(),
            message: format!(
                "Encountered error while reading extension file '{}': {e}",
                label
                    .package_identifier()
                    .package_fragment()
                    .get_relative_str(label.name())
            ),
            observed_files: if is_builtins {
                Vec::new()
            } else {
                vec![(path.clone(), None)]
            },
        })?;
        let own_hash = zel_util::sha256::digest(&bytes);
        let source = String::from_utf8_lossy(&bytes).to_string();
        let mut ctx = EvalContext::new(
            path.parent_directory().unwrap_or_else(PathFragment::empty),
            Some(Arc::clone(&self.cfg.lookup.fs)),
        );
        ctx.semantics.experimental_sibling_repository_layout = self.cfg.sibling_repository_layout;
        ctx.thread.semantics.experimental_sibling_repository_layout =
            self.cfg.sibling_repository_layout;
        ctx.file_name = path.path_string().to_string();
        ctx.strict_names = true;
        let is_scl = !is_prelude && label.name().ends_with(".scl");
        ctx.allow_type_syntax = !is_scl && is_builtins;
        ctx.captured_prints = Some(Vec::new());
        *ctx.module_env.label_ctx.lock().unwrap() = Some((
            label.package_identifier().clone(),
            self.cfg
                .lookup
                .resolver
                .mapping(label.package_identifier().repository()),
        ));
        if is_scl {
            // `.scl` files predeclare only `visibility` and `struct` + Starlark universe.
            ctx.globals.remove("json");
            ctx.globals.remove("proto");
            ctx.globals.insert(
                "struct".to_string(),
                Value::BuiltinFunction("struct".to_string()),
            );
            ctx.globals.insert(
                "visibility".to_string(),
                Value::BuiltinFunction("host:visibility".to_string()),
            );
        } else {
            self.install_bzl_globals(&mut ctx);
            if is_builtins {
                let m = crate::builtins::builtins_module(&ctx.globals);
                ctx.globals.insert("_builtins".to_string(), m);
            } else {
                self.inject_builtin_toplevels(&mut ctx, false);
            }
        }
        let mut chain2 = chain.to_vec();
        chain2.push(label.canonical_form());
        let bzl_init = Arc::new(Mutex::new(BzlInitSlot {
            initializing: !is_prelude,
            visibility: None,
        }));
        self.install_host(
            &mut ctx,
            None,
            Some(Arc::clone(&bzl_init)),
            label.package_identifier().clone(),
        );
        let sink: Arc<Mutex<Vec<PackageEvent>>> = Arc::new(Mutex::new(Vec::new()));
        #[allow(clippy::type_complexity)]
        let child_digests: Arc<Mutex<Vec<([u8; 32], Vec<(PathFragment, [u8; 32])>)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let bzl_vis_failed = Arc::new(Mutex::new(false));
        self.install_load_hook(
            &mut ctx,
            label.package_identifier().repository().clone(),
            label.package_identifier().clone(),
            Arc::clone(&sink),
            chain2,
            Arc::clone(&child_digests),
            Some(Arc::clone(&bzl_vis_failed)),
        );
        let res = ctx.eval_file_checked(&source, None);
        bzl_init.lock().unwrap().initializing = false;
        let vis_failed = *bzl_vis_failed.lock().unwrap();
        let recorded_visibility = bzl_init
            .lock()
            .unwrap()
            .visibility
            .take()
            .unwrap_or(BzlVisibility::Public);
        let mut events: Vec<PackageEvent> = std::mem::take(&mut *sink.lock().unwrap());
        if let Some(prints) = ctx.captured_prints.take() {
            for (loc, msg) in prints {
                events.push(PackageEvent {
                    kind: EventKind::Debug,
                    location: Some(fmt_loc(&loc)),
                    message: msg,
                });
            }
        }
        let mut observed_files: Vec<(PathFragment, [u8; 32])> = Vec::new();
        if !is_builtins {
            observed_files.push((path.clone(), own_hash));
            for (_, child_obs) in child_digests.lock().unwrap().iter() {
                for (p, h) in child_obs {
                    if !observed_files.iter().any(|(ep, _)| ep == p) {
                        observed_files.push((p.clone(), *h));
                    }
                }
            }
        }
        if let Err(errs) = res {
            if vis_failed {
                return Err(BzlError {
                    events,
                    message: format!(
                        "module {} contains .bzl load visibility violations",
                        label.canonical_form()
                    ),
                    observed_files: observed_files
                        .into_iter()
                        .map(|(p, h)| (p, Some(h)))
                        .collect(),
                });
            }
            for line in errs {
                let (loc, msg) = split_location(&line);
                events.push(PackageEvent::error(loc, msg));
            }
            return Err(BzlError {
                events,
                message: format!(
                    "{} of module '{}' failed",
                    if ctx.static_failure {
                        "compilation"
                    } else {
                        "initialization"
                    },
                    label
                        .package_identifier()
                        .package_fragment()
                        .get_relative_str(label.name())
                ),
                observed_files: observed_files
                    .into_iter()
                    .map(|(p, h)| (p, Some(h)))
                    .collect(),
            });
        }
        let module_digest = {
            // `ConfiguredRuleClassProvider.BundledFileSystem`: the digest of a bundled builtins file is
            // the hash of its full virtual path, not of its contents.
            let mut fp = if is_builtins {
                let vpath = label
                    .package_identifier()
                    .package_fragment()
                    .get_relative_str(label.name());
                zel_util::sha256::digest(format!("/virtual_builtins_bzl/{vpath}").as_bytes())
                    .to_vec()
            } else {
                own_hash.to_vec()
            };
            for (d, _) in child_digests.lock().unwrap().iter() {
                fp.extend_from_slice(d);
            }
            // `BzlLoadFunction.getAndDigestPredeclaredEnvironment`: `.bzl` files loaded for BUILD
            // files also fold in the transitive digest of `@_builtins//:exports.bzl` (no autoloads).
            if !is_scl
                && label.package_identifier().repository().name() != crate::builtins::BUILTINS_REPO
            {
                if let Ok(exports_label) = Label::parse_canonical("@@_builtins//:exports.bzl") {
                    if let Ok(m) = self.load_bzl(&exports_label, &[]) {
                        fp.extend_from_slice(&m.digest);
                    }
                }
            }
            zel_util::sha256::digest(&fp)
        };
        // Export rules/macros: assign names by variable.
        {
            let mut reg = self.registry.lock().unwrap();
            for (k, v) in &ctx.globals {
                if let Value::Host(kind, id) = v {
                    let idx: usize = id.parse().unwrap_or(usize::MAX);
                    match kind.as_str() {
                        "rule" => {
                            if let Some(r) = reg.rules.get_mut(idx) {
                                if r.name.is_none() {
                                    if !crate::rule_class::is_valid_rule_class_name(k)
                                        || (r.test != k.ends_with("_test"))
                                    {
                                        return Err(BzlError {
                                            events,
                                            message: format!(
                                                "Invalid rule class name '{k}'{}",
                                                if !r.test && k.ends_with("_test") {
                                                    ", non-test rule class names must not end with '_test'"
                                                } else if r.test && !k.ends_with("_test") {
                                                    ", test rule class names must end with '_test'"
                                                } else {
                                                    ""
                                                }
                                            ),
                                            observed_files: observed_files
                                                .into_iter()
                                                .map(|(p, h)| (p, Some(h)))
                                                .collect(),
                                        });
                                    }
                                    r.name = Some(k.clone());
                                    let rc = starlark_rule_class(
                                        k,
                                        r.attrs.clone(),
                                        r.executable,
                                        r.test,
                                        r.outputs.clone(),
                                        r.build_setting,
                                    );
                                    let mut rc = rc;
                                    rc.outputs_fn = r.outputs_fn.clone();
                                    // Toolchain types / exec constraints given as strings are labels
                                    // relative to the defining .bzl (and its repository mapping).
                                    let mut kwargs = r.kwargs.clone();
                                    {
                                        let mapping = self
                                            .cfg
                                            .lookup
                                            .resolver
                                            .mapping(label.package_identifier().repository());
                                        let cctx = crate::attr::ConvertCtx {
                                            pkg: label.package_identifier(),
                                            mapping: &mapping,
                                            rule_class: "rule",
                                        };
                                        let conv = |v: &Value| -> Value {
                                            match v {
                                                Value::String(s) => {
                                                    crate::attr::convert_label(s, &cctx)
                                                        .map(Value::Label)
                                                        .unwrap_or_else(|_| v.clone())
                                                }
                                                Value::Struct(st)
                                                    if st
                                                        .fields
                                                        .contains_key("_toolchain_type") =>
                                                {
                                                    let mut f = st.fields.clone();
                                                    if let Some(Value::String(n)) =
                                                        f.get("name").cloned()
                                                    {
                                                        if let Ok(l) =
                                                            crate::attr::convert_label(&n, &cctx)
                                                        {
                                                            f.insert(
                                                                "name".into(),
                                                                Value::Label(l),
                                                            );
                                                        }
                                                    }
                                                    Value::Struct(Arc::new(
                                                        zel_starlark::StructValue::new(f),
                                                    ))
                                                }
                                                o => o.clone(),
                                            }
                                        };
                                        for key in ["toolchains", "exec_compatible_with"] {
                                            if let Some(v) = kwargs.get(key).cloned() {
                                                if let Ok(items) =
                                                    zel_starlark::eval::builtins::value_to_sequence(
                                                        &v,
                                                    )
                                                {
                                                    let out: Vec<Value> =
                                                        items.iter().map(&conv).collect();
                                                    kwargs.insert(
                                                        key.to_string(),
                                                        Value::List(Arc::new(Mutex::new(
                                                            zel_starlark::StarlarkList::new(out),
                                                        ))),
                                                    );
                                                }
                                            }
                                        }
                                        if let Some(Value::Dict(d)) =
                                            kwargs.get("exec_groups").cloned()
                                        {
                                            let mut next_eg = zel_starlark::StarlarkDict::new();
                                            for (gk, gv) in d.lock().unwrap().items() {
                                                let next_gv = if let Value::Struct(gs) = gv {
                                                    let mut gf = gs.fields.clone();
                                                    for key in
                                                        ["toolchains", "exec_compatible_with"]
                                                    {
                                                        if let Some(v) = gf.get(key).cloned() {
                                                            if let Ok(items) = zel_starlark::eval::builtins::value_to_sequence(&v) {
                                                                let out: Vec<Value> = items.iter().map(&conv).collect();
                                                                gf.insert(
                                                                    key.to_string(),
                                                                    Value::List(Arc::new(Mutex::new(zel_starlark::StarlarkList::new(out)))),
                                                                );
                                                            }
                                                        }
                                                    }
                                                    Value::Struct(Arc::new(
                                                        zel_starlark::StructValue::new(gf),
                                                    ))
                                                } else {
                                                    gv
                                                };
                                                let _ = next_eg.insert(gk, next_gv);
                                            }
                                            kwargs.insert(
                                                "exec_groups".to_string(),
                                                Value::Dict(Arc::new(Mutex::new(next_eg))),
                                            );
                                        }
                                    }
                                    rc.starlark =
                                        Some(Arc::new(crate::rule_class::StarlarkRuleSpec {
                                            implementation: r.implementation.clone(),
                                            kwargs,
                                            defined_in: label.canonical_form(),
                                            definition_digest: {
                                                use std::fmt::Write as _;
                                                let mut hex =
                                                    String::with_capacity(module_digest.len() * 2);
                                                for b in &module_digest {
                                                    let _ = write!(hex, "{b:02x}");
                                                }
                                                hex
                                            },
                                            definition_stack: r.definition_stack.clone(),
                                            initializers: r.initializers.clone(),
                                            build_setting_flag: r.build_setting_flag,
                                        }));
                                    r.class = Some(Arc::new(rc));
                                }
                            }
                        }
                        "macro" => {
                            if let Some(m) = reg.macros.get_mut(idx) {
                                if m.name.is_none() {
                                    m.name = Some(k.clone());
                                }
                            }
                        }
                        "provider" => {
                            crate::provider::export_provider(idx, k, &label.canonical_form());
                        }
                        "aspect" => {
                            crate::aspect::export_aspect(idx, k, &label.canonical_form());
                        }
                        _ => {}
                    }
                }
            }
        }
        let mut exports = BTreeMap::new();
        for (k, v) in &ctx.globals {
            if !k.starts_with('_') {
                exports.insert(k.clone(), v.clone());
            }
        }
        Ok((
            Arc::new(BzlModule {
                exports,
                visibility: recorded_visibility,
                digest: module_digest,
                observed_files,
            }),
            ctx.globals,
        ))
    }
}

/// Names of functions available (as globals in BUILD files, and under `native.`).
const BUILD_FUNCTIONS: &[&str] = &[
    "glob",
    "subpackages",
    "package",
    "exports_files",
    "package_group",
    "licenses",
    "existing_rule",
    "existing_rules",
    "package_name",
    "repository_name",
    "repo_name",
    "package_relative_label",
    "environment_group",
    "module_name",
    "module_version",
    "package_default_visibility",
];

const ATTR_FUNCTIONS: &[&str] = &[
    "int",
    "string",
    "bool",
    "string_list",
    "string_dict",
    "string_list_dict",
    "label",
    "label_list",
    "label_keyed_string_dict",
    "string_keyed_label_dict",
    "int_list",
    "output",
    "output_list",
];

fn split_location(line: &str) -> (Option<String>, String) {
    // `path:line:col: message` where path may itself contain colons only on Windows; split on the
    // third colon from a `path:digits:digits: ` prefix.
    let bytes = line.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b':' && bytes[i + 1].is_ascii_digit() {
            // parse digits ':' digits ':' ' '
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b':' {
                let mut k = j + 1;
                let start2 = k;
                while k < bytes.len() && bytes[k].is_ascii_digit() {
                    k += 1;
                }
                if k > start2 && k + 1 < bytes.len() && bytes[k] == b':' && bytes[k + 1] == b' ' {
                    return (Some(line[..k].to_string()), line[k + 2..].to_string());
                }
            }
        }
        i += 1;
    }
    (None, line.to_string())
}

fn parse_load_label(
    raw: &str,
    owner_pkg: &PackageIdentifier,
    mapping: &RepositoryMapping,
) -> Result<Label, String> {
    if raw == "@" {
        // Bazel parses a bare `@` as an empty repository with an empty target name.
        return Err("invalid target name '': empty target name".to_string());
    }
    if raw.starts_with("//") || raw.starts_with('@') {
        Label::parse_with_repo_mapping(raw, mapping).map_err(|e| e.to_string())?;
        // Relative to the owner's repository for `//pkg:file`.
        if raw.starts_with("//") {
            let parsed = Label::parse_with_repo_mapping(raw, mapping).map_err(|e| e.to_string())?;
            let id = PackageIdentifier::create(
                owner_pkg.repository().clone(),
                parsed.package_identifier().package_fragment().clone(),
            )
            .map_err(|e| e.to_string())?;
            return Label::create(id, parsed.name()).map_err(|e| e.to_string());
        }
        return Label::parse_with_repo_mapping(raw, mapping).map_err(|e| e.to_string());
    }
    if let Some(name) = raw.strip_prefix(':') {
        return Label::create(owner_pkg.clone(), name).map_err(|e| e.to_string());
    }
    // `load("foo.bzl", ...)`: a label relative to the loading file's package (accepted by Bazel,
    // e.g. protobuf's BUILD files).
    if !raw.is_empty() && !raw.contains(':') {
        return Label::create(owner_pkg.clone(), raw).map_err(|e| e.to_string());
    }
    Err(format!(
        "in load statement: '{raw}' is not a valid label; it must start with '//', '@' or ':'"
    ))
}

/// Parses `REPO.bazel` for `ignore_directories()`.
pub fn parse_repo_file(fs: &dyn FileSystem, repo_root: &PathFragment) -> Option<RepoFile> {
    eval_repo_file_checked(
        fs,
        repo_root,
        &RepositoryName::main(),
        &RepositoryMapping::always_fallback(),
    )
    .ok()
    .flatten()
    .map(|(file, _)| file)
}

fn eval_repo_file_checked(
    fs: &dyn FileSystem,
    repo_root: &PathFragment,
    repo: &RepositoryName,
    repo_mapping: &RepositoryMapping,
) -> Result<Option<(RepoFile, PackageArgs)>, String> {
    let path = repo_root.get_relative_str("REPO.bazel");
    let bytes = match fs.read_file(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("error reading REPO.bazel file at {path}: {e}")),
    };
    let src = String::from_utf8_lossy(&bytes).to_string();
    #[derive(Default, Clone)]
    struct RepoEvalState {
        file: RepoFile,
        args: PackageArgs,
        repo_called: bool,
        ignore_called: bool,
    }
    let out = Arc::new(Mutex::new(RepoEvalState::default()));
    let mut ctx = EvalContext::new(repo_root.clone(), None);
    ctx.file_name = path.path_string().to_string();
    ctx.strict_names = true;
    // `REPO.bazel` contains only `repo` and `ignore_directories` + the Starlark universe (no `json`/`proto`).
    ctx.globals.remove("json");
    ctx.globals.remove("proto");
    ctx.globals
        .insert("repo".into(), Value::BuiltinFunction("host:repo".into()));
    ctx.globals.insert(
        "ignore_directories".into(),
        Value::BuiltinFunction("host:ignore_directories".into()),
    );
    let o2 = Arc::clone(&out);
    let root_pkg = PackageIdentifier::create(repo.clone(), PathFragment::empty())
        .map_err(|e| e.to_string())?;
    let mapping = repo_mapping.clone();
    ctx.host = Some(Arc::new(move |_ctx, name, pos, kw| match name {
        "ignore_directories" => {
            let mut g = o2.lock().unwrap();
            if g.ignore_called {
                return Err(StarlarkError(
                    "'ignored_directories()' can only be called once".into(),
                ));
            }
            check_kwargs("ignore_directories", &kw, &["dirs"]).map_err(StarlarkError)?;
            if pos.len() > 1 {
                return Err(StarlarkError(format!(
                    "ignore_directories() accepts no more than 1 positional argument but got {}",
                    pos.len()
                )));
            }
            if !pos.is_empty() && kw.contains_key("dirs") {
                return Err(StarlarkError(
                    "ignore_directories() got multiple values for argument 'dirs'".into(),
                ));
            }
            let dirs = pos.first().or_else(|| kw.get("dirs")).ok_or_else(|| {
                StarlarkError(
                    "ignore_directories() missing 1 required positional argument: dirs".into(),
                )
            })?;
            let items = zel_starlark::eval::builtins::value_to_sequence(dirs).map_err(|_| {
                StarlarkError(format!(
                    "in call to ignore_directories(), parameter 'dirs' got value of type '{}', want 'sequence'",
                    dirs.type_name()
                ))
            })?;
            let mut list = Vec::with_capacity(items.len());
            for (idx, item) in items.into_iter().enumerate() {
                match item {
                    Value::String(s) => list.push(s),
                    other => {
                        return Err(StarlarkError(format!(
                            "at index {idx} of dirs, got element of type {}, want string",
                            other.type_name()
                        )))
                    }
                }
            }
            g.file.ignored_directories = list;
            g.ignore_called = true;
            Ok(Value::None)
        }
        "repo" => {
            let mut g = o2.lock().unwrap();
            if g.repo_called {
                return Err(StarlarkError(
                    "'repo' can only be called once in the REPO.bazel file".into(),
                ));
            }
            if g.ignore_called {
                return Err(StarlarkError(
                    "if repo() is called, it must be called before any other functions".into(),
                ));
            }
            if !pos.is_empty() {
                return Err(StarlarkError(
                    "repo() accepts no positional arguments".into(),
                ));
            }
            if kw.is_empty() {
                return Err(StarlarkError(
                    "at least one argument must be given to the 'repo' function".into(),
                ));
            }
            let cctx = ConvertCtx {
                pkg: &root_pkg,
                mapping: &mapping,
                rule_class: "repo",
            };
            let mut parsed = PackageArgs::default();
            apply_package_args(&mut parsed, &kw, &cctx).map_err(StarlarkError)?;
            g.args.merge_from(&parsed);
            g.repo_called = true;
            Ok(Value::None)
        }
        other => Err(StarlarkError(format!(
            "unknown REPO.bazel function {other}"
        ))),
    }));
    if let Err(errs) = ctx.eval_file_checked(&src, Some(("REPO.bazel files", false))) {
        return Err(errs.join("\n"));
    }
    let g = out.lock().unwrap().clone();
    Ok(Some((g.file, g.args)))
}

fn apply_package_args(
    args: &mut PackageArgs,
    kw: &BTreeMap<String, Value>,
    cctx: &ConvertCtx<'_>,
) -> Result<(), String> {
    let label_list = |name: &str, v: &Value| -> Result<Vec<Label>, String> {
        let a = Attribute::new(name, AttrType::LabelList);
        match convert_value(v, &a, cctx)? {
            AttrValue::LabelList(l) => {
                let mut seen: BTreeSet<Label> = BTreeSet::new();
                let mut dupes: Vec<Label> = Vec::new();
                for x in &l {
                    if !seen.insert(x.clone()) && !dupes.contains(x) {
                        dupes.push(x.clone());
                    }
                }
                if !dupes.is_empty() {
                    dupes.sort_by(java_label_cmp);
                    return Err(format!(
                        "duplicate label(s) in {name}: {}",
                        dupes
                            .into_iter()
                            .map(|d| d.canonical_form())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                Ok(l)
            }
            _ => Err(format!("attribute '{name}' cannot be configurable")),
        }
    };
    for (k, v) in kw {
        match k.as_str() {
            "default_visibility" => {
                let a = Attribute::new(k, AttrType::LabelList);
                let labels = match convert_plain(v, a.ty, k, cctx)? {
                    AttrValue::LabelList(l) => l,
                    _ => unreachable!(),
                };
                let simplified = crate::pkg_spec::validate_and_simplify_visibility(&labels)?;
                args.default_visibility = Some(simplified);
            }
            "default_testonly" => match convert_plain(v, AttrType::Bool, k, cctx)? {
                AttrValue::Bool(b) => args.default_testonly = Some(b),
                _ => unreachable!(),
            },
            "default_deprecation" => match convert_plain(v, AttrType::String, k, cctx)? {
                AttrValue::Str(s) => args.default_deprecation = Some(s),
                _ => unreachable!(),
            },
            "features" => match convert_plain(v, AttrType::StringList, k, cctx)? {
                AttrValue::StrList(l) => {
                    args.features = PackageArgs::merge_feature_lists(&args.features, &l);
                }
                _ => unreachable!(),
            },
            "licenses" => match convert_plain(v, AttrType::StringList, k, cctx)? {
                AttrValue::StrList(l) => {
                    args.licenses = parse_license_strings(&l, cctx)?;
                    args.licenses_set = true;
                }
                _ => unreachable!(),
            },
            "default_compatible_with" => {
                args.default_compatible_with = label_list(k, v)?;
                args.default_compatible_with_set = true;
            }
            "default_restricted_to" => {
                args.default_restricted_to = label_list(k, v)?;
                args.default_restricted_to_set = true;
            }
            "default_applicable_licenses" | "default_package_metadata" => {
                if args.default_package_metadata_set {
                    return Err("Can not set both default_package_metadata and default_applicable_licenses. Move all declarations to default_package_metadata.".to_string());
                }
                args.default_package_metadata = label_list(k, v)?;
                args.default_package_metadata_set = true;
            }
            "default_hdrs_check" => match convert_plain(v, AttrType::String, k, cctx)? {
                AttrValue::Str(s) => args.default_hdrs_check = Some(s),
                _ => unreachable!(),
            },
            "transitive_visibility" => {
                if matches!(v, Value::None) {
                    return Err(format!(
                        "expected value of type 'string' for attribute '{k}' in '{}' rule, but got None (NoneType)",
                        cctx.rule_class
                    ));
                }
                match convert_plain(v, AttrType::Label, k, cctx)? {
                    AttrValue::Label(l) => args.transitive_visibility = l,
                    _ => unreachable!(),
                }
            }
            other => return Err(format!("unexpected keyword argument: {other}")),
        }
    }
    Ok(())
}

include!("loader_host.rs");
