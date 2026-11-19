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

//! Bazel Spawn Runners, Local & Sandboxed Execution, and Persistent Worker Protocol.
//! Mirrors `src/main/java/com/google/devtools/build/lib/sandbox` and `worker`.

pub mod linux;
pub mod process_wrapper;
pub mod sandbox;
pub mod worker;

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use zel_proto::worker_protocol::{Input as WorkerInput, WorkRequest, WorkResponse};
use zel_util::{Clock, JavaClock};
use zel_vfs::{FileSystem, PathFragment};

pub use process_wrapper::ResourceUsage;

/// `PathFragment` wrapper whose `Ord` implements `PathFragment.HIERARCHICAL_COMPARATOR`:
/// `'/'` sorts before any other differing byte so `a/b` sorts before `a-b` and children of a
/// directory immediately follow it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HierarchicalPath(pub PathFragment);

impl Ord for HierarchicalPath {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.hierarchical_cmp(&other.0)
    }
}

impl PartialOrd for HierarchicalPath {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl From<PathFragment> for HierarchicalPath {
    fn from(p: PathFragment) -> Self {
        Self(p)
    }
}

impl From<&str> for HierarchicalPath {
    fn from(s: &str) -> Self {
        Self(PathFragment::create(s))
    }
}

/// Recursively deletes `path` without following symlinks (`Path.deleteTree`), retrying after
/// `chmod 0700` if a directory is not readable or writable. Does nothing if `path` does not exist.
pub fn delete_tree(path: &Path) -> io::Result<()> {
    let md = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if md.file_type().is_symlink() || !md.is_dir() {
        let rm = {
            #[cfg(windows)]
            {
                if md.file_type().is_symlink() {
                    std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path))
                } else {
                    std::fs::remove_file(path)
                }
            }
            #[cfg(not(windows))]
            {
                std::fs::remove_file(path)
            }
        };
        match rm {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    } else {
        let entries = match std::fs::read_dir(path) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
                }
                std::fs::read_dir(path)?
            }
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            delete_tree(&entry.path())?;
        }
        match std::fs::remove_dir(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                #[cfg(unix)]
                if let Some(parent) = path.parent() {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(pmd) = std::fs::symlink_metadata(parent) {
                        let mode = pmd.permissions().mode();
                        let _ = std::fs::set_permissions(
                            parent,
                            std::fs::Permissions::from_mode(mode | 0o700),
                        );
                    }
                }
                std::fs::remove_dir(path)
            }
            Err(e) => Err(e),
        }
    }
}

/// Deletes all children of `path` without removing `path` itself (`Path.deleteTreesBelow`).
pub fn delete_trees_below(path: &Path) -> io::Result<()> {
    let entries = match std::fs::read_dir(path) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
            }
            std::fs::read_dir(path)?
        }
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        delete_tree(&entry.path())?;
    }
    Ok(())
}

/// `TreeDeleter`: interface to execute tree deletions under different scheduling policies.
pub trait TreeDeleter: Send + Sync {
    fn delete_tree(&self, path: &Path) -> io::Result<()>;
    fn shutdown(&self) {}
}

/// Synchronous `TreeDeleter` (`SynchronousTreeDeleter`).
#[derive(Debug, Default, Clone, Copy)]
pub struct SynchronousTreeDeleter;

impl TreeDeleter for SynchronousTreeDeleter {
    fn delete_tree(&self, path: &Path) -> io::Result<()> {
        delete_tree(path)
    }
}

/// Sets executable bits (`| 0o111` on Unix) preserving existing mode bits and propagating errors.
pub fn set_executable_mode(path: &Path) -> io::Result<()> {
    let md = std::fs::metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = md.permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    {
        let mut perms = md.permissions();
        if perms.readonly() {
            perms.set_readonly(false);
            std::fs::set_permissions(path, perms)?;
        }
    }
    Ok(())
}

/// `PathMapper` (`Spawn.getPathMapper()`).
#[derive(Clone, Default)]
pub enum PathMapper {
    #[default]
    Noop,
    Prefix {
        from: PathFragment,
        to: PathFragment,
    },
    Custom(Arc<dyn Fn(&PathFragment) -> PathFragment + Send + Sync>),
}

impl std::fmt::Debug for PathMapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Noop => write!(f, "PathMapper::Noop"),
            Self::Prefix { from, to } => f
                .debug_struct("PathMapper::Prefix")
                .field("from", from)
                .field("to", to)
                .finish(),
            Self::Custom(_) => write!(f, "PathMapper::Custom(..)"),
        }
    }
}

impl PartialEq for PathMapper {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Noop, Self::Noop) => true,
            (Self::Prefix { from: f1, to: t1 }, Self::Prefix { from: f2, to: t2 }) => {
                f1 == f2 && t1 == t2
            }
            (Self::Custom(a), Self::Custom(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Eq for PathMapper {}

impl PathMapper {
    pub fn is_noop(&self) -> bool {
        matches!(self, Self::Noop)
    }

    pub fn map(&self, path: &PathFragment) -> PathFragment {
        match self {
            Self::Noop => path.clone(),
            Self::Prefix { from, to } => {
                if let Ok(rel) = path.relative_to(from) {
                    to.get_relative(&rel)
                } else {
                    path.clone()
                }
            }
            Self::Custom(f) => f(path),
        }
    }
}

/// `VirtualActionInput` (embedded tools / parameter files materialized lazily before local/sandbox exec).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtualInput {
    Bytes {
        exec_path: PathFragment,
        content: Vec<u8>,
        executable: bool,
    },
    Path {
        exec_path: PathFragment,
        source_path: PathBuf,
        executable: bool,
    },
}

impl VirtualInput {
    pub fn exec_path(&self) -> &PathFragment {
        match self {
            Self::Bytes { exec_path, .. } | Self::Path { exec_path, .. } => exec_path,
        }
    }

    pub fn bytes(&self) -> io::Result<Vec<u8>> {
        match self {
            Self::Bytes { content, .. } => Ok(content.clone()),
            Self::Path { source_path, .. } => std::fs::read(source_path),
        }
    }

    pub fn atomically_write_relative_to(&self, exec_root: &Path) -> io::Result<Vec<u8>> {
        let out_path = exec_root.join(self.exec_path().path_string());
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let (bytes, executable) = match self {
            Self::Bytes {
                content,
                executable,
                ..
            } => (content.clone(), *executable),
            Self::Path {
                source_path,
                executable,
                ..
            } => (std::fs::read(source_path)?, *executable),
        };
        let tmp_name = format!(
            ".tmp-{}-{}-{}",
            out_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("virtual"),
            std::process::id(),
            VIRTUAL_TMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let tmp_path = out_path.with_file_name(tmp_name);
        std::fs::write(&tmp_path, &bytes)?;
        if executable {
            set_executable_mode(&tmp_path)?;
            // `PathActionInput` sets executable twice on the final path too.
        }
        std::fs::rename(&tmp_path, &out_path)?;
        if executable {
            set_executable_mode(&out_path)?;
        }
        Ok(zel_util::sha256::digest(&bytes).to_vec())
    }
}

static VIRTUAL_TMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunfilesSymlinksMode {
    #[default]
    Create,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunfilesTarget {
    File {
        exec_path: PathFragment,
        is_symlink: bool,
    },
    TreeArtifact {
        exec_path: PathFragment,
        /// Parent-relative paths of declared children (`TreeArtifactValue.getChildren`).
        children: Vec<PathFragment>,
    },
    Fileset {
        exec_path: PathFragment,
        symlinks: Vec<(PathFragment, PathFragment)>,
    },
    NestedRunfilesTree,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunfilesTreeData {
    pub exec_path: PathFragment,
    pub workspace_name: String,
    pub mapping: BTreeMap<PathFragment, Option<RunfilesTarget>>,
    pub build_runfile_links: bool,
    pub symlinks_mode: RunfilesSymlinksMode,
}

impl Default for RunfilesTreeData {
    fn default() -> Self {
        Self {
            exec_path: PathFragment::empty(),
            workspace_name: String::new(),
            mapping: BTreeMap::new(),
            build_runfile_links: false,
            symlinks_mode: RunfilesSymlinksMode::Create,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesetInputData {
    pub exec_path: PathFragment,
    pub symlinks: Vec<(PathFragment, PathFragment)>,
}

impl Default for FilesetInputData {
    fn default() -> Self {
        Self {
            exec_path: PathFragment::empty(),
            symlinks: Vec::new(),
        }
    }
}

/// An expanded spawn input entry (`SpawnInputExpander`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpandedInput {
    File {
        exec_path: PathFragment,
        is_symlink: bool,
    },
    Virtual(VirtualInput),
    Empty,
    EmptyDirectory(PathFragment),
}

impl ExpandedInput {
    pub fn is_symlink(&self) -> bool {
        matches!(
            self,
            Self::File {
                is_symlink: true,
                ..
            }
        )
    }

    pub fn exec_path(&self) -> Option<&PathFragment> {
        match self {
            Self::File { exec_path, .. } => Some(exec_path),
            Self::Virtual(v) => Some(v.exec_path()),
            Self::Empty => None,
            Self::EmptyDirectory(p) => Some(p),
        }
    }
}

/// `RunfilesTreeUpdater`: per-updater permanent deduplication of runfiles tree updates.
#[derive(Default)]
pub struct RunfilesTreeUpdater {
    updated: Mutex<HashMap<PathFragment, Arc<UpdateSlot>>>,
}

struct UpdateSlot {
    done: Mutex<Option<Result<(), String>>>,
    cv: Condvar,
}

impl RunfilesTreeUpdater {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update_runfiles(&self, exec_root: &Path, trees: &[RunfilesTreeData]) -> io::Result<()> {
        for tree in trees {
            if tree.build_runfile_links {
                continue;
            }
            let slot = {
                let mut map = self.updated.lock().unwrap();
                if let Some(existing) = map.get(&tree.exec_path) {
                    Some(existing.clone())
                } else {
                    let s = Arc::new(UpdateSlot {
                        done: Mutex::new(None),
                        cv: Condvar::new(),
                    });
                    map.insert(tree.exec_path.clone(), s.clone());
                    drop(map);
                    // Guard against panics leaving waiters blocked forever.
                    struct CompleteGuard<'a> {
                        slot: &'a UpdateSlot,
                        result: Option<Result<(), String>>,
                    }
                    impl Drop for CompleteGuard<'_> {
                        fn drop(&mut self) {
                            let res = self.result.take().unwrap_or_else(|| {
                                Err("runfiles tree update panicked".to_string())
                            });
                            *self.slot.done.lock().unwrap() = Some(res);
                            self.slot.cv.notify_all();
                        }
                    }
                    let mut guard = CompleteGuard {
                        slot: &s,
                        result: None,
                    };
                    let res = update_single_runfiles_tree(exec_root, tree);
                    let stored = res.as_ref().map(|_| ()).map_err(|e| e.to_string());
                    guard.result = Some(stored);
                    drop(guard);
                    return_or_ok(res)?;
                    continue;
                }
            };
            if let Some(s) = slot {
                let mut done = s.done.lock().unwrap();
                while done.is_none() {
                    done = s.cv.wait(done).unwrap();
                }
                if let Some(Err(msg)) = &*done {
                    return Err(io::Error::new(io::ErrorKind::Other, msg.clone()));
                }
            }
        }
        Ok(())
    }
}

fn return_or_ok(res: io::Result<()>) -> io::Result<()> {
    res
}

fn update_single_runfiles_tree(exec_root: &Path, tree: &RunfilesTreeData) -> io::Result<()> {
    let rf_str = tree.exec_path.path_string();
    let runfiles_dir = exec_root.join(rf_str);
    let input_manifest_rel = if let Some(base) = rf_str.strip_suffix(".runfiles") {
        format!("{base}.runfiles_manifest")
    } else {
        format!("{rf_str}_manifest")
    };
    let input_manifest = exec_root.join(&input_manifest_rel);
    if !input_manifest.exists() {
        return Ok(());
    }
    let output_manifest = runfiles_dir.join("MANIFEST");
    if tree.symlinks_mode == RunfilesSymlinksMode::Create
        && !std::fs::symlink_metadata(&output_manifest)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    {
        if let (Ok(out_bytes), Ok(in_bytes)) = (
            std::fs::read(&output_manifest),
            std::fs::read(&input_manifest),
        ) {
            if zel_util::sha256::digest(&out_bytes) == zel_util::sha256::digest(&in_bytes) {
                return Ok(());
            }
        }
    }
    if !runfiles_dir.exists() {
        std::fs::create_dir_all(&runfiles_dir)?;
    }
    match tree.symlinks_mode {
        RunfilesSymlinksMode::Create => {
            let mut symlinks = BTreeMap::new();
            for (k, v) in &tree.mapping {
                let target = match v {
                    None => None,
                    Some(RunfilesTarget::File {
                        exec_path,
                        is_symlink,
                    }) => {
                        let p = exec_root.join(exec_path.path_string());
                        if *is_symlink {
                            Some(PathFragment::create(
                                &std::fs::read_link(&p)?.to_string_lossy(),
                            ))
                        } else {
                            Some(PathFragment::create(&p.to_string_lossy()))
                        }
                    }
                    Some(RunfilesTarget::TreeArtifact { exec_path, .. }) => {
                        Some(PathFragment::create(
                            &exec_root.join(exec_path.path_string()).to_string_lossy(),
                        ))
                    }
                    Some(RunfilesTarget::Fileset { exec_path, .. }) => Some(PathFragment::create(
                        &exec_root.join(exec_path.path_string()).to_string_lossy(),
                    )),
                    Some(RunfilesTarget::NestedRunfilesTree) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "nested runfiles tree inside runfiles mapping",
                        ));
                    }
                };
                symlinks.insert(k.clone(), target);
            }
            sync_symlink_tree(&runfiles_dir, &symlinks)?;
            std::fs::create_dir_all(runfiles_dir.join(&tree.workspace_name))?;
            link_manifest(&input_manifest, &output_manifest)?;
        }
        RunfilesSymlinksMode::Skip => {
            delete_trees_below(&runfiles_dir)?;
            link_manifest(&input_manifest, &output_manifest)?;
            std::fs::create_dir_all(runfiles_dir.join(&tree.workspace_name))?;
        }
    }
    Ok(())
}

/// Deletes `output_manifest` and symlinks it to `input_manifest` (`SymlinkTreeHelper.linkManifest`).
pub fn link_manifest(input_manifest: &Path, output_manifest: &Path) -> io::Result<()> {
    if let Ok(md) = std::fs::symlink_metadata(output_manifest) {
        if md.is_dir() && !md.file_type().is_symlink() {
            delete_tree(output_manifest)?;
        } else {
            std::fs::remove_file(output_manifest)?;
        }
    }
    if let Some(parent) = output_manifest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(input_manifest, output_manifest)?;
    #[cfg(not(unix))]
    std::fs::copy(input_manifest, output_manifest)?;
    Ok(())
}

/// In-place repair of a symlink tree (`SymlinkTreeHelper.createSymlinks`):
/// rejects empty/absolute keys and escaping `..` segments, normalizes `.`/`..`, deletes
/// non-matching types/stale entries with `delete_tree`, leaves existing regular files untruncated
/// for empty placeholders, and runs directory creation after the symlink pass so a directory
/// prefix replaces a symlink.
pub fn sync_symlink_tree(
    symlink_tree_root: &Path,
    symlinks: &BTreeMap<PathFragment, Option<PathFragment>>,
) -> io::Result<()> {
    #[derive(Default)]
    struct DirNode {
        symlinks: BTreeMap<String, Option<PathFragment>>,
        directories: BTreeMap<String, DirNode>,
    }

    let mut root = DirNode::default();
    for (raw_key, target) in symlinks {
        if raw_key.is_empty() || raw_key.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid symlink tree path: '{}'", raw_key.path_string()),
            ));
        }
        let path = PathFragment::create(raw_key.path_string());
        if path.is_empty() || path.is_absolute() || path.contains_uplevel_references() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid symlink tree path: '{}'", raw_key.path_string()),
            ));
        }
        let mut node = &mut root;
        if let Some(parent) = path.parent_directory() {
            for seg in parent.segments() {
                node = node.directories.entry(seg.to_string()).or_default();
            }
        }
        node.symlinks
            .insert(path.base_name().to_string(), target.clone());
    }

    fn ensure_symlink(link: &Path, target: &PathFragment) -> io::Result<()> {
        if let Ok(existing) = std::fs::read_link(link) {
            let existing_frag = PathFragment::create(&existing.to_string_lossy());
            let target_frag = PathFragment::create(target.path_string());
            if existing_frag == target_frag {
                return Ok(());
            }
        }
        if std::fs::symlink_metadata(link).is_ok() {
            delete_tree(link)?;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target.path_string(), link)?;
        #[cfg(not(unix))]
        {
            let src = Path::new(target.path_string());
            if src.is_file() {
                std::fs::copy(src, link)?;
            } else if src.is_dir() {
                fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
                    std::fs::create_dir_all(dst)?;
                    for entry in std::fs::read_dir(src)? {
                        let entry = entry?;
                        let p = entry.path();
                        let d = dst.join(entry.file_name());
                        if p.is_dir() {
                            copy_dir(&p, &d)?;
                        } else {
                            std::fs::copy(&p, &d)?;
                        }
                    }
                    Ok(())
                }
                copy_dir(src, link)?;
            } else {
                std::fs::write(link, b"")?;
            }
        }
        Ok(())
    }

    fn sync_dir(at: &Path, mut node: DirNode) -> io::Result<()> {
        match std::fs::symlink_metadata(at) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                std::fs::create_dir_all(at)?;
            }
            Ok(lmd) if lmd.file_type().is_symlink() => {
                // If symlink points to a real directory, keep only if not replaced; Wait: if a path
                // is both a symlink and a directory prefix, the directory pass must replace it
                // with a real directory so files aren't written through the symlink.
                delete_tree(at)?;
                std::fs::create_dir_all(at)?;
            }
            Ok(lmd) if !lmd.is_dir() => {
                delete_tree(at)?;
                std::fs::create_dir_all(at)?;
            }
            Ok(lmd) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let perms = lmd.permissions().mode();
                    if (perms & 0o700) != 0o700 {
                        std::fs::set_permissions(
                            at,
                            std::fs::Permissions::from_mode(perms | 0o700),
                        )?;
                    }
                }
                let _ = lmd;
            }
            Err(e) => return Err(e),
        }

        for entry in std::fs::read_dir(at)? {
            let entry = entry?;
            let basename = entry.file_name().to_string_lossy().into_owned();
            let next = entry.path();
            let ft = entry.file_type()?;
            if let Some(next_dir) = node.directories.remove(&basename) {
                // If both symlink and directory exist for the same segment, directory wins.
                node.symlinks.remove(&basename);
                if !ft.is_dir() || ft.is_symlink() {
                    delete_tree(&next)?;
                }
                sync_dir(&next, next_dir)?;
            } else if let Some(val) = node.symlinks.remove(&basename) {
                match val {
                    None => {
                        if !ft.is_file() || ft.is_symlink() {
                            delete_tree(&next)?;
                            std::fs::write(&next, b"")?;
                        }
                        // Existing regular file is left untruncated like Bazel.
                    }
                    Some(target) => {
                        if !ft.is_symlink() {
                            delete_tree(&next)?;
                        }
                        ensure_symlink(&next, &target)?;
                    }
                }
            } else {
                delete_tree(&next)?;
            }
        }

        // Symlink pass first, then directory pass (so a directory prefix replaces any symlink).
        for (name, val) in node.symlinks {
            if node.directories.contains_key(&name) {
                continue;
            }
            let next = at.join(&name);
            match val {
                None => {
                    std::fs::write(&next, b"")?;
                }
                Some(target) => {
                    ensure_symlink(&next, &target)?;
                }
            }
        }
        for (name, child) in node.directories {
            sync_dir(&at.join(&name), child)?;
        }
        Ok(())
    }

    sync_dir(symlink_tree_root, root)
}

pub fn map_for_runfiles(
    path_mapper: &PathMapper,
    runfiles_dir: &PathFragment,
    exec_path: &PathFragment,
) -> PathFragment {
    if path_mapper.is_noop() {
        return exec_path.clone();
    }
    let rf_name = runfiles_dir.base_name();
    if let Some(exe_name) = rf_name.strip_suffix(".runfiles") {
        let executable = runfiles_dir
            .parent_directory()
            .unwrap_or_else(PathFragment::empty)
            .get_relative_str(exe_name);
        let mapped_rf = path_mapper
            .map(&executable)
            .parent_directory()
            .unwrap_or_else(PathFragment::empty)
            .get_relative_str(rf_name);
        if let Ok(rel) = exec_path.relative_to(runfiles_dir) {
            return mapped_rf.get_relative(&rel);
        }
    }
    path_mapper.map(exec_path)
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Spawn {
    pub mnemonic: String,
    pub owner_label: String,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub inputs: Vec<PathFragment>,
    pub outputs: Vec<PathFragment>,
    /// The subset of `outputs` that are directories (tree artifacts): created before the spawn
    /// runs (an empty directory is a valid tree) like `SandboxOutputs.dirs()`.
    pub output_dirs: Vec<PathFragment>,
    pub execution_info: BTreeMap<String, String>,
    /// `--local_sigkill_grace_seconds`; `None` = Bazel's default of 15s.
    pub kill_delay: Option<Duration>,
    /// Inputs that are part of the tool (hashed into the persistent worker key,
    /// `Spawn.getToolFiles` / `WorkerFilesHash`).
    pub tool_inputs: Vec<PathFragment>,
    /// Outputs that may legitimately be absent after a successful run
    /// (`Spawn.isMandatoryOutput` is false: e.g. the jar of a `JavaCompileAction` attempt with a
    /// reduced classpath that asked for the full-classpath fallback).
    pub optional_outputs: Vec<PathFragment>,
    /// Path mapper applied to inputs/outputs (`Spawn.getPathMapper`).
    pub path_mapper: PathMapper,
    /// Virtual inputs (`VirtualActionInput`, e.g. embedded `BinTools`) materialized lazily.
    pub virtual_inputs: Vec<VirtualInput>,
    /// Unresolved symlink inputs (`ActionInput.isSymlink()`).
    pub symlink_inputs: Vec<PathFragment>,
    /// Empty marker inputs (`EmptyActionInput`).
    pub empty_inputs: Vec<PathFragment>,
    /// Tree artifact inputs (`parent_exec_path -> declared child parent-relative paths`).
    /// An empty `Vec` represents an empty tree artifact (which is kept as an empty directory).
    pub tree_inputs: BTreeMap<PathFragment, Vec<PathFragment>>,
    /// Fileset inputs to expand.
    pub fileset_inputs: Vec<FilesetInputData>,
    /// Runfiles trees to expand into spawn inputs and update before local/worker execution.
    pub runfiles_trees: Vec<RunfilesTreeData>,
    /// Tool tree artifacts (`parent_exec_path -> declared child parent-relative paths`).
    pub tool_tree_inputs: BTreeMap<PathFragment, Vec<PathFragment>>,
    /// Tool runfiles trees (`WorkerFilesHash` + `WorkerSpawnRunner` runfiles updates).
    pub tool_runfiles_trees: Vec<RunfilesTreeData>,
    /// Optional client environment used by `LocalSpawnRunner` (`LocalEnvProvider`).
    pub client_env: Option<BTreeMap<String, String>>,
    /// Optional `process-wrapper` binary path (`BinTools.getEmbeddedPath("process-wrapper")`).
    pub process_wrapper_path: Option<String>,
}

impl Spawn {
    /// Declared outputs that must exist after a successful run.
    pub fn mandatory_outputs(&self) -> impl Iterator<Item = &PathFragment> {
        self.outputs
            .iter()
            .filter(move |o| !self.optional_outputs.contains(o))
    }

    /// The `timeout` execution requirement in seconds (`Spawn.getTimeout`).
    pub fn timeout_secs(&self) -> Option<f64> {
        self.execution_info
            .get("timeout")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|s| *s > 0.0)
    }

    pub fn uses_path_mapping(&self) -> bool {
        !self.path_mapper.is_noop()
    }

    pub fn has_tools(&self) -> bool {
        !self.tool_inputs.is_empty()
            || !self.tool_tree_inputs.is_empty()
            || !self.tool_runfiles_trees.is_empty()
    }

    /// Expands `inputs`, `virtual_inputs`, `symlink_inputs`, `empty_inputs`, `tree_inputs`,
    /// `fileset_inputs`, and `runfiles_trees` into a `HIERARCHICAL_COMPARATOR`-ordered map
    /// (`SpawnInputExpander.getInputMapping`), applying `path_mapper` and `base_directory`.
    pub fn build_input_mapping(
        &self,
        base_directory: &PathFragment,
    ) -> Result<BTreeMap<HierarchicalPath, ExpandedInput>, String> {
        let mut out: BTreeMap<HierarchicalPath, ExpandedInput> = BTreeMap::new();
        let add = |map: &mut BTreeMap<HierarchicalPath, ExpandedInput>,
                   target: PathFragment,
                   entry: ExpandedInput|
         -> Result<(), String> {
            if target.is_absolute() {
                return Err(format!(
                    "input target location must be relative: {}",
                    target
                ));
            }
            let key = if base_directory.is_empty() {
                target
            } else {
                base_directory.get_relative(&target)
            };
            map.insert(HierarchicalPath(key), entry);
            Ok(())
        };

        for p in &self.inputs {
            if self.tree_inputs.contains_key(p)
                || self.runfiles_trees.iter().any(|r| &r.exec_path == p)
            {
                continue;
            }
            let is_symlink = self.symlink_inputs.contains(p);
            if self.empty_inputs.contains(p) {
                add(&mut out, self.path_mapper.map(p), ExpandedInput::Empty)?;
            } else {
                add(
                    &mut out,
                    self.path_mapper.map(p),
                    ExpandedInput::File {
                        exec_path: p.clone(),
                        is_symlink,
                    },
                )?;
            }
        }
        for p in &self.symlink_inputs {
            if !self.inputs.contains(p) {
                add(
                    &mut out,
                    self.path_mapper.map(p),
                    ExpandedInput::File {
                        exec_path: p.clone(),
                        is_symlink: true,
                    },
                )?;
            }
        }
        for p in &self.empty_inputs {
            if !self.inputs.contains(p) {
                add(&mut out, self.path_mapper.map(p), ExpandedInput::Empty)?;
            }
        }
        for v in &self.virtual_inputs {
            add(
                &mut out,
                self.path_mapper.map(v.exec_path()),
                ExpandedInput::Virtual(v.clone()),
            )?;
        }
        for (parent, children) in &self.tree_inputs {
            if children.is_empty() {
                add(
                    &mut out,
                    self.path_mapper.map(parent),
                    ExpandedInput::EmptyDirectory(parent.clone()),
                )?;
            } else {
                let mapped_parent = self.path_mapper.map(parent);
                for child_rel in children {
                    let child_exec = parent.get_relative(child_rel);
                    add(
                        &mut out,
                        mapped_parent.get_relative(child_rel),
                        ExpandedInput::File {
                            exec_path: child_exec,
                            is_symlink: false,
                        },
                    )?;
                }
            }
        }
        for fs_in in &self.fileset_inputs {
            for (name, target) in &fs_in.symlinks {
                add(
                    &mut out,
                    fs_in.exec_path.get_relative(name),
                    ExpandedInput::File {
                        exec_path: target.clone(),
                        is_symlink: false,
                    },
                )?;
            }
        }
        for rf in &self.runfiles_trees {
            if rf.exec_path.is_absolute() {
                return Err(format!("runfiles root must be relative: {}", rf.exec_path));
            }
            for (key, target) in &rf.mapping {
                let loc = rf.exec_path.get_relative(key);
                match target {
                    None => {
                        add(
                            &mut out,
                            map_for_runfiles(&self.path_mapper, &rf.exec_path, &loc),
                            ExpandedInput::Empty,
                        )?;
                    }
                    Some(RunfilesTarget::NestedRunfilesTree) => {
                        return Err(format!("nested runfiles artifact not allowed: {}", loc));
                    }
                    Some(RunfilesTarget::TreeArtifact {
                        exec_path,
                        children,
                    }) => {
                        let mapped_loc = map_for_runfiles(&self.path_mapper, &rf.exec_path, &loc);
                        if children.is_empty() {
                            add(
                                &mut out,
                                mapped_loc,
                                ExpandedInput::EmptyDirectory(exec_path.clone()),
                            )?;
                        } else {
                            for child_rel in children {
                                add(
                                    &mut out,
                                    mapped_loc.get_relative(child_rel),
                                    ExpandedInput::File {
                                        exec_path: exec_path.get_relative(child_rel),
                                        is_symlink: false,
                                    },
                                )?;
                            }
                        }
                    }
                    Some(RunfilesTarget::Fileset { symlinks, .. }) => {
                        for (name, link_target) in symlinks {
                            add(
                                &mut out,
                                loc.get_relative(name),
                                ExpandedInput::File {
                                    exec_path: link_target.clone(),
                                    is_symlink: false,
                                },
                            )?;
                        }
                    }
                    Some(RunfilesTarget::File {
                        exec_path,
                        is_symlink,
                    }) => {
                        add(
                            &mut out,
                            map_for_runfiles(&self.path_mapper, &rf.exec_path, &loc),
                            ExpandedInput::File {
                                exec_path: exec_path.clone(),
                                is_symlink: *is_symlink,
                            },
                        )?;
                    }
                }
            }
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnStatus {
    Success,
    NonZeroExit,
    Timeout,
    ExecutionFailed,
    MissingInputs,
    MissingOutputs,
}

/// The command line/environment/cwd actually used (shown in failure messages; differs from the
/// spawn's own with `--sandbox_debug`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutedCommand {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: String,
    pub debug_output: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnResult {
    pub status: SpawnStatus,
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub wall_time_ms: u64,
    pub runner_name: String,
    /// Paths of missing declared inputs/outputs for `MissingInputs`/`MissingOutputs`.
    pub missing: Vec<String>,
    pub usage: Option<ResourceUsage>,
    pub executed_command: Option<ExecutedCommand>,
}

impl SpawnResult {
    pub fn is_ok(&self) -> bool {
        self.status == SpawnStatus::Success && self.exit_code == 0
    }
}

/// Default grace period between SIGTERM and SIGKILL (`--local_sigkill_grace_seconds`, 15s).
pub const DEFAULT_KILL_DELAY_MS: u64 = 15_000;

/// Hooks used by dynamic execution (`SandboxedSpawnStrategy.StopConcurrentSpawns` and thread
/// interruption in Bazel). `lock_outputs(exit_code)` is called by a runner right before it
/// publishes its outputs into the exec root; returning `false` means this branch lost the race
/// and must not touch any output (the runner then reports `ErrorKind::Interrupted`). `cancel`
/// kills the running process.
#[derive(Clone, Default)]
pub struct ExecHooks {
    pub cancel: Option<process_wrapper::CancelFlag>,
    pub lock_outputs: Option<std::sync::Arc<dyn Fn(i32) -> bool + Send + Sync>>,
    pub runfiles_updater: Option<Arc<RunfilesTreeUpdater>>,
}

impl ExecHooks {
    /// Calls the output lock; `true` when execution may publish outputs.
    pub fn lock(&self, exit_code: i32) -> bool {
        self.lock_outputs.as_ref().is_none_or(|f| f(exit_code))
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|c| c.is_cancelled())
    }
}

/// Error returned by a branch that was cancelled / lost the dynamic-execution race.
pub fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "spawn cancelled")
}

/// Executes a `Spawn` locally inside `execroot` through the process-wrapper port
/// (`LocalSpawnRunner`): own process group, captured stdout/stderr, `timeout` execution
/// requirement enforced with SIGTERM then SIGKILL after the kill delay, declared inputs must
/// exist, stale outputs are removed and output parent directories created beforehand, and
/// declared outputs must exist afterwards (otherwise status is `MissingOutputs`).
pub fn execute_spawn_local(
    spawn: &Spawn,
    execroot: &PathFragment,
    fs: &dyn FileSystem,
) -> io::Result<SpawnResult> {
    execute_spawn_local_with(spawn, execroot, fs, &ExecHooks::default())
}

/// [`execute_spawn_local`] with dynamic-execution hooks. A standalone spawn writes its outputs
/// in place, so (like `LocalSpawnRunner`) the output lock is taken before the process starts.
pub fn execute_spawn_local_with(
    spawn: &Spawn,
    execroot: &PathFragment,
    fs: &dyn FileSystem,
    hooks: &ExecHooks,
) -> io::Result<SpawnResult> {
    let clock = JavaClock;
    let start_ms = clock.current_time_millis();
    let mk = |status, exit_code, stdout: Vec<u8>, stderr: Vec<u8>| SpawnResult {
        status,
        exit_code,
        stdout,
        stderr,
        wall_time_ms: clock.current_time_millis().saturating_sub(start_ms),
        runner_name: "local".to_string(),
        missing: Vec::new(),
        usage: None,
        executed_command: None,
    };
    if spawn.argv.is_empty() {
        return Ok(mk(
            SpawnStatus::ExecutionFailed,
            1,
            Vec::new(),
            b"empty command line\n".to_vec(),
        ));
    }

    let exec_root_path = Path::new(execroot.path_string());
    // Materialize virtual action inputs (e.g., BinTools) and update runfiles trees before exec.
    for v in &spawn.virtual_inputs {
        v.atomically_write_relative_to(exec_root_path)?;
    }
    if !spawn.runfiles_trees.is_empty() {
        if let Some(updater) = &hooks.runfiles_updater {
            updater.update_runfiles(exec_root_path, &spawn.runfiles_trees)?;
        } else {
            RunfilesTreeUpdater::new().update_runfiles(exec_root_path, &spawn.runfiles_trees)?;
        }
    }

    // Declared inputs must exist (symlinks are followed like stat()).
    let virtual_paths: Vec<&PathFragment> =
        spawn.virtual_inputs.iter().map(|v| v.exec_path()).collect();
    let missing_inputs: Vec<String> = spawn
        .inputs
        .iter()
        .filter(|inp| !virtual_paths.contains(inp) && !spawn.empty_inputs.contains(inp))
        .filter(|inp| std::fs::symlink_metadata(execroot.get_relative(inp).path_string()).is_err())
        .map(|i| i.path_string().to_string())
        .collect();
    if !missing_inputs.is_empty() {
        let mut r = mk(SpawnStatus::MissingInputs, 1, Vec::new(), Vec::new());
        r.missing = missing_inputs;
        return Ok(r);
    }

    if hooks.is_cancelled() || !hooks.lock(0) {
        return Err(cancelled_error());
    }
    // Prepare outputs: delete stale ones, create parent directories.
    for out in &spawn.outputs {
        let abs_out = execroot.get_relative(out);
        let p = Path::new(abs_out.path_string());
        delete_tree(p)?;
        if let Some(parent) = abs_out.parent_directory() {
            fs.create_directory_and_parents(&parent)?;
        }
    }
    for dir in &spawn.output_dirs {
        fs.create_directory_and_parents(&execroot.get_relative(dir))?;
    }

    // Environment contract: spawn's variables + optional absolute TMPDIR when unset (plus Darwin Xcode rewrite).
    let mut env = if let Some(ce) = &spawn.client_env {
        sandbox::rewrite_local_env_for_current_os(&spawn.env, ce, &spawn.execution_info)
    } else if cfg!(target_os = "macos") {
        sandbox::rewrite_local_env_preserving_tmpdir(
            &spawn.env,
            &BTreeMap::new(),
            &spawn.execution_info,
        )
    } else {
        let mut e = spawn.env.clone();
        if let Some(t) = e.get("TMPDIR").cloned() {
            if !Path::new(&t).is_absolute() {
                e.insert("TMPDIR".to_string(), "/tmp".to_string());
            }
        }
        e
    };

    let mut args = spawn.argv.clone();
    if let Some(pw) = &spawn.process_wrapper_path {
        let kill_delay_dur = spawn
            .execution_info
            .get("zel-kill-delay-ms")
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_millis)
            .or(spawn.kill_delay)
            .unwrap_or(Duration::from_millis(DEFAULT_KILL_DELAY_MS));
        let mut cb = process_wrapper::CommandLineBuilder {
            wrapper: pw.clone(),
            args: args.clone(),
            timeout: spawn.timeout_secs().map(Duration::from_secs_f64),
            kill_delay: Some(kill_delay_dur),
            graceful_sigterm: spawn
                .execution_info
                .contains_key("supports-graceful-termination"),
            ..Default::default()
        };
        let _ = &mut cb;
        args = cb.build();
    }
    // LocalSpawnRunner: a relative argv[0] with a parent directory is made absolute under execroot.
    if let Some(arg0) = args.first_mut() {
        let p = Path::new(arg0.as_str());
        if !p.is_absolute() && p.parent().is_some_and(|par| !par.as_os_str().is_empty()) {
            *arg0 = exec_root_path.join(p).to_string_lossy().into_owned();
        }
    }

    let kill_delay = spawn
        .execution_info
        .get("zel-kill-delay-ms")
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .or(spawn.kill_delay)
        .unwrap_or(Duration::from_millis(DEFAULT_KILL_DELAY_MS));
    let using_external_pw = spawn.process_wrapper_path.is_some();
    let opt = process_wrapper::ProcessWrapperOptions {
        graceful_sigterm: spawn
            .execution_info
            .contains_key("supports-graceful-termination"),
        timeout_secs: if using_external_pw {
            0.0
        } else {
            spawn.timeout_secs().unwrap_or(0.0)
        },
        kill_delay_secs: kill_delay.as_secs_f64(),
        args,
        cancel: hooks.cancel.clone(),
        close_stdin: true,
        ..Default::default()
    };
    let _ = &mut env;
    let o = match process_wrapper::run(&opt, Some(&env), Some(execroot.path_string())) {
        Ok(o) => o,
        Err(e) => {
            return Ok(mk(
                SpawnStatus::ExecutionFailed,
                127,
                Vec::new(),
                format!("Failed to execute '{}': {e}\n", spawn.argv[0]).into_bytes(),
            ));
        }
    };
    if o.cancelled {
        return Err(cancelled_error());
    }
    let usage = o.usage;
    let done = |status, code, stdout, stderr| {
        let mut r = mk(status, code, stdout, stderr);
        r.usage = usage;
        r
    };
    if o.timed_out {
        let secs = spawn.timeout_secs().unwrap_or(0.0);
        let mut stderr = o.stderr;
        stderr.extend_from_slice(format!("Action timed out after {secs:.0}s\n").as_bytes());
        return Ok(done(SpawnStatus::Timeout, 142, o.stdout, stderr));
    }
    let code = o.termination.exit_code();
    if code != 0 {
        return Ok(done(SpawnStatus::NonZeroExit, code, o.stdout, o.stderr));
    }
    let missing_outputs: Vec<String> = spawn
        .mandatory_outputs()
        .filter(|o| std::fs::symlink_metadata(execroot.get_relative(o).path_string()).is_err())
        .map(|o| o.path_string().to_string())
        .collect();
    if !missing_outputs.is_empty() {
        let mut r = done(SpawnStatus::MissingOutputs, 0, o.stdout, o.stderr);
        r.missing = missing_outputs;
        return Ok(r);
    }
    Ok(done(SpawnStatus::Success, 0, o.stdout, o.stderr))
}

/// Builds a `WorkRequest` protobuf message for persistent workers (`worker_protocol.proto`).
pub fn build_worker_request(
    spawn: &Spawn,
    request_id: i32,
    input_digests: &[(String, Vec<u8>)],
) -> WorkRequest {
    let inputs = input_digests
        .iter()
        .map(|(path, digest)| WorkerInput {
            path: path.clone(),
            digest: digest.clone(),
        })
        .collect();
    WorkRequest {
        arguments: spawn.argv.clone(),
        inputs,
        request_id,
        cancel: false,
        verbosity: 0,
        sandbox_dir: String::new(),
    }
}

pub fn worker_response_to_spawn_result(resp: &WorkResponse) -> SpawnResult {
    SpawnResult {
        status: if resp.exit_code == 0 {
            SpawnStatus::Success
        } else {
            SpawnStatus::NonZeroExit
        },
        exit_code: resp.exit_code,
        stdout: Vec::new(),
        stderr: resp.output.as_bytes().to_vec(),
        wall_time_ms: 0,
        runner_name: "worker".to_string(),
        missing: Vec::new(),
        usage: None,
        executed_command: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_request_round_trip() {
        let spawn = Spawn {
            mnemonic: "Javac".to_string(),
            owner_label: "//src:lib".to_string(),
            argv: vec!["--flagfile=params".to_string()],
            ..Default::default()
        };
        let req = build_worker_request(&spawn, 7, &[("src/A.java".to_string(), vec![1, 2, 3])]);
        let encoded = req.encode_to_vec();
        let decoded = WorkRequest::decode(&encoded).unwrap();
        assert_eq!(decoded.request_id, 7);
        assert_eq!(decoded.inputs[0].path, "src/A.java");
    }

    #[cfg(unix)]
    fn tmp(name: &str) -> PathFragment {
        let d = std::env::temp_dir().join(format!("zel-sbx-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        PathFragment::create(d.to_str().unwrap())
    }

    #[cfg(unix)]
    fn sh(script: &str, outputs: &[&str], inputs: &[&str]) -> Spawn {
        Spawn {
            mnemonic: "Genrule".into(),
            owner_label: "//:x".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            env: BTreeMap::new(),
            inputs: inputs.iter().map(|s| PathFragment::create(s)).collect(),
            outputs: outputs.iter().map(|s| PathFragment::create(s)).collect(),
            execution_info: BTreeMap::new(),
            ..Default::default()
        }
    }

    #[cfg(unix)]
    #[test]
    fn nonzero_exit_is_reported_with_output() {
        let root = tmp("nz");
        let fs = zel_vfs::LocalFileSystem::default();
        let r = execute_spawn_local(&sh("echo out; echo err >&2; exit 3", &[], &[]), &root, &fs)
            .unwrap();
        assert_eq!(r.status, SpawnStatus::NonZeroExit);
        assert_eq!(r.exit_code, 3);
        assert_eq!(r.stdout, b"out\n");
        assert_eq!(r.stderr, b"err\n");
    }

    #[cfg(unix)]
    #[test]
    fn missing_output_and_input_are_errors_not_stubs() {
        let root = tmp("mo");
        let fs = zel_vfs::LocalFileSystem::default();
        let r = execute_spawn_local(&sh("true", &["a/out.txt"], &[]), &root, &fs).unwrap();
        assert_eq!(r.status, SpawnStatus::MissingOutputs);
        assert_eq!(r.missing, vec!["a/out.txt"]);
        let r = execute_spawn_local(&sh("true", &[], &["nope.txt"]), &root, &fs).unwrap();
        assert_eq!(r.status, SpawnStatus::MissingInputs);
        let r = execute_spawn_local(&sh("echo hi > a/out.txt", &["a/out.txt"], &[]), &root, &fs)
            .unwrap();
        assert!(r.is_ok());
        assert_eq!(
            std::fs::read_to_string(format!("{}/a/out.txt", root)).unwrap(),
            "hi\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stale_outputs_removed_and_env_is_exact() {
        let root = tmp("env");
        let fs = zel_vfs::LocalFileSystem::default();
        std::fs::write(format!("{}/o", root), "stale").unwrap();
        let mut s = sh("test ! -e o && echo \"$FOO:$HOME\" > o", &["o"], &[]);
        s.env.insert("FOO".into(), "bar".into());
        let r = execute_spawn_local(&s, &root, &fs).unwrap();
        assert!(r.is_ok(), "{:?}", r);
        assert_eq!(
            std::fs::read_to_string(format!("{}/o", root)).unwrap(),
            "bar:\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_process_group() {
        let root = tmp("to");
        let fs = zel_vfs::LocalFileSystem::default();
        let mut s = sh("sleep 30 & sleep 30", &[], &[]);
        s.execution_info.insert("timeout".into(), "0.2".into());
        s.execution_info
            .insert("zel-kill-delay-ms".into(), "100".into());
        let t = std::time::Instant::now();
        let r = execute_spawn_local(&s, &root, &fs).unwrap();
        assert_eq!(r.status, SpawnStatus::Timeout);
        assert!(t.elapsed().as_secs() < 5);
    }

    #[cfg(unix)]
    #[test]
    fn local_spawn_closes_stdin_absolutizes_rel_argv0_no_path_injection_and_writes_virtual() {
        let root = tmp("local_spawn");
        let root_p = Path::new(root.path_string());
        std::fs::create_dir_all(root_p.join("tools")).unwrap();
        std::fs::write(
            root_p.join("tools/run.sh"),
            "#!/bin/sh\n# Verify stdin is closed (cat returns empty), argv0 was absolutized, PATH not in environ\nin=$(cat)\nenv_out=$(/usr/bin/env)\necho \"in=$in|env=[$env_out]|arg0=$0\" > out.txt\n",
        )
        .unwrap();
        #[cfg(unix)]
        set_executable_mode(&root_p.join("tools/run.sh")).unwrap();
        let fs = zel_vfs::LocalFileSystem::default();
        let s = Spawn {
            mnemonic: "Gen".into(),
            owner_label: "//:x".into(),
            argv: vec!["tools/run.sh".into()],
            outputs: vec![PathFragment::create("out.txt")],
            virtual_inputs: vec![VirtualInput::Bytes {
                exec_path: PathFragment::create("_bin/helper"),
                content: b"#!/bin/sh\necho ok\n".to_vec(),
                executable: true,
            }],
            ..Default::default()
        };
        let r = execute_spawn_local(&s, &root, &fs).unwrap();
        assert!(r.is_ok(), "{:?}", String::from_utf8_lossy(&r.stderr));
        let out = std::fs::read_to_string(root_p.join("out.txt")).unwrap();
        assert!(out.contains("in=|"), "{out}");
        assert!(
            !out.contains("PATH="),
            "PATH must not be injected into process env: {out}"
        );
        assert!(
            out.contains(&format!("arg0={}/tools/run.sh", root)),
            "{out}"
        );
        assert!(root_p.join("_bin/helper").exists());
    }
}
