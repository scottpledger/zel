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

//! File-system change detection (`DiffAwareness` / `FileSystemValueChecker`).
//!
//! * [`StatDiffAwareness`] is the default: before each command it stats every `FILE_STATE` and
//!   `DIRECTORY_LISTING_STATE` node in the graph (multi-threaded, like Bazel's
//!   `FileSystemValueChecker`) and reports the ones that no longer match, without hashing.
//!   "Racy" files (modified within [`RACY_WINDOW_MS`] of being observed) are always reported
//!   since their mtime/size cannot prove they are unchanged; re-evaluating them is cheap thanks
//!   to change pruning.
//! * [`ExplicitDiffAwareness`] takes the modified paths from the caller (a file-watching
//!   service, `--watchfs`-style integration, or tests).
//!
//! This build has no native file-watching backend (inotify/FSEvents need FFI or crates), so
//! `--watchfs` falls back to the stat-based checker.

use crate::engine::InMemoryMemoizingEvaluator;
use crate::{
    DirectoryListingStateFunction, DirectoryListingStateValue, FileStateValue, SkyFunctionName, SkyKey,
};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use zel_vfs::{FileSystem, PathFragment};

/// Files modified this close to the time their state was observed cannot be trusted.
pub const RACY_WINDOW_MS: u64 = 2000;

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Which keys changed since the last command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModifiedFileSet {
    Nothing,
    /// Exactly these keys are dirty.
    Keys(Vec<SkyKey>),
}

pub trait DiffAwareness: Send + Sync {
    fn name(&self) -> &'static str;
    fn get_diff(&self, evaluator: &InMemoryMemoizingEvaluator, fs: &dyn FileSystem) -> ModifiedFileSet;
}

pub struct StatDiffAwareness {
    pub threads: usize,
}

impl Default for StatDiffAwareness {
    fn default() -> Self {
        Self { threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16) }
    }
}

fn is_dirty(key: &SkyKey, value: &dyn crate::SkyValue, fs: &dyn FileSystem) -> bool {
    match key {
        SkyKey::FileState(rp) => {
            let Some(old) = value.as_any().downcast_ref::<FileStateValue>() else { return true };
            let path = rp.as_path();
            let st = fs.stat(&path, false);
            match (old, st) {
                (FileStateValue::NonExistent, Err(e)) => e.kind() != std::io::ErrorKind::NotFound,
                (FileStateValue::NonExistent, Ok(_)) => true,
                (_, Err(_)) => true,
                (FileStateValue::Directory, Ok(s)) => !s.is_directory || s.is_symlink,
                (FileStateValue::Symlink { target }, Ok(s)) => {
                    !s.is_symlink || fs.read_symbolic_link(&path).map(|t| t != *target).unwrap_or(true)
                }
                (FileStateValue::RegularFile { size, mtime_ms, observed_ms, .. }, Ok(s)) => {
                    if s.is_symlink || s.is_directory {
                        return true;
                    }
                    // mtime 0 means the file system has no meaningful mtimes (in-memory FS).
                    *mtime_ms == 0
                        || s.size != *size
                        || s.last_modified_time_ms != *mtime_ms
                        || observed_ms.saturating_sub(*mtime_ms) < RACY_WINDOW_MS
                }
            }
        }
        SkyKey::DirectoryListingState(rp) => {
            let Some(old) = value.as_any().downcast_ref::<DirectoryListingStateValue>() else { return true };
            match DirectoryListingStateFunction::compute_state(fs, &rp.as_path()) {
                Ok(now) => now != *old,
                Err(_) => true,
            }
        }
        _ => false,
    }
}

impl DiffAwareness for StatDiffAwareness {
    fn name(&self) -> &'static str {
        "stat"
    }

    fn get_diff(&self, evaluator: &InMemoryMemoizingEvaluator, fs: &dyn FileSystem) -> ModifiedFileSet {
        let nodes = evaluator.snapshot_values(&[SkyFunctionName::FILE_STATE, SkyFunctionName::DIRECTORY_LISTING]);
        if nodes.is_empty() {
            return ModifiedFileSet::Nothing;
        }
        let threads = self.threads.clamp(1, nodes.len());
        let chunk = nodes.len().div_ceil(threads);
        let dirty: std::sync::Mutex<Vec<SkyKey>> = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for part in nodes.chunks(chunk) {
                let dirty = &dirty;
                s.spawn(move || {
                    let mut local = Vec::new();
                    for (k, v) in part {
                        if is_dirty(k, v.as_ref(), fs) {
                            local.push(k.clone());
                        }
                    }
                    dirty.lock().unwrap().extend(local);
                });
            }
        });
        let mut d = dirty.into_inner().unwrap();
        if d.is_empty() {
            ModifiedFileSet::Nothing
        } else {
            d.sort();
            ModifiedFileSet::Keys(d)
        }
    }
}

/// Diff provided from outside (a file-watching service).
pub struct ExplicitDiffAwareness {
    pending: std::sync::Mutex<Vec<PathFragment>>,
}

impl Default for ExplicitDiffAwareness {
    fn default() -> Self {
        Self::new()
    }
}

impl ExplicitDiffAwareness {
    pub fn new() -> Self {
        Self { pending: std::sync::Mutex::new(Vec::new()) }
    }

    pub fn notify_modified(&self, path: PathFragment) {
        self.pending.lock().unwrap().push(path);
    }
}

impl DiffAwareness for ExplicitDiffAwareness {
    fn name(&self) -> &'static str {
        "explicit"
    }

    fn get_diff(&self, evaluator: &InMemoryMemoizingEvaluator, _fs: &dyn FileSystem) -> ModifiedFileSet {
        let paths: Vec<PathFragment> = std::mem::take(&mut *self.pending.lock().unwrap());
        if paths.is_empty() {
            return ModifiedFileSet::Nothing;
        }
        // A modified path dirties its own FILE_STATE and the listing of its parent directory.
        let mut keys = Vec::new();
        let set: std::collections::HashSet<PathFragment> = paths.iter().cloned().collect();
        for (k, _) in evaluator.snapshot_values(&[SkyFunctionName::FILE_STATE, SkyFunctionName::DIRECTORY_LISTING]) {
            match &k {
                SkyKey::FileState(rp) if set.contains(&rp.as_path()) => keys.push(k.clone()),
                SkyKey::DirectoryListingState(rp) => {
                    let d = rp.as_path();
                    if paths.iter().any(|p| p.parent_directory().as_ref() == Some(&d)) {
                        keys.push(k.clone());
                    }
                }
                _ => {}
            }
        }
        if keys.is_empty() {
            ModifiedFileSet::Nothing
        } else {
            ModifiedFileSet::Keys(keys)
        }
    }
}

/// Convenience: apply a diff to the evaluator, returning the number of keys dirtied.
pub fn apply_diff(evaluator: &InMemoryMemoizingEvaluator, diff: &ModifiedFileSet) -> usize {
    match diff {
        ModifiedFileSet::Nothing => 0,
        ModifiedFileSet::Keys(k) => evaluator.invalidate(k),
    }
}

pub fn default_awareness() -> Arc<dyn DiffAwareness> {
    Arc::new(StatDiffAwareness::default())
}
