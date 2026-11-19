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

//! Bazel Virtual File System (`PathFragment`, `Path`, `Root`, `RootedPath`, `FileSystem`).

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
#[cfg(unix)]
use std::path::Path as StdPath;
use std::path::PathBuf as StdPathBuf;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, RwLock};
use std::time::UNIX_EPOCH;
use zel_util::{md5, sha256};

/// A normalized path fragment matching `com.google.devtools.build.lib.vfs.PathFragment`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathFragment {
    normalized: String,
    drive_str_len: u8,
}

impl PathFragment {
    pub const SEPARATOR_CHAR: char = '/';

    pub fn empty() -> Self {
        Self {
            normalized: String::new(),
            drive_str_len: 0,
        }
    }

    /// Creates a new normalized `PathFragment` from a raw string.
    pub fn create(raw: &str) -> Self {
        if raw.is_empty() {
            return Self::empty();
        }
        let mut replaced: String = raw
            .chars()
            .map(|c| if c == '\\' { '/' } else { c })
            .collect();
        if let Some(rest) = replaced.strip_prefix("//?/") {
            let rb = rest.as_bytes();
            if rb.len() >= 2 && rb[0].is_ascii_alphabetic() && rb[1] == b':' {
                replaced = rest.to_string();
            }
        }
        let bytes = replaced.as_bytes();

        let (prefix, drive_str_len, rest) = if bytes.len() >= 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes.len() == 2 || bytes[2] == b'/')
        {
            let drive = (bytes[0] as char).to_ascii_uppercase();
            let rest_slice = if bytes.len() >= 3 { &replaced[3..] } else { "" };
            (format!("{drive}:/"), 3u8, rest_slice)
        } else if let Some(stripped) = replaced.strip_prefix('/') {
            ("/".to_string(), 1u8, stripped)
        } else {
            (String::new(), 0u8, replaced.as_str())
        };

        let is_abs = drive_str_len > 0;
        let mut stack: Vec<&str> = Vec::new();
        for seg in rest.split('/') {
            if seg.is_empty() || seg == "." {
                continue;
            }
            if seg == ".." {
                if let Some(&last) = stack.last() {
                    if last != ".." {
                        stack.pop();
                        continue;
                    }
                }
                if !is_abs {
                    stack.push("..");
                }
            } else {
                stack.push(seg);
            }
        }

        let joined = stack.join("/");
        let normalized = if is_abs {
            format!("{prefix}{joined}")
        } else {
            joined
        };

        Self {
            normalized,
            drive_str_len,
        }
    }

    pub fn create_already_normalized(normalized: impl Into<String>) -> Self {
        let s = normalized.into();
        let bytes = s.as_bytes();
        let drive_str_len = if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'/'
        {
            3
        } else if s.starts_with('/') {
            1
        } else {
            0
        };
        Self {
            normalized: s,
            drive_str_len,
        }
    }

    pub fn path_string(&self) -> &str {
        &self.normalized
    }

    pub fn is_empty(&self) -> bool {
        self.normalized.is_empty()
    }

    pub fn is_absolute(&self) -> bool {
        self.drive_str_len > 0
    }

    pub fn drive_str_length(&self) -> usize {
        self.drive_str_len as usize
    }

    pub fn segments(&self) -> Vec<&str> {
        let body = &self.normalized[self.drive_str_length()..];
        if body.is_empty() {
            Vec::new()
        } else {
            body.split('/').collect()
        }
    }

    pub fn segment_count(&self) -> usize {
        self.segments().len()
    }

    pub fn is_multi_segment(&self) -> bool {
        self.segment_count() > 1
    }

    pub fn get_segment(&self, index: usize) -> Option<&str> {
        self.segments().into_iter().nth(index)
    }

    pub fn get_segment_checked(&self, index: usize) -> Result<&str, String> {
        self.segments()
            .into_iter()
            .nth(index)
            .ok_or_else(|| format!("Illegal segment index: {index}"))
    }

    pub fn sub_fragment(&self, begin_index: usize) -> Self {
        let segs = self.segments();
        if begin_index > segs.len() {
            panic!("path: {}, beginIndex: {}", self, begin_index);
        }
        if begin_index == 0 {
            return self.clone();
        }
        if begin_index == segs.len() {
            return Self::empty();
        }
        Self::create_already_normalized(segs[begin_index..].join("/"))
    }

    pub fn sub_fragment_range(&self, begin_index: usize, end_index: usize) -> Self {
        let segs = self.segments();
        if begin_index > end_index || end_index > segs.len() {
            panic!(
                "path: {}, beginIndex: {} endIndex: {}",
                self, begin_index, end_index
            );
        }
        if begin_index == 0 && self.is_absolute() {
            let prefix = &self.normalized[..self.drive_str_length()];
            Self::create_already_normalized(format!(
                "{}{}",
                prefix,
                segs[begin_index..end_index].join("/")
            ))
        } else {
            Self::create_already_normalized(segs[begin_index..end_index].join("/"))
        }
    }

    pub fn base_name(&self) -> &str {
        let body = &self.normalized[self.drive_str_length()..];
        if body.is_empty() {
            return "";
        }
        match body.rfind('/') {
            Some(idx) => &body[idx + 1..],
            None => body,
        }
    }

    pub fn parent_directory(&self) -> Option<Self> {
        let body = &self.normalized[self.drive_str_length()..];
        if body.is_empty() {
            return None;
        }
        let prefix = &self.normalized[..self.drive_str_length()];
        match body.rfind('/') {
            Some(idx) => Some(Self::create_already_normalized(format!(
                "{}{}",
                prefix,
                &body[..idx]
            ))),
            None => {
                if self.is_absolute() {
                    Some(Self::create_already_normalized(prefix))
                } else {
                    Some(Self::empty())
                }
            }
        }
    }

    pub fn get_relative(&self, other: &PathFragment) -> Self {
        if other.is_absolute() || self.is_empty() {
            return other.clone();
        }
        if other.is_empty() {
            return self.clone();
        }
        if self.normalized.ends_with('/') {
            Self::create(&format!("{}{}", self.normalized, other.normalized))
        } else {
            Self::create(&format!("{}/{}", self.normalized, other.normalized))
        }
    }

    pub fn get_relative_str(&self, other: &str) -> Self {
        self.get_relative(&Self::create(other))
    }

    pub fn starts_with(&self, other: &PathFragment) -> bool {
        if self.drive_str_len != other.drive_str_len {
            return false;
        }
        if other.normalized.len() > self.normalized.len() {
            return false;
        }
        if other.is_empty() {
            return true;
        }
        if self.normalized == other.normalized {
            return true;
        }
        if other.normalized.len() == other.drive_str_length() {
            // Other is root ("/" or "C:/")
            return self.normalized.starts_with(&other.normalized);
        }
        self.normalized.starts_with(&other.normalized)
            && self.normalized.as_bytes().get(other.normalized.len()) == Some(&b'/')
    }

    pub fn relative_to(&self, base: &PathFragment) -> Result<Self, String> {
        if !self.starts_with(base) {
            return Err(format!(
                "Path '{}' is not under '{}' , cannot relativize",
                self, base
            ));
        }
        if self.normalized == base.normalized {
            return Ok(Self::empty());
        }
        let start = if base.is_empty() || base.normalized.len() == base.drive_str_length() {
            base.normalized.len()
        } else {
            base.normalized.len() + 1
        };
        Ok(Self::create_already_normalized(&self.normalized[start..]))
    }

    pub fn common_prefix_length(&self, other: &Self) -> usize {
        self.normalized
            .bytes()
            .zip(other.normalized.bytes())
            .take_while(|(a, b)| a == b)
            .count()
    }

    /// Hierarchical comparator matching `PathFragment.HIERARCHICAL_COMPARATOR`.
    pub fn hierarchical_cmp(&self, other: &Self) -> Ordering {
        let k = self.common_prefix_length(other);
        let s1 = self.normalized.as_bytes();
        let s2 = other.normalized.as_bytes();
        if k == s1.len() || k == s2.len() {
            return s1.len().cmp(&s2.len());
        }
        let c1 = s1[k];
        let c2 = s2[k];
        if c1 == b'/' {
            return Ordering::Less;
        }
        if c2 == b'/' {
            return Ordering::Greater;
        }
        c1.cmp(&c2)
    }

    pub fn contains_uplevel_references(&self) -> bool {
        self.normalized == ".." || self.normalized.starts_with("../")
    }
}

impl fmt::Debug for PathFragment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PathFragment({:?})", self.normalized)
    }
}

impl fmt::Display for PathFragment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.normalized)
    }
}

impl From<&str> for PathFragment {
    fn from(s: &str) -> Self {
        Self::create(s)
    }
}

/// Digest hash function used by the VFS (`SHA-256` or `MD5`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DigestHashFunction {
    #[default]
    Sha256,
    Md5,
}

impl DigestHashFunction {
    pub fn hash_bytes(&self, data: &[u8]) -> Vec<u8> {
        match self {
            Self::Sha256 => sha256::digest(data).to_vec(),
            Self::Md5 => md5::digest(data).to_vec(),
        }
    }

    pub fn hex_hash_bytes(&self, data: &[u8]) -> String {
        zel_util::to_hex(&self.hash_bytes(data))
    }
}

/// Metadata for a file or directory in the VFS, matching `FileStatus.java`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStatus {
    pub is_file: bool,
    pub is_directory: bool,
    pub is_symlink: bool,
    pub is_special_file: bool,
    pub size: u64,
    pub last_modified_time_ms: u64,
    pub node_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DirentType {
    File,
    Directory,
    Symlink,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dirent {
    pub name: String,
    pub dirent_type: DirentType,
}

/// Virtual File System abstraction matching `com.google.devtools.build.lib.vfs.FileSystem`.
pub trait FileSystem: Send + Sync {
    fn digest_hash_function(&self) -> DigestHashFunction;
    fn stat(&self, path: &PathFragment, follow_symlinks: bool) -> io::Result<FileStatus>;
    fn exists(&self, path: &PathFragment, follow_symlinks: bool) -> bool {
        self.stat(path, follow_symlinks).is_ok()
    }
    fn read_file(&self, path: &PathFragment) -> io::Result<Vec<u8>>;
    fn write_file(&self, path: &PathFragment, content: &[u8]) -> io::Result<()>;
    fn create_directory(&self, path: &PathFragment) -> io::Result<bool>;
    fn create_directory_and_parents(&self, path: &PathFragment) -> io::Result<()>;
    fn readdir(&self, path: &PathFragment) -> io::Result<Vec<Dirent>>;
    fn delete(&self, path: &PathFragment) -> io::Result<bool>;
    fn rename(&self, source: &PathFragment, target: &PathFragment) -> io::Result<()>;
    fn create_symbolic_link(
        &self,
        link_path: &PathFragment,
        target_fragment: &PathFragment,
    ) -> io::Result<()>;
    fn read_symbolic_link(&self, link_path: &PathFragment) -> io::Result<PathFragment>;

    fn get_digest(&self, path: &PathFragment) -> io::Result<Vec<u8>> {
        let bytes = self.read_file(path)?;
        Ok(self.digest_hash_function().hash_bytes(&bytes))
    }
}

/// Host OS filesystem implementation.
#[derive(Debug, Clone, Default)]
pub struct LocalFileSystem {
    hash_function: DigestHashFunction,
}

impl LocalFileSystem {
    pub fn new(hash_function: DigestHashFunction) -> Self {
        Self { hash_function }
    }

    fn to_std_path(path: &PathFragment) -> StdPathBuf {
        StdPathBuf::from(path.path_string())
    }
}

impl FileSystem for LocalFileSystem {
    fn digest_hash_function(&self) -> DigestHashFunction {
        self.hash_function
    }

    fn stat(&self, path: &PathFragment, follow_symlinks: bool) -> io::Result<FileStatus> {
        let std_path = Self::to_std_path(path);
        let meta = if follow_symlinks {
            fs::metadata(&std_path)?
        } else {
            fs::symlink_metadata(&std_path)?
        };
        let ft = meta.file_type();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let node_id = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                meta.ino() ^ meta.dev().rotate_left(32)
            }
            #[cfg(not(unix))]
            {
                0
            }
        };
        Ok(FileStatus {
            is_file: ft.is_file(),
            is_directory: ft.is_dir(),
            is_symlink: ft.is_symlink(),
            is_special_file: !ft.is_file() && !ft.is_dir() && !ft.is_symlink(),
            size: meta.len(),
            last_modified_time_ms: mtime,
            node_id,
        })
    }

    fn read_file(&self, path: &PathFragment) -> io::Result<Vec<u8>> {
        fs::read(Self::to_std_path(path))
    }

    fn write_file(&self, path: &PathFragment, content: &[u8]) -> io::Result<()> {
        fs::write(Self::to_std_path(path), content)
    }

    fn create_directory(&self, path: &PathFragment) -> io::Result<bool> {
        let p = Self::to_std_path(path);
        if p.is_dir() {
            return Ok(false);
        }
        fs::create_dir(&p)?;
        Ok(true)
    }

    fn create_directory_and_parents(&self, path: &PathFragment) -> io::Result<()> {
        fs::create_dir_all(Self::to_std_path(path))
    }

    fn readdir(&self, path: &PathFragment) -> io::Result<Vec<Dirent>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(Self::to_std_path(path))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let ft = entry.file_type()?;
            let dirent_type = if ft.is_symlink() {
                DirentType::Symlink
            } else if ft.is_dir() {
                DirentType::Directory
            } else if ft.is_file() {
                DirentType::File
            } else {
                DirentType::Unknown
            };
            entries.push(Dirent { name, dirent_type });
        }
        entries.sort();
        Ok(entries)
    }

    fn delete(&self, path: &PathFragment) -> io::Result<bool> {
        let p = Self::to_std_path(path);
        match fs::symlink_metadata(&p) {
            Ok(meta) => {
                if meta.is_dir() {
                    fs::remove_dir(&p)?;
                } else {
                    fs::remove_file(&p)?;
                }
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn rename(&self, source: &PathFragment, target: &PathFragment) -> io::Result<()> {
        fs::rename(Self::to_std_path(source), Self::to_std_path(target))
    }

    fn create_symbolic_link(
        &self,
        link_path: &PathFragment,
        target_fragment: &PathFragment,
    ) -> io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                StdPath::new(target_fragment.path_string()),
                Self::to_std_path(link_path),
            )
        }
        #[cfg(not(unix))]
        {
            let _ = (link_path, target_fragment);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "symlinks not supported on this platform",
            ))
        }
    }

    fn read_symbolic_link(&self, link_path: &PathFragment) -> io::Result<PathFragment> {
        let target = fs::read_link(Self::to_std_path(link_path))?;
        Ok(PathFragment::create(&target.to_string_lossy()))
    }
}

#[derive(Debug, Clone)]
enum InMemoryNode {
    File {
        content: Vec<u8>,
        mtime_ms: u64,
        node_id: u64,
    },
    Directory {
        mtime_ms: u64,
        node_id: u64,
    },
    Symlink {
        target: PathFragment,
        mtime_ms: u64,
        node_id: u64,
    },
}

/// Thread-safe in-memory VFS matching `com.google.devtools.build.lib.vfs.inmemoryfs.InMemoryFileSystem`.
#[derive(Debug, Clone)]
pub struct InMemoryFileSystem {
    hash_function: DigestHashFunction,
    nodes: Arc<RwLock<BTreeMap<PathFragment, InMemoryNode>>>,
    next_node_id: Arc<AtomicU64>,
}

impl Default for InMemoryFileSystem {
    fn default() -> Self {
        Self::new(DigestHashFunction::Sha256)
    }
}

impl InMemoryFileSystem {
    pub fn new(hash_function: DigestHashFunction) -> Self {
        let mut map = BTreeMap::new();
        map.insert(
            PathFragment::create("/"),
            InMemoryNode::Directory {
                mtime_ms: 0,
                node_id: 1,
            },
        );
        Self {
            hash_function,
            nodes: Arc::new(RwLock::new(map)),
            next_node_id: Arc::new(AtomicU64::new(2)),
        }
    }

    fn alloc_id(&self) -> u64 {
        self.next_node_id.fetch_add(1, AtomicOrdering::Relaxed)
    }

    fn resolve_path(
        map: &BTreeMap<PathFragment, InMemoryNode>,
        path: &PathFragment,
        follow_symlinks: bool,
    ) -> io::Result<PathFragment> {
        let mut depth = 0usize;
        Self::resolve_path_depth(map, path, follow_symlinks, &mut depth)
    }

    fn resolve_path_depth(
        map: &BTreeMap<PathFragment, InMemoryNode>,
        path: &PathFragment,
        follow_last: bool,
        depth: &mut usize,
    ) -> io::Result<PathFragment> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("path must be absolute: {path}"),
            ));
        }
        let segs = path.segments();
        let prefix = &path.path_string()[..path.drive_str_length()];
        let mut cur = PathFragment::create_already_normalized(prefix);
        for (idx, seg) in segs.iter().enumerate() {
            let is_last = idx + 1 == segs.len();
            cur = cur.get_relative_str(seg);
            if !is_last || follow_last {
                while let Some(InMemoryNode::Symlink { target, .. }) = map.get(&cur) {
                    *depth += 1;
                    if *depth > 40 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "too many levels of symbolic links",
                        ));
                    }
                    let next = if target.is_absolute() {
                        target.clone()
                    } else if let Some(parent) = cur.parent_directory() {
                        parent.get_relative(target)
                    } else {
                        cur.get_relative(target)
                    };
                    cur = Self::resolve_path_depth(map, &next, true, depth)?;
                }
            }
        }
        Ok(cur)
    }
}

impl FileSystem for InMemoryFileSystem {
    fn digest_hash_function(&self) -> DigestHashFunction {
        self.hash_function
    }

    fn stat(&self, path: &PathFragment, follow_symlinks: bool) -> io::Result<FileStatus> {
        let map = self.nodes.read().unwrap();
        let resolved = Self::resolve_path(&map, path, follow_symlinks)?;
        match map.get(&resolved) {
            Some(InMemoryNode::File {
                content,
                mtime_ms,
                node_id,
            }) => Ok(FileStatus {
                is_file: true,
                is_directory: false,
                is_symlink: false,
                is_special_file: false,
                size: content.len() as u64,
                last_modified_time_ms: *mtime_ms,
                node_id: *node_id,
            }),
            Some(InMemoryNode::Directory { mtime_ms, node_id }) => Ok(FileStatus {
                is_file: false,
                is_directory: true,
                is_symlink: false,
                is_special_file: false,
                size: 0,
                last_modified_time_ms: *mtime_ms,
                node_id: *node_id,
            }),
            Some(InMemoryNode::Symlink {
                target,
                mtime_ms,
                node_id,
            }) => Ok(FileStatus {
                is_file: false,
                is_directory: false,
                is_symlink: true,
                is_special_file: false,
                size: target.path_string().len() as u64,
                last_modified_time_ms: *mtime_ms,
                node_id: *node_id,
            }),
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("No such file or directory: {path}"),
            )),
        }
    }

    fn read_file(&self, path: &PathFragment) -> io::Result<Vec<u8>> {
        let map = self.nodes.read().unwrap();
        let resolved = Self::resolve_path(&map, path, true)?;
        match map.get(&resolved) {
            Some(InMemoryNode::File { content, .. }) => Ok(content.clone()),
            Some(InMemoryNode::Directory { .. }) => Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                format!("Is a directory: {path}"),
            )),
            _ => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("No such file: {path}"),
            )),
        }
    }

    fn write_file(&self, path: &PathFragment, content: &[u8]) -> io::Result<()> {
        let id = self.alloc_id();
        let mut map = self.nodes.write().unwrap();
        if let Some(parent) = path.parent_directory() {
            let resolved_parent = Self::resolve_path(&map, &parent, true)?;
            if !map.contains_key(&resolved_parent) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Parent directory does not exist: {parent}"),
                ));
            }
        }
        map.insert(
            path.clone(),
            InMemoryNode::File {
                content: content.to_vec(),
                mtime_ms: 0,
                node_id: id,
            },
        );
        Ok(())
    }

    fn create_directory(&self, path: &PathFragment) -> io::Result<bool> {
        let id = self.alloc_id();
        let mut map = self.nodes.write().unwrap();
        if let Some(node) = map.get(path) {
            return match node {
                InMemoryNode::Directory { .. } => Ok(false),
                _ => Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("File exists: {path}"),
                )),
            };
        }
        if let Some(parent) = path.parent_directory() {
            let resolved_parent = Self::resolve_path(&map, &parent, true)?;
            if !map.contains_key(&resolved_parent) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Parent directory does not exist: {parent}"),
                ));
            }
        }
        map.insert(
            path.clone(),
            InMemoryNode::Directory {
                mtime_ms: 0,
                node_id: id,
            },
        );
        Ok(true)
    }

    fn create_directory_and_parents(&self, path: &PathFragment) -> io::Result<()> {
        let segs = path.segments();
        let prefix = &path.path_string()[..path.drive_str_length()];
        let mut curr = PathFragment::create(prefix);
        self.create_directory(&curr)?;
        for seg in segs {
            curr = curr.get_relative_str(seg);
            self.create_directory(&curr)?;
        }
        Ok(())
    }

    fn readdir(&self, path: &PathFragment) -> io::Result<Vec<Dirent>> {
        let map = self.nodes.read().unwrap();
        let resolved = Self::resolve_path(&map, path, true)?;
        match map.get(&resolved) {
            Some(InMemoryNode::Directory { .. }) => {}
            Some(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    format!("Not a directory: {path}"),
                ))
            }
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("No such directory: {path}"),
                ))
            }
        }
        let mut entries = Vec::new();
        for (k, node) in map.iter() {
            if k != &resolved && k.parent_directory().as_ref() == Some(&resolved) {
                let dirent_type = match node {
                    InMemoryNode::File { .. } => DirentType::File,
                    InMemoryNode::Directory { .. } => DirentType::Directory,
                    InMemoryNode::Symlink { .. } => DirentType::Symlink,
                };
                entries.push(Dirent {
                    name: k.base_name().to_string(),
                    dirent_type,
                });
            }
        }
        entries.sort();
        Ok(entries)
    }

    fn delete(&self, path: &PathFragment) -> io::Result<bool> {
        let mut map = self.nodes.write().unwrap();
        Ok(map.remove(path).is_some())
    }

    fn rename(&self, source: &PathFragment, target: &PathFragment) -> io::Result<()> {
        let mut map = self.nodes.write().unwrap();
        let node = map.remove(source).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("No such file: {source}"))
        })?;
        map.insert(target.clone(), node);
        Ok(())
    }

    fn create_symbolic_link(
        &self,
        link_path: &PathFragment,
        target_fragment: &PathFragment,
    ) -> io::Result<()> {
        let id = self.alloc_id();
        let mut map = self.nodes.write().unwrap();
        map.insert(
            link_path.clone(),
            InMemoryNode::Symlink {
                target: target_fragment.clone(),
                mtime_ms: 0,
                node_id: id,
            },
        );
        Ok(())
    }

    fn read_symbolic_link(&self, link_path: &PathFragment) -> io::Result<PathFragment> {
        let map = self.nodes.read().unwrap();
        let resolved = Self::resolve_path(&map, link_path, false)?;
        match map.get(&resolved) {
            Some(InMemoryNode::Symlink { target, .. }) => Ok(target.clone()),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Not a symlink: {link_path}"),
            )),
        }
    }
}

/// Root + relative `PathFragment` pair matching `com.google.devtools.build.lib.vfs.RootedPath`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RootedPath {
    root: PathFragment,
    root_relative_path: PathFragment,
}

impl RootedPath {
    pub fn to_rooted_path(
        root: PathFragment,
        root_relative_path: PathFragment,
    ) -> Result<Self, String> {
        if !root.is_absolute() {
            return Err(format!("root must be absolute: {root}"));
        }
        if root_relative_path.is_absolute() {
            return Err(format!(
                "root_relative_path must be relative: {root_relative_path}"
            ));
        }
        Ok(Self {
            root,
            root_relative_path,
        })
    }

    pub fn root(&self) -> &PathFragment {
        &self.root
    }

    pub fn root_relative_path(&self) -> &PathFragment {
        &self.root_relative_path
    }

    pub fn as_path(&self) -> PathFragment {
        self.root.get_relative(&self.root_relative_path)
    }
}

/// Evaluates Bazel `glob(include, exclude, exclude_directories)` patterns over a `FileSystem`,
/// matching `com.google.devtools.build.lib.vfs.UnixGlob`.
pub fn evaluate_glob(
    fs: &dyn FileSystem,
    package_dir: &PathFragment,
    include_patterns: &[String],
    exclude_patterns: &[String],
    exclude_directories: bool,
) -> io::Result<Vec<String>> {
    let mut all_rel_paths = Vec::new();
    collect_package_files_recursive(
        fs,
        package_dir,
        &PathFragment::empty(),
        exclude_directories,
        &mut all_rel_paths,
    )?;

    let mut matched = Vec::new();
    for rel in &all_rel_paths {
        let included = include_patterns.iter().any(|pat| glob_matches(pat, rel));
        if !included {
            continue;
        }
        let excluded = exclude_patterns.iter().any(|pat| glob_matches(pat, rel));
        if !excluded {
            matched.push(rel.clone());
        }
    }
    matched.sort();
    matched.dedup();
    Ok(matched)
}

fn collect_package_files_recursive(
    fs: &dyn FileSystem,
    package_dir: &PathFragment,
    rel_dir: &PathFragment,
    exclude_directories: bool,
    out: &mut Vec<String>,
) -> io::Result<()> {
    let abs_dir = package_dir.get_relative(rel_dir);
    let entries = match fs.readdir(&abs_dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries {
        let child_rel = rel_dir.get_relative_str(&entry.name);
        let child_abs = package_dir.get_relative(&child_rel);
        if entry.dirent_type == DirentType::Directory {
            // Skip subpackages (directories containing BUILD or BUILD.bazel)
            if fs.exists(&child_abs.get_relative_str("BUILD"), true)
                || fs.exists(&child_abs.get_relative_str("BUILD.bazel"), true)
            {
                continue;
            }
            if !exclude_directories {
                out.push(child_rel.path_string().to_string());
            }
            collect_package_files_recursive(fs, package_dir, &child_rel, exclude_directories, out)?;
        } else {
            out.push(child_rel.path_string().to_string());
        }
    }
    Ok(())
}

/// Matches a Bazel glob pattern (`*`, `**`, `?`) against a relative path.
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    let pat_segs: Vec<&str> = if pattern.is_empty() {
        Vec::new()
    } else {
        pattern.split('/').collect()
    };
    let path_segs: Vec<&str> = if path.is_empty() {
        Vec::new()
    } else {
        path.split('/').collect()
    };
    glob_match_segments(&pat_segs, &path_segs)
}

fn glob_match_segments(pat: &[&str], path: &[&str]) -> bool {
    if pat.is_empty() {
        return path.is_empty();
    }
    if pat[0] == "**" {
        if glob_match_segments(&pat[1..], path) {
            return true;
        }
        if !path.is_empty() && glob_match_segments(pat, &path[1..]) {
            return true;
        }
        return false;
    }
    if path.is_empty() {
        return false;
    }
    segment_wildcard_match(pat[0].as_bytes(), path[0].as_bytes())
        && glob_match_segments(&pat[1..], &path[1..])
}

fn segment_wildcard_match(pat: &[u8], text: &[u8]) -> bool {
    if pat.is_empty() {
        return text.is_empty();
    }
    if pat[0] == b'*' {
        for skip in 0..=text.len() {
            if segment_wildcard_match(&pat[1..], &text[skip..]) {
                return true;
            }
        }
        false
    } else if pat[0] == b'?' {
        !text.is_empty() && segment_wildcard_match(&pat[1..], &text[1..])
    } else {
        !text.is_empty() && pat[0] == text[0] && segment_wildcard_match(&pat[1..], &text[1..])
    }
}

/// Symlink-resolution error matching Bazel's `FileSymlinkCycleException` /
/// `FileSymlinkInfiniteExpansionException` / `IOException`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymlinkError {
    Cycle { chain: Vec<PathFragment> },
    InfiniteExpansion { chain: Vec<PathFragment> },
    Io(String),
}

impl fmt::Display for SymlinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cycle { chain } => {
                let s: Vec<&str> = chain.iter().map(|p| p.path_string()).collect();
                write!(f, "Symlink cycle: {}", s.join("- > "))
            }
            Self::InfiniteExpansion { chain } => {
                let s: Vec<&str> = chain.iter().map(|p| p.path_string()).collect();
                write!(f, "Infinite symlink expansion: {}", s.join("- > "))
            }
            Self::Io(m) => f.write_str(m),
        }
    }
}

/// Result of resolving `path` with full Bazel `FileFunction` symlink cycle and ancestor/descendant
/// infinite-expansion tracking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymlinkResolution {
    /// `None` if the resolved target does not exist (e.g. a dangling symlink or missing path).
    pub status: Option<FileStatus>,
    /// Real path after following symlinks (when resolvable).
    pub real_path: PathFragment,
    /// `true` if the original path itself was a symlink (`stat(..., false).is_symlink`).
    pub is_symlink: bool,
    /// Non-empty if resolution encountered a symlink pointing to an ancestor of a previously
    /// visited path in the chain (`unboundedAncestorSymlinkExpansionChain`).
    pub unbounded_ancestor_chain: Option<Vec<PathFragment>>,
}

struct SymlinkResolutionState {
    logical_chain: Vec<PathFragment>,
    sorted_logical_chain: BTreeSet<PathFragment>,
    unbounded_ancestor_chain: Option<Vec<PathFragment>>,
}

impl SymlinkResolutionState {
    fn new() -> Self {
        Self {
            logical_chain: Vec::new(),
            sorted_logical_chain: BTreeSet::new(),
            unbounded_ancestor_chain: None,
        }
    }

    fn check_path(&mut self, path: &PathFragment, note: bool) -> Result<(), SymlinkError> {
        if self.sorted_logical_chain.contains(path) {
            let pos = self
                .logical_chain
                .iter()
                .position(|p| p == path)
                .unwrap_or(0);
            return Err(SymlinkError::Cycle {
                chain: self.logical_chain[pos..].to_vec(),
            });
        }
        if let Some(floor) = self.sorted_logical_chain.range(..path.clone()).next_back() {
            if path.starts_with(floor) {
                let pos = self
                    .logical_chain
                    .iter()
                    .position(|p| p == floor)
                    .unwrap_or(0);
                let mut chain = self.logical_chain[pos..].to_vec();
                chain.push(path.clone());
                return Err(SymlinkError::InfiniteExpansion { chain });
            }
        }
        if let Some(ceil) = self.sorted_logical_chain.range(path.clone()..).next() {
            if ceil.starts_with(path) && self.unbounded_ancestor_chain.is_none() {
                let pos = self
                    .logical_chain
                    .iter()
                    .position(|p| p == ceil)
                    .unwrap_or(0);
                let mut chain = self.logical_chain[pos..].to_vec();
                chain.push(path.clone());
                self.unbounded_ancestor_chain = Some(chain);
            }
        }
        if note {
            self.sorted_logical_chain.insert(path.clone());
            self.logical_chain.push(path.clone());
        }
        Ok(())
    }
}

struct PartialFileValue {
    real_path: PathFragment,
    status: Option<FileStatus>,
    is_symlink: bool,
    logical_chain_during_resolution: Vec<PathFragment>,
}

fn resolve_symlink_inner(
    fs: &dyn FileSystem,
    path: &PathFragment,
    state: &mut SymlinkResolutionState,
    depth: usize,
) -> Result<PartialFileValue, SymlinkError> {
    if depth > 64 {
        return Err(SymlinkError::Io(
            "too many levels of symbolic links".to_string(),
        ));
    }
    let (mut cur_path, mut cur_status, parent_chain) = match path.parent_directory() {
        Some(parent) => {
            let mut parent_state = SymlinkResolutionState::new();
            let parent_val = resolve_symlink_inner(fs, &parent, &mut parent_state, depth + 1)?;
            if state.unbounded_ancestor_chain.is_none() {
                state.unbounded_ancestor_chain = parent_state.unbounded_ancestor_chain;
            }
            let base = path.base_name();
            let from_ancestors = parent_val.real_path.get_relative_str(base);
            if !parent_val
                .status
                .as_ref()
                .map(|s| s.is_directory)
                .unwrap_or(false)
            {
                return Ok(PartialFileValue {
                    real_path: from_ancestors,
                    status: None,
                    is_symlink: false,
                    logical_chain_during_resolution: Vec::new(),
                });
            }
            for p in &parent_val.logical_chain_during_resolution {
                state.check_path(&p.get_relative_str(base), true)?;
            }
            let st = match fs.stat(&from_ancestors, false) {
                Ok(s) => Some(s),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(SymlinkError::Io(e.to_string())),
            };
            let chain: Vec<PathFragment> = parent_val
                .logical_chain_during_resolution
                .into_iter()
                .map(|p| p.get_relative_str(base))
                .collect();
            (from_ancestors, st, chain)
        }
        None => {
            state.check_path(path, true)?;
            let st = match fs.stat(path, false) {
                Ok(s) => Some(s),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(SymlinkError::Io(e.to_string())),
            };
            (path.clone(), st, vec![path.clone()])
        }
    };

    let mut is_symlink = false;
    let mut chain_accum = parent_chain;
    let mut cur_depth = depth;
    while let Some(st) = &cur_status {
        if !st.is_symlink {
            break;
        }
        is_symlink = true;
        cur_depth += 1;
        if cur_depth > 64 {
            return Err(SymlinkError::Io(
                "too many levels of symbolic links".to_string(),
            ));
        }
        let target = fs
            .read_symbolic_link(&cur_path)
            .map_err(|e| SymlinkError::Io(e.to_string()))?;
        let next = if target.is_absolute() {
            target
        } else if let Some(parent) = cur_path.parent_directory() {
            parent.get_relative(&target)
        } else {
            cur_path.get_relative(&target)
        };
        state.check_path(&next, false)?;
        let resolved_next = resolve_symlink_inner(fs, &next, state, cur_depth)?;
        cur_path = resolved_next.real_path;
        cur_status = resolved_next.status;
        chain_accum.extend(resolved_next.logical_chain_during_resolution);
    }

    Ok(PartialFileValue {
        real_path: cur_path,
        status: cur_status,
        is_symlink,
        logical_chain_during_resolution: chain_accum,
    })
}

/// Resolves `path` in `fs` following Bazel's `FileFunction` symlink cycle and ancestor/descendant
/// infinite-expansion detection.
pub fn resolve_symlink_chain(
    fs: &dyn FileSystem,
    path: &PathFragment,
) -> Result<SymlinkResolution, SymlinkError> {
    let mut state = SymlinkResolutionState::new();
    let v = resolve_symlink_inner(fs, path, &mut state, 0)?;
    Ok(SymlinkResolution {
        status: v.status,
        real_path: v.real_path,
        is_symlink: v.is_symlink,
        unbounded_ancestor_chain: state.unbounded_ancestor_chain,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_fragment_normalization() {
        assert_eq!(PathFragment::create("").path_string(), "");
        assert_eq!(PathFragment::create("/").path_string(), "/");
        assert_eq!(PathFragment::create("///a//b/./c/").path_string(), "/a/b/c");
        assert_eq!(PathFragment::create("a/b/../c").path_string(), "a/c");
        assert_eq!(
            PathFragment::create("../a/../../b").path_string(),
            "../../b"
        );
        assert_eq!(PathFragment::create("/a/../../b").path_string(), "/b");
        assert_eq!(
            PathFragment::create("c:\\foo\\bar\\").path_string(),
            "C:/foo/bar"
        );
        assert_eq!(PathFragment::create("c:/foo").drive_str_length(), 3);
    }

    #[test]
    fn test_path_fragment_operations() {
        let p = PathFragment::create("/workspace/src/main/java");
        assert!(p.is_absolute());
        assert_eq!(p.base_name(), "java");
        assert_eq!(
            p.parent_directory().unwrap().path_string(),
            "/workspace/src/main"
        );
        assert!(p.starts_with(&PathFragment::create("/workspace/src")));
        assert!(!p.starts_with(&PathFragment::create("/workspace/sr")));
        assert_eq!(
            p.relative_to(&PathFragment::create("/workspace"))
                .unwrap()
                .path_string(),
            "src/main/java"
        );
        // sub_fragment(0) preserves absolute drive, (0, 0) on abs is root
        assert_eq!(p.sub_fragment(0).path_string(), "/workspace/src/main/java");
        assert_eq!(p.sub_fragment(1).path_string(), "src/main/java");
        assert_eq!(p.sub_fragment(4).path_string(), "");
        assert_eq!(PathFragment::create("/").sub_fragment(0).path_string(), "/");
        assert_eq!(p.sub_fragment_range(0, 0).path_string(), "/");
        assert_eq!(p.sub_fragment_range(0, 2).path_string(), "/workspace/src");
        assert_eq!(p.sub_fragment_range(1, 3).path_string(), "src/main");
        assert_eq!(
            PathFragment::create("C:/a/b")
                .sub_fragment_range(0, 0)
                .path_string(),
            "C:/"
        );
        assert!(p.get_segment_checked(5).is_err());
    }

    #[test]
    #[should_panic(expected = "endIndex")]
    fn test_sub_fragment_range_oob_panics() {
        let p = PathFragment::create("/a/b");
        let _ = p.sub_fragment_range(0, 5);
    }

    #[test]
    fn test_hierarchical_comparator() {
        let p1 = PathFragment::create("foo/bar/quz");
        let p2 = PathFragment::create("foo/bar-baz/quz");
        assert_eq!(p1.hierarchical_cmp(&p2), Ordering::Less);
    }

    #[test]
    fn test_in_memory_fs() {
        let fs = InMemoryFileSystem::default();
        let dir = PathFragment::create("/workspace/pkg");
        fs.create_directory_and_parents(&dir).unwrap();
        let build_file = dir.get_relative_str("BUILD");
        fs.write_file(&build_file, b"cc_binary(name = 'app')")
            .unwrap();
        assert!(fs.exists(&build_file, true));
        assert_eq!(
            fs.read_file(&build_file).unwrap(),
            b"cc_binary(name = 'app')"
        );
        let entries = fs.readdir(&dir).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "BUILD");
        // Distinct node_ids for distinct directories/files.
        let dir2 = PathFragment::create("/workspace/pkg2");
        fs.create_directory_and_parents(&dir2).unwrap();
        assert_ne!(
            fs.stat(&dir, true).unwrap().node_id,
            fs.stat(&dir2, true).unwrap().node_id
        );
    }

    #[test]
    fn test_resolve_symlink_chain_detects_cycle_descendant_and_ancestor() {
        let fs = InMemoryFileSystem::default();
        fs.create_directory_and_parents(&PathFragment::create("/ws/pkg"))
            .unwrap();
        // Cycle: a -> b, b -> a
        fs.create_symbolic_link(
            &PathFragment::create("/ws/pkg/a"),
            &PathFragment::create("b"),
        )
        .unwrap();
        fs.create_symbolic_link(
            &PathFragment::create("/ws/pkg/b"),
            &PathFragment::create("a"),
        )
        .unwrap();
        assert!(matches!(
            resolve_symlink_chain(&fs, &PathFragment::create("/ws/pkg/a")),
            Err(SymlinkError::Cycle { .. })
        ));
        // Descendant: desc -> /ws/pkg/desc/sub
        fs.create_symbolic_link(
            &PathFragment::create("/ws/pkg/desc"),
            &PathFragment::create("/ws/pkg/desc/sub"),
        )
        .unwrap();
        assert!(matches!(
            resolve_symlink_chain(&fs, &PathFragment::create("/ws/pkg/desc")),
            Err(SymlinkError::InfiniteExpansion { .. })
        ));
        // Ancestor: anc -> ..
        fs.create_symbolic_link(
            &PathFragment::create("/ws/pkg/anc"),
            &PathFragment::create(".."),
        )
        .unwrap();
        let res = resolve_symlink_chain(&fs, &PathFragment::create("/ws/pkg/anc")).unwrap();
        assert!(res.status.unwrap().is_directory);
        assert!(res.unbounded_ancestor_chain.is_some());
    }
}
