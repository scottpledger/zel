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
    convert_label, convert_plain, convert_value, infer_attribute, AttrType, AttrValue, Attribute,
    ConvertCtx, SelectPart,
};
use crate::glob::{self, GlobContext, GlobOp};
use crate::lookup::{
    EventKind, PackageEvent, PackageLookup, PackageLookupConfig, RepoFile, RepoResolver,
};
use crate::pkg_spec::{parse_visibility, PackageSpec};
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
    /// Whether to apply `//tools/build_rules:prelude_bazel` (main repo only).
    pub use_prelude: bool,
    /// Handler for host calls that belong to an embedding runtime (Bzlmod repository rules and
    /// `repository_ctx`/`module_ctx` methods called from helper `.bzl` files evaluated here).
    /// Consulted first for names accepted by [`is_external_host_call`].
    pub external_host: Option<zel_starlark::HostFn>,
    /// Lets an embedding runtime evaluate selected `.bzl` files itself (files defining repository
    /// rules / module extensions): returns `None` to let the loader evaluate the file.
    pub bzl_override: Option<BzlOverride>,
}

/// See [`LoaderConfig::bzl_override`].
pub type BzlOverride = Arc<dyn Fn(&Label) -> Option<Result<BTreeMap<String, Value>, String>> + Send + Sync>;

/// Whether a host call name (`call:repository_rule`, `method:module_ctx.file`, ...) is routed to
/// [`LoaderConfig::external_host`].
pub fn is_external_host_call(name: &str) -> bool {
    name.starts_with("method:")
        || name.starts_with("attr:")
        || name.starts_with("native_fn:")
        || matches!(
            name,
            "call:repository_rule" | "call:native_repo_rule" | "call:module_extension" | "repository_rule"
                | "module_extension" | "tag_class" | "extension_metadata"
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
            use_prelude: true,
            external_host: None,
            bzl_override: None,
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

/// An evaluated `.bzl` module.
struct BzlModule {
    exports: BTreeMap<String, Value>,
    /// `BzlLoadValue.getTransitiveDigest()` stand-in: SHA-256 over the file's digest and the
    /// transitive digests of its direct loads (the real one also covers the `@_builtins` digest of
    /// the Bazel binary, which has no counterpart here).
    digest: [u8; 32],
}

/// Failure to load a `.bzl` file.
#[derive(Clone)]
struct BzlError {
    events: Vec<PackageEvent>,
    message: String,
}

type BzlResult = Result<Arc<BzlModule>, BzlError>;

struct RuleDef {
    name: Option<String>,
    attrs: Vec<Attribute>,
    executable: bool,
    test: bool,
    outputs: Vec<(String, String)>,
    build_setting: bool,
    class: Option<Arc<RuleClass>>,
    implementation: Value,
    kwargs: BTreeMap<String, Value>,
    definition_stack: Vec<(String, String)>,
}

struct MacroDef {
    name: Option<String>,
    attrs: Vec<Attribute>,
    implementation: Value,
    /// Package of the `.bzl` file that defined the macro.
    def_pkg: PackageIdentifier,
    /// `file:line:col` of the statement that defined the macro (`macro(...)` call).
    def_location: String,
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
    /// Location (`path:line:col`) at which each target was declared.
    target_locations: BTreeMap<String, String>,
    /// Name → location for symbolic macro instances.
    macro_names: BTreeMap<String, String>,
    output_prefixes: BTreeMap<String, String>,
    args: PackageArgs,
    package_called: bool,
    any_target_declared: bool,
    exported_files: BTreeMap<String, Vec<Label>>,
    group_includes: BTreeMap<String, Vec<Label>>,
    environment_groups: BTreeMap<String, (Vec<Label>, Vec<Label>)>,
    events: Vec<PackageEvent>,
    contains_errors: bool,
    macro_queue: std::collections::VecDeque<MacroCall>,
    /// Stack of `(macro instance name, defining package)` for the macro being expanded.
    macro_stack: Vec<(String, PackageIdentifier)>,
    /// Parallel to `macro_stack`: (BUILD/caller call site, macro symbol name, macro definition location).
    macro_frames: Vec<(String, String, String)>,
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
    "DefaultInfo", "OutputGroupInfo", "RunEnvironmentInfo", "InstrumentedFilesInfo", "CcInfo",
    "JavaInfo", "PyInfo", "ProtoInfo", "DebugPackageInfo", "ToolchainInfo", "TemplateVariableInfo",
    "FilesToRunProvider", "analysis_test_transition", "cc_common", "java_common", "proto_common",
    "platform_common", "config_common", "apple_common", "testing", "coverage_common", "config",
    "transition", "exec_group", "subrule", "aspect", "module_extension", "repository_rule",
    "tag_class", "visibility", "Actions", "native_common", "AnalysisTestResultInfo",
    "AnalysisFailureInfo", "configuration_field", "PackageSpecificationInfo",
];

fn provider(name: &str) -> Value {
    Value::Provider { name: name.to_string() }
}

fn mk_struct(fields: Vec<(&str, Value)>) -> Value {
    Value::Struct(Arc::new(StructValue::new(
        fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
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
                *v = mk_struct(vec![("apple_toolchain", host("apple_common.apple_toolchain"))]);
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
            ("exec", host("config.exec")),
            ("target", host("config.target")),
            ("none", host("config.none")),
        ]),
    );
    ctx.globals.insert("aspect".into(), host("aspect"));
    ctx.globals.insert("subrule".into(), host("subrule"));
    ctx.globals.insert("analysis_test_transition".into(), host("analysis_test_transition"));
    ctx.globals.insert("configuration_field".into(), host("configuration_field"));
    ctx.globals.insert(
        "coverage_common".into(),
        mk_struct(vec![("instrumented_files_info", host("coverage_common.instrumented_files_info"))]),
    );
    ctx.globals.insert("transition".into(), host("transition"));
    ctx.globals.insert("exec_group".into(), host("exec_group"));
}

fn mk_list(v: Vec<Value>) -> Value {
    Value::List(Arc::new(Mutex::new(StarlarkList::new(v))))
}

fn mk_dict(items: Vec<(Value, Value)>) -> Value {
    Value::Dict(Arc::new(Mutex::new(
        StarlarkDict::from_entries(items).unwrap_or_default(),
    )))
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

fn check_kwargs(
    func: &str,
    kw: &BTreeMap<String, Value>,
    allowed: &[&str],
) -> Result<(), String> {
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
            format!("file '{}' does not contain a symbol '{name}'", label.canonical_form())
        })
    }

    /// Loads the `.bzl` file `label` and returns all its public globals (what `load()` sees).
    /// Errors carry the rendered events of the failing module followed by Bazel's message.
    pub fn load_bzl_exports(self: &Arc<Self>, label: &Label) -> Result<BTreeMap<String, Value>, String> {
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
    pub fn load_package(self: &Arc<Self>, id: &PackageIdentifier) -> Result<Package, PackageLoadError> {
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
                        message: format!("error loading package '{}': {msg}", id.package_fragment()),
                        events: pkg.events,
                    }),
                }
            }
            other => {
                let pkg_display = if id.repository().is_main() {
                    id.package_fragment().to_string()
                } else if matches!(other, crate::lookup::PackageLookup::RepoNotFound(_)) {
                    format!("@@[unknown repo '{}' requested from @@]//{}", id.repository().name(), id.package_fragment())
                } else {
                    // The repository exists: its canonical name (`@@bazel_tools//platforms`).
                    format!("{}//{}", id.repository().canonical_form(), id.package_fragment())
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
            target_locations: BTreeMap::new(),
            macro_names: BTreeMap::new(),
            output_prefixes: BTreeMap::new(),
            args: PackageArgs::default(),
            package_called: false,
            any_target_declared: false,
            exported_files: BTreeMap::new(),
            group_includes: BTreeMap::new(),
            environment_groups: BTreeMap::new(),
            events: Vec::new(),
            contains_errors: false,
            macro_queue: Default::default(),
            macro_stack: Vec::new(),
            macro_frames: Vec::new(),
            macro_violations: BTreeMap::new(),
            loads: Vec::new(),
        };
        // repo() defaults from REPO.bazel apply to every package of the repository.
        if let Some(repo_args) = parse_repo_file_args(self.fs(), &repo_root) {
            st.args = repo_args;
        }
        let state = Arc::new(Mutex::new(st));

        let mut ctx = EvalContext::new(pkg_dir.clone(), Some(Arc::clone(&self.cfg.lookup.fs)));
        ctx.file_name = build_file.path_string().to_string();
        ctx.strict_names = true;
        ctx.captured_prints = Some(Vec::new());
        self.install_build_globals(&mut ctx);
        self.inject_builtin_toplevels(&mut ctx);
        self.install_host(&mut ctx, Some(Arc::clone(&state)), id.clone());
        let sink: Arc<Mutex<Vec<PackageEvent>>> = Arc::new(Mutex::new(Vec::new()));
        self.install_load_hook(&mut ctx, id.repository().clone(), id.clone(), Arc::clone(&sink), Vec::new(), Arc::new(Mutex::new(Vec::new())));
        self.install_preload_hook(&mut ctx, id.repository().clone(), id.clone(), Arc::clone(&sink));

        // Prelude (main repository only).
        let mut prelude_errors: Vec<PackageEvent> = Vec::new();
        if self.cfg.use_prelude && id.repository().is_main() {
            let prelude = repo_root.get_relative_str("tools/build_rules/prelude_bazel");
            if self.fs().stat(&prelude, true).map(|s| s.is_file).unwrap_or(false) {
                match self.load_prelude(&prelude) {
                    Ok(exports) => {
                        for (k, v) in exports {
                            if !k.starts_with('_') && !ctx.globals.contains_key(&k) {
                                ctx.globals.insert(k, v);
                            }
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
                    let loc = if msg.starts_with("Traceback (most recent call last):") { None } else { loc };
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
        if run_macros {
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
            if ok {
                self.finish_package(&state);
            }
        }

        let mut s = state.lock().unwrap();
        let mut targets = std::mem::take(&mut s.targets);
        // exports_files() input files.
        let default_vis = s
            .args
            .default_visibility
            .as_ref()
            .map(|v| v.iter().map(|l| l.canonical_form()).collect())
            .unwrap_or_default();
        let bf_name = build_file.base_name();
        if !targets.contains_key(bf_name) {
            if let Ok(label) = Label::create(id.clone(), bf_name) {
                targets.insert(bf_name.to_string(), Target::InputFile { label });
            }
        }
        let package = Package {
            id: id.clone(),
            build_file: build_file.clone(),
            targets,
            loads: std::mem::take(&mut s.loads).into_iter().chain(ctx.loads.clone()).collect(),
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

    /// Predeclared symbols of `BUILD` files: native rules, `glob`, `package`, ...
    fn install_build_globals(&self, ctx: &mut EvalContext) {
        for name in self.rule_classes.keys() {
            ctx.globals.insert(
                name.clone(),
                Value::BuiltinFunction(format!("host:rule.{name}")),
            );
        }
        for f in BUILD_FUNCTIONS {
            ctx.globals
                .insert((*f).to_string(), Value::BuiltinFunction(format!("host:{f}")));
        }
        self.install_common_globals(ctx);
    }

    /// Symbols shared by `BUILD` and `.bzl` files.
    fn install_common_globals(&self, ctx: &mut EvalContext) {
        for n in ["select", "depset", "struct", "Label", "provider", "rule_placeholder"] {
            ctx.globals
                .entry(n.to_string())
                .or_insert_with(|| Value::BuiltinFunction(n.to_string()));
        }
        let mut native = BTreeMap::new();
        for name in self.rule_classes.keys() {
            native.insert(name.clone(), Value::BuiltinFunction(format!("host:rule.{name}")));
        }
        for f in BUILD_FUNCTIONS {
            native.insert((*f).to_string(), Value::BuiltinFunction(format!("host:{f}")));
        }
        ctx.globals
            .insert("native".to_string(), Value::Struct(Arc::new(StructValue::new(native))));
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
            attr.insert((*t).to_string(), Value::BuiltinFunction(format!("host:attr.{t}")));
        }
        ctx.globals
            .insert("attr".to_string(), Value::Struct(Arc::new(StructValue::new(attr))));
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
            Value::BuiltinFunction("host:proto_common_do_not_use.incompatible_enable_proto_toolchain_resolution".into()),
        );
        ctx.globals
            .insert("proto_common_do_not_use".to_string(), Value::Struct(Arc::new(StructValue::new(pcdnu))));
        ctx.globals
            .entry("exec_transition".to_string())
            .or_insert_with(|| Value::BuiltinFunction("host:exec_transition".to_string()));
        // `visibility([...])` in .bzl files restricts who may load the file; load visibility is not
        // enforced yet, the call itself is valid (and returns None).
        ctx.globals
            .insert("visibility".to_string(), Value::BuiltinFunction("host:visibility".to_string()));
        // Native `cc_common`: rules_cc's Starlark is layered on `cc_common.internal_DO_NOT_USE()`.
        let mut cc_common = BTreeMap::new();
        cc_common.insert(
            "internal_DO_NOT_USE".to_string(),
            Value::BuiltinFunction("host:cc_common.internal_DO_NOT_USE".to_string()),
        );
        ctx.globals
            .insert("cc_common".to_string(), Value::Struct(Arc::new(StructValue::new(cc_common))));
        // `rule`, `aspect`, etc. are handled by name for bzlmod; ours override via host.
        ctx.globals.remove("rule_placeholder");
    }

    fn install_host(
        self: &Arc<Self>,
        ctx: &mut EvalContext,
        state: Option<Arc<Mutex<PackageState>>>,
        owner: PackageIdentifier,
    ) {
        let this = Arc::clone(self);
        ctx.host = Some(Arc::new(move |ctx, name, pos, kw| {
            this.dispatch_host(ctx, state.as_ref(), &owner, name, pos, kw)
        }));
    }

    fn install_load_hook(
        self: &Arc<Self>,
        ctx: &mut EvalContext,
        repo: RepositoryName,
        owner_pkg: PackageIdentifier,
        sink: Arc<Mutex<Vec<PackageEvent>>>,
        chain: Vec<String>,
        child_digests: Arc<Mutex<Vec<[u8; 32]>>>,
    ) {
        let this = Arc::clone(self);
        ctx.load_hook = Some(Arc::new(move |module_label: &str| {
            let mapping = this.cfg.lookup.resolver.mapping(&repo);
            let label = parse_load_label(module_label, &owner_pkg, &mapping)?;
            match this.load_bzl(&label, &chain) {
                Ok(m) => {
                    child_digests.lock().unwrap().push(m.digest);
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
                        Err("The label must reference a file with extension \".bzl\" or \".scl\"".to_string())
                    }
                });
                match parsed {
                    Ok(l) => labels.push(l),
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
            for label in labels {
                if let Err(e) = this.load_bzl(&label, &[]) {
                    sink.lock().unwrap().extend(e.events.clone());
                    return Err(zel_starlark::PreloadFailure { events: Vec::new(), fatal: e.message });
                }
                canonical.push(label.canonical_form());
            }
            Ok(canonical)
        }));
    }

    // ------------------------------------------------------------------ .bzl loading

    fn load_prelude(self: &Arc<Self>, path: &PathFragment) -> Result<BTreeMap<String, Value>, BzlError> {
        let label = Label::parse_canonical("//tools/build_rules:prelude_bazel").unwrap();
        self.eval_bzl_file(&label, path, &[]).map(|m| m.exports.clone())
    }

    fn load_bzl(self: &Arc<Self>, label: &Label, chain: &[String]) -> BzlResult {
        let key = label.canonical_form();
        if chain.contains(&key) {
            let mut cyc = chain.to_vec();
            cyc.push(key.clone());
            return Err(BzlError {
                events: Vec::new(),
                message: format!(
                    "Starlark load cycle detected: {}",
                    cyc.join(" -> ")
                ),
            });
        }
        if let Some(r) = self.bzl_cache.lock().unwrap().get(&key) {
            return r.clone();
        }
        let result = self.load_bzl_uncached(label, chain);
        self.bzl_cache.lock().unwrap().insert(key, result.clone());
        result
    }

    fn load_bzl_uncached(self: &Arc<Self>, label: &Label, chain: &[String]) -> BzlResult {
        if let Some(h) = &self.cfg.bzl_override {
            if let Some(r) = h(label) {
                return r
                    .map(|exports| Arc::new(BzlModule { exports, digest: [0u8; 32] }))
                    .map_err(|message| BzlError { events: Vec::new(), message });
            }
        }
        let pkg = label.package_identifier();
        let fail = |message: String| BzlError {
            events: Vec::new(),
            message,
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

    /// Overrides predeclared symbols with `exported_toplevels` of Bazel's builtins (`cc_common`,
    /// `java_common`, `apple_common`, ...). If the builtins fail to initialise the placeholders
    /// stay in place; the error is available from [`PackageLoader::builtins_error`].
    fn inject_builtin_toplevels(self: &Arc<Self>, ctx: &mut EvalContext) {
        {
            let mut st = self.builtin_toplevels.lock().unwrap();
            match &*st {
                BuiltinToplevels::Ready(m) => {
                    Self::apply_builtin_map(ctx, m);
                    return;
                }
                BuiltinToplevels::Failed(_) | BuiltinToplevels::Loading => return,
                BuiltinToplevels::Unloaded => *st = BuiltinToplevels::Loading,
            }
        }
        let result = (|| -> Result<BTreeMap<String, Value>, String> {
            let label = Label::parse_canonical("@@_builtins//:exports.bzl").map_err(|e| e.to_string())?;
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
                    fields.insert("CcToolchainInfo".to_string(), Value::Provider { name: "CcToolchainInfo".into() });
                    out.insert("cc_common".to_string(), Value::Struct(Arc::new(StructValue::new(fields))));
                }
            }
            // `exported_rules` become members of `native` (and BUILD-file symbols).
            if let Some(r @ Value::Dict(_)) = exports.get("exported_rules") {
                out.insert(EXPORTED_RULES_KEY.to_string(), r.clone());
            }
            Ok(out)
        })();
        let mut st = self.builtin_toplevels.lock().unwrap();
        match result {
            Ok(m) => {
                Self::apply_builtin_map(ctx, &m);
                *st = BuiltinToplevels::Ready(m);
            }
            Err(e) => *st = BuiltinToplevels::Failed(e),
        }
    }

    fn apply_builtin_map(ctx: &mut EvalContext, m: &BTreeMap<String, Value>) {
        for (k, v) in m {
            if k == EXPORTED_RULES_KEY {
                Self::apply_exported_rules(ctx, v);
            } else {
                ctx.globals.insert(k.clone(), v.clone());
            }
        }
    }

    /// Adds Starlarkified native rules (`exported_rules`: `cc_toolchain_suite`, the "removed rule"
    /// failure stubs, ...) to `native` and, for BUILD files, as predeclared symbols.
    fn apply_exported_rules(ctx: &mut EvalContext, rules: &Value) {
        let Value::Dict(d) = rules else { return };
        let mut fields = match ctx.globals.get("native") {
            Some(Value::Struct(s)) => s.fields.clone(),
            _ => BTreeMap::new(),
        };
        for (k, v) in d.lock().unwrap().entries.iter() {
            if let Value::String(name) = k {
                // Rule names may carry a `+`/`-` prefix (default-on / default-off); strip it.
                let n = name.trim_start_matches(['+', '-']).to_string();
                fields.insert(n, v.clone());
            }
        }
        ctx.globals.insert("native".to_string(), Value::Struct(Arc::new(StructValue::new(fields))));
    }

    /// Why Bazel's Starlark builtins could not be initialised, if they could not.
    pub fn builtins_error(self: &Arc<Self>) -> Option<String> {
        let mut c = EvalContext::default();
        self.inject_builtin_toplevels(&mut c);
        match &*self.builtin_toplevels.lock().unwrap() {
            BuiltinToplevels::Failed(e) => Some(e.clone()),
            _ => None,
        }
    }

    fn eval_bzl_file(self: &Arc<Self>, label: &Label, path: &PathFragment, chain: &[String]) -> BzlResult {
        let bytes = self.fs().read_file(path).map_err(|e| BzlError {
            events: Vec::new(),
            message: format!(
                "Encountered error while reading extension file '{}': {e}",
                label.package_identifier().package_fragment().get_relative_str(label.name())
            ),
        })?;
        let source = String::from_utf8_lossy(&bytes).to_string();
        let mut ctx = EvalContext::new(
            path.parent_directory().unwrap_or_else(PathFragment::empty),
            Some(Arc::clone(&self.cfg.lookup.fs)),
        );
        ctx.file_name = path.path_string().to_string();
        ctx.strict_names = true;
        ctx.allow_type_syntax = label.package_identifier().repository().name() == crate::builtins::BUILTINS_REPO;
        ctx.captured_prints = Some(Vec::new());
        *ctx.module_env.label_ctx.lock().unwrap() = Some((
            label.package_identifier().clone(),
            self.cfg.lookup.resolver.mapping(label.package_identifier().repository()),
        ));
        self.install_bzl_globals(&mut ctx);
        if label.package_identifier().repository().name() == crate::builtins::BUILTINS_REPO {
            let m = crate::builtins::builtins_module(&ctx.globals);
            ctx.globals.insert("_builtins".to_string(), m);
        } else {
            self.inject_builtin_toplevels(&mut ctx);
        }
        let mut chain2 = chain.to_vec();
        chain2.push(label.canonical_form());
        self.install_host(&mut ctx, None, label.package_identifier().clone());
        let sink: Arc<Mutex<Vec<PackageEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let child_digests: Arc<Mutex<Vec<[u8; 32]>>> = Arc::new(Mutex::new(Vec::new()));
        self.install_load_hook(
            &mut ctx,
            label.package_identifier().repository().clone(),
            label.package_identifier().clone(),
            Arc::clone(&sink),
            chain2,
            Arc::clone(&child_digests),
        );
        let res = ctx.eval_file_checked(&source, None);
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
        if let Err(errs) = res {
            for line in errs {
                let (loc, msg) = split_location(&line);
                events.push(PackageEvent::error(loc, msg));
            }
            return Err(BzlError {
                events,
                message: format!(
                    "{} of module '{}' failed",
                    if ctx.static_failure { "compilation" } else { "initialization" },
                    label.package_identifier().package_fragment().get_relative_str(label.name())
                ),
            });
        }
        let module_digest = {
            // `ConfiguredRuleClassProvider.BundledFileSystem`: the digest of a bundled builtins file is
            // the hash of its path in the virtual file system, not of its contents.
            let mut fp = if label.package_identifier().repository().name() == crate::builtins::BUILTINS_REPO {
                zel_util::sha256::digest(format!("/virtual_builtins_bzl/{}", label.name()).as_bytes()).to_vec()
            } else {
                zel_util::sha256::digest(&bytes).to_vec()
            };
            for d in child_digests.lock().unwrap().iter() {
                fp.extend_from_slice(d);
            }
            // `BzlLoadFunction.getAndDigestPredeclaredEnvironment`: `.bzl` files loaded for BUILD
            // files also fold in the transitive digest of `@_builtins//:exports.bzl` (no autoloads).
            if label.package_identifier().repository().name() != crate::builtins::BUILTINS_REPO {
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
                                    // Toolchain types / exec constraints given as strings are labels
                                    // relative to the defining .bzl (and its repository mapping).
                                    let mut kwargs = r.kwargs.clone();
                                    {
                                        let mapping = self.cfg.lookup.resolver.mapping(label.package_identifier().repository());
                                        let cctx = crate::attr::ConvertCtx {
                                            pkg: label.package_identifier(),
                                            mapping: &mapping,
                                            rule_class: "rule",
                                        };
                                        let conv = |v: &Value| -> Value {
                                            match v {
                                                Value::String(s) if s.starts_with('@') || s.starts_with("//") || s.starts_with(':') => {
                                                    crate::attr::convert_label(s, &cctx).map(Value::Label).unwrap_or_else(|_| v.clone())
                                                }
                                                Value::Struct(st) if st.fields.contains_key("_toolchain_type") => {
                                                    let mut f = st.fields.clone();
                                                    if let Some(Value::String(n)) = f.get("name").cloned() {
                                                        if let Ok(l) = crate::attr::convert_label(&n, &cctx) {
                                                            f.insert("name".into(), Value::Label(l));
                                                        }
                                                    }
                                                    Value::Struct(Arc::new(zel_starlark::StructValue::new(f)))
                                                }
                                                o => o.clone(),
                                            }
                                        };
                                        for key in ["toolchains", "exec_compatible_with"] {
                                            if let Some(v) = kwargs.get(key).cloned() {
                                                if let Ok(items) = zel_starlark::eval::builtins::value_to_sequence(&v) {
                                                    let out: Vec<Value> = items.iter().map(&conv).collect();
                                                    kwargs.insert(
                                                        key.to_string(),
                                                        Value::List(Arc::new(Mutex::new(zel_starlark::StarlarkList::new(out)))),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    rc.starlark = Some(Arc::new(crate::rule_class::StarlarkRuleSpec {
                                        implementation: r.implementation.clone(),
                                        kwargs,
                                        defined_in: label.canonical_form(),
                                        definition_digest: module_digest
                                            .iter()
                                            .map(|b| format!("{b:02x}"))
                                            .collect(),
                                        definition_stack: r.definition_stack.clone(),
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
        Ok(Arc::new(BzlModule { exports, digest: module_digest }))
    }
}

/// Names of functions available (as globals in BUILD files, and under `native.`).
const BUILD_FUNCTIONS: &[&str] = &[
    "glob", "subpackages", "package", "exports_files", "package_group", "licenses",
    "existing_rule", "existing_rules", "package_name", "repository_name", "repo_name",
    "package_relative_label", "environment_group", "module_name", "module_version",
    "package_default_visibility",
];

const ATTR_FUNCTIONS: &[&str] = &[
    "int", "string", "bool", "string_list", "string_dict", "string_list_dict", "label",
    "label_list", "label_keyed_string_dict", "string_keyed_label_dict", "int_list", "output", "output_list",
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
    let (file, _) = eval_repo_file(fs, repo_root)?;
    Some(file)
}

fn parse_repo_file_args(fs: &dyn FileSystem, repo_root: &PathFragment) -> Option<PackageArgs> {
    eval_repo_file(fs, repo_root).map(|(_, a)| a)
}

fn eval_repo_file(fs: &dyn FileSystem, repo_root: &PathFragment) -> Option<(RepoFile, PackageArgs)> {
    let path = repo_root.get_relative_str("REPO.bazel");
    let bytes = fs.read_file(&path).ok()?;
    let src = String::from_utf8_lossy(&bytes).to_string();
    let out = Arc::new(Mutex::new((RepoFile::default(), PackageArgs::default())));
    let mut ctx = EvalContext::new(repo_root.clone(), None);
    ctx.file_name = path.path_string().to_string();
    ctx.strict_names = true;
    ctx.globals.insert("repo".into(), Value::BuiltinFunction("host:repo".into()));
    ctx.globals.insert(
        "ignore_directories".into(),
        Value::BuiltinFunction("host:ignore_directories".into()),
    );
    let o2 = Arc::clone(&out);
    let main = PackageIdentifier::create_in_main_repo("").ok()?;
    ctx.host = Some(Arc::new(move |_ctx, name, _pos, kw| match name {
        "ignore_directories" => {
            let dirs = _pos.first().or_else(|| kw.get("dirs")).ok_or_else(|| {
                StarlarkError("ignore_directories() missing required argument 'dirs'".into())
            })?;
            let list = sl_values(dirs).map_err(StarlarkError)?;
            o2.lock().unwrap().0.ignored_directories = list;
            Ok(Value::None)
        }
        "repo" => {
            let mapping = RepositoryMapping::always_fallback();
            let cctx = ConvertCtx {
                pkg: &main,
                mapping: &mapping,
                rule_class: "repo",
            };
            let mut g = o2.lock().unwrap();
            apply_package_args(&mut g.1, &kw, &cctx).map_err(StarlarkError)?;
            Ok(Value::None)
        }
        other => Err(StarlarkError(format!("unknown REPO.bazel function {other}"))),
    }));
    let errs = ctx.eval_file_checked(&src, Some(("REPO.bazel files", false)));
    if errs.is_err() {
        return None;
    }
    let g = out.lock().unwrap().clone();
    Some(g)
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
                let mut seen = BTreeSet::new();
                let mut dupes = BTreeSet::new();
                for x in &l {
                    if !seen.insert(x.canonical_form()) {
                        dupes.insert(x.canonical_form());
                    }
                }
                if !dupes.is_empty() {
                    return Err(format!(
                        "duplicate label(s) in {name}: {}",
                        dupes.into_iter().collect::<Vec<_>>().join(", ")
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
                parse_visibility(&labels)?;
                args.default_visibility = Some(labels);
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
                    for f in l {
                        if !args.features.contains(&f) {
                            args.features.push(f);
                        }
                    }
                }
                _ => unreachable!(),
            },
            "licenses" => match convert_plain(v, AttrType::StringList, k, cctx)? {
                AttrValue::StrList(l) => args.licenses = l,
                _ => unreachable!(),
            },
            "default_compatible_with" => args.default_compatible_with = label_list(k, v)?,
            "default_restricted_to" => args.default_restricted_to = label_list(k, v)?,
            "default_applicable_licenses" | "default_package_metadata" => {
                if !args.default_package_metadata.is_empty() {
                    return Err("Can not set both default_package_metadata and default_applicable_licenses. Move all declarations to default_package_metadata.".to_string());
                }
                args.default_package_metadata = label_list(k, v)?;
            }
            "default_hdrs_check" => match convert_plain(v, AttrType::String, k, cctx)? {
                AttrValue::Str(s) => args.default_hdrs_check = Some(s),
                _ => unreachable!(),
            },
            "transitive_visibility" => match convert_plain(v, AttrType::Label, k, cctx)? {
                AttrValue::Label(l) => args.transitive_visibility = l,
                _ => unreachable!(),
            },
            other => return Err(format!("unexpected keyword argument: {other}")),
        }
    }
    Ok(())
}

include!("loader_host.rs");
