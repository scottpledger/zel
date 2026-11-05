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

//! Skyframe Incremental Evaluation Engine (`SkyKey`, `SkyValue`, `SkyFunction`, `InMemoryMemoizingEvaluator`).
//! Mirrors `src/main/java/com/google/devtools/build/skyframe` and `build/lib/skyframe`.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use zel_analysis::{BuildConfigurationKey, ConfiguredTarget};
use zel_cmdline::{Label, PackageIdentifier};
use zel_packages::{Package, Target};
use zel_rules::analyze_target;
use zel_vfs::{FileSystem, PathFragment, RootedPath};

pub mod cycles;
pub mod diff;
pub mod engine;
pub mod executor;
pub mod recording_fs;

pub use cycles::{CycleInfo, CyclesReporter};
pub use executor::{loading_signature, SkyframeExecutor, SyncReport};
pub use engine::{
    Environment, ErrorInfo, EvaluationOptions, EvaluationResult, EvaluationStats,
    InMemoryMemoizingEvaluator, Version,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SkyFunctionName(pub &'static str);

impl SkyFunctionName {
    pub const FILE_STATE: Self = Self("FILE_STATE");
    pub const FILE: Self = Self("FILE");
    pub const DIRECTORY_LISTING: Self = Self("DIRECTORY_LISTING");
    pub const PACKAGE_LOOKUP: Self = Self("PACKAGE_LOOKUP");
    pub const BZL_LOAD: Self = Self("BZL_LOAD");
    pub const PACKAGE: Self = Self("PACKAGE");
    pub const PACKAGE_LOAD: Self = Self("PACKAGE_LOAD");
    pub const CONFIGURED_TARGET: Self = Self("CONFIGURED_TARGET");
    pub const ACTION_EXECUTION: Self = Self("ACTION_EXECUTION");
    pub const PRECOMPUTED: Self = Self("PRECOMPUTED");
}

impl fmt::Display for SkyFunctionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Canonical `SkyKey` representation across core Bazel SkyFunctions.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SkyKey {
    FileState(RootedPath),
    DirectoryListingState(RootedPath),
    PackageLookup(PackageIdentifier),
    Package(PackageIdentifier),
    /// Loads a package through the server's `PackageLoader`, yielding the loader's exact
    /// `Result<Package, PackageLoadError>` (see [`PackageLoadValue`]).
    PackageLoad(PackageIdentifier),
    ConfiguredTarget {
        label: Label,
        config_hash: String,
    },
    Precomputed(String),
    /// Execution of one action, identified by its primary output exec path.
    ActionExecution(String),
    Custom {
        function_name: SkyFunctionName,
        argument: String,
    },
}

impl SkyKey {
    pub fn function_name(&self) -> SkyFunctionName {
        match self {
            Self::FileState(_) => SkyFunctionName::FILE_STATE,
            Self::DirectoryListingState(_) => SkyFunctionName::DIRECTORY_LISTING,
            Self::ActionExecution(_) => SkyFunctionName::ACTION_EXECUTION,
            Self::PackageLookup(_) => SkyFunctionName::PACKAGE_LOOKUP,
            Self::Package(_) => SkyFunctionName::PACKAGE,
            Self::PackageLoad(_) => SkyFunctionName::PACKAGE_LOAD,
            Self::ConfiguredTarget { .. } => SkyFunctionName::CONFIGURED_TARGET,
            Self::Precomputed(_) => SkyFunctionName::PRECOMPUTED,
            Self::Custom { function_name, .. } => *function_name,
        }
    }
}

impl fmt::Display for SkyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileState(rp) => write!(f, "FILE_STATE:[{}]", rp.as_path()),
            Self::DirectoryListingState(rp) => write!(f, "DIRECTORY_LISTING_STATE:[{}]", rp.as_path()),
            Self::ActionExecution(id) => write!(f, "ACTION_EXECUTION:[{id}]"),
            Self::PackageLookup(pkg) => write!(f, "PACKAGE_LOOKUP:[{pkg}]"),
            Self::Package(pkg) => write!(f, "PACKAGE:[{pkg}]"),
            Self::PackageLoad(pkg) => write!(f, "PACKAGE_LOAD:[{pkg}]"),
            Self::ConfiguredTarget { label, config_hash } => {
                write!(f, "CONFIGURED_TARGET:[{label}@{config_hash}]")
            }
            Self::Precomputed(name) => write!(f, "PRECOMPUTED:[{name}]"),
            Self::Custom {
                function_name,
                argument,
            } => write!(f, "{function_name}:[{argument}]"),
        }
    }
}

/// Trait implemented by all Skyframe values.
pub trait SkyValue: Any + fmt::Debug + Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn eq_value(&self, other: &dyn SkyValue) -> bool;
}

/// `FileStateValue`: the observable state of one path (not following symlinks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStateValue {
    /// `mtime_ms` is recorded for stat-based diff detection but does not take part in value
    /// equality: a touched-but-identical file is a pruned change.
    RegularFile { size: u64, digest: Vec<u8>, mtime_ms: u64, observed_ms: u64 },
    Directory,
    Symlink { target: PathFragment },
    NonExistent,
}

impl SkyValue for FileStateValue {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn eq_value(&self, other: &dyn SkyValue) -> bool {
        match (self, other.as_any().downcast_ref::<Self>()) {
            (
                Self::RegularFile { size: s1, digest: d1, .. },
                Some(Self::RegularFile { size: s2, digest: d2, .. }),
            ) => s1 == s2 && d1 == d2,
            (a, Some(b)) => a == b,
            _ => false,
        }
    }
}

/// Entry kind in a [`DirectoryListingStateValue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Unknown,
}

/// `DirectoryListingStateValue`: sorted names and kinds of a directory's entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryListingStateValue {
    pub exists: bool,
    pub entries: Vec<(String, EntryKind)>,
}

impl SkyValue for DirectoryListingStateValue {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn eq_value(&self, other: &dyn SkyValue) -> bool {
        other.as_any().downcast_ref::<Self>() == Some(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageLookupValue {
    pub package_exists: bool,
    pub build_file_path: Option<RootedPath>,
}

impl SkyValue for PackageLookupValue {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn eq_value(&self, other: &dyn SkyValue) -> bool {
        other.as_any().downcast_ref::<Self>() == Some(self)
    }
}

/// Represents a loaded target inside a `PackageValue`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleTarget {
    pub label: Label,
    pub rule_class: String,
    pub srcs: Vec<String>,
    pub deps: Vec<String>,
    pub cmd: Option<String>,
    pub outs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageValue {
    pub package_id: PackageIdentifier,
    pub build_file: RootedPath,
    pub targets: BTreeMap<String, RuleTarget>,
    pub package: Option<Package>,
}

impl SkyValue for PackageValue {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn eq_value(&self, other: &dyn SkyValue) -> bool {
        other.as_any().downcast_ref::<Self>() == Some(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredTargetValue {
    pub configured_target: ConfiguredTarget,
}

impl SkyValue for ConfiguredTargetValue {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn eq_value(&self, other: &dyn SkyValue) -> bool {
        other.as_any().downcast_ref::<Self>() == Some(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringSkyValue(pub String);

impl SkyValue for StringSkyValue {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn eq_value(&self, other: &dyn SkyValue) -> bool {
        other.as_any().downcast_ref::<Self>() == Some(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkyFunctionError {
    pub message: String,
    pub transient: bool,
}

impl SkyFunctionError {
    pub fn persistent(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            transient: false,
        }
    }

    pub fn transient(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            transient: true,
        }
    }
}

impl fmt::Display for SkyFunctionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Core `SkyFunction` trait matching `com.google.devtools.build.skyframe.SkyFunction`.
pub trait SkyFunction: Send + Sync {
    /// Computes the `SkyValue` for `key`. Returns `Ok(None)` when `env.values_missing()` is true.
    fn compute(
        &self,
        key: &SkyKey,
        env: &mut Environment<'_>,
    ) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError>;
}

/// Built-in `FILE_STATE` SkyFunction backed by `zel_vfs::FileSystem`. Digests are cached by
/// `(path, size, mtime)` so re-checking an unchanged file does not re-read it.
pub struct FileStateFunction {
    pub fs: Arc<dyn FileSystem>,
    digests: std::sync::Mutex<std::collections::HashMap<PathFragment, (u64, u64, Vec<u8>)>>,
}

impl FileStateFunction {
    pub fn new(fs: Arc<dyn FileSystem>) -> Self {
        Self { fs, digests: Default::default() }
    }

    /// Computes the current state of `path` (shared with the diff checker).
    pub fn compute_state(&self, full_path: &PathFragment) -> Result<FileStateValue, SkyFunctionError> {
        // Taken *before* the stat so that "racy" detection is conservative.
        let observed_ms = diff::now_ms();
        match self.fs.stat(full_path, false) {
            Ok(st) => {
                if st.is_symlink {
                    let target = self
                        .fs
                        .read_symbolic_link(full_path)
                        .map_err(|e| SkyFunctionError::transient(e.to_string()))?;
                    Ok(FileStateValue::Symlink { target })
                } else if st.is_directory {
                    Ok(FileStateValue::Directory)
                } else {
                    // A cached digest is trusted only if the file's mtime is meaningful (non-zero)
                    // and was not racy when the digest was computed.
                    let cached = self
                        .digests
                        .lock()
                        .unwrap()
                        .get(full_path)
                        .filter(|(sz, mt, _)| *sz == st.size && *mt == st.last_modified_time_ms && *mt != 0)
                        .map(|(_, _, d)| d.clone());
                    let digest = match cached {
                        Some(d) => d,
                        None => {
                            let d = self
                                .fs
                                .get_digest(full_path)
                                .map_err(|e| SkyFunctionError::transient(e.to_string()))?;
                            let racy = observed_ms.saturating_sub(st.last_modified_time_ms) < diff::RACY_WINDOW_MS;
                            if !racy {
                                self.digests
                                    .lock()
                                    .unwrap()
                                    .insert(full_path.clone(), (st.size, st.last_modified_time_ms, d.clone()));
                            }
                            d
                        }
                    };
                    Ok(FileStateValue::RegularFile {
                        size: st.size,
                        digest,
                        mtime_ms: st.last_modified_time_ms,
                        observed_ms,
                    })
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FileStateValue::NonExistent),
            Err(e) => Err(SkyFunctionError::transient(e.to_string())),
        }
    }
}

impl SkyFunction for FileStateFunction {
    fn compute(
        &self,
        key: &SkyKey,
        _env: &mut Environment<'_>,
    ) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
        let SkyKey::FileState(rooted_path) = key else {
            return Err(SkyFunctionError::persistent("Expected FileState SkyKey"));
        };
        Ok(Some(Arc::new(self.compute_state(&rooted_path.as_path())?)))
    }
}

/// Built-in `DIRECTORY_LISTING_STATE` SkyFunction (`DirectoryListingStateFunction`).
pub struct DirectoryListingStateFunction {
    pub fs: Arc<dyn FileSystem>,
}

impl DirectoryListingStateFunction {
    pub fn compute_state(fs: &dyn FileSystem, path: &PathFragment) -> Result<DirectoryListingStateValue, SkyFunctionError> {
        match fs.readdir(path) {
            Ok(entries) => {
                let mut v: Vec<(String, EntryKind)> = entries
                    .into_iter()
                    .map(|e| {
                        (
                            e.name,
                            match e.dirent_type {
                                zel_vfs::DirentType::File => EntryKind::File,
                                zel_vfs::DirentType::Directory => EntryKind::Directory,
                                zel_vfs::DirentType::Symlink => EntryKind::Symlink,
                                zel_vfs::DirentType::Unknown => EntryKind::Unknown,
                            },
                        )
                    })
                    .collect();
                v.sort();
                Ok(DirectoryListingStateValue { exists: true, entries: v })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.kind() == std::io::ErrorKind::NotADirectory => {
                Ok(DirectoryListingStateValue { exists: false, entries: vec![] })
            }
            Err(e) => Err(SkyFunctionError::transient(e.to_string())),
        }
    }
}

impl SkyFunction for DirectoryListingStateFunction {
    fn compute(
        &self,
        key: &SkyKey,
        _env: &mut Environment<'_>,
    ) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
        let SkyKey::DirectoryListingState(rp) = key else {
            return Err(SkyFunctionError::persistent("Expected DirectoryListingState SkyKey"));
        };
        Ok(Some(Arc::new(Self::compute_state(self.fs.as_ref(), &rp.as_path())?)))
    }
}

/// Built-in `PACKAGE_LOOKUP` SkyFunction that checks `BUILD.bazel` then `BUILD`.
pub struct PackageLookupFunction {
    pub workspace_root: PathFragment,
    /// Resolver of external repositories (per runtime; see [`zel_packages::lookup::RepoResolverSlot`]).
    pub repo_resolver: zel_packages::lookup::RepoResolverSlot,
}

impl SkyFunction for PackageLookupFunction {
    fn compute(
        &self,
        key: &SkyKey,
        env: &mut Environment<'_>,
    ) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
        let SkyKey::PackageLookup(pkg_id) = key else {
            return Err(SkyFunctionError::persistent(
                "Expected PackageLookup SkyKey",
            ));
        };
        let pkg_frag = pkg_id.package_fragment();
        // Packages of external repositories live under the repository's own root.
        let repo_root = if pkg_id.repository().is_main() {
            self.workspace_root.clone()
        } else {
            match self
                .repo_resolver
                .get()
                .and_then(|r| r.repo_root(pkg_id.repository()))
            {
                Some(root) => root,
                None => {
                    let repo_display = format!("@@[unknown repo '{}' requested from @@]", pkg_id.repository().name());
                    return Err(SkyFunctionError::persistent(format!(
                        "no such package '{repo_display}//{}': The repository '{repo_display}' could not be resolved: No repository visible as '@{}' from main repository",
                        pkg_frag,
                        pkg_id.repository().name()
                    )))
                }
            }
        };
        let build_bazel_rp = RootedPath::to_rooted_path(
            repo_root.clone(),
            pkg_frag.get_relative_str("BUILD.bazel"),
        )
        .map_err(SkyFunctionError::persistent)?;
        let build_rp = RootedPath::to_rooted_path(
            repo_root.clone(),
            pkg_frag.get_relative_str("BUILD"),
        )
        .map_err(SkyFunctionError::persistent)?;

        let mut vs = env.get_values(&[
            SkyKey::FileState(build_bazel_rp.clone()),
            SkyKey::FileState(build_rp.clone()),
        ]);
        if env.values_missing() {
            return Ok(None);
        }
        let v2 = vs.pop().flatten();
        let v1 = vs.pop().flatten();

        if let Some(fs1) = v1.and_then(|v| v.as_any().downcast_ref::<FileStateValue>().cloned()) {
            if matches!(fs1, FileStateValue::RegularFile { .. }) {
                return Ok(Some(Arc::new(PackageLookupValue {
                    package_exists: true,
                    build_file_path: Some(build_bazel_rp),
                })));
            }
        }
        if let Some(fs2) = v2.and_then(|v| v.as_any().downcast_ref::<FileStateValue>().cloned()) {
            if matches!(fs2, FileStateValue::RegularFile { .. }) {
                return Ok(Some(Arc::new(PackageLookupValue {
                    package_exists: true,
                    build_file_path: Some(build_rp),
                })));
            }
        }

        Ok(Some(Arc::new(PackageLookupValue {
            package_exists: false,
            build_file_path: None,
        })))
    }
}

/// Dependencies discovered by running an opaque loader under the recording file system.
struct Discovered<T> {
    result: T,
    keys: Vec<SkyKey>,
}

/// Shared helper: run `load` with recording, memoizing the outcome across function restarts so a
/// restart (needed to fetch the discovered FILE_STATE deps) does not re-run the loader.
fn load_with_recording<T: Clone>(
    pending: &std::sync::Mutex<std::collections::HashMap<SkyKey, Discovered<T>>>,
    key: &SkyKey,
    env: &mut Environment<'_>,
    load: impl FnOnce() -> T,
) -> Option<T> {
    let existing = pending.lock().unwrap().get(key).map(|d| (d.result.clone(), d.keys.clone()));
    let (result, keys) = match existing {
        Some(x) => x,
        None => {
            recording_fs::start_recording();
            let result = load();
            let rec = recording_fs::finish_recording();
            let keys = recording_fs::dependency_keys(&rec);
            pending.lock().unwrap().insert(key.clone(), Discovered { result: result.clone(), keys: keys.clone() });
            (result, keys)
        }
    };
    env.get_values(&keys);
    if env.values_missing() {
        return None;
    }
    pending.lock().unwrap().remove(key);
    Some(result)
}

/// Built-in `PACKAGE` SkyFunction that loads and evaluates a package's `BUILD` / `BUILD.bazel`
/// file. Every file read / directory listed while evaluating the BUILD file (the BUILD file,
/// loaded `.bzl` files, `glob()` directories, ...) is recorded and becomes a real dependency.
pub struct PackageFunction {
    pub fs: Arc<dyn FileSystem>,
    pending: std::sync::Mutex<std::collections::HashMap<SkyKey, Discovered<Result<Package, String>>>>,
    repo_resolver: zel_packages::lookup::RepoResolverSlot,
}

impl PackageFunction {
    pub fn new(fs: Arc<dyn FileSystem>) -> Self {
        Self::with_resolver(fs, Default::default())
    }

    /// Like [`new`](Self::new) with the per-runtime external repository resolver slot.
    pub fn with_resolver(fs: Arc<dyn FileSystem>, repo_resolver: zel_packages::lookup::RepoResolverSlot) -> Self {
        Self { fs, pending: Default::default(), repo_resolver }
    }
}

impl SkyFunction for PackageFunction {
    fn compute(
        &self,
        key: &SkyKey,
        env: &mut Environment<'_>,
    ) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
        let SkyKey::Package(pkg_id) = key else {
            return Err(SkyFunctionError::persistent("Expected Package SkyKey"));
        };
        let Some(lookup_val) = env.get_value(&SkyKey::PackageLookup(pkg_id.clone())) else {
            return Ok(None);
        };
        let lookup = lookup_val
            .as_any()
            .downcast_ref::<PackageLookupValue>()
            .ok_or_else(|| SkyFunctionError::persistent("Invalid PackageLookupValue"))?;

        if !lookup.package_exists {
            return Err(SkyFunctionError::persistent(format!(
                "no such package '{}': BUILD file not found",
                pkg_id.package_fragment()
            )));
        }
        let build_rp = lookup.build_file_path.clone().unwrap();
        let _ = env.get_value(&SkyKey::FileState(build_rp.clone()));
        if env.values_missing() {
            return Ok(None);
        }
        let recording_fs = recording_fs::RecordingFileSystem::new(Arc::clone(&self.fs));
        let outcome = load_with_recording(&self.pending, key, env, || {
            let content_bytes = recording_fs
                .read_file(&build_rp.as_path())
                .map_err(|e| e.to_string())?;
            let content = String::from_utf8_lossy(&content_bytes);
            let workspace_root = build_rp.root().clone();
            let resolver: Arc<dyn zel_packages::lookup::RepoResolver> =
                match self.repo_resolver.get() {
                    Some(r) => r,
                    None => Arc::new(zel_packages::SimpleRepoResolver::new(workspace_root)),
                };
            Package::evaluate_build_file_with(
                pkg_id.clone(),
                build_rp.as_path(),
                &content,
                Arc::clone(&recording_fs),
                resolver,
            )
        });
        let Some(outcome) = outcome else { return Ok(None) };
        let (targets, package_opt) = match outcome {
            Ok(pkg) => {
                let mut map = BTreeMap::new();
                for rule in pkg.rules() {
                    map.insert(
                        rule.label.name().to_string(),
                        RuleTarget {
                            label: rule.label.clone(),
                            rule_class: rule.rule_class.clone(),
                            srcs: rule.srcs.clone(),
                            deps: rule.deps.clone(),
                            cmd: rule.cmd.clone(),
                            outs: rule.outs.clone(),
                        },
                    );
                }
                (map, Some(pkg))
            }
            Err(e) => return Err(SkyFunctionError::transient(e)),
        };

        Ok(Some(Arc::new(PackageValue {
            package_id: pkg_id.clone(),
            build_file: build_rp,
            targets,
            package: package_opt,
        })))
    }
}

/// Value of a `PACKAGE_LOAD` node: exactly what `PackageLoader::load_package_uncached` returned
/// (successful package incl. its events, or the load error).
#[derive(Debug, Clone, PartialEq)]
pub struct PackageLoadValue(pub Result<Package, zel_packages::PackageLoadError>);

impl SkyValue for PackageLoadValue {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn eq_value(&self, other: &dyn SkyValue) -> bool {
        other.as_any().downcast_ref::<Self>() == Some(self)
    }
}

/// The loader the `PACKAGE_LOAD` function uses (it changes per command: flags such as
/// `--deleted_packages`). Installed by the executor right before evaluating.
#[derive(Default)]
pub struct PackageLoadContext {
    /// Weak: the loader's config points back at the package cache (avoids a reference cycle).
    pub loader: std::sync::Mutex<Option<std::sync::Weak<zel_packages::PackageLoader>>>,
    /// Number of times the loader actually ran (as opposed to being served from the graph).
    pub loads: std::sync::atomic::AtomicUsize,
}

/// `PACKAGE_LOAD`: runs the server's `PackageLoader` under the recording file system and records
/// every observed file/directory as a dependency. The `loading_options` precomputed node makes
/// all packages depend on the loading configuration.
pub struct PackageLoadFunction {
    pub ctx: Arc<PackageLoadContext>,
    pending: std::sync::Mutex<std::collections::HashMap<SkyKey, Discovered<PackageLoadValue>>>,
}

pub const LOADING_OPTIONS_KEY: &str = "package_loading_options";

impl PackageLoadFunction {
    pub fn new(ctx: Arc<PackageLoadContext>) -> Self {
        Self { ctx, pending: Default::default() }
    }
}

impl SkyFunction for PackageLoadFunction {
    fn compute(
        &self,
        key: &SkyKey,
        env: &mut Environment<'_>,
    ) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
        let SkyKey::PackageLoad(pkg_id) = key else {
            return Err(SkyFunctionError::persistent("Expected PackageLoad SkyKey"));
        };
        if env.get_value(&SkyKey::Precomputed(LOADING_OPTIONS_KEY.to_string())).is_none() {
            return Ok(None);
        }
        let loader = self
            .ctx
            .loader
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|w| w.upgrade())
            .ok_or_else(|| SkyFunctionError::persistent("no PackageLoader installed for PACKAGE_LOAD"))?;
        let ctx = &self.ctx;
        let out = load_with_recording(&self.pending, key, env, || {
            ctx.loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            PackageLoadValue(loader.load_package_uncached(pkg_id))
        });
        Ok(out.map(|v| Arc::new(v) as Arc<dyn SkyValue>))
    }
}

/// Produces the package loader, analysis configuration and top-level configuration used by a
/// dependency-aware [`ConfiguredTargetFunction`] (invoked once per computation so that file
/// changes are always observed).
pub type AnalysisSetup = Arc<
    dyn Fn() -> (Arc<zel_packages::PackageLoader>, zel_rules::AnalysisConfig, BuildConfigurationKey)
        + Send
        + Sync,
>;

/// Built-in `CONFIGURED_TARGET` SkyFunction that analyzes a target in a `PackageValue`.
///
/// With `analysis: Some(..)` the target is analysed by [`zel_rules::TargetResolver`] with full
/// dependency analysis (dependencies, transitions, toolchain resolution, visibility);
/// analysis errors become persistent errors carrying Bazel's message. Without it the legacy
/// leaf-rule path ([`analyze_target`]) is used.
pub struct ConfiguredTargetFunction {
    pub default_config: BuildConfigurationKey,
    pub analysis: Option<AnalysisSetup>,
}

impl ConfiguredTargetFunction {
    pub fn new(default_config: BuildConfigurationKey) -> Self {
        Self { default_config, analysis: None }
    }

    pub fn with_analysis(default_config: BuildConfigurationKey, setup: AnalysisSetup) -> Self {
        Self { default_config, analysis: Some(setup) }
    }
}

impl SkyFunction for ConfiguredTargetFunction {
    fn compute(
        &self,
        key: &SkyKey,
        env: &mut Environment<'_>,
    ) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
        let SkyKey::ConfiguredTarget { label, .. } = key else {
            return Err(SkyFunctionError::persistent(
                "Expected ConfiguredTarget SkyKey",
            ));
        };
        let pkg_key = SkyKey::Package(label.package_identifier().clone());
        let Some(pkg_val_any) = env.get_value(&pkg_key) else {
            return Ok(None);
        };
        if let Some(setup) = &self.analysis {
            let (loader, acfg, cfg) = setup();
            let resolver = zel_rules::TargetResolver::new(loader, acfg);
            let ct = resolver.analyze(label, &cfg).map_err(SkyFunctionError::persistent)?;
            return Ok(Some(Arc::new(ConfiguredTargetValue { configured_target: (*ct).clone() })));
        }
        let pkg_val = pkg_val_any
            .as_any()
            .downcast_ref::<PackageValue>()
            .ok_or_else(|| SkyFunctionError::persistent("Invalid PackageValue"))?;

        let target = if let Some(ref pkg) = pkg_val.package {
            if let Some(t) = pkg.targets.get(label.name()) {
                t.clone()
            } else {
                Target::InputFile {
                    label: label.clone(),
                }
            }
        } else if let Some(rt) = pkg_val.targets.get(label.name()) {
            Target::Rule(zel_packages::RuleInstance {
                label: rt.label.clone(),
                rule_class: rt.rule_class.clone(),
                attrs: BTreeMap::new(),
                srcs: rt.srcs.clone(),
                deps: rt.deps.clone(),
                outs: rt.outs.clone(),
                tools: Vec::new(),
                data: Vec::new(),
                visibility: Vec::new(),
                cmd: rt.cmd.clone(),
                executable: rt.rule_class.ends_with("_binary"),
                testonly: rt.rule_class.ends_with("_test"),
                extra: Default::default(),
            })
        } else {
            Target::InputFile {
                label: label.clone(),
            }
        };

        let ct =
            analyze_target(&target, &self.default_config).map_err(SkyFunctionError::persistent)?;
        Ok(Some(Arc::new(ConfiguredTargetValue {
            configured_target: ct,
        })))
    }
}

#[cfg(test)]
mod engine_tests;
#[cfg(test)]
mod tests;
