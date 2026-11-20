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
pub use engine::{
    Environment, ErrorInfo, EvaluationOptions, EvaluationResult, EvaluationStats,
    InMemoryMemoizingEvaluator, Version,
};
pub use executor::{loading_signature, SkyframeExecutor, SyncReport};

/// Whether a [`SkyFunction`] reads external/untracked state (`FunctionHermeticity.java`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FunctionHermeticity {
    Hermetic,
    NonHermetic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SkyFunctionName(pub &'static str);

impl SkyFunctionName {
    pub const FILE_STATE: Self = Self("FILE_STATE");
    pub const FILE: Self = Self("FILE");
    pub const DIRECTORY_LISTING: Self = Self("DIRECTORY_LISTING");
    pub const PACKAGE_LOOKUP: Self = Self("PACKAGE_LOOKUP");
    pub const CONTAINING_PACKAGE_LOOKUP: Self = Self("CONTAINING_PACKAGE_LOOKUP");
    pub const BZL_LOAD: Self = Self("BZL_LOAD");
    pub const PACKAGE: Self = Self("PACKAGE");
    pub const PACKAGE_LOAD: Self = Self("PACKAGE_LOAD");
    pub const PACKAGE_ERROR: Self = Self("PACKAGE_ERROR");
    pub const PACKAGE_ERROR_MESSAGE: Self = Self("PACKAGE_ERROR_MESSAGE");
    pub const CONFIGURED_TARGET: Self = Self("CONFIGURED_TARGET");
    pub const ACTION_EXECUTION: Self = Self("ACTION_EXECUTION");
    pub const REPOSITORY_DIRECTORY: Self = Self("REPOSITORY_DIRECTORY");
    pub const PRECOMPUTED: Self = Self("PRECOMPUTED");

    pub fn hermeticity(&self) -> FunctionHermeticity {
        match self.0 {
            "FILE_STATE" | "DIRECTORY_LISTING" | "ACTION_EXECUTION" => {
                FunctionHermeticity::NonHermetic
            }
            _ => FunctionHermeticity::Hermetic,
        }
    }
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
    File(RootedPath),
    DirectoryListingState(RootedPath),
    PackageLookup(PackageIdentifier),
    ContainingPackageLookup(PackageIdentifier),
    Package(PackageIdentifier),
    /// Loads a package through the server's `PackageLoader`, yielding the loader's exact
    /// `Result<Package, PackageLoadError>` (see [`PackageLoadValue`]).
    PackageLoad(PackageIdentifier),
    PackageError(PackageIdentifier),
    PackageErrorMessage(PackageIdentifier),
    ConfiguredTarget {
        label: Label,
        config_hash: String,
    },
    Precomputed(String),
    /// Fetch/materialization of an external repository directory (`RepositoryDirectoryValue`).
    RepositoryDirectory(zel_cmdline::RepositoryName),
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
            Self::File(_) => SkyFunctionName::FILE,
            Self::DirectoryListingState(_) => SkyFunctionName::DIRECTORY_LISTING,
            Self::ActionExecution(_) => SkyFunctionName::ACTION_EXECUTION,
            Self::PackageLookup(_) => SkyFunctionName::PACKAGE_LOOKUP,
            Self::ContainingPackageLookup(_) => SkyFunctionName::CONTAINING_PACKAGE_LOOKUP,
            Self::Package(_) => SkyFunctionName::PACKAGE,
            Self::PackageLoad(_) => SkyFunctionName::PACKAGE_LOAD,
            Self::PackageError(_) => SkyFunctionName::PACKAGE_ERROR,
            Self::PackageErrorMessage(_) => SkyFunctionName::PACKAGE_ERROR_MESSAGE,
            Self::ConfiguredTarget { .. } => SkyFunctionName::CONFIGURED_TARGET,
            Self::RepositoryDirectory(_) => SkyFunctionName::REPOSITORY_DIRECTORY,
            Self::Precomputed(_) => SkyFunctionName::PRECOMPUTED,
            Self::Custom { function_name, .. } => *function_name,
        }
    }

    /// `SkyKey.getCanonicalName()`: `<FUNCTION_NAME>:<arg>` with newlines replaced by `_`.
    pub fn canonical_name(&self) -> String {
        let format_rp = |rp: &RootedPath| -> String {
            format!("[{}]/[{}]", rp.root_value(), rp.root_relative_path())
        };
        let raw_arg = match self {
            Self::FileState(rp) | Self::File(rp) | Self::DirectoryListingState(rp) => format_rp(rp),
            Self::ActionExecution(id) => id.clone(),
            Self::PackageLookup(pkg)
            | Self::ContainingPackageLookup(pkg)
            | Self::Package(pkg)
            | Self::PackageLoad(pkg)
            | Self::PackageError(pkg)
            | Self::PackageErrorMessage(pkg) => pkg.to_string(),
            Self::ConfiguredTarget { label, config_hash } => format!("{label}@{config_hash}"),
            Self::Precomputed(name) => name.clone(),
            Self::RepositoryDirectory(repo) => format!("@@{}", repo.name()),
            Self::Custom { argument, .. } => argument.clone(),
        };
        format!("{}:{}", self.function_name(), raw_arg.replace('\n', "_"))
    }
}

impl fmt::Display for SkyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileState(rp) => write!(f, "FILE_STATE:[{}]", rp.as_path()),
            Self::File(rp) => write!(f, "FILE:[{}]", rp.as_path()),
            Self::DirectoryListingState(rp) => {
                write!(f, "DIRECTORY_LISTING_STATE:[{}]", rp.as_path())
            }
            Self::ActionExecution(id) => write!(f, "ACTION_EXECUTION:[{id}]"),
            Self::PackageLookup(pkg) => write!(f, "PACKAGE_LOOKUP:[{pkg}]"),
            Self::ContainingPackageLookup(pkg) => {
                write!(f, "CONTAINING_PACKAGE_LOOKUP:[{pkg}]")
            }
            Self::Package(pkg) => write!(f, "PACKAGE:[{pkg}]"),
            Self::PackageLoad(pkg) => write!(f, "PACKAGE_LOAD:[{pkg}]"),
            Self::PackageError(pkg) => write!(f, "PACKAGE_ERROR:[{pkg}]"),
            Self::PackageErrorMessage(pkg) => write!(f, "PACKAGE_ERROR_MESSAGE:[{pkg}]"),
            Self::ConfiguredTarget { label, config_hash } => {
                write!(f, "CONFIGURED_TARGET:[{label}@{config_hash}]")
            }
            Self::Precomputed(name) => write!(f, "PRECOMPUTED:[{name}]"),
            Self::RepositoryDirectory(repo) => {
                write!(f, "REPOSITORY_DIRECTORY:[@@{}]", repo.name())
            }
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
    /// `NotComparableSkyValue`: when `true`, rebuilt values never suppress downstream dirtying via
    /// change pruning.
    fn is_not_comparable(&self) -> bool {
        false
    }
}

/// Wrapper marking any `T` as a `NotComparableSkyValue` so rebuilding always increments
/// `last_changed`.
#[derive(Debug, Clone)]
pub struct NotComparableSkyValue<T: fmt::Debug + Send + Sync + 'static>(pub T);

impl<T: fmt::Debug + Send + Sync + 'static> SkyValue for NotComparableSkyValue<T> {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn eq_value(&self, _other: &dyn SkyValue) -> bool {
        false
    }
    fn is_not_comparable(&self) -> bool {
        true
    }
}

/// `RepositoryDirectoryValue`: result of fetching or materializing an external repository.
/// Implements `NotComparableSkyValue` semantics (`eq_value` always returns `false`) so any
/// re-evaluated repository directory unconditionally marks downstream Skyframe dependents dirty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepositoryDirectoryValue {
    Success {
        path: PathFragment,
        exclude_from_vendoring: bool,
    },
    Failure {
        error_msg: String,
    },
}

impl RepositoryDirectoryValue {
    pub fn is_repository_exists(&self) -> bool {
        matches!(self, Self::Success { .. })
    }
}

impl SkyValue for RepositoryDirectoryValue {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn eq_value(&self, _other: &dyn SkyValue) -> bool {
        false
    }

    fn is_not_comparable(&self) -> bool {
        true
    }
}

/// Comparison result of `FileStateValue::compare_contents`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileContentsComparison {
    Same,
    SameExceptChangeTime,
    Different,
}

/// `FileStateValue`: the observable state of one path (not following symlinks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStateValue {
    /// `mtime_ms` is recorded for stat-based diff detection but does not take part in value
    /// equality: a touched-but-identical file is a pruned change.
    RegularFile {
        size: u64,
        digest: Vec<u8>,
        mtime_ms: u64,
        observed_ms: u64,
    },
    /// Unreadable regular file whose digest could not be read (`MetadataBackedFileStateValue` / proxy).
    RegularFileProxy {
        size: u64,
        mtime_ms: u64,
        node_id: u64,
    },
    /// Special file (FIFO, socket, device) with size 0 and no digest (`SpecialFileStateValue`).
    SpecialFile {
        mtime_ms: u64,
        node_id: u64,
    },
    Directory,
    Symlink {
        target: PathFragment,
    },
    NonExistent,
}

impl FileStateValue {
    pub fn is_file(&self) -> bool {
        matches!(
            self,
            Self::RegularFile { .. } | Self::RegularFileProxy { .. } | Self::SpecialFile { .. }
        )
    }

    pub fn digest(&self) -> Option<&[u8]> {
        match self {
            Self::RegularFile { digest, .. } => Some(digest.as_slice()),
            _ => None,
        }
    }

    pub fn size(&self) -> Option<u64> {
        match self {
            Self::RegularFile { size, .. } | Self::RegularFileProxy { size, .. } => Some(*size),
            Self::SpecialFile { .. } => Some(0),
            _ => None,
        }
    }

    /// `FileStateValue.getValueFingerprint`
    pub fn value_fingerprint(&self) -> Vec<u8> {
        let mut fp = zel_util::Fingerprint::new();
        match self {
            Self::RegularFile { size, digest, .. } => {
                fp.add_u64(*size);
                fp.add_bytes(digest);
            }
            Self::RegularFileProxy {
                size,
                mtime_ms,
                node_id,
            } => {
                fp.add_u64(*size);
                fp.add_u64(*mtime_ms);
                fp.add_u64(*node_id);
            }
            Self::SpecialFile { mtime_ms, node_id } => {
                fp.add_u64(0);
                fp.add_u64(*mtime_ms);
                fp.add_u64(*node_id);
            }
            Self::Symlink { target } => {
                fp.add_str(target.path_string());
            }
            Self::Directory => {
                fp.add_str("DirectoryFileStateValue");
            }
            Self::NonExistent => {
                fp.add_str("NonexistentFileStateValue");
            }
        }
        fp.digest_and_reset().to_vec()
    }

    /// `FileStateValue.compareContents`
    pub fn compare_contents(&self, other: &Self) -> FileContentsComparison {
        match (self, other) {
            (
                Self::RegularFile {
                    size: s1,
                    digest: d1,
                    mtime_ms: m1,
                    ..
                },
                Self::RegularFile {
                    size: s2,
                    digest: d2,
                    mtime_ms: m2,
                    ..
                },
            ) => {
                if s1 == s2 && d1 == d2 {
                    if m1 == m2 {
                        FileContentsComparison::Same
                    } else {
                        FileContentsComparison::SameExceptChangeTime
                    }
                } else {
                    FileContentsComparison::Different
                }
            }
            (
                Self::RegularFileProxy {
                    size: s1,
                    mtime_ms: m1,
                    node_id: n1,
                },
                Self::RegularFileProxy {
                    size: s2,
                    mtime_ms: m2,
                    node_id: n2,
                },
            ) => {
                if s1 == s2 && m1 == m2 && n1 == n2 {
                    FileContentsComparison::Same
                } else {
                    FileContentsComparison::Different
                }
            }
            (
                Self::SpecialFile {
                    mtime_ms: m1,
                    node_id: n1,
                },
                Self::SpecialFile {
                    mtime_ms: m2,
                    node_id: n2,
                },
            ) => {
                if m1 == m2 && n1 == n2 {
                    FileContentsComparison::Same
                } else {
                    FileContentsComparison::Different
                }
            }
            (a, b) if a == b => FileContentsComparison::Same,
            _ => FileContentsComparison::Different,
        }
    }
}

impl SkyValue for FileStateValue {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn eq_value(&self, other: &dyn SkyValue) -> bool {
        match (self, other.as_any().downcast_ref::<Self>()) {
            (
                Self::RegularFile {
                    size: s1,
                    digest: d1,
                    ..
                },
                Some(Self::RegularFile {
                    size: s2,
                    digest: d2,
                    ..
                }),
            ) => s1 == s2 && d1 == d2,
            (a, Some(b)) => a == b,
            _ => false,
        }
    }
}

/// Resolved file value after symlink traversal (`FileValue`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileValue {
    pub exists: bool,
    pub is_file: bool,
    pub is_directory: bool,
    pub is_symlink: bool,
    pub real_path: RootedPath,
    pub real_state: FileStateValue,
}

impl SkyValue for FileValue {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn eq_value(&self, other: &dyn SkyValue) -> bool {
        match other.as_any().downcast_ref::<Self>() {
            Some(o) => {
                self.exists == o.exists
                    && self.is_file == o.is_file
                    && self.is_directory == o.is_directory
                    && self.is_symlink == o.is_symlink
                    && self.real_path == o.real_path
                    && self.real_state.eq_value(&o.real_state)
            }
            None => false,
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

/// Classification of a `PackageLookupValue` outcome matching Bazel's `PackageLookupValue` subclasses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageLookupReason {
    Valid,
    NoBuildFile,
    Deleted,
    InvalidName,
    IncorrectRepo,
    MissingRepo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageLookupValue {
    pub package_exists: bool,
    pub build_file_path: Option<RootedPath>,
    pub reason: PackageLookupReason,
    pub error_message: Option<String>,
}

impl PackageLookupValue {
    pub fn success(build_file_path: RootedPath) -> Self {
        Self {
            package_exists: true,
            build_file_path: Some(build_file_path),
            reason: PackageLookupReason::Valid,
            error_message: None,
        }
    }

    pub fn no_build_file(error_message: Option<String>) -> Self {
        Self {
            package_exists: false,
            build_file_path: None,
            reason: PackageLookupReason::NoBuildFile,
            error_message,
        }
    }

    pub fn deleted(error_message: Option<String>) -> Self {
        Self {
            package_exists: false,
            build_file_path: None,
            reason: PackageLookupReason::Deleted,
            error_message,
        }
    }

    pub fn invalid_name(error_message: String) -> Self {
        Self {
            package_exists: false,
            build_file_path: None,
            reason: PackageLookupReason::InvalidName,
            error_message: Some(error_message),
        }
    }

    pub fn missing_repo(error_message: String) -> Self {
        Self {
            package_exists: false,
            build_file_path: None,
            reason: PackageLookupReason::MissingRepo,
            error_message: Some(error_message),
        }
    }

    pub fn incorrect_repo(error_message: String) -> Self {
        Self {
            package_exists: false,
            build_file_path: None,
            reason: PackageLookupReason::IncorrectRepo,
            error_message: Some(error_message),
        }
    }
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
    pub catastrophic: bool,
}

impl SkyFunctionError {
    pub fn persistent(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            transient: false,
            catastrophic: false,
        }
    }

    pub fn transient(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            transient: true,
            catastrophic: false,
        }
    }

    pub fn catastrophic_transient(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            transient: true,
            catastrophic: true,
        }
    }

    pub fn catastrophic_persistent(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            transient: false,
            catastrophic: true,
        }
    }

    pub fn with_catastrophic(mut self, catastrophic: bool) -> Self {
        self.catastrophic = catastrophic;
        self
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
    #[allow(clippy::type_complexity)]
    digests: std::sync::Mutex<std::collections::HashMap<PathFragment, (u64, u64, Vec<u8>)>>,
}

impl FileStateFunction {
    pub fn new(fs: Arc<dyn FileSystem>) -> Self {
        Self {
            fs,
            digests: Default::default(),
        }
    }

    /// Computes the current state of `path` (shared with the diff checker).
    pub fn compute_state(
        &self,
        full_path: &PathFragment,
    ) -> Result<FileStateValue, SkyFunctionError> {
        // Taken *before* the stat so that "racy" detection is conservative.
        let observed_ms = diff::now_ms();
        match self.fs.stat(full_path, false) {
            Ok(st) => {
                if st.is_symlink {
                    let target = self.fs.read_symbolic_link(full_path).map_err(|e| {
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
                        ) {
                            SkyFunctionError::catastrophic_transient(format!(
                                "Inconsistent filesystem: {e}"
                            ))
                        } else {
                            SkyFunctionError::transient(e.to_string())
                        }
                    })?;
                    Ok(FileStateValue::Symlink { target })
                } else if st.is_directory {
                    Ok(FileStateValue::Directory)
                } else if st.is_special_file {
                    Ok(FileStateValue::SpecialFile {
                        mtime_ms: st.last_modified_time_ms,
                        node_id: st.node_id,
                    })
                } else {
                    // A cached digest is trusted only if the file's mtime is meaningful (non-zero)
                    // and was not racy when the digest was computed.
                    let cached = self
                        .digests
                        .lock()
                        .unwrap()
                        .get(full_path)
                        .filter(|(sz, mt, _)| {
                            *sz == st.size && *mt == st.last_modified_time_ms && *mt != 0
                        })
                        .map(|(_, _, d)| d.clone());
                    let digest = match cached {
                        Some(d) => d,
                        None => match self.fs.get_digest(full_path) {
                            Ok(d) => {
                                let racy = observed_ms.saturating_sub(st.last_modified_time_ms)
                                    < diff::RACY_WINDOW_MS;
                                if !racy {
                                    self.digests.lock().unwrap().insert(
                                        full_path.clone(),
                                        (st.size, st.last_modified_time_ms, d.clone()),
                                    );
                                }
                                d
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                                return Ok(FileStateValue::RegularFileProxy {
                                    size: st.size,
                                    mtime_ms: st.last_modified_time_ms,
                                    node_id: st.node_id,
                                });
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                return Err(SkyFunctionError::catastrophic_transient(format!(
                                    "Inconsistent filesystem reading {full_path}: {e}"
                                )));
                            }
                            Err(e) => return Err(SkyFunctionError::transient(e.to_string())),
                        },
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

const MAX_SYMLINK_HOPS: usize = 40;

fn re_root_preserving(
    base_root: &PathFragment,
    abs: &PathFragment,
) -> Result<RootedPath, SkyFunctionError> {
    if let Ok(rel) = abs.relative_to(base_root) {
        RootedPath::to_rooted_path(base_root.clone(), rel).map_err(SkyFunctionError::persistent)
    } else {
        crate::recording_fs::rooted(abs).ok_or_else(|| {
            SkyFunctionError::persistent(format!("Symlink target must be absolute: {abs}"))
        })
    }
}

fn resolve_file_state_inner(
    env: &mut Environment<'_>,
    base_root: &PathFragment,
    orig_abs: &PathFragment,
    target_abs: &PathFragment,
    logical_chain: &mut Vec<PathFragment>,
    depth: usize,
) -> Result<Option<(RootedPath, FileStateValue, bool)>, SkyFunctionError> {
    if depth > MAX_SYMLINK_HOPS {
        return Err(SkyFunctionError::persistent(format!(
            "Infinite symlink expansion detected while trying to find {orig_abs}"
        )));
    }
    let (from_ancestors_abs, parent_missing) = match target_abs.parent_directory() {
        Some(parent_abs) if parent_abs != *base_root && parent_abs.starts_with(base_root) => {
            let mut parent_chain = logical_chain.clone();
            let Some((real_parent_rp, parent_state, _)) = resolve_file_state_inner(
                env,
                base_root,
                orig_abs,
                &parent_abs,
                &mut parent_chain,
                depth + 1,
            )?
            else {
                return Ok(None);
            };
            let real_parent_abs = real_parent_rp.as_path();
            let joined = real_parent_abs.get_relative_str(target_abs.base_name());
            if !matches!(parent_state, FileStateValue::Directory) {
                (joined, true)
            } else {
                (joined, false)
            }
        }
        _ => (target_abs.clone(), false),
    };

    if parent_missing {
        let rp = re_root_preserving(base_root, &from_ancestors_abs)?;
        let Some(leaf_val) = env.get_value(&SkyKey::FileState(rp.clone())) else {
            return Ok(None);
        };
        if let Some(FileStateValue::NonExistent) =
            leaf_val.as_any().downcast_ref::<FileStateValue>()
        {
            return Ok(Some((rp, FileStateValue::NonExistent, false)));
        }
    }

    let cur_abs = from_ancestors_abs;
    let mut saw_symlink = false;
    let mut hops = depth;
    if logical_chain.contains(&cur_abs) {
        return Err(SkyFunctionError::persistent(format!(
            "Symlink cycle detected while trying to find {orig_abs}"
        )));
    }
    if logical_chain
        .iter()
        .any(|earlier| cur_abs != *earlier && cur_abs.starts_with(earlier))
    {
        return Err(SkyFunctionError::persistent(format!(
            "Infinite symlink expansion detected while trying to find {orig_abs}"
        )));
    }
    logical_chain.push(cur_abs.clone());

    let cur_rp = re_root_preserving(base_root, &cur_abs)?;
    let Some(v) = env.get_value(&SkyKey::FileState(cur_rp.clone())) else {
        return Ok(None);
    };
    let state = v
        .as_any()
        .downcast_ref::<FileStateValue>()
        .cloned()
        .ok_or_else(|| SkyFunctionError::persistent("Invalid FileStateValue"))?;
    match &state {
        FileStateValue::Symlink { target } => {
            saw_symlink = true;
            hops += 1;
            if hops > MAX_SYMLINK_HOPS {
                return Err(SkyFunctionError::persistent(format!(
                    "Infinite symlink expansion detected while trying to find {orig_abs}"
                )));
            }
            let next_abs = if target.is_absolute() {
                target.clone()
            } else if let Some(parent) = cur_abs.parent_directory() {
                parent.get_relative(target)
            } else {
                target.clone()
            };
            if logical_chain.contains(&next_abs) {
                return Err(SkyFunctionError::persistent(format!(
                    "Symlink cycle detected while trying to find {orig_abs}"
                )));
            }
            if logical_chain
                .iter()
                .any(|earlier| next_abs != *earlier && next_abs.starts_with(earlier))
            {
                return Err(SkyFunctionError::persistent(format!(
                    "Infinite symlink expansion detected while trying to find {orig_abs}"
                )));
            }
            let Some((resolved_rp, resolved_state, _)) =
                resolve_file_state_inner(env, base_root, orig_abs, &next_abs, logical_chain, hops)?
            else {
                return Ok(None);
            };
            Ok(Some((resolved_rp, resolved_state, saw_symlink)))
        }
        _ => Ok(Some((cur_rp, state, saw_symlink))),
    }
}

/// Resolves a `RootedPath` through `SkyKey::FileState`, following ancestor and leaf symlinks up to
/// [`MAX_SYMLINK_HOPS`] and detecting cycles and descendant infinite expansions.
fn resolve_file_state(
    env: &mut Environment<'_>,
    start_rp: &RootedPath,
) -> Result<Option<(RootedPath, FileStateValue, bool)>, SkyFunctionError> {
    let orig_abs = start_rp.as_path();
    let mut chain = Vec::new();
    resolve_file_state_inner(env, start_rp.root(), &orig_abs, &orig_abs, &mut chain, 0)
}

/// Built-in `FILE` SkyFunction (`FileFunction`), resolving symlinks over `FILE_STATE`.
pub struct FileFunction;

impl SkyFunction for FileFunction {
    fn compute(
        &self,
        key: &SkyKey,
        env: &mut Environment<'_>,
    ) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
        let SkyKey::File(rp) = key else {
            return Err(SkyFunctionError::persistent("Expected File SkyKey"));
        };
        let Some((real_path, real_state, is_symlink)) = resolve_file_state(env, rp)? else {
            return Ok(None);
        };
        let exists = !matches!(real_state, FileStateValue::NonExistent);
        let is_file = real_state.is_file();
        let is_directory = matches!(real_state, FileStateValue::Directory);
        Ok(Some(Arc::new(FileValue {
            exists,
            is_file,
            is_directory,
            is_symlink,
            real_path,
            real_state,
        })))
    }
}

/// Built-in `DIRECTORY_LISTING_STATE` SkyFunction (`DirectoryListingStateFunction`).
pub struct DirectoryListingStateFunction {
    pub fs: Arc<dyn FileSystem>,
}

impl DirectoryListingStateFunction {
    pub fn compute_state(
        fs: &dyn FileSystem,
        path: &PathFragment,
    ) -> Result<DirectoryListingStateValue, SkyFunctionError> {
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
                Ok(DirectoryListingStateValue {
                    exists: true,
                    entries: v,
                })
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    || e.kind() == std::io::ErrorKind::NotADirectory =>
            {
                Ok(DirectoryListingStateValue {
                    exists: false,
                    entries: vec![],
                })
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
            return Err(SkyFunctionError::persistent(
                "Expected DirectoryListingState SkyKey",
            ));
        };
        Ok(Some(Arc::new(Self::compute_state(
            self.fs.as_ref(),
            &rp.as_path(),
        )?)))
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
        if let Err(msg) = zel_cmdline::validator::validate_package_name(pkg_frag.path_string()) {
            return Ok(Some(Arc::new(PackageLookupValue::invalid_name(format!(
                "Invalid package name '{pkg_id}': {msg}"
            )))));
        }
        // `//external` in the main repository is a legacy WORKSPACE-only pseudo-package.
        if pkg_id.repository().is_main() && pkg_frag.path_string() == "external" {
            return Ok(Some(Arc::new(PackageLookupValue::no_build_file(Some(
                "The //external package is not available without WORKSPACE support.".to_string(),
            )))));
        }
        // Packages of external repositories live under the repository's own root.
        let repo_root = if pkg_id.repository().is_main() {
            self.workspace_root.clone()
        } else {
            match self
                .repo_resolver
                .get()
                .map(|r| r.resolve_repo_root(pkg_id.repository()))
            {
                Some(Ok(root)) => root,
                Some(Err(zel_packages::lookup::RepoRootError::FetchFailed(err))) => {
                    let short = zel_packages::lookup::short_repo_fetch_error(&err.message);
                    return Err(SkyFunctionError::transient(format!(
                        "no such package '{}//{}': {short}",
                        pkg_id.repository().canonical_form(),
                        pkg_frag,
                    )));
                }
                None | Some(Err(zel_packages::lookup::RepoRootError::NotFound)) => {
                    let repo = pkg_id.repository();
                    let dym = self
                        .repo_resolver
                        .get()
                        .map(|r| {
                            r.mapping(&zel_cmdline::RepositoryName::main())
                                .did_you_mean(repo.name())
                        })
                        .unwrap_or_default();
                    let repo_display = if repo.is_main() || !repo.is_visible() {
                        repo.canonical_form()
                    } else {
                        format!("@@[unknown repo '{}' requested from @@{dym}]", repo.name())
                    };
                    let not_vis = repo.not_visible_error_message().unwrap_or_else(|| {
                        format!(
                            "No repository visible as '@{}' from main repository{dym}",
                            repo.name()
                        )
                    });
                    return Err(SkyFunctionError::persistent(format!(
                        "no such package '{repo_display}//{}': The repository '{repo_display}' could not be resolved: {not_vis}",
                        pkg_frag,
                    )));
                }
            }
        };

        // Probe BUILD.bazel first, then BUILD only if BUILD.bazel is not a file (so changes
        // to BUILD do not invalidate lookup when BUILD.bazel already exists).
        for filename in ["BUILD.bazel", "BUILD"] {
            let candidate_rp =
                RootedPath::to_rooted_path(repo_root.clone(), pkg_frag.get_relative_str(filename))
                    .map_err(SkyFunctionError::persistent)?;
            let Some((_, state, _)) = resolve_file_state(env, &candidate_rp)? else {
                return Ok(None);
            };
            if state.is_file() {
                return Ok(Some(Arc::new(PackageLookupValue::success(candidate_rp))));
            }
        }

        Ok(Some(Arc::new(PackageLookupValue::no_build_file(Some(format!(
            "BUILD file not found in any of the following directories. Add a BUILD file to a directory to mark it as a package.\n - {}",
            if pkg_frag.is_empty() { "." } else { pkg_frag.path_string() }
        ))))))
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
    let existing = pending
        .lock()
        .unwrap()
        .get(key)
        .map(|d| (d.result.clone(), d.keys.clone()));
    let (result, keys) = match existing {
        Some(x) => x,
        None => {
            recording_fs::start_recording();
            let result = load();
            let rec = recording_fs::finish_recording();
            let keys = recording_fs::dependency_keys(&rec);
            pending.lock().unwrap().insert(
                key.clone(),
                Discovered {
                    result: result.clone(),
                    keys: keys.clone(),
                },
            );
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
    pending:
        std::sync::Mutex<std::collections::HashMap<SkyKey, Discovered<Result<Package, String>>>>,
    repo_resolver: zel_packages::lookup::RepoResolverSlot,
}

impl PackageFunction {
    pub fn new(fs: Arc<dyn FileSystem>) -> Self {
        Self::with_resolver(fs, Default::default())
    }

    /// Like [`new`](Self::new) with the per-runtime external repository resolver slot.
    pub fn with_resolver(
        fs: Arc<dyn FileSystem>,
        repo_resolver: zel_packages::lookup::RepoResolverSlot,
    ) -> Self {
        Self {
            fs,
            pending: Default::default(),
            repo_resolver,
        }
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
            let reason = match lookup.reason {
                PackageLookupReason::InvalidName => {
                    return Err(SkyFunctionError::persistent(
                        lookup.error_message.clone().unwrap_or_else(|| {
                            format!(
                                "no such package '{}': Invalid package name",
                                pkg_id.package_fragment()
                            )
                        }),
                    ));
                }
                PackageLookupReason::Deleted => {
                    "Package is considered deleted due to --deleted_packages".to_string()
                }
                _ => lookup
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "BUILD file not found".to_string()),
            };
            return Err(SkyFunctionError::persistent(format!(
                "no such package '{}': {reason}",
                pkg_id.package_fragment()
            )));
        }
        let build_rp = lookup.build_file_path.clone().ok_or_else(|| {
            SkyFunctionError::persistent(
                "PackageLookupValue marked present without build_file_path",
            )
        })?;
        let Some((_, build_state, _)) = resolve_file_state(env, &build_rp)? else {
            return Ok(None);
        };
        if !build_state.is_file() {
            return Err(SkyFunctionError::persistent(format!(
                "no such package '{}': BUILD file {} is not a file",
                pkg_id.package_fragment(),
                build_rp.as_path()
            )));
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
        let Some(outcome) = outcome else {
            return Ok(None);
        };
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
        Self {
            ctx,
            pending: Default::default(),
        }
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
        if env
            .get_value(&SkyKey::Precomputed(LOADING_OPTIONS_KEY.to_string()))
            .is_none()
        {
            return Ok(None);
        }
        let loader = self
            .ctx
            .loader
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|w| w.upgrade())
            .ok_or_else(|| {
                SkyFunctionError::persistent("no PackageLoader installed for PACKAGE_LOAD")
            })?;
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
    dyn Fn() -> (
            Arc<zel_packages::PackageLoader>,
            zel_rules::AnalysisConfig,
            BuildConfigurationKey,
        ) + Send
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
        Self {
            default_config,
            analysis: None,
        }
    }

    pub fn with_analysis(default_config: BuildConfigurationKey, setup: AnalysisSetup) -> Self {
        Self {
            default_config,
            analysis: Some(setup),
        }
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
            let ct = resolver
                .analyze(label, &cfg)
                .map_err(SkyFunctionError::persistent)?;
            return Ok(Some(Arc::new(ConfiguredTargetValue {
                configured_target: (*ct).clone(),
            })));
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

/// Result of [`ContainingPackageLookupFunction`], matching `ContainingPackageLookupValue.java`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainingPackageLookupValue {
    ContainingPackage {
        containing_package_name: PackageIdentifier,
        containing_package_root: PathFragment,
    },
    NoContainingPackage,
    DeletedPackage {
        package_id: PackageIdentifier,
        error_message: String,
    },
}

impl ContainingPackageLookupValue {
    pub const fn has_containing_package(&self) -> bool {
        matches!(self, Self::ContainingPackage { .. })
    }

    pub const fn containing_package_name(&self) -> Option<&PackageIdentifier> {
        match self {
            Self::ContainingPackage {
                containing_package_name,
                ..
            } => Some(containing_package_name),
            _ => None,
        }
    }
}

impl SkyValue for ContainingPackageLookupValue {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn eq_value(&self, other: &dyn SkyValue) -> bool {
        other.as_any().downcast_ref::<Self>() == Some(self)
    }
}

/// Built-in `CONTAINING_PACKAGE_LOOKUP` SkyFunction (`ContainingPackageLookupFunction.java`).
pub struct ContainingPackageLookupFunction;

impl SkyFunction for ContainingPackageLookupFunction {
    fn compute(
        &self,
        key: &SkyKey,
        env: &mut Environment<'_>,
    ) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
        let SkyKey::ContainingPackageLookup(pkg_id) = key else {
            return Err(SkyFunctionError::persistent(
                "Expected ContainingPackageLookup SkyKey",
            ));
        };
        let Some(lookup_any) = env.get_value(&SkyKey::PackageLookup(pkg_id.clone())) else {
            return Ok(None);
        };
        let lookup = lookup_any
            .as_any()
            .downcast_ref::<PackageLookupValue>()
            .ok_or_else(|| SkyFunctionError::persistent("Invalid PackageLookupValue"))?;

        if lookup.package_exists {
            let root = lookup
                .build_file_path
                .as_ref()
                .map(|rp| rp.root().clone())
                .unwrap_or_else(PathFragment::empty);
            return Ok(Some(Arc::new(
                ContainingPackageLookupValue::ContainingPackage {
                    containing_package_name: pkg_id.clone(),
                    containing_package_root: root,
                },
            )));
        }

        if lookup.reason == PackageLookupReason::Deleted && pkg_id.package_fragment().is_empty() {
            return Ok(Some(Arc::new(
                ContainingPackageLookupValue::DeletedPackage {
                    package_id: pkg_id.clone(),
                    error_message: lookup
                        .error_message
                        .clone()
                        .unwrap_or_else(|| "Package is considered deleted".to_string()),
                },
            )));
        }

        let Some(parent_frag) = pkg_id.package_fragment().parent_directory() else {
            return Ok(Some(Arc::new(
                ContainingPackageLookupValue::NoContainingPackage,
            )));
        };
        if pkg_id.package_fragment().is_empty() {
            return Ok(Some(Arc::new(
                ContainingPackageLookupValue::NoContainingPackage,
            )));
        }
        let parent_id =
            PackageIdentifier::create_unvalidated(pkg_id.repository().clone(), parent_frag);
        let Some(parent_val) = env.get_value(&SkyKey::ContainingPackageLookup(parent_id)) else {
            return Ok(None);
        };
        Ok(Some(parent_val))
    }
}

/// Outcome of [`PackageErrorMessageFunction`], matching `PackageErrorMessageValue.java`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageErrorMessageValue {
    NoError,
    Error,
    NoSuchPackageException(String),
}

impl SkyValue for PackageErrorMessageValue {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn eq_value(&self, other: &dyn SkyValue) -> bool {
        other.as_any().downcast_ref::<Self>() == Some(self)
    }
}

/// Built-in `PACKAGE_ERROR_MESSAGE` SkyFunction (`PackageErrorMessageFunction.java`).
pub struct PackageErrorMessageFunction;

impl SkyFunction for PackageErrorMessageFunction {
    fn compute(
        &self,
        key: &SkyKey,
        env: &mut Environment<'_>,
    ) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
        let SkyKey::PackageErrorMessage(pkg_id) = key else {
            return Err(SkyFunctionError::persistent(
                "Expected PackageErrorMessage SkyKey",
            ));
        };
        match env.get_value_or_error(&SkyKey::Package(pkg_id.clone())) {
            Ok(None) => Ok(None),
            Ok(Some(pkg_any)) => {
                let had_errors = pkg_any
                    .as_any()
                    .downcast_ref::<PackageValue>()
                    .and_then(|pv| pv.package.as_ref())
                    .map(|p| p.contains_errors)
                    .unwrap_or(false);
                let val = if had_errors {
                    PackageErrorMessageValue::Error
                } else {
                    PackageErrorMessageValue::NoError
                };
                Ok(Some(Arc::new(val)))
            }
            Err(err) => Ok(Some(Arc::new(
                PackageErrorMessageValue::NoSuchPackageException(err.message),
            ))),
        }
    }
}

/// Built-in `PACKAGE_ERROR` SkyFunction (`PackageErrorFunction.java`): fails persistently with a
/// `BuildFileContainsErrorsException`-style message when a loaded `Package` has `contains_errors`.
pub struct PackageErrorFunction;

impl SkyFunction for PackageErrorFunction {
    fn compute(
        &self,
        key: &SkyKey,
        env: &mut Environment<'_>,
    ) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
        let SkyKey::PackageError(pkg_id) = key else {
            return Err(SkyFunctionError::persistent("Expected PackageError SkyKey"));
        };
        let Some(pkg_any) = env.get_value(&SkyKey::Package(pkg_id.clone())) else {
            return Ok(None);
        };
        let had_errors = pkg_any
            .as_any()
            .downcast_ref::<PackageValue>()
            .and_then(|pv| pv.package.as_ref())
            .map(|p| p.contains_errors)
            .unwrap_or(false);
        if had_errors {
            Err(SkyFunctionError::persistent(format!(
                "Package '{}' contains errors",
                pkg_id.package_fragment()
            )))
        } else {
            Ok(Some(Arc::new(StringSkyValue(String::new()))))
        }
    }
}

#[cfg(test)]
mod engine_tests;
#[cfg(test)]
mod tests;
