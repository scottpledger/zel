//! Discovery, selection (Minimal Version Selection) and final dependency graph computation.
//! Mirrors `Discovery`, `Selection`, `BazelModuleResolutionFunction` and `BazelDepGraphFunction`.

use crate::model::*;
use crate::module_file::{eval_module_file, ModuleFileOptions};
use crate::registry::{FileHashes, KnownHashesMode, Registry};
use crate::version::Version;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const DEFAULT_REGISTRY: &str = "https://bcr.bazel.build/";

/// Insertion-ordered map (Java `ImmutableMap` semantics).
#[derive(Debug, Clone)]
pub struct OrderedMap<K, V> {
    pub keys: Vec<K>,
    map: HashMap<K, V>,
}

impl<K: std::hash::Hash + Eq + Clone, V> Default for OrderedMap<K, V> {
    fn default() -> Self {
        Self {
            keys: vec![],
            map: HashMap::new(),
        }
    }
}

impl<K: std::hash::Hash + Eq + Clone, V> OrderedMap<K, V> {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert(&mut self, k: K, v: V) {
        if !self.map.contains_key(&k) {
            self.keys.push(k.clone());
        }
        self.map.insert(k, v);
    }
    pub fn get(&self, k: &K) -> Option<&V> {
        self.map.get(k)
    }
    pub fn get_mut(&mut self, k: &K) -> Option<&mut V> {
        self.map.get_mut(k)
    }
    pub fn contains_key(&self, k: &K) -> bool {
        self.map.contains_key(k)
    }
    pub fn len(&self) -> usize {
        self.keys.len()
    }
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.keys.iter().map(move |k| (k, &self.map[k]))
    }
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.keys.iter().map(move |k| &self.map[k])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckMode {
    Off,
    Warning,
    Error,
}

impl CheckMode {
    pub fn parse(s: &str) -> Option<CheckMode> {
        match s.to_ascii_lowercase().as_str() {
            "off" => Some(CheckMode::Off),
            "warning" => Some(CheckMode::Warning),
            "error" => Some(CheckMode::Error),
            _ => None,
        }
    }
}

// Test hook: when set, the embedded `bazel_tools` module keeps no module-extension usages.
#[cfg(test)]
thread_local! {
    /// Unit tests normally tolerate registries lacking `bazel_tools`' dependencies; set to test the strict (production) behaviour.
    pub(crate) static STRICT_BAZEL_TOOLS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static HERMETIC_BAZEL_TOOLS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) struct StrictBazelToolsGuard(bool);

#[cfg(test)]
impl Drop for StrictBazelToolsGuard {
    fn drop(&mut self) {
        STRICT_BAZEL_TOOLS.with(|h| h.set(self.0));
    }
}

#[cfg(test)]
pub(crate) fn scoped_strict_bazel_tools() -> StrictBazelToolsGuard {
    let prev = STRICT_BAZEL_TOOLS.with(|h| h.replace(true));
    StrictBazelToolsGuard(prev)
}

#[cfg(test)]
pub(crate) struct HermeticBazelToolsGuard(bool);

#[cfg(test)]
impl Drop for HermeticBazelToolsGuard {
    fn drop(&mut self) {
        HERMETIC_BAZEL_TOOLS.with(|h| h.set(self.0));
    }
}

#[cfg(test)]
pub(crate) fn scoped_hermetic_bazel_tools() -> HermeticBazelToolsGuard {
    let prev = HERMETIC_BAZEL_TOOLS.with(|h| h.replace(true));
    HermeticBazelToolsGuard(prev)
}

#[derive(Clone)]
pub struct ResolveOptions {
    pub workspace_root: PathBuf,
    /// `--registry` values; empty means the default (BCR).
    pub registries: Vec<String>,
    pub check_direct_dependencies: CheckMode,
    pub bazel_compatibility_mode: CheckMode,
    pub bazel_version: String,
    pub ignore_dev_dependency: bool,
    pub isolated_extension_usages: bool,
    /// `--allow_yanked_versions` (`all` or `name@version,...`). `None` = nothing allowed.
    pub allowed_yanked: Option<Vec<String>>,
    pub allow_all_yanked: bool,
    /// `--override_module name=path`
    pub override_modules: Vec<(String, String)>,
    /// `--inject_module name=path`
    pub inject_modules: Vec<(String, String)>,
    /// `--inject_repository name=path`
    pub inject_repositories: Vec<(String, String)>,
    pub known_hashes: Option<FileHashes>,
    pub known_hashes_mode: KnownHashesMode,
    /// Previously selected yanked versions from `MODULE.bazel.lock` (reused under `Update` and `Error`).
    pub locked_yanked: Option<BTreeMap<ModuleKey, String>>,
    pub module_mirrors: Vec<String>,
    /// Repository cache root used to serve lockfile-recorded registry files offline.
    pub repository_cache: Option<PathBuf>,
    pub vendor_dir: Option<PathBuf>,
    pub downloader_rewriter: Option<crate::download::UrlRewriter>,
    /// Fetches the repository described by an archive/git override so its MODULE.bazel can be read.
    #[allow(clippy::type_complexity)]
    pub repo_fetcher: Option<Arc<dyn Fn(&str, &RepoSpec) -> Result<PathBuf, String> + Send + Sync>>,
    pub credential_helpers: Vec<(String, PathBuf)>,
    pub client_env: BTreeMap<String, String>,
}

pub fn default_bazel_version() -> String {
    zel_util::default_bazel_version()
}

impl ResolveOptions {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            registries: vec![],
            check_direct_dependencies: CheckMode::Warning,
            bazel_compatibility_mode: CheckMode::Error,
            bazel_version: default_bazel_version(),
            ignore_dev_dependency: false,
            isolated_extension_usages: false,
            allowed_yanked: None,
            allow_all_yanked: false,
            override_modules: vec![],
            inject_modules: vec![],
            inject_repositories: vec![],
            known_hashes: None,
            known_hashes_mode: KnownHashesMode::Ignore,
            locked_yanked: None,
            module_mirrors: vec![],
            repository_cache: None,
            vendor_dir: None,
            downloader_rewriter: None,
            repo_fetcher: None,
            credential_helpers: vec![],
            client_env: BTreeMap::new(),
        }
    }
}

pub fn expand_registry_url(raw: &str, workspace: &Path) -> String {
    if !raw.contains("%workspace%") {
        return raw.to_string();
    }
    let ws = workspace.to_string_lossy().replace('\\', "/");
    raw.replace("%workspace%", &ws)
}

/// Parses `--module_mirrors` entries (`[registry=]url[,url...]`), validating each mirror URL and
/// checking that any explicit registry key is among the configured registries.
pub fn parse_module_mirrors(
    raw_mirrors: &[String],
    raw_registries: &[String],
) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut configured: Vec<String> = if raw_registries.is_empty() {
        vec![DEFAULT_REGISTRY.trim_end_matches('/').to_string()]
    } else {
        raw_registries
            .iter()
            .map(|r| r.trim_end_matches('/').to_string())
            .collect()
    };
    configured.dedup();
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in raw_mirrors {
        let (reg_key, urls_part) = match entry.split_once('=') {
            Some((k, v)) => (k.trim_end_matches('/').to_string(), v),
            None => (String::new(), entry.as_str()),
        };
        if !reg_key.is_empty() && !configured.iter().any(|c| c == &reg_key) {
            return Err(format!(
                "While parsing option --module_mirrors={entry}: Registry '{reg_key}' is not among the configured registries"
            ));
        }
        let mut urls = Vec::new();
        if !urls_part.is_empty() {
            for u in urls_part.split(',') {
                let u_trimmed = u.trim_end_matches('/');
                crate::registry::validate_registry_url(u_trimmed).map_err(|e| {
                    format!(
                        "While parsing option --module_mirrors={entry}: Invalid mirror URL: {e}"
                    )
                })?;
                urls.push(u_trimmed.to_string());
            }
        }
        map.insert(reg_key, urls);
    }
    Ok(map)
}

/// `ModuleExtensionId`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExtensionId {
    /// Canonical label of the defining bzl file, e.g. `@@rules_python+//python/extensions:python.bzl`.
    pub bzl_file: String,
    pub name: String,
    pub isolation_key: Option<(ModuleKey, String)>,
}

impl ExtensionId {
    pub fn is_innate(&self) -> bool {
        self.name.contains(' ')
    }
    /// Repo name (canonical, without `@@`) of the bzl file label.
    pub fn bzl_repo(&self) -> String {
        let r = self.bzl_file.trim_start_matches('@');
        r.split("//").next().unwrap_or("").to_string()
    }
    pub fn bzl_package_and_file(&self) -> (String, String) {
        let rest = self.bzl_file.split("//").nth(1).unwrap_or("");
        let (p, n) = rest.split_once(':').unwrap_or(("", rest));
        (p.to_string(), n.to_string())
    }
}

#[derive(Debug)]
pub struct Resolution {
    pub root_module_name: String,
    pub overrides: OrderedMap<String, ModuleOverride>,
    pub dep_graph: OrderedMap<ModuleKey, Module>,
    pub unpruned_dep_graph: OrderedMap<ModuleKey, InterimModule>,
    /// Graph right after discovery (before selection), used by `mod` for `--include_unused`.
    pub discovered_graph: OrderedMap<ModuleKey, InterimModule>,
    pub registry_file_hashes: FileHashes,
    pub selected_yanked_versions: BTreeMap<ModuleKey, String>,
    pub warnings: Vec<String>,
    /// canonical repo name -> module
    pub canonical_repo_names: OrderedMap<String, ModuleKey>,
    pub extension_usages: Vec<(ExtensionId, ModuleKey, ExtensionUsage)>,
    pub extension_unique_names: BTreeMap<ExtensionId, String>,
    /// extension -> (overridden repo name -> canonical overriding repo name)
    pub repo_overrides: BTreeMap<ExtensionId, BTreeMap<String, String>>,
    /// Paths of all MODULE.bazel files of the root module (for watching/lockfile hashes).
    pub root_module_file_paths: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct ResolveError {
    pub message: String,
    pub warnings: Vec<String>,
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn re<T>(m: impl Into<String>) -> Result<T, ResolveError> {
    Err(ResolveError {
        message: m.into(),
        warnings: vec![],
    })
}

pub fn builtin_modules() -> Vec<String> {
    vec!["bazel_tools".to_string()]
}

struct Ctx<'a> {
    opts: &'a ResolveOptions,
    registries: Vec<Registry>,
    hashes: FileHashes,
    warnings: Vec<String>,
    root_overrides: Vec<(String, ModuleOverride)>,
    root_name: String,
    root_module_file_paths: Vec<PathBuf>,
    local_registries: HashMap<String, usize>,
    mirror_map: BTreeMap<String, Vec<String>>,
}

fn read_file(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok()
}

pub fn root_module_file(workspace: &Path) -> PathBuf {
    workspace.join("MODULE.bazel")
}

/// Shared `include()` file loader for both the root module and non-registry module overrides.
fn read_included_module_file(
    repo_root: &Path,
    label: &str,
    recorded_paths: Option<&Arc<std::sync::Mutex<Vec<PathBuf>>>>,
) -> Result<String, String> {
    let stripped = label.strip_prefix("//").unwrap_or(label);
    let (pkg, target) = match stripped.split_once(':') {
        Some((p, t)) => (p, t),
        None => (stripped, stripped.rsplit('/').next().unwrap_or(stripped)),
    };
    let rel = if pkg.is_empty() {
        target.to_string()
    } else {
        format!("{pkg}/{target}")
    };
    if !pkg.is_empty()
        && !repo_root.join(pkg).join("BUILD.bazel").is_file()
        && !repo_root.join(pkg).join("BUILD").is_file()
    {
        return Err(format!(
            "\u{1}PKG:BUILD file not found in any of the following directories. Add a BUILD file to a directory to mark it as a package.\n - {pkg}"
        ));
    }
    let base = rel.rsplit('/').next().unwrap_or("");
    if base.starts_with('.') {
        return Err("the name of the file to be included must not start with '.'".to_string());
    }
    let p = repo_root.join(&rel);
    if !p.is_file() {
        return Err("not a regular file".to_string());
    }
    if let Some(rec) = recorded_paths {
        rec.lock().unwrap().push(p.clone());
    }
    std::fs::read_to_string(&p).map_err(|e| e.to_string())
}

impl Ctx<'_> {
    fn registry_for(&mut self, url: &str) -> Result<&Registry, String> {
        if let Some(i) = self.local_registries.get(url) {
            return Ok(&self.registries[*i]);
        }
        let raw_key = url.trim_end_matches('/');
        let mirrors = self
            .mirror_map
            .get(raw_key)
            .or_else(|| self.mirror_map.get(""))
            .cloned()
            .unwrap_or_default();
        let expanded = expand_registry_url(url, &self.opts.workspace_root);
        let mut r = Registry::new(&expanded)?
            .with_mirrors(mirrors)
            .with_repo_cache(self.opts.repository_cache.clone())
            .with_vendor_dir(self.opts.vendor_dir.clone())
            .with_rewriter(self.opts.downloader_rewriter.clone());
        if let Some(k) = &self.opts.known_hashes {
            r = r.with_known_hashes(k.clone(), self.opts.known_hashes_mode);
        }
        self.registries.push(r);
        let i = self.registries.len() - 1;
        self.local_registries.insert(url.to_string(), i);
        Ok(&self.registries[i])
    }

    fn default_registry_urls(&self) -> Vec<String> {
        if self.opts.registries.is_empty() {
            vec![DEFAULT_REGISTRY.to_string()]
        } else {
            self.opts.registries.clone()
        }
    }

    fn non_registry_module_dir(&mut self, name: &str, spec: &RepoSpec) -> Result<PathBuf, String> {
        match spec.rule_name.as_str() {
            "local_repository" => {
                let p = spec.attr("path").and_then(|v| v.as_str()).unwrap_or("");
                let path = Path::new(p);
                let joined = if path.is_absolute() { path.to_path_buf() } else { self.opts.workspace_root.join(path) };
                let full = lexically_normalized(&joined);
                if !full.is_dir() {
                    // The failure of `local.bzl`'s `_get_dir_path`, reported like a repo fetch failure.
                    let msg = format!(
                        "The repository's path is \"{p}\" (absolute: \"{}\") but it does not exist or is not a directory.",
                        full.display()
                    );
                    let f = "{OUTPUT_BASE}/external/bazel_tools/tools/build_defs/repo/local.bzl";
                    return Err(format!(
                        "ERROR: {f}:55:13: An error occurred during the fetch of repository '{name}+':\n   Traceback (most recent call last):\n\tFile \"{f}\", line 62, column 25, in _local_repository_impl\n\t\tpath = _get_dir_path(rctx)\n\tFile \"{f}\", line 55, column 13, in _get_dir_path\n\t\tfail(\nError in fail: {msg}\nERROR: Error computing the main repository mapping: error during computation of main repo mapping: {msg}"
                    ));
                }
                Ok(full)
            }
            _ => match &self.opts.repo_fetcher {
                Some(f) => f(name, spec),
                None => Err(format!(
                    "cannot fetch the '{}' override for module '{name}': no repository fetcher available",
                    spec.rule_name
                )),
            },
        }
    }

    /// `bazel_tools` is a real module in Bazel (its MODULE.tools is evaluated); its `bazel_dep`s
    /// (rules_cc, rules_java, rules_python, protobuf, ...) make those repositories visible to
    /// `@bazel_tools` files.
    fn load_bazel_tools_module(&mut self, key: &ModuleKey) -> Result<InterimModule, String> {
        let Some(content) = zel_bazel_tools::read("MODULE.bazel") else {
            return Ok(synthetic_module(key));
        };
        let content = String::from_utf8_lossy(content).to_string();
        let opts = ModuleFileOptions {
            key: ModuleKey::new("bazel_tools".to_string(), Version::empty()),
            ignore_dev_deps: true,
            isolated_extension_usages: self.opts.isolated_extension_usages,
            registry_url: None,
            file_name: "<bazel_tools>/MODULE.bazel".into(),
            include_reader: None,
            builtin_modules: vec![],
        };
        let mut m = self.eval(content, opts, key)?;
        // Every `bazel_dep` of MODULE.tools is resolved like any other module: a registry that
        // lacks one fails the build (`module X not found in registries`), as in Bazel.
        // Unit tests build registries that only hold the modules under test; they keep the lenient
        // behaviour (dependencies the registries do not know are dropped) unless `STRICT_BAZEL_TOOLS` is set.
        #[cfg(test)]
        if !STRICT_BAZEL_TOOLS.with(|h| h.get()) {
            let urls = self.default_registry_urls();
            let mut keep = Vec::new();
            for (n, k) in std::mem::take(&mut m.deps) {
                let mut found = false;
                for u in &urls {
                    let mut hashes = FileHashes::new();
                    if let Ok(reg) = self.registry_for(u) {
                        if matches!(reg.get_module_file(&k, &mut hashes), Ok(Some(_))) {
                            found = true;
                            break;
                        }
                    }
                }
                if found {
                    keep.push((n, k));
                }
            }
            m.deps = keep;
            let have: Vec<String> = m.deps.iter().map(|(n, _)| n.clone()).collect();
            m.extension_usages.retain(|u| {
                let b = u.bzl_file.trim_start_matches('@');
                match b.split_once("//") {
                    Some((repo, _)) if !repo.is_empty() && !repo.starts_with('@') => {
                        have.iter().any(|h| h == repo) || repo == "bazel_tools"
                    }
                    _ => true,
                }
            });
        }
        m.original_deps = m.deps.clone();
        // `repo_name = None` deps (rules_apple, rules_swift, abseil-cpp, bazel_features, ...) stay: they are
        // graph edges that bump the selected versions (`MODULE.tools`: "version bump"), as in Bazel.
        // bazel_tools registers its launcher/test-runner/rc toolchains (as in MODULE.tools).
        // Hermetic unit tests (no network) drop bazel_tools' own extension repositories
        // (remote_coverage_tools downloads an archive); see `HERMETIC_BAZEL_TOOLS`.
        #[cfg(test)]
        if HERMETIC_BAZEL_TOOLS.with(|h| h.get()) {
            m.extension_usages.clear();
        }
        m.execution_platforms_to_register.clear();
        Ok(m)
    }

    /// Fetches and evaluates the MODULE.bazel of `key` (non-root).
    fn load_module(&mut self, key: &ModuleKey) -> Result<InterimModule, String> {
        if key.name == "bazel_tools" {
            return self.load_bazel_tools_module(key);
        }
        let ov = self
            .root_overrides
            .iter()
            .find(|(n, _)| *n == key.name)
            .map(|(_, o)| o.clone());
        if let Some(ModuleOverride::NonRegistry(spec)) = &ov {
            if key.name == "bazel_tools" {
                return Ok(synthetic_module(key));
            }
            let dir = self.non_registry_module_dir(&key.name, spec)?;
            let mf = dir.join("MODULE.bazel");
            let content = match read_file(&mf) {
                Some(c) => c,
                None => {
                    // The repository fetch of the override fails first when the directory is not a repo at all.
                    let is_repo = ["MODULE.bazel", "REPO.bazel", "WORKSPACE", "WORKSPACE.bazel"]
                        .iter()
                        .any(|f| dir.join(f).exists());
                    if !is_repo {
                        return Err(format!(
                            "fetching {n}+: java.io.IOException: No MODULE.bazel, REPO.bazel, or WORKSPACE file found in {{OUTPUT_BASE}}/external/{n}+",
                            n = key.name
                        ));
                    }
                    return Err(format!(
                        "MODULE.bazel expected but not found at {}",
                        mf.display()
                    ));
                }
            };
            let dir2 = dir.clone();
            let opts = ModuleFileOptions {
                key: key.clone(),
                ignore_dev_deps: true,
                isolated_extension_usages: self.opts.isolated_extension_usages,
                registry_url: None,
                file_name: mf.display().to_string(),
                include_reader: Some(Arc::new(move |label: &str| {
                    read_included_module_file(&dir2, label, None)
                })),
                builtin_modules: builtin_modules(),
            };
            return self.eval(content, opts, key);
        }
        let urls: Vec<String> = match &ov {
            Some(ModuleOverride::SingleVersion { registry, .. })
            | Some(ModuleOverride::MultipleVersion { registry, .. })
                if !registry.is_empty() =>
            {
                vec![registry.clone()]
            }
            _ => self.default_registry_urls(),
        };
        let mut hashes = FileHashes::new();
        let mut found: Option<(String, String)> = None;
        let mut not_found_trace: Vec<String> = Vec::new();
        for u in &urls {
            let reg = self.registry_for(u)?;
            if let Some(c) = reg.get_module_file(key, &mut hashes)? {
                found = Some((reg.url().to_string(), c));
                break;
            }
            not_found_trace.push(format!("{}: not found", reg.module_file_url(key)));
        }
        for (k, v) in hashes {
            self.hashes.insert(k, v);
        }
        let Some((reg_url, content)) = found else {
            if key.version.is_empty() {
                return Err(format!(
                    "bad bazel_dep on module '{}' with no version. Did you forget to specify a version, or a non-registry override?",
                    key.name
                ));
            }
            return Err(format!(
                "module {key} not found in registries:\n* {}",
                not_found_trace.join("\n* ")
            ));
        };
        let opts = ModuleFileOptions {
            key: key.clone(),
            ignore_dev_deps: true,
            isolated_extension_usages: self.opts.isolated_extension_usages,
            registry_url: Some(reg_url.clone()),
            file_name: format!(
                "{reg_url}/modules/{}/{}/MODULE.bazel",
                key.name, key.version
            ),
            include_reader: None,
            builtin_modules: builtin_modules(),
        };
        self.eval(content, opts, key)
    }

    fn eval(
        &mut self,
        content: String,
        opts: ModuleFileOptions,
        key: &ModuleKey,
    ) -> Result<InterimModule, String> {
        let r = eval_module_file(&content, opts)?;
        self.warnings.extend(r.warnings);
        let m = r.module;
        if m.name != key.name {
            return Err(format!(
                "the MODULE.bazel file of {key} declares a different name ({})",
                m.name
            ));
        }
        if !key.version.is_empty() && m.version != key.version {
            return Err(format!(
                "the MODULE.bazel file of {key} declares a different version ({})",
                m.version
            ));
        }
        Ok(m)
    }
}

/// Collapses `.` and `..` path components without touching the file system.
fn lexically_normalized(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `Discovery.maybeReportDependencyChain`: the events already reported for the failing module
/// file (`ERROR: ...` lines) are kept as-is and only the final exception message gets the
/// `in module dependency chain` prefix.
fn chain_message(chain: &str, e: &str) -> String {
    let lines: Vec<&str> = e.lines().collect();
    if let Some(last) = lines.iter().rposition(|l| l.starts_with("ERROR: ")) {
        if last > 0 || lines.len() > 1 {
            let events = lines[..last].join("\n");
            let trailer = lines[last..].join("\n");
            if events.contains("An error occurred during the fetch of repository") {
                return e.to_string();
            }
            let trailer = trailer
                .strip_prefix("ERROR: ")
                .unwrap_or(&trailer)
                .to_string();
            let tail = format!("in module dependency chain {chain}: {trailer}");
            return if events.is_empty() {
                tail
            } else {
                format!("{events}\nERROR: {tail}")
            };
        }
        let t = lines[last].strip_prefix("ERROR: ").unwrap_or(lines[last]);
        return format!("in module dependency chain {chain}: {t}");
    }
    format!("in module dependency chain {chain}: {e}")
}

fn synthetic_module(key: &ModuleKey) -> InterimModule {
    InterimModule {
        key: key.clone(),
        name: key.name.clone(),
        version: key.version.clone(),
        compatibility_level: 0,
        repo_name: key.name.clone(),
        bazel_compatibility: vec![],
        toolchains_to_register: vec![],
        execution_platforms_to_register: vec![],
        deps: vec![],
        dep_max_compat: BTreeMap::new(),
        original_deps: vec![],
        nodep_deps: vec![],
        nodep_max_compat: vec![],
        registry_url: None,
        extension_usages: vec![],
        flag_aliases: vec![],
    }
}

/// Reads the root module (applying `include`s) and returns it plus overrides.
#[allow(clippy::type_complexity)]
fn eval_root(
    opts: &ResolveOptions,
    ctx_warnings: &mut Vec<String>,
) -> Result<(InterimModule, Vec<(String, ModuleOverride)>, Vec<PathBuf>), String> {
    let path = root_module_file(&opts.workspace_root);
    let mut paths = vec![path.clone()];
    let content = read_file(&path).unwrap_or_default();
    let ws = opts.workspace_root.clone();
    let included: Arc<std::sync::Mutex<Vec<PathBuf>>> = Arc::new(std::sync::Mutex::new(vec![]));
    let inc2 = included.clone();
    let mo = ModuleFileOptions {
        key: ModuleKey::root(),
        ignore_dev_deps: opts.ignore_dev_dependency,
        isolated_extension_usages: opts.isolated_extension_usages,
        registry_url: None,
        file_name: path.display().to_string(),
        include_reader: Some(Arc::new(move |label: &str| {
            read_included_module_file(&ws, label, Some(&inc2))
        })),
        builtin_modules: builtin_modules(),
    };
    let mut r = eval_module_file(&content, mo)?;
    paths.extend(included.lock().unwrap().iter().cloned());
    ctx_warnings.extend(r.warnings.clone());
    // Inject --inject_repository repos (dev-dependency innate local_repository usage).
    if !opts.inject_repositories.is_empty() {
        let ext = "@bazel_tools//tools/build_defs/repo:local.bzl local_repository".to_string();
        let idx = r
            .module
            .extension_usages
            .iter()
            .position(|u| u.is_innate() && u.extension_name == ext);
        let mut usage = match idx {
            Some(i) => r.module.extension_usages.remove(i),
            None => ExtensionUsage {
                bzl_file: "//:MODULE.bazel".into(),
                extension_name: ext.clone(),
                isolation_key: None,
                proxies: vec![],
                tags: vec![],
                repo_overrides: BTreeMap::new(),
            },
        };
        let mut proxy = Proxy {
            proxy_name: String::new(),
            dev_dependency: true,
            location: "<builtin>".into(),
            imports: vec![],
            containing_module_file: "MODULE.bazel".into(),
        };
        for (n, p) in &opts.inject_repositories {
            usage.tags.push(Tag {
                tag_name: "repo".into(),
                attrs: vec![
                    ("name".into(), AttrVal::Str(n.clone())),
                    ("path".into(), AttrVal::Str(p.clone())),
                ],
                dev_dependency: true,
                location: "<builtin>".into(),
            });
            proxy.imports.push((n.clone(), n.clone()));
        }
        usage.proxies.push(proxy);
        r.module.extension_usages.push(usage);
    }
    let mut overrides = r.overrides;
    // Command-line overrides take precedence.
    for (name, path) in &opts.override_modules {
        let spec = RepoSpec::new(
            LOCAL_BZL,
            "local_repository",
            vec![("path".into(), AttrVal::Str(path.clone()))],
        );
        overrides.retain(|(n, _)| n != name);
        overrides.push((name.clone(), ModuleOverride::NonRegistry(spec)));
    }
    for (name, path) in &opts.inject_modules {
        if overrides.iter().any(|(n, _)| n == name)
            && opts.override_modules.iter().any(|(n, _)| n == name)
        {
            return Err(format!(
                "module '{name}' is given to both --inject_module and --override_module; use --inject_module to add a new module, or --override_module to override a module that is already a dependency"
            ));
        }
        if *name == r.module.name {
            return Err(format!(
                "--inject_module cannot inject the root module '{name}'"
            ));
        }
        if builtin_modules().contains(name) {
            return Err(format!("--inject_module cannot inject '{name}' as it is a built-in module; use --override_module to override it instead"));
        }
        let spec = RepoSpec::new(
            LOCAL_BZL,
            "local_repository",
            vec![("path".into(), AttrVal::Str(path.clone()))],
        );
        overrides.retain(|(n, _)| n != name);
        overrides.push((name.clone(), ModuleOverride::NonRegistry(spec)));
        r.module
            .nodep_deps
            .push(ModuleKey::new(name.clone(), Version::empty()));
        r.module.nodep_max_compat.push(-1);
    }
    // built-in modules override
    for b in builtin_modules() {
        if !overrides.iter().any(|(n, _)| *n == b) {
            overrides.push((
                b.clone(),
                ModuleOverride::NonRegistry(RepoSpec::new(LOCAL_BZL, "local_repository", vec![])),
            ));
        }
    }
    // Validation from computeForRootModule.
    let module = r.module;
    for u in &module.extension_usages {
        if u.isolation_key.is_some() && u.proxies[0].imports.is_empty() {
            return Err(format!(
                "the isolated usage at {} of extension {} defined in {} has no effect as no repositories are imported from it. Either import one or more repositories generated by the extension with use_repo or remove the usage.",
                u.proxies[0].location,
                u.extension_name,
                // `ModuleFileGlobals.normalizeLabelString` maps the empty repo name to the module's repo name.
                if u.bzl_file.starts_with("//") {
                    format!("@{}{}", if module.repo_name.is_empty() { &module.name } else { &module.repo_name }, u.bzl_file)
                } else {
                    u.bzl_file.clone()
                }
            ));
        }
    }
    for (n, o) in &overrides {
        if let ModuleOverride::SingleVersion { version, .. } = o {
            if let Some(dep) =
                module
                    .dep(n)
                    .or(module.deps.iter().map(|(_, k)| k).find(|k| k.name == *n))
            {
                if !dep.version.is_empty() && *version < dep.version {
                    return Err(format!(
                        "module '{n}' is overridden to use version '{version}', which is lower than the version '{}' requested by the root module",
                        dep.version
                    ));
                }
            }
        }
    }
    if let Some((_, o)) = overrides.iter().find(|(n, _)| *n == module.name) {
        return Err(format!("invalid override for the root module found: {o}"));
    }
    Ok((module, overrides, paths))
}

fn apply_overrides(
    root_name: &str,
    overrides: &[(String, ModuleOverride)],
    k: &ModuleKey,
) -> ModuleKey {
    if root_name == k.name {
        return ModuleKey::root();
    }
    let v = match overrides.iter().find(|(n, _)| *n == k.name).map(|(_, o)| o) {
        Some(ModuleOverride::NonRegistry(_)) => Version::empty(),
        Some(ModuleOverride::SingleVersion { version, .. }) if !version.is_empty() => {
            version.clone()
        }
        _ => k.version.clone(),
    };
    ModuleKey::new(k.name.clone(), v)
}

struct Discovered {
    graph: OrderedMap<ModuleKey, InterimModule>,
}

fn discover(ctx: &mut Ctx, root: &InterimModule) -> Result<Discovered, ResolveError> {
    let mut prev_names: HashSet<String> = HashSet::new();
    prev_names.insert(root.name.clone());
    loop {
        let mut graph: OrderedMap<ModuleKey, InterimModule> = OrderedMap::new();
        let mut preds: HashMap<ModuleKey, ModuleKey> = HashMap::new();
        let mut unfulfilled: HashSet<String> = HashSet::new();
        let rn = ctx.root_name.clone();
        let ovs = ctx.root_overrides.clone();
        let transform = |k: &ModuleKey| apply_overrides(&rn, &ovs, k);
        graph.insert(ModuleKey::root(), root.with_deps_transformed(&transform));
        let mut horizon = vec![ModuleKey::root()];
        while !horizon.is_empty() {
            let mut next: Vec<ModuleKey> = Vec::new();
            for mk in &horizon {
                let m = graph.get(mk).unwrap().clone();
                for (_, dk) in &m.deps {
                    if graph.contains_key(dk) || next.contains(dk) {
                        continue;
                    }
                    preds.entry(dk.clone()).or_insert_with(|| mk.clone());
                    next.push(dk.clone());
                }
                for dk in &m.nodep_deps {
                    if graph.contains_key(dk) || next.contains(dk) {
                        continue;
                    }
                    if !prev_names.contains(&dk.name) {
                        unfulfilled.insert(dk.name.clone());
                        continue;
                    }
                    preds.entry(dk.clone()).or_insert_with(|| mk.clone());
                    next.push(dk.clone());
                }
            }
            for dk in &next {
                let m = ctx.load_module(dk).map_err(|e| {
                    // Discovery.maybeReportDependencyChain only adds the chain for BAD_MODULE /
                    // MODULE_NOT_FOUND; registry access (I/O) errors and override fetch errors are reported as-is.
                    if e.starts_with("Error accessing registry")
                        || e.contains("The repository's path is")
                        || e.contains("error running 'git fetch")
                        || e.contains("MODULE.bazel expected but not found at")
                        || e.contains("No MODULE.bazel, REPO.bazel, or WORKSPACE file found in")
                    {
                        return ResolveError {
                            message: e,
                            warnings: vec![],
                        };
                    }
                    let mut chain = vec![dk.clone()];
                    let mut p = dk.clone();
                    while let Some(pp) = preds.get(&p) {
                        chain.push(pp.clone());
                        p = pp.clone();
                    }
                    chain.reverse();
                    let chain_s = chain
                        .iter()
                        .map(|c| c.to_string())
                        .collect::<Vec<_>>()
                        .join(" -> ");
                    ResolveError {
                        message: chain_message(&chain_s, &e),
                        warnings: vec![],
                    }
                })?;
                graph.insert(
                    dk.clone(),
                    m.with_deps_transformed(&|k| apply_overrides(&rn, &ovs, k)),
                );
            }
            horizon = next;
        }
        // Drop nodep edges to modules not in the graph.
        let keys: Vec<ModuleKey> = graph.keys.clone();
        for k in keys {
            let m = graph.get(&k).unwrap().clone();
            if !m.nodep_deps.iter().all(|d| graph.contains_key(d)) {
                let mut m2 = m.clone();
                let mut kept_deps = Vec::new();
                let mut kept_compat = Vec::new();
                for (idx, d) in m.nodep_deps.iter().enumerate() {
                    if graph.contains_key(d) {
                        kept_deps.push(d.clone());
                        kept_compat.push(m.nodep_max_compat.get(idx).copied().unwrap_or(-1));
                    }
                }
                m2.nodep_deps = kept_deps;
                m2.nodep_max_compat = kept_compat;
                graph.insert(k, m2);
            }
        }
        prev_names = graph.values().map(|m| m.name.clone()).collect();
        if !unfulfilled.iter().any(|n| prev_names.contains(n)) {
            check_injected(&graph, &ctx.opts.inject_modules)?;
            return Ok(Discovered { graph });
        }
    }
}

fn check_injected(
    graph: &OrderedMap<ModuleKey, InterimModule>,
    injected: &[(String, String)],
) -> Result<(), ResolveError> {
    if injected.is_empty() {
        return Ok(());
    }
    for m in graph.values() {
        if m.key.is_root() || injected.iter().any(|(n, _)| *n == m.name) {
            continue;
        }
        for (_, dk) in &m.original_deps {
            if injected.iter().any(|(n, _)| *n == dk.name) {
                return re(format!(
                    "--inject_module cannot inject '{}' as it is already in the dependency graph: '{}' depends on '{}'; use --override_module to override it instead",
                    dk.name, m.key, dk
                ));
            }
        }
    }
    Ok(())
}

struct Selected {
    resolved: OrderedMap<ModuleKey, InterimModule>,
    unpruned: OrderedMap<ModuleKey, InterimModule>,
}

fn select(
    graph: &OrderedMap<ModuleKey, InterimModule>,
    overrides: &[(String, ModuleOverride)],
) -> Result<Selected, ResolveError> {
    let mut by_name: HashMap<&str, HashSet<&Version>> = HashMap::new();
    for k in &graph.keys {
        by_name.entry(&k.name).or_default().insert(&k.version);
    }
    let mut allowed_by_compat: HashMap<(String, i64), BTreeSet<Version>> = HashMap::new();
    let mut has_mvo: HashSet<String> = HashSet::new();
    for (name, o) in overrides {
        if let ModuleOverride::MultipleVersion { versions, .. } = o {
            has_mvo.insert(name.clone());
            for v in versions {
                let k = ModuleKey::new(name.clone(), v.clone());
                let Some(dm) = graph.get(&k) else {
                    return re(format!(
                        "multiple_version_override for module {name} contains version {v}, but it doesn't exist in the dependency graph"
                    ));
                };
                allowed_by_compat
                    .entry((name.clone(), dm.compatibility_level))
                    .or_default()
                    .insert(v.clone());
            }
        }
    }
    let compat_of =
        |k: &ModuleKey| -> i64 { graph.get(k).map(|m| m.compatibility_level).unwrap_or(0) };
    // Validate max_compatibility_level >= dep's compatibility_level on every discovered module.
    for m in graph.values() {
        for (rname, dk) in &m.deps {
            let max_cl = m.dep_max_compat.get(rname).copied().unwrap_or(-1);
            if max_cl >= 0 {
                let dep_cl = compat_of(dk);
                if max_cl < dep_cl {
                    return re(format!(
                        "invalid max_compatibility_level {max_cl} for {dk} in {}: cannot be less than compatibility_level {dep_cl}",
                        m.key
                    ));
                }
            }
        }
        for (idx, dk) in m.nodep_deps.iter().enumerate() {
            let max_cl = m.nodep_max_compat.get(idx).copied().unwrap_or(-1);
            if max_cl >= 0 {
                let dep_cl = compat_of(dk);
                if max_cl < dep_cl {
                    return re(format!(
                        "invalid max_compatibility_level {max_cl} for {dk} in {}: cannot be less than compatibility_level {dep_cl}",
                        m.key
                    ));
                }
            }
        }
    }
    let target_allowed = |name: &str, cl: i64, version: &Version| -> Version {
        if has_mvo.contains(name) {
            allowed_by_compat
                .get(&(name.to_string(), cl))
                .and_then(|set| set.range(version.clone()..).next().cloned())
                .unwrap_or_else(Version::empty)
        } else {
            Version::empty()
        }
    };
    let mut selected: HashMap<(String, i64, Version), Version> = HashMap::new();
    for k in &graph.keys {
        let cl = compat_of(k);
        let g = (k.name.clone(), cl, target_allowed(&k.name, cl, &k.version));
        let e = selected.entry(g).or_insert_with(|| k.version.clone());
        if k.version > *e {
            *e = k.version.clone();
        }
    }
    // Collect distinct (DepSpec: ModuleKey, max_compat_level) pairs that have > 1 candidate selected version.
    let mut flex_specs: Vec<((ModuleKey, i64), Vec<ModuleKey>)> = Vec::new();
    let mut seen_flex: HashSet<(ModuleKey, i64)> = HashSet::new();
    let candidates_for = |dk: &ModuleKey, max_cl: i64| -> Vec<ModuleKey> {
        let min_cl = compat_of(dk);
        let upper = if max_cl < 0 { min_cl } else { max_cl };
        if upper <= min_cl {
            let t = target_allowed(&dk.name, min_cl, &dk.version);
            let v = selected
                .get(&(dk.name.clone(), min_cl, t))
                .cloned()
                .unwrap_or_else(|| dk.version.clone());
            return vec![ModuleKey::new(dk.name.clone(), v)];
        }
        let mut cands: Vec<(i64, ModuleKey)> = Vec::new();
        for ((name, cl, targ), sel_v) in &selected {
            if name == &dk.name && *cl >= min_cl && *cl <= upper && *sel_v >= dk.version {
                if has_mvo.contains(name) && targ.is_empty() {
                    continue;
                }
                if has_mvo.contains(name) && *targ != target_allowed(name, *cl, sel_v) {
                    continue;
                }
                cands.push((*cl, ModuleKey::new(dk.name.clone(), sel_v.clone())));
            }
        }
        cands.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.version.cmp(&b.1.version)));
        cands.dedup_by(|a, b| a.1 == b.1);
        if cands.is_empty() {
            let t = target_allowed(&dk.name, min_cl, &dk.version);
            let v = selected
                .get(&(dk.name.clone(), min_cl, t))
                .cloned()
                .unwrap_or_else(|| dk.version.clone());
            vec![ModuleKey::new(dk.name.clone(), v)]
        } else {
            cands.into_iter().map(|(_, mk)| mk).collect()
        }
    };
    for m in graph.values() {
        for (rname, dk) in &m.deps {
            let max_cl = m.dep_max_compat.get(rname).copied().unwrap_or(-1);
            if max_cl >= 0 && seen_flex.insert((dk.clone(), max_cl)) {
                let cands = candidates_for(dk, max_cl);
                if cands.len() > 1 {
                    flex_specs.push(((dk.clone(), max_cl), cands));
                }
            }
        }
        for (idx, dk) in m.nodep_deps.iter().enumerate() {
            let max_cl = m.nodep_max_compat.get(idx).copied().unwrap_or(-1);
            if max_cl >= 0 && seen_flex.insert((dk.clone(), max_cl)) {
                let cands = candidates_for(dk, max_cl);
                if cands.len() > 1 {
                    flex_specs.push(((dk.clone(), max_cl), cands));
                }
            }
        }
    }
    let transform_module =
        |old: &InterimModule, choice_map: &HashMap<(ModuleKey, i64), ModuleKey>| -> InterimModule {
            let resolve_one = |dk: &ModuleKey, max_cl: i64| -> ModuleKey {
                if let Some(chosen) = choice_map.get(&(dk.clone(), max_cl)) {
                    return chosen.clone();
                }
                candidates_for(dk, max_cl)
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| dk.clone())
            };
            let mut m = old.clone();
            m.deps = old
                .deps
                .iter()
                .map(|(rname, dk)| {
                    let max_cl = old.dep_max_compat.get(rname).copied().unwrap_or(-1);
                    (rname.clone(), resolve_one(dk, max_cl))
                })
                .collect();
            m.nodep_deps = old
                .nodep_deps
                .iter()
                .enumerate()
                .map(|(idx, dk)| {
                    let max_cl = old.nodep_max_compat.get(idx).copied().unwrap_or(-1);
                    resolve_one(dk, max_cl)
                })
                .collect();
            m
        };
    let walk = |ignore_nodeps: bool,
                choice_map: &HashMap<(ModuleKey, i64), ModuleKey>|
     -> Result<OrderedMap<ModuleKey, InterimModule>, ResolveError> {
        let mut out = OrderedMap::new();
        let mut known: HashSet<ModuleKey> = HashSet::new();
        let mut seen_by_name: HashMap<String, (ModuleKey, i64)> = HashMap::new();
        let mut q: VecDeque<(ModuleKey, Option<ModuleKey>)> = VecDeque::new();
        q.push_back((ModuleKey::root(), None));
        known.insert(ModuleKey::root());
        while let Some((key, from)) = q.pop_front() {
            let Some(old) = graph.get(&key) else {
                return re(format!(
                    "Module {key} unexpectedly missing from the dependency graph"
                ));
            };
            let module = transform_module(old, choice_map);
            if !key.is_root() && !has_mvo.contains(&key.name) {
                let cl = module.compatibility_level;
                if let Some((prev_key, prev_cl)) = seen_by_name.get(&key.name) {
                    if *prev_key != key {
                        return re(format!(
                            "Found compatibility level conflict for module {}: {} (compatibility_level {}) and {} (compatibility_level {}) are both selected. Consider adding a multiple_version_override if you want to depend on multiple versions of {} simultaneously",
                            key.name, prev_key, prev_cl, key, cl, key.name
                        ));
                    }
                } else {
                    seen_by_name.insert(key.name.clone(), (key.clone(), cl));
                }
            }
            if let Some((_, ModuleOverride::MultipleVersion { versions, .. })) =
                overrides.iter().find(|(n, _)| *n == key.name)
            {
                if target_allowed(&key.name, module.compatibility_level, &key.version).is_empty() {
                    return re(format!(
                        "{} depends on {} which is not allowed by the multiple_version_override on {}, which allows only [{}]",
                        from.map(|f| f.to_string()).unwrap_or_default(),
                        key,
                        key.name,
                        versions.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ")
                    ));
                }
            }
            let mut seen: HashMap<&ModuleKey, &String> = HashMap::new();
            for (rname, dk) in &module.deps {
                if let Some(prev) = seen.insert(dk, rname) {
                    return re(format!(
                        "{} depends on {} at least twice (with repo names {} and {}). Consider adding a multiple_version_override if you want to depend on multiple versions of {} simultaneously",
                        key, dk, rname, prev, dk.name
                    ));
                }
            }
            let mut children: Vec<ModuleKey> = module.deps.iter().map(|(_, k)| k.clone()).collect();
            if !ignore_nodeps {
                children.extend(module.nodep_deps.iter().cloned());
            }
            for d in children {
                if known.insert(d.clone()) {
                    q.push_back((d, Some(key.clone())));
                }
            }
            out.insert(key, module);
        }
        Ok(out)
    };
    let mut indices = vec![0usize; flex_specs.len()];
    let mut first_err: Option<ResolveError> = None;
    let mut winning_choices: Option<HashMap<(ModuleKey, i64), ModuleKey>> = None;
    loop {
        let mut choice_map = HashMap::with_capacity(flex_specs.len());
        for (i, (spec, cands)) in flex_specs.iter().enumerate() {
            choice_map.insert(spec.clone(), cands[indices[i]].clone());
        }
        match walk(false, &choice_map) {
            Ok(_) => {
                winning_choices = Some(choice_map);
                break;
            }
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
        // Advance mixed-radix odometer (capped at reasonable search space).
        let mut pos = flex_specs.len();
        let mut advanced = false;
        while pos > 0 {
            pos -= 1;
            indices[pos] += 1;
            if indices[pos] < flex_specs[pos].1.len() {
                advanced = true;
                break;
            }
            indices[pos] = 0;
        }
        if !advanced {
            break;
        }
    }
    let choice_map = match winning_choices {
        Some(c) => c,
        None => return Err(first_err.unwrap()),
    };
    let pruned = walk(true, &choice_map)?;
    let mut unpruned = OrderedMap::new();
    for (k, m) in graph.iter() {
        unpruned.insert(k.clone(), transform_module(m, &choice_map));
    }
    Ok(Selected {
        resolved: pruned,
        unpruned,
    })
}

fn bazel_version_satisfies(cur: &str, constraint: &str) -> bool {
    // Mirrors Bazel's `BazelModuleResolutionFunction.checkBazelCompatibility`:
    // Extract `<major>.<minor>.<patch>` and treat `-pre...` as a prerelease (`-1 < 0`),
    // while `rc...` or any other suffix compares equal (`0`) to the release `<major>.<minor>.<patch>`.
    fn parse(v: &str) -> Option<(u64, u64, u64, i8)> {
        let v = v.trim();
        let bytes = v.as_bytes();
        let mut i = 0;
        let mut nums = [0u64; 3];
        for (idx, slot) in nums.iter_mut().enumerate() {
            if idx > 0 {
                if bytes.get(i) != Some(&b'.') {
                    return None;
                }
                i += 1;
            }
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i == start {
                return None;
            }
            *slot = v[start..i].parse().ok()?;
        }
        let suffix = &v[i..];
        let pre_rank = if suffix.starts_with("-pre") {
            -1i8
        } else {
            0i8
        };
        Some((nums[0], nums[1], nums[2], pre_rank))
    }
    let Some(cur_v) = parse(cur) else {
        return true;
    };
    let (op, rest) = if let Some(r) = constraint.strip_prefix(">=") {
        (">=", r)
    } else if let Some(r) = constraint.strip_prefix("<=") {
        ("<=", r)
    } else if let Some(r) = constraint.strip_prefix('>') {
        (">", r)
    } else if let Some(r) = constraint.strip_prefix('<') {
        ("<", r)
    } else if let Some(r) = constraint.strip_prefix('-') {
        ("-", r)
    } else {
        return true;
    };
    let Some(want) = parse(rest) else {
        return true;
    };
    let o = cur_v.cmp(&want);
    match op {
        ">=" => o != std::cmp::Ordering::Less,
        "<=" => o != std::cmp::Ordering::Greater,
        ">" => o == std::cmp::Ordering::Greater,
        "<" => o == std::cmp::Ordering::Less,
        "-" => o != std::cmp::Ordering::Equal,
        _ => true,
    }
}

fn is_allowed_yanked(opts: &ResolveOptions, key: &ModuleKey) -> bool {
    opts.allow_all_yanked
        || opts
            .allowed_yanked
            .as_ref()
            .is_some_and(|l| l.iter().any(|x| *x == key.to_string()))
}

pub fn resolve(opts: &ResolveOptions) -> Result<Resolution, ResolveError> {
    let _auth_guard = crate::registry::push_auth_context(
        opts.credential_helpers.clone(),
        opts.client_env.clone(),
    );
    let mut warnings = Vec::new();
    let (root, overrides, root_paths) =
        eval_root(opts, &mut warnings).map_err(|m| ResolveError {
            message: m,
            warnings: warnings.clone(),
        })?;
    for raw_url in &opts.registries {
        // `RegistryFunction` expands `%workspace%` before URI parsing and strips trailing slashes.
        let expanded = expand_registry_url(raw_url, &opts.workspace_root);
        let url = expanded.trim_end_matches('/');
        crate::registry::validate_registry_url(url).map_err(|e| ResolveError {
            message: format!("Invalid registry URL: {url}: {e}"),
            warnings: warnings.clone(),
        })?;
    }
    let mirror_map =
        parse_module_mirrors(&opts.module_mirrors, &opts.registries).map_err(|m| ResolveError {
            message: m,
            warnings: warnings.clone(),
        })?;
    let mut ctx = Ctx {
        opts,
        registries: vec![],
        hashes: FileHashes::new(),
        warnings,
        root_overrides: overrides.clone(),
        root_name: root.name.clone(),
        root_module_file_paths: root_paths,
        local_registries: HashMap::new(),
        mirror_map,
    };
    let fail = |ctx: &Ctx, e: ResolveError| ResolveError {
        message: e.message,
        warnings: ctx.warnings.clone(),
    };
    let disc = discover(&mut ctx, &root).map_err(|e| fail(&ctx, e))?;
    // verify overrides on existent modules
    let existent: HashSet<&str> = disc.graph.values().map(|m| m.name.as_str()).collect();
    let nonexistent: Vec<&str> = overrides
        .iter()
        .map(|(n, _)| n.as_str())
        .filter(|n| !existent.contains(n) && !builtin_modules().iter().any(|b| b == n))
        .collect();
    if !nonexistent.is_empty() {
        return Err(fail(
            &ctx,
            ResolveError {
                message: format!(
                    "the root module specifies overrides on nonexistent module(s): {}",
                    nonexistent.join(", ")
                ),
                warnings: vec![],
            },
        ));
    }
    let sel = select(&disc.graph, &overrides).map_err(|e| fail(&ctx, e))?;

    // yanked versions (`YankedVersionsFunction`: I/O and parse failures fail open with a warning,
    // and lockfile modes `Error` / `Update` reuse locked yanked state without re-fetching metadata.json).
    let mut selected_yanked = BTreeMap::new();
    for m in sel.resolved.values() {
        let Some(url) = &m.registry_url else { continue };
        let is_file_reg = url.starts_with("file://");
        let yanked_reason: Option<String> = if !is_file_reg
            && opts.known_hashes_mode == KnownHashesMode::Enforce
        {
            opts.locked_yanked
                .as_ref()
                .and_then(|ly| ly.get(&m.key).cloned())
        } else if !is_file_reg
            && opts.known_hashes_mode == KnownHashesMode::UseAndUpdate
            && opts.locked_yanked.is_some()
        {
            if let Some(reason) = opts
                .locked_yanked
                .as_ref()
                .and_then(|ly| ly.get(&m.key).cloned())
            {
                Some(reason)
            } else {
                let source_url = format!(
                    "{}/modules/{}/{}/source.json",
                    url.trim_end_matches('/'),
                    m.name,
                    m.version
                );
                let is_source_locked = opts
                    .known_hashes
                    .as_ref()
                    .and_then(|kh| kh.get(&source_url))
                    .is_some_and(|h| h.is_some());
                if is_source_locked {
                    None
                } else {
                    let mut hashes = FileHashes::new();
                    let y = match ctx
                        .registry_for(url)
                        .and_then(|reg| reg.yanked_versions(&m.name, &mut hashes))
                    {
                        Ok(y) => y,
                        Err(e) => {
                            ctx.warnings.push(format!(
                                "WARNING: Could not read metadata file for module {} from registry {url}: {e}",
                                m.name
                            ));
                            None
                        }
                    };
                    ctx.warnings
                        .extend(crate::registry::take_registry_warnings());
                    y.and_then(|y| y.get(&m.version).cloned())
                }
            }
        } else {
            let mut hashes = FileHashes::new();
            let y = match ctx
                .registry_for(url)
                .and_then(|reg| reg.yanked_versions(&m.name, &mut hashes))
            {
                Ok(y) => y,
                Err(e) => {
                    ctx.warnings.push(format!(
                        "WARNING: Could not read metadata file for module {} from registry {url}: {e}",
                        m.name
                    ));
                    None
                }
            };
            ctx.warnings
                .extend(crate::registry::take_registry_warnings());
            y.and_then(|y| y.get(&m.version).cloned())
        };
        if let Some(info) = yanked_reason {
            if is_allowed_yanked(opts, &m.key) {
                selected_yanked.insert(m.key.clone(), info);
            } else {
                return Err(fail(
                    &ctx,
                    ResolveError {
                        message: format!(
                            "Yanked version detected in your resolved dependency graph: {}, for the reason: {}.\nYanked versions may contain serious vulnerabilities and should not be used. To fix this, use a bazel_dep on a newer version of this module. To continue using this version, allow it using the --allow_yanked_versions flag or the BZLMOD_ALLOW_YANKED_VERSIONS env variable.",
                            m.key, info
                        ),
                        warnings: vec![],
                    },
                ));
            }
        }
    }

    // check_direct_dependencies
    if opts.check_direct_dependencies != CheckMode::Off {
        let discovered_root = disc.graph.get(&ModuleKey::root()).unwrap();
        let resolved_root = sel.resolved.get(&ModuleKey::root()).unwrap();
        let mut failure = false;
        for (rn, dk) in &discovered_root.deps {
            let resolved = resolved_root.dep(rn);
            if Some(dk) != resolved {
                let msg = format!(
                    "For repository '{}', the root module requires module version {}, but got {} in the resolved dependency graph. Please update the version in your MODULE.bazel or set --check_direct_dependencies=off",
                    rn,
                    dk,
                    resolved.map(|k| k.to_string()).unwrap_or_else(|| "null".into())
                );
                if opts.check_direct_dependencies == CheckMode::Warning {
                    ctx.warnings.push(msg);
                } else {
                    ctx.warnings.push(format!("ERROR: {msg}"));
                    failure = true;
                }
            }
        }
        if failure {
            return Err(fail(
                &ctx,
                ResolveError {
                    message: "Direct dependency check failed.".into(),
                    warnings: vec![],
                },
            ));
        }
    }
    // bazel compatibility
    if opts.bazel_compatibility_mode != CheckMode::Off && !opts.bazel_version.is_empty() {
        for m in sel.resolved.values() {
            for c in &m.bazel_compatibility {
                if !bazel_version_satisfies(&opts.bazel_version, c) {
                    let msg = format!(
                        "Bazel version {} is not compatible with module \"{}\" (bazel_compatibility: [{}])",
                        opts.bazel_version,
                        m.key,
                        m.bazel_compatibility.join(", ")
                    );
                    if opts.bazel_compatibility_mode == CheckMode::Warning {
                        ctx.warnings.push(msg);
                    } else {
                        ctx.warnings.push(format!("ERROR: {msg}"));
                        return Err(fail(
                            &ctx,
                            ResolveError {
                                message: "Bazel compatibility check failed".into(),
                                warnings: vec![],
                            },
                        ));
                    }
                }
            }
        }
    }

    // Repo specs for registry modules.
    let mut dep_graph: OrderedMap<ModuleKey, Module> = OrderedMap::new();
    let module_hashes = ctx.hashes.clone();
    for (k, m) in sel.resolved.iter() {
        let mut spec = None;
        if let Some(url) = &m.registry_url {
            let mut h = FileHashes::new();
            let s = {
                let reg = ctx.registry_for(url).map_err(|e| ResolveError {
                    message: e,
                    warnings: vec![],
                })?;
                reg.get_repo_spec(k, &module_hashes, &mut h).map_err(|e| {
                    fail(
                        &ctx,
                        ResolveError {
                            message: e,
                            warnings: vec![],
                        },
                    )
                })?
            };
            for (a, b) in h {
                ctx.hashes.insert(a, b);
            }
            spec = Some(s);
        } else if let Some((_, ModuleOverride::NonRegistry(s))) =
            overrides.iter().find(|(n, _)| *n == k.name)
        {
            if k.name != "bazel_tools" {
                spec = Some(s.clone());
            }
        }
        // maybeAppendAdditionalPatches
        if let Some((
            _,
            ModuleOverride::SingleVersion {
                patches,
                patch_cmds,
                patch_strip,
                ..
            },
        )) = overrides.iter().find(|(n, _)| *n == k.name)
        {
            if (!patches.is_empty() || !patch_cmds.is_empty() || *patch_strip != 0)
                && spec.is_some()
            {
                let s = spec.as_mut().unwrap();
                s.set_attr(
                    "patches",
                    AttrVal::List(patches.iter().map(|p| AttrVal::Str(p.clone())).collect()),
                );
                s.set_attr(
                    "patch_cmds",
                    AttrVal::List(patch_cmds.iter().map(|p| AttrVal::Str(p.clone())).collect()),
                );
                s.set_attr(
                    "patch_args",
                    AttrVal::List(vec![AttrVal::Str(format!("-p{patch_strip}"))]),
                );
            }
        }
        dep_graph.insert(
            k.clone(),
            Module {
                interim: m.clone(),
                repo_spec: spec,
            },
        );
    }

    // Canonical repo names.
    let mut name_counts: HashMap<&str, usize> = HashMap::new();
    for k in &dep_graph.keys {
        *name_counts.entry(&k.name).or_default() += 1;
    }
    let mut canonical = OrderedMap::new();
    for k in &dep_graph.keys {
        canonical.insert(
            k.canonical_repo_name(name_counts[k.name.as_str()] > 1),
            k.clone(),
        );
    }
    let key_to_canonical: HashMap<ModuleKey, String> = canonical
        .iter()
        .map(|(c, k)| (k.clone(), c.clone()))
        .collect();

    // `InterimModule` additional patches: Starlark list of labels parsed as main-repo labels.
    let (root_deps_mapping, root_owner) = dep_graph
        .get(&ModuleKey::root())
        .map(|rm| module_repo_mapping_bazel_deps_only(rm, &key_to_canonical))
        .unwrap_or_default();
    for (k, m) in dep_graph.map.iter_mut() {
        if let Some((_, ModuleOverride::SingleVersion { patches, .. })) =
            overrides.iter().find(|(n, _)| *n == k.name)
        {
            if !patches.is_empty() {
                if let Some(s) = m.repo_spec.as_mut() {
                    let parsed_patches = patches
                        .iter()
                        .map(|p| {
                            let norm = if p.starts_with('@')
                                || p.starts_with("//")
                                || p.starts_with(':')
                            {
                                p.clone()
                            } else {
                                format!(":{p}")
                            };
                            let lbl = convert_label(&norm, &root_deps_mapping, &root_owner)
                                .unwrap_or_else(|_| p.clone());
                            AttrVal::Label(lbl)
                        })
                        .collect();
                    s.set_attr("patches", AttrVal::List(parsed_patches));
                }
            }
        }
    }

    // Extension usages & unique names.
    let mut usages: Vec<(ExtensionId, ModuleKey, ExtensionUsage)> = Vec::new();
    for m in dep_graph.values() {
        let (mapping, owner) = module_repo_mapping_bazel_deps_only(m, &key_to_canonical);
        for u in &m.extension_usages {
            let bzl_file = if u.is_innate() {
                // Innate extension: fabricated, lives in the using module's repo.
                format!("@@{owner}//:MODULE.bazel")
            } else {
                convert_label(&u.bzl_file, &mapping, &owner).map_err(|e| ResolveError {
                    message: format!(
                        "invalid label for module extension found at {}: {e}",
                        u.proxies[0].location
                    ),
                    warnings: vec![],
                })?
            };
            usages.push((
                ExtensionId {
                    bzl_file,
                    name: u.extension_name.clone(),
                    isolation_key: u.isolation_key.clone(),
                },
                m.key.clone(),
                u.clone(),
            ));
        }
    }
    let mut unique: BTreeMap<ExtensionId, String> = BTreeMap::new();
    let mut taken: HashSet<String> = HashSet::new();
    let mut seen_ids: Vec<ExtensionId> = vec![];
    for (id, _, _) in &usages {
        if !seen_ids.contains(id) {
            seen_ids.push(id.clone());
        }
    }
    for id in seen_ids {
        let mut attempt = 1;
        loop {
            let cand = unique_name_candidate(&id, attempt);
            if taken.insert(cand.clone()) {
                unique.insert(id.clone(), cand);
                break;
            }
            attempt += 1;
        }
    }
    let mut res = Resolution {
        root_module_name: root.name.clone(),
        overrides: {
            let mut o = OrderedMap::new();
            for (n, v) in overrides {
                o.insert(n, v);
            }
            o
        },
        dep_graph,
        unpruned_dep_graph: sel.unpruned,
        discovered_graph: disc.graph,
        registry_file_hashes: ctx.hashes.clone(),
        selected_yanked_versions: selected_yanked,
        warnings: ctx.warnings.clone(),
        canonical_repo_names: canonical,
        extension_usages: usages,
        extension_unique_names: unique,
        repo_overrides: BTreeMap::new(),
        root_module_file_paths: ctx.root_module_file_paths.clone(),
    };
    // Repo overrides from the root module's usages.
    let root_mapping = res.repo_mapping_without_overrides(&ModuleKey::root());
    let mut ro: BTreeMap<ExtensionId, BTreeMap<String, String>> = BTreeMap::new();
    for (id, mk, u) in &res.extension_usages {
        if mk.is_root() {
            for (exported, ov) in &u.repo_overrides {
                if let Some(c) = root_mapping.get(&ov.overriding_repo_name) {
                    ro.entry(id.clone())
                        .or_default()
                        .insert(exported.clone(), c.clone());
                }
            }
        }
    }
    res.repo_overrides = ro;
    Ok(res)
}

fn canonicalize_pattern(p: &str, owner: &str, mapping: &BTreeMap<String, String>) -> String {
    if let Some(r) = p.strip_prefix("@@") {
        return format!("@@{r}");
    }
    if let Some(r) = p.strip_prefix('@') {
        let (repo, rest) = r
            .split_once("//")
            .map(|(a, b)| (a, format!("//{b}")))
            .unwrap_or((r, String::new()));
        let canon = mapping
            .get(repo)
            .cloned()
            .unwrap_or_else(|| repo.to_string());
        return format!("@@{canon}{rest}");
    }
    format!("@@{owner}{p}")
}

fn unique_name_candidate(id: &ExtensionId, attempt: usize) -> String {
    let disamb = if attempt == 1 {
        String::new()
    } else {
        attempt.to_string()
    };
    let ext_name = if id.is_innate() {
        id.name[id.name.rfind(' ').unwrap() + 1..].to_string()
    } else {
        id.name.clone()
    };
    let repo = id.bzl_repo();
    match &id.isolation_key {
        Some((mk, usage)) => format!(
            "{repo}+_{ext_name}{disamb}+{}+{}+{usage}",
            mk.name, mk.version
        ),
        None => format!("{repo}+{ext_name}{disamb}"),
    }
}

/// `Module.getRepoMappingWithBazelDepsOnly` as `(apparent -> canonical, owner)`.
fn module_repo_mapping_bazel_deps_only(
    m: &Module,
    canon: &HashMap<ModuleKey, String>,
) -> (HashMap<String, String>, String) {
    let mut map = HashMap::new();
    if m.key.is_root() {
        map.insert(String::new(), String::new());
    }
    let owner = canon.get(&m.key).cloned().unwrap_or_default();
    if !m.repo_name.is_empty() {
        map.insert(m.repo_name.clone(), owner.clone());
    }
    for (rn, dk) in &m.deps {
        if let Some(c) = canon.get(dk) {
            map.insert(rn.clone(), c.clone());
        }
    }
    (map, owner)
}

/// Converts an extension or override label (`@apparent//pkg:file.bzl`, `//pkg:file.bzl`, or `:file.bzl`)
/// to canonical `@@canonical//pkg:file.bzl` form relative to a module's mapping and owner repo.
/// `//conditions` and `//visibility` without an `@` prefix always resolve in the main repo (`Label.ABSOLUTE_PACKAGE_NAMES`).
fn convert_label(
    raw: &str,
    mapping: &HashMap<String, String>,
    owner: &str,
) -> Result<String, String> {
    if let Some(r) = raw.strip_prefix("@@") {
        return Ok(format!("@@{r}"));
    }
    if let Some(r) = raw.strip_prefix('@') {
        let (a, b) = r.split_once("//").unwrap_or((r, ""));
        return match mapping.get(a) {
            Some(c) => Ok(format!("@@{c}//{b}")),
            None => Err(format!("no repo visible as '@{a}' here")),
        };
    }
    let rest = if raw.starts_with("//") {
        raw.to_string()
    } else if let Some(t) = raw.strip_prefix(':') {
        format!("//:{t}")
    } else {
        format!("//:{raw}")
    };
    let body = rest.trim_start_matches("//");
    let pkg = body.split_once(':').map(|(p, _)| p).unwrap_or(body);
    let target_repo = if pkg == "conditions" || pkg == "visibility" {
        ""
    } else {
        owner
    };
    Ok(format!("@@{target_repo}{rest}"))
}

impl Resolution {
    /// A resolution with only the (empty) root module, for bootstrap fetches.
    pub fn empty() -> Resolution {
        let root = InterimModule {
            key: ModuleKey::root(),
            name: String::new(),
            version: Version::empty(),
            compatibility_level: 0,
            repo_name: String::new(),
            bazel_compatibility: vec![],
            toolchains_to_register: vec![],
            execution_platforms_to_register: vec![],
            deps: vec![],
            dep_max_compat: BTreeMap::new(),
            original_deps: vec![],
            nodep_deps: vec![],
            nodep_max_compat: vec![],
            registry_url: None,
            extension_usages: vec![],
            flag_aliases: vec![],
        };
        let mut dg = OrderedMap::new();
        dg.insert(
            ModuleKey::root(),
            Module {
                interim: root,
                repo_spec: None,
            },
        );
        let mut names = OrderedMap::new();
        names.insert(String::new(), ModuleKey::root());
        Resolution {
            root_module_name: String::new(),
            overrides: OrderedMap::new(),
            dep_graph: dg,
            unpruned_dep_graph: OrderedMap::new(),
            discovered_graph: OrderedMap::new(),
            registry_file_hashes: FileHashes::new(),
            selected_yanked_versions: BTreeMap::new(),
            warnings: vec![],
            canonical_repo_names: names,
            extension_usages: vec![],
            extension_unique_names: BTreeMap::new(),
            repo_overrides: BTreeMap::new(),
            root_module_file_paths: vec![],
        }
    }

    pub fn root_module(&self) -> &Module {
        self.dep_graph.get(&ModuleKey::root()).expect("root module")
    }

    pub fn canonical_name_of(&self, key: &ModuleKey) -> Option<&String> {
        self.canonical_repo_names
            .iter()
            .find(|(_, k)| *k == key)
            .map(|(c, _)| c)
    }

    fn key_to_canonical(&self) -> HashMap<ModuleKey, String> {
        self.canonical_repo_names
            .iter()
            .map(|(c, k)| (k.clone(), c.clone()))
            .collect()
    }

    /// Full repo mapping for a module: own name, deps, and repos imported from extensions
    /// (`BazelDepGraphValue.getRepositoryMapping`), as apparent -> canonical.
    pub fn repo_mapping(&self, key: &ModuleKey) -> BTreeMap<String, String> {
        self.repo_mapping_impl(key, true)
    }

    pub fn repo_mapping_without_overrides(&self, key: &ModuleKey) -> BTreeMap<String, String> {
        self.repo_mapping_impl(key, false)
    }

    /// Insertion-ordered mapping (`getRepoMappingWithBazelDepsOnly` + extension imports).
    pub fn repo_mapping_ordered(&self, key: &ModuleKey) -> Vec<(String, String)> {
        self.repo_mapping_vec(key, true)
    }

    fn repo_mapping_impl(&self, key: &ModuleKey, with_overrides: bool) -> BTreeMap<String, String> {
        self.repo_mapping_vec(key, with_overrides)
            .into_iter()
            .collect()
    }

    fn repo_mapping_vec(&self, key: &ModuleKey, with_overrides: bool) -> Vec<(String, String)> {
        let canon = self.key_to_canonical();
        let Some(m) = self.dep_graph.get(key) else {
            return vec![];
        };
        let mut out: Vec<(String, String)> = vec![];
        let put = |out: &mut Vec<(String, String)>, k: String, v: String| {
            if let Some(e) = out.iter_mut().find(|(x, _)| *x == k) {
                e.1 = v;
            } else {
                out.push((k, v));
            }
        };
        for (id, mk, u) in &self.extension_usages {
            if mk != key {
                continue;
            }
            let Some(unique) = self.extension_unique_names.get(id) else {
                continue;
            };
            for (local, exported) in u.imports() {
                let default = format!("{unique}+{exported}");
                let target = if with_overrides {
                    self.repo_overrides
                        .get(id)
                        .and_then(|o| o.get(&exported))
                        .cloned()
                        .unwrap_or(default)
                } else {
                    default
                };
                put(&mut out, local, target);
            }
        }
        if m.key.is_root() {
            put(&mut out, String::new(), String::new());
        }
        let owner = canon.get(&m.key).cloned().unwrap_or_default();
        if !m.repo_name.is_empty() {
            put(&mut out, m.repo_name.clone(), owner);
        }
        for (rn, dk) in &m.deps {
            if let Some(c) = canon.get(dk) {
                put(&mut out, rn.clone(), c.clone());
            }
        }
        out
    }

    /// The canonical repo name for the repo `exported` generated by extension `id`.
    pub fn extension_repo_name(&self, id: &ExtensionId, exported: &str) -> Option<String> {
        let unique = self.extension_unique_names.get(id)?;
        Some(
            self.repo_overrides
                .get(id)
                .and_then(|o| o.get(exported))
                .cloned()
                .unwrap_or_else(|| format!("{unique}+{exported}")),
        )
    }

    /// All usages of an extension, by module (in dep graph order).
    pub fn usages_of(&self, id: &ExtensionId) -> Vec<(&ModuleKey, &ExtensionUsage)> {
        self.extension_usages
            .iter()
            .filter(|(i, _, _)| i == id)
            .map(|(_, k, u)| (k, u))
            .collect()
    }

    pub fn extension_ids(&self) -> Vec<ExtensionId> {
        let mut out: Vec<ExtensionId> = vec![];
        for (id, _, _) in &self.extension_usages {
            if !out.contains(id) {
                out.push(id.clone());
            }
        }
        out
    }

    /// Returns all `(ExtensionId, exported_repo_name)` pairs whose `<unique>+` prefix matches
    /// `canonical`, ordered from longest `<unique>+` prefix to shortest.
    pub fn extension_candidates_for_canonical<'a>(
        &self,
        canonical: &'a str,
    ) -> Vec<(ExtensionId, &'a str)> {
        let mut candidates: Vec<(usize, ExtensionId, &'a str)> = Vec::new();
        for id in self
            .extension_ids()
            .into_iter()
            .chain(self.extension_unique_names.keys().cloned())
        {
            if candidates.iter().any(|(_, existing, _)| existing == &id) {
                continue;
            }
            let Some(unique) = self.extension_unique_names.get(&id) else {
                continue;
            };
            let Some(rest) = canonical
                .strip_prefix(unique.as_str())
                .and_then(|s| s.strip_prefix('+'))
            else {
                continue;
            };
            candidates.push((unique.len(), id, rest));
        }
        candidates.sort_by(|a, b| b.0.cmp(&a.0));
        candidates
            .into_iter()
            .map(|(_, id, rest)| (id, rest))
            .collect()
    }

    /// Finds the `(ExtensionId, exported_repo_name)` whose `<unique>+` prefix matches `canonical`,
    /// preferring the longest `<unique>+` prefix so innate `_name` repo-rule proxies or overlapping
    /// unique names cannot shadow isolated `+_name+<module>+<version>+<usage>+` extension repos.
    pub fn extension_for_canonical<'a>(
        &self,
        canonical: &'a str,
    ) -> Option<(ExtensionId, &'a str)> {
        self.extension_candidates_for_canonical(canonical)
            .into_iter()
            .next()
    }

    /// Toolchain target patterns of all modules (canonical `@@repo//...` form), in dep graph order
    /// with the root module first (`register_toolchains`).
    pub fn registered_toolchains(&self) -> Vec<String> {
        self.registered(|m| &m.toolchains_to_register)
    }

    /// Execution platform target patterns (`register_execution_platforms`).
    pub fn registered_execution_platforms(&self) -> Vec<String> {
        self.registered(|m| &m.execution_platforms_to_register)
    }

    /// `SkyframeExecutor.getFlagAliases`: `flag_alias()` entries of all modules (labels resolved
    /// with the defining module's repo mapping, canonical `@@repo//pkg:name`), plus the
    /// hard-coded Python aliases for `rules_python` modules that define none of their own.
    /// Later entries replace earlier ones with the same alias (insertion-ordered map).
    pub fn flag_aliases(&self) -> Vec<(String, String)> {
        const PY_FLAG_ALIASES: [(&str, &str); 2] = [
            (
                "build_python_zip",
                "//python/config_settings:build_python_zip",
            ),
            (
                "incompatible_default_to_explicit_init_py",
                "//python/config_settings:incompatible_default_to_explicit_init_py",
            ),
        ];
        const BAZEL_PY_FLAG_ALIASES: [(&str, &str); 2] = [
            ("python_path", "//python/config_settings:python_path"),
            (
                "experimental_python_import_all_repositories",
                "//python/config_settings:experimental_python_import_all_repositories",
            ),
        ];
        let canon = self.key_to_canonical();
        let mut out: Vec<(String, String)> = vec![];
        let put = |out: &mut Vec<(String, String)>, k: String, v: String| {
            if let Some(e) = out.iter_mut().find(|(x, _)| *x == k) {
                e.1 = v;
            } else {
                out.push((k, v));
            }
        };
        for m in self.dep_graph.values() {
            let owner = canon.get(&m.key).cloned().unwrap_or_default();
            let mapping = self.repo_mapping(&m.key);
            for (k, v) in &m.flag_aliases {
                put(
                    &mut out,
                    k.clone(),
                    canonicalize_pattern(v, &owner, &mapping),
                );
            }
            if m.name != "rules_python" || !m.flag_aliases.is_empty() {
                continue;
            }
            for (k, v) in PY_FLAG_ALIASES.iter().chain(BAZEL_PY_FLAG_ALIASES.iter()) {
                put(&mut out, (*k).to_string(), format!("@@{owner}{v}"));
            }
        }
        out
    }

    fn registered(&self, f: impl Fn(&InterimModule) -> &Vec<String>) -> Vec<String> {
        let canon = self.key_to_canonical();
        let mut out = vec![];
        for m in self.dep_graph.values() {
            let owner = canon.get(&m.key).cloned().unwrap_or_default();
            let mapping = self.repo_mapping(&m.key);
            for label in f(m) {
                out.push(canonicalize_pattern(label, &owner, &mapping));
            }
        }
        out
    }

    /// Looks up a module by canonical repo name.
    pub fn module_by_canonical(&self, name: &str) -> Option<&Module> {
        let k = self.canonical_repo_names.get(&name.to_string())?;
        self.dep_graph.get(k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::testutil::*;

    fn reg(name: &str) -> (PathBuf, String) {
        let d = tmpdir(name);
        let url = crate::file_url_for_path(&d);
        (d, url)
    }

    fn opts(ws: &Path, reg_url: &str) -> ResolveOptions {
        let mut o = ResolveOptions::new(ws);
        o.registries = vec![reg_url.to_string()];
        o
    }

    fn add(reg: &Path, name: &str, ver: &str, body: &str) {
        add_module(
            reg,
            name,
            ver,
            &format!("module(name='{name}', version='{ver}')\n{body}"),
            r#"{"type":"local_path","path":"/nonexistent"}"#,
        );
    }

    #[test]
    fn mvs_picks_highest_and_names() {
        let (r, url) = reg("res1");
        add(&r, "a", "1.0", "bazel_dep(name='c', version='1.0')");
        add(&r, "b", "1.0", "bazel_dep(name='c', version='1.2')");
        add(&r, "c", "1.0", "");
        add(&r, "c", "1.2", "");
        let ws = tmpdir("ws1");
        write(&ws, "MODULE.bazel", "module(name='root')\nbazel_dep(name='a', version='1.0')\nbazel_dep(name='b', version='1.0')\nbazel_dep(name='c', version='1.0')\n");
        let mut o = opts(&ws, &url);
        o.check_direct_dependencies = CheckMode::Off;
        let res = resolve(&o).unwrap_or_else(|e| panic!("{e}"));
        let ckey = res.dep_graph.keys.iter().find(|k| k.name == "c").unwrap();
        assert_eq!(ckey.version.to_string(), "1.2");
        assert!(res.canonical_repo_names.contains_key(&"c+".to_string()));
        assert_eq!(res.repo_mapping(&ModuleKey::root()).get("c").unwrap(), "c+");
        assert_eq!(res.repo_mapping(&ModuleKey::root()).get("").unwrap(), "");
        // unpruned/discovered graphs retain c@1.0
        assert!(res
            .discovered_graph
            .keys
            .iter()
            .any(|k| k.name == "c" && k.version.to_string() == "1.0"));
    }

    #[test]
    fn check_direct_dependencies_modes() {
        let (r, url) = reg("res2");
        add(&r, "a", "1.0", "bazel_dep(name='c', version='2.0')");
        add(&r, "c", "1.0", "");
        add(&r, "c", "2.0", "");
        let ws = tmpdir("ws2");
        write(&ws, "MODULE.bazel", "module(name='root')\nbazel_dep(name='a', version='1.0')\nbazel_dep(name='c', version='1.0')\n");
        let mut o = opts(&ws, &url);
        o.check_direct_dependencies = CheckMode::Error;
        let e = resolve(&o).err().unwrap();
        assert!(e.message.contains("Direct dependency check failed"));
        assert!(e.warnings.iter().any(|w| w.contains(
            "For repository 'c', the root module requires module version c@1.0, but got c@2.0"
        )));
        o.check_direct_dependencies = CheckMode::Warning;
        let res = resolve(&o).unwrap();
        assert!(res
            .warnings
            .iter()
            .any(|w| w.contains("For repository 'c'")));
    }

    #[test]
    fn multiple_version_override_keeps_both() {
        let (r, url) = reg("res3");
        add(&r, "a", "1.0", "bazel_dep(name='c', version='1.0')");
        add(&r, "b", "1.0", "bazel_dep(name='c', version='2.0')");
        add(&r, "c", "1.0", "");
        add(&r, "c", "2.0", "");
        let ws = tmpdir("ws3");
        write(&ws, "MODULE.bazel", "module(name='root')\nbazel_dep(name='a', version='1.0')\nbazel_dep(name='b', version='1.0')\nmultiple_version_override(module_name='c', versions=['1.0','2.0'])\n");
        let res = resolve(&opts(&ws, &url)).unwrap_or_else(|e| panic!("{e}"));
        let cs: Vec<_> = res
            .dep_graph
            .keys
            .iter()
            .filter(|k| k.name == "c")
            .collect();
        assert_eq!(cs.len(), 2);
        assert!(res.canonical_repo_names.contains_key(&"c+1.0".to_string()));
        assert!(res.canonical_repo_names.contains_key(&"c+2.0".to_string()));
    }

    #[test]
    fn single_version_override_and_local_path() {
        let (r, url) = reg("res4");
        add(&r, "a", "1.0", "");
        add(&r, "a", "1.5", "");
        add(&r, "loc", "1.0", "");
        let ws = tmpdir("ws4");
        write(
            &ws,
            "local_loc/MODULE.bazel",
            "module(name='loc', version='9.9')\nbazel_dep(name='a', version='1.5')\n",
        );
        write(&ws, "MODULE.bazel", "module(name='root')\nbazel_dep(name='a', version='1.0')\nbazel_dep(name='loc', version='1.0')\nsingle_version_override(module_name='a', version='1.5')\nlocal_path_override(module_name='loc', path='local_loc')\n");
        let mut o = opts(&ws, &url);
        o.check_direct_dependencies = CheckMode::Off;
        let res = resolve(&o).unwrap_or_else(|e| panic!("{e}"));
        let a = res.dep_graph.keys.iter().find(|k| k.name == "a").unwrap();
        assert_eq!(a.version.to_string(), "1.5");
        let loc = res.dep_graph.keys.iter().find(|k| k.name == "loc").unwrap();
        assert!(loc.version.is_empty());
        assert_eq!(
            res.dep_graph
                .get(loc)
                .unwrap()
                .repo_spec
                .as_ref()
                .unwrap()
                .rule_name,
            "local_repository"
        );
    }

    #[test]
    fn missing_module_and_yanked() {
        let (r, url) = reg("res5");
        add(&r, "a", "1.0", "");
        write(
            &r,
            "modules/a/metadata.json",
            r#"{"yanked_versions":{"1.0":"CVE-1234"}}"#,
        );
        let ws = tmpdir("ws5");
        write(
            &ws,
            "MODULE.bazel",
            "module(name='root')\nbazel_dep(name='a', version='1.0')\n",
        );
        let e = resolve(&opts(&ws, &url)).err().unwrap();
        assert!(e.message.contains("Yanked version detected in your resolved dependency graph: a@1.0, for the reason: CVE-1234."), "{}", e.message);
        let mut o = opts(&ws, &url);
        o.allowed_yanked = Some(vec!["a@1.0".into()]);
        let res = resolve(&o).unwrap();
        assert_eq!(res.selected_yanked_versions.len(), 1);
        write(
            &ws,
            "MODULE.bazel",
            "module(name='root')\nbazel_dep(name='nope', version='1.0')\n",
        );
        let e = resolve(&opts(&ws, &url)).err().unwrap();
        assert!(
            e.message
                .contains("module nope@1.0 not found in registries"),
            "{}",
            e.message
        );
        assert!(
            e.message
                .contains("in module dependency chain <root> -> nope@1.0"),
            "{}",
            e.message
        );
    }

    #[test]
    fn registered_toolchains_are_canonicalized() {
        let (r, url) = reg("res7");
        add(&r, "a", "1.0", "register_toolchains('//tc:all', '@a//other:x')\nregister_execution_platforms('//p:plat')");
        let ws = tmpdir("ws7");
        write(&ws, "MODULE.bazel", "module(name='root')\nbazel_dep(name='a', version='1.0')\nregister_toolchains('//:mine', '@a//:t')\n");
        let mut o = opts(&ws, &url);
        o.check_direct_dependencies = CheckMode::Off;
        let res = resolve(&o).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            res.registered_toolchains(),
            vec![
                "@@//:mine",
                "@@a+//:t",
                "@@a+//tc:all",
                "@@a+//other:x",
                // bazel_tools' own registrations (MODULE.tools).
                "@@bazel_tools//tools/launcher:all",
                "@@bazel_tools//tools/test:all",
                "@@bazel_tools+winsdk_configure+local_config_winsdk//:all",
                "@@bazel_tools//tools/res:empty_rc_toolchain",
            ]
        );
        assert_eq!(res.registered_execution_platforms(), vec!["@@a+//p:plat"]);
    }

    #[test]
    fn prerelease_loses_to_release_and_extensions_named() {
        let (r, url) = reg("res6");
        add(&r, "a", "1.0-rc1", "");
        add(&r, "a", "1.0", "");
        add(&r, "b", "1.0", "bazel_dep(name='a', version='1.0-rc1')\next = use_extension('//:e.bzl','e')\nuse_repo(ext,'x')\n");
        let ws = tmpdir("ws6");
        write(&ws, "MODULE.bazel", "module(name='root')\nbazel_dep(name='a', version='1.0')\nbazel_dep(name='b', version='1.0')\next = use_extension('@b//:ext.bzl','myext')\nuse_repo(ext,'hub')\n");
        let mut o = opts(&ws, &url);
        o.check_direct_dependencies = CheckMode::Off;
        let res = resolve(&o).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            res.dep_graph
                .keys
                .iter()
                .find(|k| k.name == "a")
                .unwrap()
                .version
                .to_string(),
            "1.0"
        );
        let id = &res.extension_ids()[0];
        assert!(id.bzl_file.starts_with("@@b+//"), "{}", id.bzl_file);
        assert_eq!(res.extension_unique_names.get(id).unwrap(), "b++myext");
        assert_eq!(
            res.repo_mapping(&ModuleKey::root()).get("hub").unwrap(),
            "b++myext+hub"
        );
    }

    #[test]
    fn yanked_metadata_errors_fail_open_with_warning() {
        let (r, url) = reg("res_yank_open");
        add(&r, "a", "1.0", "");
        write(
            &r,
            "modules/a/metadata.json",
            r#"{"yanked_versions":{"not a valid version!!!":"bad"}}"#,
        );
        let ws = tmpdir("ws_yank_open");
        write(
            &ws,
            "MODULE.bazel",
            "module(name='root')\nbazel_dep(name='a', version='1.0')\n",
        );
        let res = resolve(&opts(&ws, &url)).unwrap();
        assert!(
            res.warnings
                .iter()
                .any(|w| w.contains("Could not read metadata file for module a")),
            "{:?}",
            res.warnings
        );

        // Transport/I/O error on metadata.json (directory instead of file) also fails open.
        let (r2, url2) = reg("res_yank_io");
        add(&r2, "a", "1.0", "");
        std::fs::create_dir_all(r2.join("modules/a/metadata.json")).unwrap();
        let res2 = resolve(&opts(&ws, &url2)).unwrap();
        assert!(
            res2.warnings
                .iter()
                .any(|w| w.contains("Could not read metadata file for module a")),
            "{:?}",
            res2.warnings
        );
    }

    #[test]
    fn non_root_extension_labels_and_svo_patches_resolve_in_correct_repos() {
        // convert_label resolves repo-relative in owner, //conditions and //visibility in main repo,
        // and rejects @//... in non-root modules.
        let mut non_root_map = HashMap::new();
        non_root_map.insert("b".to_string(), "b+".to_string());
        assert_eq!(
            convert_label("//pkg:ext.bzl", &non_root_map, "b+").unwrap(),
            "@@b+//pkg:ext.bzl"
        );
        assert_eq!(
            convert_label(":ext.bzl", &non_root_map, "b+").unwrap(),
            "@@b+//:ext.bzl"
        );
        assert_eq!(
            convert_label("//conditions:default", &non_root_map, "b+").unwrap(),
            "@@//conditions:default"
        );
        assert_eq!(
            convert_label("//visibility:public", &non_root_map, "b+").unwrap(),
            "@@//visibility:public"
        );
        assert!(convert_label("@//pkg:ext.bzl", &non_root_map, "b+")
            .unwrap_err()
            .contains("no repo visible as '@'"));

        // Non-root @//pkg extension usage fails with no repo visible as '@'.
        let (r, url) = reg("res_nr_ext");
        add(
            &r,
            "b",
            "1.0",
            "ext = use_extension('@//pkg:ext.bzl', 'e')\nuse_repo(ext, 'x')\n",
        );
        let ws = tmpdir("ws_nr_ext");
        write(
            &ws,
            "MODULE.bazel",
            "module(name='root')\nbazel_dep(name='b', version='1.0')\n",
        );
        let err = resolve(&opts(&ws, &url)).unwrap_err();
        assert!(
            err.message.contains("no repo visible as '@'"),
            "{}",
            err.message
        );

        // single_version_override patches are parsed as main-repo labels (@@//:fix.patch).
        let (r2, url2) = reg("res_svo_patch");
        add(&r2, "a", "1.0", "");
        add(&r2, "a", "1.1", "");
        let ws2 = tmpdir("ws_svo_patch");
        write(
            &ws2,
            "MODULE.bazel",
            "module(name='root')\nbazel_dep(name='a', version='1.0')\nsingle_version_override(module_name='a', version='1.1', patches=['//:fix.patch'], patch_strip=1)\n",
        );
        let res = resolve(&opts(&ws2, &url2)).unwrap();
        let a_mod = res
            .dep_graph
            .get(&ModuleKey::new("a", Version::parse("1.1").unwrap()))
            .unwrap();
        let spec = a_mod.repo_spec.as_ref().unwrap();
        assert_eq!(
            spec.attr("patches"),
            Some(&AttrVal::List(vec![AttrVal::Label(
                "@@//:fix.patch".into()
            )]))
        );
    }

    #[test]
    fn bazel_compatibility_handles_prerelease_and_rc_suffixes() {
        assert!(!bazel_version_satisfies("6.4.0-pre-1", ">=6.4.0"));
        assert!(bazel_version_satisfies("5.1.4-pre.20220421.3", "<=5.1.4"));
        assert!(bazel_version_satisfies("6.4.0rc1", ">=6.4.0"));
        assert!(bazel_version_satisfies("6.4.0rc1", "<=6.4.0"));
        assert!(!bazel_version_satisfies("6.4.0rc1", "-6.4.0"));
        assert!(bazel_version_satisfies("6.4.0", "-6.4.0-pre.1"));
    }

    #[test]
    fn compatibility_levels_max_compat_and_mirrors() {
        let (r, url) = reg("res_compat");
        add_module(
            &r,
            "lib",
            "1.0",
            "module(name='lib', version='1.0', compatibility_level=1)\n",
            r#"{"type":"local_path","path":"/nonexistent"}"#,
        );
        add_module(
            &r,
            "lib",
            "2.0",
            "module(name='lib', version='2.0', compatibility_level=2)\n",
            r#"{"type":"local_path","path":"/nonexistent"}"#,
        );
        add_module(
            &r,
            "consumer",
            "1.0",
            "module(name='consumer', version='1.0')\nbazel_dep(name='lib', version='2.0')\n",
            r#"{"type":"local_path","path":"/nonexistent"}"#,
        );

        // Compatibility conflict when root wants compat 1 and consumer wants compat 2 without max_compatibility_level.
        let ws = tmpdir("ws_compat");
        write(
            &ws,
            "MODULE.bazel",
            "module(name='root')\nbazel_dep(name='lib', version='1.0')\nbazel_dep(name='consumer', version='1.0')\n",
        );
        let mut o = opts(&ws, &url);
        o.check_direct_dependencies = CheckMode::Off;
        let err = resolve(&o).unwrap_err();
        assert!(
            err.message.contains("compatibility level"),
            "{}",
            err.message
        );

        // Setting max_compatibility_level = 2 allows upgrading to lib@2.0 (compat 2).
        write(
            &ws,
            "MODULE.bazel",
            "module(name='root')\nbazel_dep(name='lib', version='1.0', max_compatibility_level=2)\nbazel_dep(name='consumer', version='1.0')\n",
        );
        let res = resolve(&o).unwrap();
        let lib_key = res.dep_graph.keys.iter().find(|k| k.name == "lib").unwrap();
        assert_eq!(lib_key.version.to_string(), "2.0");

        // Prerelease-aware bazel_version_satisfies and module_mirrors parsing.
        assert!(!bazel_version_satisfies("9.0.0-pre.1", ">=9.0.0"));
        assert!(bazel_version_satisfies("9.0.0", ">=9.0.0-rc1"));
        let mm = parse_module_mirrors(
            &[
                "https://bcr.bazel.build=https://mirror1.example.com,https://mirror2.example.com"
                    .into(),
                "https://default-mirror.example.com".into(),
            ],
            &[
                "https://bcr.bazel.build".into(),
                "https://other.example.com".into(),
            ],
        )
        .unwrap();
        assert_eq!(
            mm.get("").unwrap(),
            &vec!["https://default-mirror.example.com".to_string()]
        );
        assert_eq!(
            mm.get("https://bcr.bazel.build").unwrap(),
            &vec![
                "https://mirror1.example.com".to_string(),
                "https://mirror2.example.com".to_string()
            ]
        );
        assert_eq!(
            crate::registry::vendored_registry_path(
                Path::new("/v"),
                "https://BCR.Bazel.Build/modules/foo/1.0/MODULE.bazel"
            ),
            Some(PathBuf::from(
                "/v/_registries/bcr.bazel.build/modules/foo/1.0/MODULE.bazel"
            ))
        );
    }
}
