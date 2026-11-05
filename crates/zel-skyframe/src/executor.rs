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

//! `SkyframeExecutor`: the persistent, server-lifetime owner of the Skyframe graph.
//!
//! One instance lives in the server. At the start of every command [`SkyframeExecutor::sync`]
//! injects the command's loading options, runs file-system diff detection and invalidates the
//! changed `FILE_STATE` / `DIRECTORY_LISTING_STATE` nodes (and, transitively, everything built
//! from them). Package loading goes through `PACKAGE_LOAD` nodes via the
//! [`zel_packages::PackageCache`] hook, so unchanged packages are not re-evaluated across
//! commands.

use crate::diff::{apply_diff, default_awareness, DiffAwareness, ModifiedFileSet};
use crate::engine::{EvaluationOptions, EvaluationResult, InMemoryMemoizingEvaluator};
use crate::recording_fs::RecordingFileSystem;
use crate::{
    DirectoryListingStateFunction, Environment, FileStateFunction, PackageFunction, PackageLoadContext,
    PackageLoadFunction, PackageLoadValue, PackageLookupFunction, SkyFunction, SkyFunctionError,
    SkyFunctionName, SkyKey, SkyValue, StringSkyValue, LOADING_OPTIONS_KEY,
};
use std::any::Any;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use zel_cmdline::PackageIdentifier;
use zel_packages::{Package, PackageCache, PackageLoadError, PackageLoader};
use zel_vfs::{FileSystem, PathFragment, RootedPath};

/// What `sync` did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncReport {
    /// Name of the diff strategy used.
    pub diff: String,
    /// Number of file-system keys reported modified.
    pub modified_keys: usize,
    /// Number of nodes dirtied (modified keys plus transitive reverse dependencies).
    pub dirtied_nodes: usize,
    /// The loading options changed since the previous command.
    pub loading_options_changed: bool,
}

pub struct SkyframeExecutor {
    pub evaluator: InMemoryMemoizingEvaluator,
    raw_fs: Arc<dyn FileSystem>,
    recording_fs: Arc<dyn FileSystem>,
    pkg_ctx: Arc<PackageLoadContext>,
    diff: Mutex<Arc<dyn DiffAwareness>>,
    last_loading_options: Mutex<Option<String>>,
}

impl SkyframeExecutor {
    pub fn new(raw_fs: Arc<dyn FileSystem>, workspace_root: PathFragment) -> Arc<Self> {
        Self::with_functions(raw_fs, workspace_root, Vec::new())
    }

    /// Like [`new`](Self::new) plus extra registered functions (e.g. `CONFIGURED_TARGET`).
    pub fn with_functions(
        raw_fs: Arc<dyn FileSystem>,
        workspace_root: PathFragment,
        extra: Vec<(SkyFunctionName, Arc<dyn SkyFunction>)>,
    ) -> Arc<Self> {
        Self::with_functions_and_resolver(raw_fs, workspace_root, extra, Default::default())
    }

    /// Like [`with_functions`](Self::with_functions); external repositories are resolved through
    /// `repo_resolver` (owned by the caller, one slot per runtime — no process-global state).
    pub fn with_functions_and_resolver(
        raw_fs: Arc<dyn FileSystem>,
        workspace_root: PathFragment,
        extra: Vec<(SkyFunctionName, Arc<dyn SkyFunction>)>,
        repo_resolver: zel_packages::lookup::RepoResolverSlot,
    ) -> Arc<Self> {
        let mut evaluator = InMemoryMemoizingEvaluator::new();
        let pkg_ctx = Arc::new(PackageLoadContext::default());
        evaluator
            .register_function(SkyFunctionName::FILE_STATE, Arc::new(FileStateFunction::new(Arc::clone(&raw_fs))))
            .register_function(
                SkyFunctionName::DIRECTORY_LISTING,
                Arc::new(DirectoryListingStateFunction { fs: Arc::clone(&raw_fs) }),
            )
            .register_function(SkyFunctionName::PACKAGE_LOOKUP, Arc::new(PackageLookupFunction { workspace_root, repo_resolver: repo_resolver.clone() }))
            .register_function(SkyFunctionName::PACKAGE, Arc::new(PackageFunction::with_resolver(Arc::clone(&raw_fs), repo_resolver)))
            .register_function(
                SkyFunctionName::PACKAGE_LOAD,
                Arc::new(PackageLoadFunction::new(Arc::clone(&pkg_ctx))),
            );
        for (name, f) in extra {
            evaluator.register_function(name, f);
        }
        Arc::new(Self {
            evaluator,
            recording_fs: RecordingFileSystem::new(Arc::clone(&raw_fs)),
            raw_fs,
            pkg_ctx,
            diff: Mutex::new(default_awareness()),
            last_loading_options: Mutex::new(None),
        })
    }

    /// Registers an additional function (e.g. `CONFIGURED_TARGET`, `ACTION_EXECUTION`).
    /// Must be called before the executor is shared.
    pub fn register(self: &mut Arc<Self>, name: SkyFunctionName, f: Arc<dyn SkyFunction>) -> bool {
        match Arc::get_mut(self) {
            Some(me) => {
                me.evaluator.register_function(name, f);
                true
            }
            None => false,
        }
    }

    /// File system for package loaders: it records observed paths so `PACKAGE_LOAD` can turn them
    /// into dependencies.
    pub fn recording_fs(&self) -> Arc<dyn FileSystem> {
        Arc::clone(&self.recording_fs)
    }

    pub fn set_diff_awareness(&self, d: Arc<dyn DiffAwareness>) {
        *self.diff.lock().unwrap() = d;
    }

    /// Per-command synchronization: injects the loading options and invalidates whatever changed
    /// on disk since the last command.
    pub fn sync(&self, loading_options: &str) -> SyncReport {
        let mut report = SyncReport::default();
        {
            let mut last = self.last_loading_options.lock().unwrap();
            report.loading_options_changed = last.as_deref() != Some(loading_options);
            *last = Some(loading_options.to_string());
        }
        self.evaluator.inject_precomputed(
            SkyKey::Precomputed(LOADING_OPTIONS_KEY.to_string()),
            Arc::new(StringSkyValue(loading_options.to_string())),
        );
        let diff = self.diff.lock().unwrap().clone();
        report.diff = diff.name().to_string();
        let modified = diff.get_diff(&self.evaluator, self.raw_fs.as_ref());
        if let ModifiedFileSet::Keys(k) = &modified {
            report.modified_keys = k.len();
        }
        report.dirtied_nodes = apply_diff(&self.evaluator, &modified);
        report
    }

    /// Number of times the package loader actually ran.
    pub fn package_loads(&self) -> usize {
        self.pkg_ctx.loads.load(Ordering::SeqCst)
    }

    /// The `PackageCache` hook to install in `LoaderConfig::package_cache`.
    pub fn package_cache(self: &Arc<Self>) -> Arc<dyn PackageCache> {
        Arc::new(SkyframePackageCache { exec: Arc::clone(self) })
    }

    /// `--discard_analysis_cache`: drops analysis and action-execution nodes.
    pub fn discard_analysis_cache(&self) -> usize {
        self.evaluator.delete_functions(&[SkyFunctionName::CONFIGURED_TARGET, SkyFunctionName::ACTION_EXECUTION])
    }

    pub fn evaluate(&self, roots: &[SkyKey], opts: &EvaluationOptions) -> EvaluationResult {
        self.evaluator.evaluate_with(roots, opts)
    }
}

struct SkyframePackageCache {
    exec: Arc<SkyframeExecutor>,
}

impl PackageCache for SkyframePackageCache {
    fn load(&self, loader: &Arc<PackageLoader>, id: &PackageIdentifier) -> Result<Package, PackageLoadError> {
        *self.exec.pkg_ctx.loader.lock().unwrap() = Some(Arc::downgrade(loader));
        let key = SkyKey::PackageLoad(id.clone());
        let opts = EvaluationOptions { keep_going: true, threads: 4, cancel: None };
        let res = self.exec.evaluator.evaluate_with(std::slice::from_ref(&key), &opts);
        match res.get::<PackageLoadValue>(&key) {
            Some(v) => v.0.clone(),
            None => {
                let msg = res
                    .errors
                    .get(&key)
                    .map(|e| e.message.clone())
                    .unwrap_or_else(|| "package loading did not complete".to_string());
                Err(PackageLoadError { message: msg, events: Vec::new() })
            }
        }
    }
}

/// Hash of the inputs that influence package loading but are not file reads of the loader
/// itself: the caller's option strings plus the content of the workspace/module root files.
pub fn loading_signature(fs: &dyn FileSystem, workspace_root: &PathFragment, extra: &[String]) -> String {
    let mut h = zel_util::Fingerprint::new();
    for e in extra {
        h.add_str(e);
    }
    for f in ["MODULE.bazel", "MODULE.bazel.lock", "WORKSPACE", "WORKSPACE.bazel", "WORKSPACE.bzlmod", "REPO.bazel", ".bazelignore"] {
        let p = workspace_root.get_relative_str(f);
        match fs.read_file(&p) {
            Ok(b) => {
                h.add_str(f);
                h.add_bytes(&b);
            }
            Err(_) => {
                h.add_str(f);
                h.add_str("<absent>");
            }
        }
    }
    h.hex_digest_and_reset()
}

// ---------------------------------------------------------------------------------------
// ACTION_EXECUTION

/// Execution backend for `ACTION_EXECUTION` nodes (`ActionExecutionFunction`).
pub trait ActionRunner: Send + Sync {
    /// Inputs of the action: source files and the ids of actions generating other inputs.
    fn inputs(&self, action_id: &str) -> Option<ActionInputs>;
    /// Runs the action; returns a digest identifying its outputs.
    fn run(&self, action_id: &str) -> Result<String, String>;
}

#[derive(Debug, Clone, Default)]
pub struct ActionInputs {
    pub sources: Vec<RootedPath>,
    pub generated: Vec<String>,
}

/// Value of an executed action: digest of its outputs (the output metadata in Bazel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionExecutionValue {
    pub output_digest: String,
}

impl SkyValue for ActionExecutionValue {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn eq_value(&self, other: &dyn SkyValue) -> bool {
        other.as_any().downcast_ref::<Self>() == Some(self)
    }
}

/// An action only re-runs when one of its inputs actually changed; if re-running yields
/// identical outputs, dependents are not re-run (change pruning).
pub struct ActionExecutionFunction {
    pub runner: Arc<dyn ActionRunner>,
}

impl SkyFunction for ActionExecutionFunction {
    fn compute(
        &self,
        key: &SkyKey,
        env: &mut Environment<'_>,
    ) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
        let SkyKey::ActionExecution(id) = key else {
            return Err(SkyFunctionError::persistent("Expected ActionExecution SkyKey"));
        };
        let inputs = self
            .runner
            .inputs(id)
            .ok_or_else(|| SkyFunctionError::persistent(format!("unknown action {id}")))?;
        let mut keys: Vec<SkyKey> = inputs.sources.iter().cloned().map(SkyKey::FileState).collect();
        keys.extend(inputs.generated.iter().cloned().map(SkyKey::ActionExecution));
        env.get_values(&keys);
        if env.values_missing() {
            return Ok(None);
        }
        let digest = self.runner.run(id).map_err(SkyFunctionError::persistent)?;
        Ok(Some(Arc::new(ActionExecutionValue { output_digest: digest })))
    }
}
