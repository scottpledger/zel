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

//! VFS higher-level types and helpers: `Root`, `VfsPath`, `SearchPath`, `BasicActionInput`, and
//! `FileSystemUtils` (`src/main/java/com/google/devtools/build/lib/vfs/`).

use crate::{Dirent, DirentType, FileStatus, FileSystem, OsPathPolicy, PathFragment};
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;

///Filesystem root abstraction matching `com.google.devtools.build.lib.vfs.Root` (`PathRoot` vs
/// `AbsoluteRoot`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Root {
    /// `Root.fromPath(path)`: rooted at a specific directory path (`is_absolute()` is `false`).
    PathRoot(PathFragment),
    /// `Root.absoluteRoot(fs)`: encompasses all absolute paths (`is_absolute()` is `true`,
    /// `as_path()` is `None`, `Display` is `"<absolute root>"`).
    AbsoluteRoot,
}

impl Root {
    pub fn from_path(path: PathFragment) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err(format!("Root path must be absolute: {path}"));
        }
        Ok(Self::PathRoot(path))
    }

    pub const fn absolute_root() -> Self {
        Self::AbsoluteRoot
    }

    /// `Root.isAbsolute()`: `true` for `AbsoluteRoot`, `false` for `PathRoot`.
    pub const fn is_absolute(&self) -> bool {
        matches!(self, Self::AbsoluteRoot)
    }

    /// `Root.asPath()`: `None` for `AbsoluteRoot`, `Some(&PathFragment)` for `PathRoot`.
    pub const fn as_path(&self) -> Option<&PathFragment> {
        match self {
            Self::PathRoot(p) => Some(p),
            Self::AbsoluteRoot => None,
        }
    }

    /// `Root.getRelative(PathFragment)`: `AbsoluteRoot` requires an absolute fragment and returns
    /// it unchanged; `PathRoot` resolves relative to the root path.
    pub fn get_relative(&self, rel: &PathFragment) -> Result<PathFragment, String> {
        match self {
            Self::AbsoluteRoot => {
                if !rel.is_absolute() {
                    return Err(format!(
                        "AbsoluteRoot.getRelative requires an absolute path, got '{rel}'"
                    ));
                }
                Ok(rel.clone())
            }
            Self::PathRoot(base) => Ok(base.get_relative(rel)),
        }
    }

    pub fn get_relative_str(&self, rel: &str) -> Result<PathFragment, String> {
        self.get_relative(&PathFragment::create(rel))
    }

    /// `Root.relativize(PathFragment)`: requires an absolute fragment; `AbsoluteRoot` returns it
    /// unchanged, `PathRoot` computes `path.relative_to(base)`.
    pub fn relativize(&self, path: &PathFragment) -> Result<PathFragment, String> {
        if !path.is_absolute() {
            return Err(format!("Cannot relativize non-absolute path '{path}'"));
        }
        match self {
            Self::AbsoluteRoot => Ok(path.clone()),
            Self::PathRoot(base) => path.relative_to(base),
        }
    }

    /// `Root.contains(PathFragment)`: requires an absolute fragment (`false` if relative).
    pub fn contains(&self, path: &PathFragment) -> bool {
        if !path.is_absolute() {
            return false;
        }
        match self {
            Self::AbsoluteRoot => true,
            Self::PathRoot(base) => path.starts_with(base),
        }
    }
}

impl fmt::Display for Root {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PathRoot(p) => write!(f, "{p}"),
            Self::AbsoluteRoot => f.write_str("<absolute root>"),
        }
    }
}

static NEXT_FS_ID: AtomicUsize = AtomicUsize::new(1);

/// Handle to a `FileSystem` with stable object identity so [`VfsPath`] can enforce
/// same-filesystem invariants (`equals`, `startsWith`, `compareTo`, `relativeTo`, `renameTo`).
#[derive(Clone)]
pub struct FileSystemHandle {
    id: usize,
    fs: Arc<dyn FileSystem>,
}

impl FileSystemHandle {
    pub fn new(fs: Arc<dyn FileSystem>) -> Self {
        Self {
            id: NEXT_FS_ID.fetch_add(1, AtomicOrdering::Relaxed),
            fs,
        }
    }

    pub fn id(&self) -> usize {
        self.id
    }

    pub fn fs(&self) -> &dyn FileSystem {
        self.fs.as_ref()
    }

    pub fn get_path(&self, path: PathFragment) -> Result<VfsPath, String> {
        VfsPath::new(self.clone(), path)
    }

    pub fn get_path_str(&self, raw: &str) -> Result<VfsPath, String> {
        self.get_path(PathFragment::create(raw))
    }
}

impl fmt::Debug for FileSystemHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FileSystemHandle(#{})", self.id)
    }
}

/// An absolute [`PathFragment`] bound to a [`FileSystemHandle`], matching
/// `com.google.devtools.build.lib.vfs.Path`.
#[derive(Clone, Debug)]
pub struct VfsPath {
    fs: FileSystemHandle,
    fragment: PathFragment,
}

impl VfsPath {
    pub const NOW_SENTINEL_TIME: i64 = -1;

    pub fn new(fs: FileSystemHandle, fragment: PathFragment) -> Result<Self, String> {
        if !fragment.is_absolute() {
            return Err(format!("Path must be absolute: '{fragment}'"));
        }
        Ok(Self { fs, fragment })
    }

    pub fn file_system(&self) -> &FileSystemHandle {
        &self.fs
    }

    pub fn as_fragment(&self) -> &PathFragment {
        &self.fragment
    }

    pub fn path_string(&self) -> &str {
        self.fragment.path_string()
    }

    pub fn base_name(&self) -> &str {
        self.fragment.base_name()
    }

    /// `Path.getParentDirectory()`: returns `None` at a filesystem root (`/` or `C:/`).
    pub fn parent_directory(&self) -> Option<Self> {
        self.fragment.parent_directory().map(|fragment| Self {
            fs: self.fs.clone(),
            fragment,
        })
    }

    /// `Path.replaceName(String)`: returns `None` at a root (`/` or `C:/`), validates `new_name`
    /// as a single basename segment.
    pub fn replace_name(&self, new_name: &str) -> Result<Option<Self>, String> {
        let Some(parent) = self.parent_directory() else {
            return Ok(None);
        };
        parent.get_child(new_name).map(Some)
    }

    pub fn get_child(&self, base_name: &str) -> Result<Self, String> {
        let child = self.fragment.get_child(base_name)?;
        Ok(Self {
            fs: self.fs.clone(),
            fragment: child,
        })
    }

    pub fn get_relative(&self, other: &PathFragment) -> Self {
        let joined = self.fragment.get_relative(other);
        Self {
            fs: self.fs.clone(),
            fragment: joined,
        }
    }

    pub fn get_relative_str(&self, other: &str) -> Self {
        self.get_relative(&PathFragment::create(other))
    }

    /// `Path.startsWith(Path)`: returns `false` when `other` belongs to a different filesystem.
    pub fn starts_with(&self, other: &Self) -> bool {
        self.fs.id == other.fs.id && self.fragment.starts_with(&other.fragment)
    }

    /// `Path.relativeTo(Path)`: rejects paths on different filesystems.
    pub fn relative_to(&self, base: &Self) -> Result<PathFragment, String> {
        if self.fs.id != base.fs.id {
            return Err("Paths reside on different filesystems".to_string());
        }
        self.fragment.relative_to(&base.fragment)
    }

    pub fn stat(&self, follow_symlinks: bool) -> io::Result<FileStatus> {
        self.fs.fs().stat(&self.fragment, follow_symlinks)
    }

    pub fn stat_nullable(&self, follow_symlinks: bool) -> Option<FileStatus> {
        self.fs.fs().stat_nullable(&self.fragment, follow_symlinks)
    }

    pub fn stat_if_found(&self, follow_symlinks: bool) -> io::Result<Option<FileStatus>> {
        self.fs.fs().stat_if_found(&self.fragment, follow_symlinks)
    }

    pub fn exists(&self, follow_symlinks: bool) -> bool {
        self.fs.fs().exists(&self.fragment, follow_symlinks)
    }

    pub fn readdir(&self, follow_symlinks: bool) -> io::Result<Vec<Dirent>> {
        self.fs
            .fs()
            .readdir_with_symlinks(&self.fragment, follow_symlinks)
    }

    pub fn rename_to(&self, target: &Self) -> io::Result<()> {
        if self.fs.id != target.fs.id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cannot rename across different filesystems",
            ));
        }
        self.fs.fs().rename(&self.fragment, &target.fragment)
    }

    pub fn create_symbolic_link_to_path(&self, target: &Self) -> io::Result<()> {
        if self.fs.id != target.fs.id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cannot create symbolic link to a path on a different filesystem",
            ));
        }
        self.fs
            .fs()
            .create_symbolic_link(&self.fragment, &target.fragment)
    }

    /// Computes a deterministic directory metadata digest matching `Path.getDirectoryDigest()`:
    /// sorted child names, with tags `x` (executable file), `-` (non-executable file), `d`
    /// (directory), `l` (dangling symlink hashed by link target text), or `s` (special).
    pub fn get_directory_digest(&self) -> io::Result<Vec<u8>> {
        let mut entries = self.fs.fs().readdir(&self.fragment)?;
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let mut buf = Vec::new();
        for entry in entries {
            let child = self.fragment.get_relative_str(&entry.name);
            let lst = self.fs.fs().stat(&child, false)?;
            if lst.is_symlink {
                match self.fs.fs().stat(&child, true) {
                    Ok(target_st) => {
                        append_status_tag(self.fs.fs(), &child, &entry.name, &target_st, &mut buf)?
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        let link_target = self.fs.fs().read_symbolic_link(&child)?;
                        buf.push(b'l');
                        buf.extend_from_slice(entry.name.as_bytes());
                        buf.push(0);
                        buf.extend_from_slice(link_target.path_string().as_bytes());
                        buf.push(b'\n');
                    }
                    Err(e) => return Err(e),
                }
            } else {
                append_status_tag(self.fs.fs(), &child, &entry.name, &lst, &mut buf)?;
            }
        }
        Ok(self.fs.fs().digest_hash_function().hash_bytes(&buf))
    }
}

fn append_status_tag(
    fs: &dyn FileSystem,
    child: &PathFragment,
    name: &str,
    st: &FileStatus,
    buf: &mut Vec<u8>,
) -> io::Result<()> {
    if st.is_directory {
        buf.push(b'd');
        buf.extend_from_slice(name.as_bytes());
        buf.push(b'\n');
    } else if st.is_file {
        let exec = st.permissions >= 0 && (st.permissions & 0o100) != 0;
        buf.push(if exec { b'x' } else { b'-' });
        buf.extend_from_slice(name.as_bytes());
        buf.push(0);
        buf.extend_from_slice(&fs.get_digest(child)?);
        buf.push(b'\n');
    } else {
        buf.push(b's');
        buf.extend_from_slice(name.as_bytes());
        buf.push(b'\n');
    }
    Ok(())
}

impl PartialEq for VfsPath {
    fn eq(&self, other: &Self) -> bool {
        self.fs.id == other.fs.id && self.fragment == other.fragment
    }
}
impl Eq for VfsPath {}

impl Hash for VfsPath {
    /// Matches `Path.hashCode()`, which hashes the path string without filesystem identity.
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.fragment.hash(state);
    }
}

impl PartialOrd for VfsPath {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for VfsPath {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.fs.id.cmp(&other.fs.id) {
            Ordering::Equal => self.fragment.cmp(&other.fragment),
            ord => ord,
        }
    }
}

impl fmt::Display for VfsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.fragment)
    }
}

/// `SearchPath.java`: parses `PATH` environment strings and resolves single-segment executable
/// names.
pub struct SearchPath;

impl SearchPath {
    pub const SEPARATOR: char = if cfg!(windows) { ';' } else { ':' };

    /// `SearchPath.parse(String)`: returns empty for `None` or `""`; splits by host `PATH`
    /// separator, normalizes lexically, and retains only absolute `PathFragment`s in order.
    pub fn parse(path_env: Option<&str>) -> Vec<PathFragment> {
        Self::parse_for_os(path_env, OsPathPolicy::host())
    }

    pub fn parse_for_os(path_env: Option<&str>, policy: OsPathPolicy) -> Vec<PathFragment> {
        let Some(raw) = path_env else {
            return Vec::new();
        };
        if raw.is_empty() {
            return Vec::new();
        }
        let sep = match policy {
            OsPathPolicy::Windows => ';',
            OsPathPolicy::Unix => ':',
        };
        raw.split(sep)
            .filter(|entry| !entry.is_empty())
            .map(|entry| PathFragment::create_for_os(entry, policy))
            .filter(|pf| pf.is_absolute())
            .collect()
    }

    /// `SearchPath.which(List<Path>, String)`: returns `Ok(None)` when `name` is empty, absolute,
    /// or multi-segment; returns `Err` when `name` is `.`, `..`, or contains `/` (such as
    /// `foo/../bar`) so `getChild` rejects it; otherwise returns the first existing executable
    /// entry (any file type, including directories).
    pub fn which(
        fs: &dyn FileSystem,
        search_dirs: &[PathFragment],
        name: &str,
    ) -> Result<Option<PathFragment>, String> {
        Self::which_for_os(fs, search_dirs, name, OsPathPolicy::host())
    }

    pub fn which_for_os(
        fs: &dyn FileSystem,
        search_dirs: &[PathFragment],
        name: &str,
        policy: OsPathPolicy,
    ) -> Result<Option<PathFragment>, String> {
        if name == "." || name == ".." {
            return Err(format!("baseName must not be '{name}'"));
        }
        let pf = PathFragment::create_for_os(name, policy);
        if pf.is_empty() || pf.is_absolute() || !pf.is_single_segment() {
            return Ok(None);
        }
        for dir in search_dirs {
            let candidate = dir.get_child_for_os(name, policy)?;
            if let Ok(st) = fs.stat(&candidate, true) {
                let executable = if st.permissions < 0 {
                    true
                } else {
                    (st.permissions & 0o100) != 0
                };
                if executable {
                    return Ok(Some(candidate));
                }
            }
        }
        Ok(None)
    }
}

/// Minimal `BasicActionInput` representation (`com.google.devtools.build.lib.actions.BasicActionInput`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BasicActionInput {
    exec_path: PathFragment,
    is_directory: bool,
    is_symlink: bool,
}

impl BasicActionInput {
    pub fn new(exec_path: PathFragment) -> Self {
        Self {
            exec_path,
            is_directory: false,
            is_symlink: false,
        }
    }

    pub fn with_type(exec_path: PathFragment, is_directory: bool, is_symlink: bool) -> Self {
        Self {
            exec_path,
            is_directory,
            is_symlink,
        }
    }

    pub fn exec_path(&self) -> &PathFragment {
        &self.exec_path
    }

    pub fn exec_path_string(&self) -> &str {
        self.exec_path.path_string()
    }

    pub fn is_directory(&self) -> bool {
        self.is_directory
    }

    pub fn is_symlink(&self) -> bool {
        self.is_symlink
    }
}

/// Static path and filesystem utilities matching `com.google.devtools.build.lib.vfs.FileSystemUtils`.
pub mod file_system_utils {
    use super::*;

    /// `FileSystemUtils.checkBaseName(String)`: rejects empty, `.`, `..`, and any string
    /// containing `/`.
    pub fn check_base_name(name: &str) -> Result<(), String> {
        if name.is_empty() {
            return Err("name must not be empty".to_string());
        }
        if name == "." || name == ".." {
            return Err(format!("baseName must not be '{name}'"));
        }
        if name.contains('/') {
            return Err(format!("baseName must not contain '/': '{name}'"));
        }
        Ok(())
    }

    /// `FileSystemUtils.commonAncestor(Path, Path)`: returns `None` when the paths reside on
    /// different filesystems or have different drive prefixes.
    pub fn common_ancestor(a: &VfsPath, b: &VfsPath) -> Option<VfsPath> {
        if a.file_system().id() != b.file_system().id() {
            return None;
        }
        common_ancestor_fragment(a.as_fragment(), b.as_fragment()).map(|frag| VfsPath {
            fs: a.file_system().clone(),
            fragment: frag,
        })
    }

    pub fn common_ancestor_fragment(a: &PathFragment, b: &PathFragment) -> Option<PathFragment> {
        if a.drive_str() != b.drive_str() {
            return None;
        }
        let segs_a = a.segments();
        let segs_b = b.segments();
        let common = segs_a
            .iter()
            .zip(segs_b.iter())
            .take_while(|(x, y)| x == y)
            .count();
        Some(a.sub_fragment_range(0, common))
    }

    /// `FileSystemUtils.relativePath(PathFragment from, PathFragment to)`: returns `../`-prefixed
    /// path from `from` to `to`, or `to` unchanged if they have no common ancestor/drive.
    pub fn relative_path(from: &PathFragment, to: &PathFragment) -> PathFragment {
        if from.drive_str() != to.drive_str() || from.is_absolute() != to.is_absolute() {
            return to.clone();
        }
        let segs_from = from.segments();
        let segs_to = to.segments();
        let common = segs_from
            .iter()
            .zip(segs_to.iter())
            .take_while(|(x, y)| x == y)
            .count();
        let mut parts: Vec<&str> = vec![".."; segs_from.len() - common];
        parts.extend_from_slice(&segs_to[common..]);
        PathFragment::create_already_normalized(parts.join("/"))
    }

    /// `FileSystemUtils.removeExtension(PathFragment)`: returns `None` when `path` has zero
    /// segments; otherwise strips from the last `.` after the last `/` (if any).
    pub fn remove_extension(path: &PathFragment) -> Option<PathFragment> {
        if path.segment_count() == 0 {
            return None;
        }
        let s = path.path_string();
        let last_slash = s.rfind('/').map(|i| i as isize).unwrap_or(-1);
        let last_dot = s.rfind('.').map(|i| i as isize).unwrap_or(-1);
        if last_dot > last_slash {
            Some(PathFragment::create_already_normalized(
                &s[..last_dot as usize],
            ))
        } else {
            Some(path.clone())
        }
    }

    /// `FileSystemUtils.replaceExtension(PathFragment, String)`: replaces any existing extension
    /// with `new_extension` (returns `None` if zero segments).
    pub fn replace_extension(path: &PathFragment, new_extension: &str) -> Option<PathFragment> {
        let stem = remove_extension(path)?;
        Some(PathFragment::create_already_normalized(format!(
            "{}{}",
            stem.path_string(),
            new_extension
        )))
    }

    /// Overload matching `replaceExtension(PathFragment, String, String)`: returns `None` unless
    /// `path` ends with `old_extension`.
    pub fn replace_matching_extension(
        path: &PathFragment,
        new_extension: &str,
        old_extension: &str,
    ) -> Option<PathFragment> {
        if path.segment_count() == 0 {
            return None;
        }
        let s = path.path_string();
        let stripped = s.strip_suffix(old_extension)?;
        Some(PathFragment::create_already_normalized(format!(
            "{stripped}{new_extension}"
        )))
    }

    pub fn append_extension(path: &PathFragment, extension: &str) -> Option<PathFragment> {
        if path.segment_count() == 0 {
            return None;
        }
        let ext = if extension.starts_with('.') || extension.is_empty() {
            extension.to_string()
        } else {
            format!(".{extension}")
        };
        Some(PathFragment::create_already_normalized(format!(
            "{}{ext}",
            path.path_string()
        )))
    }

    pub fn append_without_extension(path: &PathFragment, to_append: &str) -> Option<PathFragment> {
        if path.segment_count() == 0 {
            return None;
        }
        let s = path.path_string();
        let last_slash = s.rfind('/').map(|i| i as isize).unwrap_or(-1);
        let last_dot = s.rfind('.').map(|i| i as isize).unwrap_or(-1);
        if last_dot > last_slash {
            let idx = last_dot as usize;
            Some(PathFragment::create_already_normalized(format!(
                "{}{to_append}{}",
                &s[..idx],
                &s[idx..]
            )))
        } else {
            Some(PathFragment::create_already_normalized(format!(
                "{s}{to_append}"
            )))
        }
    }

    pub fn starts_with_any<'a, I>(path: &PathFragment, prefixes: I) -> bool
    where
        I: IntoIterator<Item = &'a PathFragment>,
    {
        prefixes.into_iter().any(|p| path.starts_with(p))
    }

    pub fn starts_with_any_ignoring_case<'a, I>(path: &PathFragment, prefixes: I) -> bool
    where
        I: IntoIterator<Item = &'a PathFragment>,
    {
        prefixes
            .into_iter()
            .any(|p| path.starts_with_ignoring_case(p))
    }

    pub fn create_empty_file(fs: &dyn FileSystem, path: &PathFragment) -> io::Result<()> {
        fs.write_file(path, b"")
    }

    pub fn touch_file(fs: &dyn FileSystem, path: &PathFragment) -> io::Result<()> {
        if fs.exists(path, false) {
            fs.set_last_modified_time(path, VfsPath::NOW_SENTINEL_TIME)
        } else {
            fs.write_file(path, b"")
        }
    }

    /// `FileSystemUtils.ensureSymbolicLink`: creates parent directories; errors if `link_path`
    /// already exists as a non-symlink; deletes and recreates if it is a symlink to another target.
    pub fn ensure_symbolic_link(
        fs: &dyn FileSystem,
        link_path: &PathFragment,
        target: &PathFragment,
    ) -> io::Result<()> {
        if let Some(parent) = link_path.parent_directory() {
            fs.create_directory_and_parents(&parent)?;
        }
        if let Some(st) = fs.stat_if_found(link_path, false)? {
            if !st.is_symlink {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("'{link_path}' is not a symbolic link"),
                ));
            }
            if let Ok(existing) = fs.read_symbolic_link(link_path) {
                if &existing == target {
                    return Ok(());
                }
            }
            fs.delete(link_path)?;
        }
        fs.create_symbolic_link(link_path, target)
    }

    /// `FileSystemUtils.copyFile`: follows symlinks on `source`, replaces a symlink destination,
    /// and preserves mtime and permissions when available.
    pub fn copy_file(
        fs: &dyn FileSystem,
        source: &PathFragment,
        target: &PathFragment,
    ) -> io::Result<()> {
        let src_st = fs.stat(source, true)?;
        let content = fs.read_file(source)?;
        if let Some(dst_st) = fs.stat_if_found(target, false)? {
            if dst_st.is_symlink {
                fs.delete(target)?;
            }
        }
        fs.write_file(target, &content)?;
        if src_st.permissions >= 0 {
            let _ = fs.chmod(target, src_st.permissions as u32);
        }
        let _ = fs.set_last_modified_time(target, src_st.last_modified_time_ms as i64);
        Ok(())
    }

    /// `FileSystemUtils.moveFile`: tries `rename`, falling back to copy (or symlink recreation)
    /// plus `delete(source)`.
    pub fn move_file(
        fs: &dyn FileSystem,
        source: &PathFragment,
        target: &PathFragment,
    ) -> io::Result<()> {
        if fs.rename(source, target).is_ok() {
            return Ok(());
        }
        let st = fs.stat(source, false)?;
        if st.is_symlink {
            let link_target = fs.read_symbolic_link(source)?;
            if fs.stat_if_found(target, false)?.is_some() {
                fs.delete(target)?;
            }
            fs.create_symbolic_link(target, &link_target)?;
        } else {
            copy_file(fs, source, target)?;
        }
        fs.delete(source)?;
        Ok(())
    }

    /// `FileSystemUtils.traverseTree`: recursively lists entries below `dir` without following
    /// symlinks.
    pub fn traverse_tree<F>(
        fs: &dyn FileSystem,
        dir: &PathFragment,
        predicate: F,
    ) -> io::Result<Vec<PathFragment>>
    where
        F: Fn(&PathFragment, DirentType) -> bool,
    {
        let mut out = Vec::new();
        traverse_tree_inner(fs, dir, &predicate, &mut out)?;
        Ok(out)
    }

    fn traverse_tree_inner<F>(
        fs: &dyn FileSystem,
        dir: &PathFragment,
        predicate: &F,
        out: &mut Vec<PathFragment>,
    ) -> io::Result<()>
    where
        F: Fn(&PathFragment, DirentType) -> bool,
    {
        for entry in fs.readdir(dir)? {
            let child = dir.get_relative_str(&entry.name);
            if predicate(&child, entry.dirent_type) {
                out.push(child.clone());
            }
            if entry.dirent_type == DirentType::Directory {
                traverse_tree_inner(fs, &child, predicate, out)?;
            }
        }
        Ok(())
    }

    /// `FileSystemUtils.copyTreesBelow`: rejects `to` under `from`; copies symlinks as symlinks.
    pub fn copy_trees_below(
        fs: &dyn FileSystem,
        from: &PathFragment,
        to: &PathFragment,
    ) -> io::Result<()> {
        if to.starts_with(from) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Cannot copy '{from}' into its own subdirectory '{to}'"),
            ));
        }
        fs.create_directory_and_parents(to)?;
        for entry in fs.readdir(from)? {
            let src = from.get_relative_str(&entry.name);
            let dst = to.get_relative_str(&entry.name);
            match entry.dirent_type {
                DirentType::Symlink => {
                    let link_target = fs.read_symbolic_link(&src)?;
                    ensure_symbolic_link(fs, &dst, &link_target)?;
                }
                DirentType::Directory => {
                    fs.create_directory(&dst)?;
                    copy_trees_below(fs, &src, &dst)?;
                }
                DirentType::File | DirentType::Unknown => {
                    copy_file(fs, &src, &dst)?;
                }
            }
        }
        Ok(())
    }

    /// `FileSystemUtils.moveTreesBelow`: sets `0755` on `from`, moves or copies children, and
    /// deletes emptied source subdirectories.
    pub fn move_trees_below(
        fs: &dyn FileSystem,
        from: &PathFragment,
        to: &PathFragment,
    ) -> io::Result<()> {
        if to.starts_with(from) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Cannot move '{from}' into its own subdirectory '{to}'"),
            ));
        }
        let _ = fs.chmod(from, 0o755);
        fs.create_directory_and_parents(to)?;
        for entry in fs.readdir(from)? {
            let src = from.get_relative_str(&entry.name);
            let dst = to.get_relative_str(&entry.name);
            if entry.dirent_type == DirentType::Directory {
                let _ = fs.chmod(&src, 0o755);
                if fs.rename(&src, &dst).is_err() {
                    fs.create_directory(&dst)?;
                    move_trees_below(fs, &src, &dst)?;
                    fs.delete(&src)?;
                }
            } else {
                move_file(fs, &src, &dst)?;
            }
        }
        Ok(())
    }

    /// `FileSystemUtils.removeDirectoryAndParents(Path base, PathFragment toRemove)`: returns
    /// `false` if `to_remove` is absolute or empty, or once an ancestor directory is non-empty.
    pub fn remove_directory_and_parents(
        fs: &dyn FileSystem,
        base: &PathFragment,
        to_remove: &PathFragment,
    ) -> bool {
        if to_remove.is_absolute() || to_remove.is_empty() {
            return false;
        }
        let mut cur = to_remove.clone();
        let mut removed_any = false;
        while !cur.is_empty() {
            let full = base.get_relative(&cur);
            match fs.readdir(&full) {
                Ok(entries) if entries.is_empty() => {
                    if fs.delete(&full).unwrap_or(false) {
                        removed_any = true;
                    } else {
                        break;
                    }
                }
                _ => break,
            }
            match cur.parent_directory() {
                Some(p) => cur = p,
                None => break,
            }
        }
        removed_any
    }

    /// `FileSystemUtils.createHardLink`: recursively creates parent/child directories and
    /// hardlinks files.
    pub fn create_hard_link_recursive(
        fs: &dyn FileSystem,
        link_path: &PathFragment,
        original_path: &PathFragment,
    ) -> io::Result<()> {
        let st = fs.stat(original_path, true)?;
        if st.is_directory {
            fs.create_directory_and_parents(link_path)?;
            for entry in fs.readdir(original_path)? {
                create_hard_link_recursive(
                    fs,
                    &link_path.get_relative_str(&entry.name),
                    &original_path.get_relative_str(&entry.name),
                )?;
            }
            Ok(())
        } else {
            if let Some(parent) = link_path.parent_directory() {
                fs.create_directory_and_parents(&parent)?;
            }
            fs.create_hard_link(link_path, original_path)
        }
    }

    /// `FileSystemUtils.maybeUpdateContent`: writes `new_content` only when `path` is missing,
    /// unreadable, or has different bytes, leaving mtime untouched when identical.
    pub fn maybe_update_content(
        fs: &dyn FileSystem,
        path: &PathFragment,
        new_content: &[u8],
    ) -> io::Result<bool> {
        if let Ok(existing) = fs.read_file(path) {
            if existing == new_content {
                return Ok(false);
            }
        }
        if let Some(parent) = path.parent_directory() {
            fs.create_directory_and_parents(&parent)?;
        }
        fs.write_file(path, new_content)?;
        Ok(true)
    }

    /// `FileSystemUtils.readWithKnownFileSize`: rejects `size > i32::MAX` and errors on short or
    /// long reads.
    pub fn read_with_known_file_size(
        fs: &dyn FileSystem,
        path: &PathFragment,
        size: u64,
    ) -> io::Result<Vec<u8>> {
        if size > i32::MAX as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Cannot read file with size larger than 2GB: {path}"),
            ));
        }
        let bytes = fs.read_file(path)?;
        if (bytes.len() as u64) < size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("Unexpected short read from '{path}'"),
            ));
        }
        if (bytes.len() as u64) > size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Too many bytes read from '{path}'"),
            ));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryFileSystem;

    #[test]
    fn root_absolute_vs_path_root() {
        let abs = Root::absolute_root();
        assert!(abs.is_absolute());
        assert!(abs.as_path().is_none());
        assert_eq!(abs.to_string(), "<absolute root>");
        assert!(abs.contains(&PathFragment::create("/a/b")));
        assert!(!abs.contains(&PathFragment::create("a/b")));
        assert_eq!(
            abs.relativize(&PathFragment::create("/a/b")).unwrap(),
            PathFragment::create("/a/b")
        );

        let pr = Root::from_path(PathFragment::create("/ws")).unwrap();
        assert!(!pr.is_absolute());
        assert_eq!(pr.to_string(), "/ws");
        assert!(pr.contains(&PathFragment::create("/ws/pkg")));
        assert!(!pr.contains(&PathFragment::create("/other")));
        assert_eq!(
            pr.relativize(&PathFragment::create("/ws/pkg/BUILD"))
                .unwrap(),
            PathFragment::create("pkg/BUILD")
        );
    }

    #[test]
    fn vfs_path_cross_fs_and_directory_digest() {
        let fs1 = FileSystemHandle::new(Arc::new(InMemoryFileSystem::default()));
        let fs2 = FileSystemHandle::new(Arc::new(InMemoryFileSystem::default()));
        let p1 = fs1.get_path_str("/ws/a").unwrap();
        let p2 = fs2.get_path_str("/ws/a").unwrap();
        assert_ne!(p1, p2);
        assert!(!p1.starts_with(&p2));
        assert!(p1.relative_to(&p2).is_err());

        let root = fs1.get_path_str("/").unwrap();
        assert!(root.parent_directory().is_none());
        assert!(root.replace_name("x").unwrap().is_none());

        fs1.fs()
            .create_directory_and_parents(&PathFragment::create("/dir"))
            .unwrap();
        fs1.fs()
            .write_file(&PathFragment::create("/dir/f.txt"), b"abc")
            .unwrap();
        fs1.fs()
            .create_symbolic_link(
                &PathFragment::create("/dir/dangling"),
                &PathFragment::create("missing"),
            )
            .unwrap();
        let dir_path = fs1.get_path_str("/dir").unwrap();
        let dig = dir_path.get_directory_digest().unwrap();
        assert!(!dig.is_empty());
    }

    #[test]
    fn search_path_parse_and_which() {
        assert!(SearchPath::parse_for_os(None, OsPathPolicy::Unix).is_empty());
        assert!(SearchPath::parse_for_os(Some(""), OsPathPolicy::Unix).is_empty());
        let dirs =
            SearchPath::parse_for_os(Some("/usr/bin::rel/dir:/bin/../sbin"), OsPathPolicy::Unix);
        assert_eq!(
            dirs,
            vec![
                PathFragment::create("/usr/bin"),
                PathFragment::create("/sbin")
            ]
        );

        let fs = InMemoryFileSystem::default();
        fs.create_directory_and_parents(&PathFragment::create("/usr/bin"))
            .unwrap();
        fs.write_file(&PathFragment::create("/usr/bin/tool"), b"#!/bin/sh")
            .unwrap();
        fs.chmod(&PathFragment::create("/usr/bin/tool"), 0o755)
            .unwrap();

        assert_eq!(
            SearchPath::which_for_os(&fs, &dirs, "tool", OsPathPolicy::Unix).unwrap(),
            Some(PathFragment::create("/usr/bin/tool"))
        );
        assert_eq!(
            SearchPath::which_for_os(&fs, &dirs, "sub/tool", OsPathPolicy::Unix).unwrap(),
            None
        );
        assert!(SearchPath::which_for_os(&fs, &dirs, ".", OsPathPolicy::Unix).is_err());
        assert!(SearchPath::which_for_os(&fs, &dirs, "a/../tool", OsPathPolicy::Unix).is_err());
    }

    #[test]
    fn file_system_utils_extensions_and_trees() {
        let p = PathFragment::create("pkg/lib.pic.o");
        assert_eq!(
            file_system_utils::remove_extension(&p),
            Some(PathFragment::create("pkg/lib.pic"))
        );
        assert_eq!(
            file_system_utils::replace_extension(&p, ".a"),
            Some(PathFragment::create("pkg/lib.pic.a"))
        );
        assert_eq!(
            file_system_utils::replace_matching_extension(&p, ".a", ".xyz"),
            None
        );
        assert_eq!(
            file_system_utils::append_without_extension(&p, "_dbg"),
            Some(PathFragment::create("pkg/lib.pic_dbg.o"))
        );
        assert_eq!(
            file_system_utils::relative_path(
                &PathFragment::create("/a/b/c"),
                &PathFragment::create("/a/d/e")
            ),
            PathFragment::create("../../d/e")
        );

        let fs = InMemoryFileSystem::default();
        fs.create_directory_and_parents(&PathFragment::create("/src/sub"))
            .unwrap();
        fs.write_file(&PathFragment::create("/src/sub/a.txt"), b"hello")
            .unwrap();
        file_system_utils::copy_trees_below(
            &fs,
            &PathFragment::create("/src"),
            &PathFragment::create("/dst"),
        )
        .unwrap();
        assert_eq!(
            fs.read_file(&PathFragment::create("/dst/sub/a.txt"))
                .unwrap(),
            b"hello"
        );
        assert!(!file_system_utils::maybe_update_content(
            &fs,
            &PathFragment::create("/dst/sub/a.txt"),
            b"hello"
        )
        .unwrap());
    }
}
