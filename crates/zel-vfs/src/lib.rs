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
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zel_util::{md5, sha1, sha256, Os};

pub mod digest_utils;
pub mod fs_utils;

pub use digest_utils::*;
pub use fs_utils::{
    file_system_utils, BasicActionInput, FileSystemHandle, Root, SearchPath, VfsPath,
};

/// Host path normalization policy matching `OsPathPolicy` (`UnixOsPathPolicy` vs
/// `WindowsOsPathPolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OsPathPolicy {
    Unix,
    Windows,
}

impl OsPathPolicy {
    pub const fn host() -> Self {
        match Os::current() {
            Os::Windows => Self::Windows,
            _ => Self::Unix,
        }
    }

    pub const fn is_case_sensitive(self) -> bool {
        matches!(self, Self::Unix)
    }

    pub const fn separator(self) -> char {
        match self {
            Self::Unix => '/',
            Self::Windows => '\\',
        }
    }
}

/// Iterator over raw path segments matching `com.google.devtools.build.lib.vfs.PathSegmentIterator`:
/// trailing `/` yields no empty segment, and scanning starts at `start + 1` so an extra `/` at the
/// start of the next segment stays attached to that segment (`"a//b"` -> `["a", "/b"]`).
#[derive(Debug, Clone)]
pub struct PathSegmentIterator<'a> {
    path: &'a str,
    start: usize,
}

impl<'a> PathSegmentIterator<'a> {
    pub fn new(path: &'a str, drive_str_length: usize) -> Self {
        Self {
            path,
            start: drive_str_length.min(path.len()),
        }
    }
}

impl<'a> Iterator for PathSegmentIterator<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if self.start >= self.path.len() {
            return None;
        }
        let search_from = (self.start + 1).min(self.path.len());
        match self.path[search_from..].find('/') {
            Some(rel_idx) => {
                let stop = search_from + rel_idx;
                let seg = &self.path[self.start..stop];
                self.start = stop + 1;
                Some(seg)
            }
            None => {
                let seg = &self.path[self.start..];
                self.start = self.path.len();
                Some(seg)
            }
        }
    }
}

/// Output file permission mode matching `com.google.devtools.build.lib.vfs.OutputPermissions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OutputPermissions {
    #[default]
    Readonly,
    Writable,
}

impl OutputPermissions {
    pub const fn from_writable(experimental_writable_outputs: bool) -> Self {
        if experimental_writable_outputs {
            Self::Writable
        } else {
            Self::Readonly
        }
    }

    pub const fn permissions_mode(self) -> u32 {
        match self {
            Self::Readonly => 0o555,
            Self::Writable => 0o755,
        }
    }
}

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

        let (prefix, drive_str_len, rest) = if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'/'
        {
            let drive = (bytes[0] as char).to_ascii_uppercase();
            (format!("{drive}:/"), 3u8, &replaced[3..])
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

    /// Creates a `PathFragment` normalized according to `policy` (`PathFragment.createForOs`).
    /// On `OsPathPolicy::Unix`, `\` is a regular segment character and `c:/foo` is relative.
    pub fn create_for_os(raw: &str, policy: OsPathPolicy) -> Self {
        if policy == OsPathPolicy::Windows {
            return Self::create(raw);
        }
        if raw.is_empty() {
            return Self::empty();
        }
        let (prefix, drive_str_len, rest) = if let Some(stripped) = raw.strip_prefix('/') {
            ("/", 1u8, stripped)
        } else {
            ("", 0u8, raw)
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

    pub fn get_relative_for_os(&self, other: &str, policy: OsPathPolicy) -> Self {
        let other_frag = Self::create_for_os(other, policy);
        if other_frag.is_absolute() || self.is_empty() {
            return other_frag;
        }
        if other_frag.is_empty() {
            return self.clone();
        }
        if self.normalized.ends_with('/') {
            Self::create_for_os(
                &format!("{}{}", self.normalized, other_frag.normalized),
                policy,
            )
        } else {
            Self::create_for_os(
                &format!("{}/{}", self.normalized, other_frag.normalized),
                policy,
            )
        }
    }

    pub fn get_child(&self, base_name: &str) -> Result<Self, String> {
        self.get_child_for_os(base_name, OsPathPolicy::host())
    }

    pub fn get_child_for_os(&self, base_name: &str, policy: OsPathPolicy) -> Result<Self, String> {
        if base_name.is_empty()
            || base_name == "."
            || base_name == ".."
            || base_name.contains('/')
            || (policy == OsPathPolicy::Windows && base_name.contains('\\'))
        {
            return Err(format!(
                "baseName must be a non-empty single segment: '{base_name}'"
            ));
        }
        let s = if self.is_empty() {
            base_name.to_string()
        } else if self.normalized.ends_with('/') {
            format!("{}{base_name}", self.normalized)
        } else {
            format!("{}/{base_name}", self.normalized)
        };
        Ok(Self {
            normalized: s,
            drive_str_len: self.drive_str_len,
        })
    }

    pub fn drive_str(&self) -> &str {
        &self.normalized[..self.drive_str_length()]
    }

    pub fn to_relative(&self) -> Self {
        Self::create_already_normalized(&self.normalized[self.drive_str_length()..])
    }

    pub fn strip_components(&self, num: isize) -> Result<Self, String> {
        if num < 0 {
            return Err(format!("numSegments must be non-negative: {num}"));
        }
        let n = num as usize;
        let segs = self.segments();
        if n >= segs.len() {
            return Ok(Self::empty());
        }
        Ok(Self::create_already_normalized(segs[n..].join("/")))
    }

    pub fn is_single_segment(&self) -> bool {
        let body = &self.normalized[self.drive_str_length()..];
        !body.is_empty() && !body.contains('/')
    }

    pub fn ends_with(&self, other: &PathFragment) -> bool {
        if other.normalized.len() > self.normalized.len() {
            return false;
        }
        if other.is_absolute() {
            return self == other;
        }
        if other.is_empty() {
            return true;
        }
        let body = &self.normalized[self.drive_str_length()..];
        if body.len() < other.normalized.len() || !body.ends_with(&other.normalized) {
            return false;
        }
        let prefix_len = body.len() - other.normalized.len();
        prefix_len == 0 || body.as_bytes()[prefix_len - 1] == b'/'
    }

    pub fn starts_with_ignoring_case(&self, other: &PathFragment) -> bool {
        if self.drive_str_len != other.drive_str_len {
            return false;
        }
        if !str_eq_ignore_unicode_case(self.drive_str(), other.drive_str()) {
            return false;
        }
        if other.is_empty() {
            return true;
        }
        let self_segs = self.segments();
        let other_segs = other.segments();
        if other_segs.len() > self_segs.len() {
            return false;
        }
        self_segs
            .iter()
            .zip(other_segs.iter())
            .all(|(a, b)| str_eq_ignore_unicode_case(a, b))
    }

    pub fn file_extension(&self) -> &str {
        let last_slash = self
            .normalized
            .rfind('/')
            .map(|i| i as isize)
            .unwrap_or(-1)
            .max(self.drive_str_length() as isize - 1);
        match self.normalized.rfind('.') {
            Some(dot) if (dot as isize) > last_slash => &self.normalized[dot + 1..],
            _ => "",
        }
    }

    pub fn safe_path_string(&self) -> &str {
        if self.normalized.is_empty() {
            "."
        } else {
            &self.normalized
        }
    }

    pub fn callable_path_string(&self) -> String {
        self.callable_path_string_for_os(OsPathPolicy::host())
    }

    pub fn callable_path_string_for_os(&self, policy: OsPathPolicy) -> String {
        if self.normalized.is_empty() {
            return ".".to_string();
        }
        let base = if !self.is_absolute() && self.is_single_segment() {
            format!("./{}", self.normalized)
        } else {
            self.normalized.clone()
        };
        if policy == OsPathPolicy::Windows {
            base.replace('/', "\\")
        } else {
            base
        }
    }

    pub fn replace_name(&self, new_name: &str) -> Option<Self> {
        let parent = self.parent_directory()?;
        Some(parent.get_relative_str(new_name))
    }

    pub fn raw_segment_iter(raw: &str, drive_str_length: usize) -> PathSegmentIterator<'_> {
        PathSegmentIterator::new(raw, drive_str_length)
    }

    pub fn is_normalized_str(raw: &str, policy: OsPathPolicy) -> bool {
        Self::create_for_os(raw, policy).path_string() == raw
    }

    pub fn contains_separator_str(raw: &str, policy: OsPathPolicy) -> bool {
        raw.contains('/') || (policy == OsPathPolicy::Windows && raw.contains('\\'))
    }

    pub fn contains_uplevel_references_str(raw: &str, policy: OsPathPolicy) -> bool {
        let sep_pred = |c: char| c == '/' || (policy == OsPathPolicy::Windows && c == '\\');
        raw.split(sep_pred).any(|seg| seg == "..")
    }

    pub fn check_all_paths_are_under<'a, I>(
        paths: I,
        except_for_under: &PathFragment,
    ) -> Result<(), String>
    where
        I: IntoIterator<Item = &'a PathFragment>,
    {
        for p in paths {
            if !p.starts_with(except_for_under) {
                return Err(format!("Path '{p}' is not under '{except_for_under}'"));
            }
        }
        Ok(())
    }
}

fn str_eq_ignore_unicode_case(a: &str, b: &str) -> bool {
    let mut ca = a.chars();
    let mut cb = b.chars();
    loop {
        match (ca.next(), cb.next()) {
            (None, None) => return true,
            (Some(x), Some(y)) => {
                if x == y {
                    continue;
                }
                let xu: Vec<char> = x.to_uppercase().collect();
                let yu: Vec<char> = y.to_uppercase().collect();
                if xu != yu {
                    let xl: Vec<char> = x.to_lowercase().collect();
                    let yl: Vec<char> = y.to_lowercase().collect();
                    if xl != yl {
                        return false;
                    }
                }
            }
            _ => return false,
        }
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

/// Digest hash function used by the VFS (`SHA-256`, `SHA-1`, or `MD5`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DigestHashFunction {
    #[default]
    Sha256,
    Sha1,
    Md5,
}

impl DigestHashFunction {
    pub const fn canonical_name(&self) -> &'static str {
        match self {
            Self::Sha256 => "SHA-256",
            Self::Sha1 => "SHA-1",
            Self::Md5 => "MD5",
        }
    }

    pub const fn names(&self) -> &'static [&'static str] {
        match self {
            Self::Sha256 => &["SHA-256", "SHA256"],
            Self::Sha1 => &["SHA-1", "SHA1"],
            Self::Md5 => &["MD5"],
        }
    }

    pub const fn digest_length(&self) -> usize {
        match self {
            Self::Sha256 => 32,
            Self::Sha1 => 20,
            Self::Md5 => 16,
        }
    }

    /// `DigestHashFunction.DigestFunctionConverter.convert`: matches `SHA-1`/`SHA1` and
    /// `SHA-256`/`SHA256` with Unicode case folding and returns `"Not a valid hash function."`
    /// otherwise.
    pub fn parse_bazel(input: &str) -> Result<Self, &'static str> {
        for candidate in [Self::Sha1, Self::Sha256] {
            if candidate
                .names()
                .iter()
                .any(|n| str_eq_ignore_unicode_case(n, input))
            {
                return Ok(candidate);
            }
        }
        Err("Not a valid hash function.")
    }

    pub fn hash_bytes(&self, data: &[u8]) -> Vec<u8> {
        match self {
            Self::Sha256 => sha256::digest(data).to_vec(),
            Self::Sha1 => sha1::digest(data).to_vec(),
            Self::Md5 => md5::digest(data).to_vec(),
        }
    }

    pub fn hex_hash_bytes(&self, data: &[u8]) -> String {
        zel_util::to_hex(&self.hash_bytes(data))
    }
}

impl fmt::Display for DigestHashFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.canonical_name())
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
    pub last_change_time_ms: u64,
    pub permissions: i32,
    pub node_id: u64,
}

impl FileStatus {
    /// `FileStatus.isFile()`: true for regular files OR special files (FIFOs, sockets, devices).
    pub const fn is_file_or_special(&self) -> bool {
        self.is_file || self.is_special_file
    }
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
    fn stat_nullable(&self, path: &PathFragment, follow_symlinks: bool) -> Option<FileStatus> {
        self.stat(path, follow_symlinks).ok()
    }
    fn stat_if_found(
        &self,
        path: &PathFragment,
        follow_symlinks: bool,
    ) -> io::Result<Option<FileStatus>> {
        match self.stat(path, follow_symlinks) {
            Ok(st) => Ok(Some(st)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn exists(&self, path: &PathFragment, follow_symlinks: bool) -> bool {
        self.stat(path, follow_symlinks).is_ok()
    }
    fn read_file(&self, path: &PathFragment) -> io::Result<Vec<u8>>;
    fn write_file(&self, path: &PathFragment, content: &[u8]) -> io::Result<()>;
    fn append_file(&self, path: &PathFragment, content: &[u8]) -> io::Result<()> {
        let mut existing = match self.read_file(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        existing.extend_from_slice(content);
        self.write_file(path, &existing)
    }
    fn create_directory(&self, path: &PathFragment) -> io::Result<bool>;
    fn create_directory_and_parents(&self, path: &PathFragment) -> io::Result<()>;
    fn readdir(&self, path: &PathFragment) -> io::Result<Vec<Dirent>>;
    fn readdir_with_symlinks(
        &self,
        path: &PathFragment,
        follow_symlinks: bool,
    ) -> io::Result<Vec<Dirent>> {
        if !follow_symlinks {
            return self.readdir(path);
        }
        let entries = self.readdir(path)?;
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let child = path.get_relative_str(&entry.name);
            let dirent_type = match self.stat_nullable(&child, true) {
                Some(st) if st.is_directory => DirentType::Directory,
                Some(st) if st.is_file => DirentType::File,
                Some(st) if st.is_symlink => DirentType::Symlink,
                _ => DirentType::Unknown,
            };
            out.push(Dirent {
                name: entry.name,
                dirent_type,
            });
        }
        Ok(out)
    }
    fn delete(&self, path: &PathFragment) -> io::Result<bool>;
    fn delete_trees_below(&self, dir: &PathFragment) -> io::Result<()> {
        let Some(st) = self.stat_if_found(dir, false)? else {
            return Ok(());
        };
        if !st.is_directory || st.is_symlink {
            return Ok(());
        }
        let entries = match self.readdir(dir) {
            Ok(e) => e,
            Err(_) => {
                let _ = self.chmod(dir, 0o700);
                self.readdir(dir)?
            }
        };
        for (idx, entry) in entries.into_iter().enumerate() {
            let child = dir.get_relative_str(&entry.name);
            if entry.dirent_type == DirentType::Directory {
                self.delete_trees_below(&child)?;
            }
            match self.delete(&child) {
                Ok(false) if idx == 0 => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        format!("Unlink of '{child}' failed"),
                    ));
                }
                Ok(_) => {}
                Err(_) if idx == 0 => {
                    let _ = self.chmod(dir, 0o700);
                    if entry.dirent_type == DirentType::Directory {
                        self.delete_trees_below(&child)?;
                    }
                    self.delete(&child)?;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    fn delete_tree(&self, path: &PathFragment) -> io::Result<bool> {
        self.delete_trees_below(path)?;
        self.delete(path)
    }
    fn rename(&self, source: &PathFragment, target: &PathFragment) -> io::Result<()>;
    fn create_symbolic_link(
        &self,
        link_path: &PathFragment,
        target_fragment: &PathFragment,
    ) -> io::Result<()>;
    fn read_symbolic_link(&self, link_path: &PathFragment) -> io::Result<PathFragment>;
    fn chmod(&self, _path: &PathFragment, _mode: u32) -> io::Result<()> {
        Ok(())
    }
    fn set_last_modified_time(&self, _path: &PathFragment, _time_ms: i64) -> io::Result<()> {
        Ok(())
    }
    fn create_hard_link(
        &self,
        link_path: &PathFragment,
        original_path: &PathFragment,
    ) -> io::Result<()> {
        if !self.exists(original_path, true) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "File \"{}\" linked from \"{}\" does not exist",
                    original_path.base_name(),
                    link_path.base_name()
                ),
            ));
        }
        if self.exists(link_path, false) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("New link path \"{}\" already exists", link_path.base_name()),
            ));
        }
        let content = self.read_file(original_path)?;
        self.write_file(link_path, &content)
    }
    fn get_xattr(
        &self,
        _path: &PathFragment,
        _name: &str,
        _follow_symlinks: bool,
    ) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn get_fast_digest(&self, _path: &PathFragment) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }

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
        let (ctime, permissions, node_id) = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let ctime_ms = if meta.ctime() >= 0 {
                    (meta.ctime() as u64)
                        .saturating_mul(1000)
                        .saturating_add((meta.ctime_nsec().max(0) as u64) / 1_000_000)
                } else {
                    0
                };
                let perms = (meta.mode() & 0o7777) as i32;
                let nid = meta.ino() ^ meta.dev().rotate_left(32);
                (ctime_ms, perms, nid)
            }
            #[cfg(not(unix))]
            {
                let perms = if meta.permissions().readonly() {
                    0o555
                } else {
                    0o755
                };
                (mtime, perms, 0)
            }
        };
        Ok(FileStatus {
            is_file: ft.is_file(),
            is_directory: ft.is_dir(),
            is_symlink: ft.is_symlink(),
            is_special_file: !ft.is_file() && !ft.is_dir() && !ft.is_symlink(),
            size: meta.len(),
            last_modified_time_ms: mtime,
            last_change_time_ms: ctime,
            permissions,
            node_id,
        })
    }

    fn read_file(&self, path: &PathFragment) -> io::Result<Vec<u8>> {
        fs::read(Self::to_std_path(path))
    }

    fn write_file(&self, path: &PathFragment, content: &[u8]) -> io::Result<()> {
        let p = Self::to_std_path(path);
        // `blaze_util::WriteFile`: unlink any existing file or symlink first so a symlink at `p`
        // is replaced rather than followed.
        if fs::symlink_metadata(&p).is_ok_and(|m| !m.is_dir()) {
            let _ = fs::remove_file(&p);
        }
        fs::write(&p, content)
    }

    fn append_file(&self, path: &PathFragment, content: &[u8]) -> io::Result<()> {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(Self::to_std_path(path))?;
        f.write_all(content)
    }

    fn create_directory(&self, path: &PathFragment) -> io::Result<bool> {
        let p = Self::to_std_path(path);
        match fs::create_dir(&p) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => match fs::metadata(&p) {
                Ok(meta) if meta.is_dir() => Ok(false),
                _ => Err(e),
            },
            Err(e) => Err(e),
        }
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
                #[cfg(windows)]
                if meta.file_type().is_symlink() {
                    fs::remove_dir(&p).or_else(|_| fs::remove_file(&p))?;
                    return Ok(true);
                }
                if meta.is_dir() {
                    fs::remove_dir(&p)?;
                } else {
                    fs::remove_file(&p)?;
                }
                Ok(true)
            }
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    || e.kind() == io::ErrorKind::NotADirectory =>
            {
                Ok(false)
            }
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
        #[cfg(windows)]
        {
            let link = Self::to_std_path(link_path);
            let raw_target = StdPath::new(target_fragment.path_string());
            let resolved_target = if raw_target.is_absolute() {
                raw_target.to_path_buf()
            } else {
                link.parent()
                    .unwrap_or_else(|| StdPath::new("."))
                    .join(raw_target)
            };
            if resolved_target.is_dir() {
                create_directory_junction_or_symlink(&resolved_target, &link)
            } else {
                std::os::windows::fs::symlink_file(raw_target, &link)
            }
        }
        #[cfg(all(not(unix), not(windows)))]
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
        let s = target.to_string_lossy();
        #[cfg(windows)]
        let s = s
            .strip_prefix(r"\??\")
            .or_else(|| s.strip_prefix(r"\\?\"))
            .unwrap_or(&s);
        Ok(PathFragment::create(&s))
    }

    fn chmod(&self, path: &PathFragment, mode: u32) -> io::Result<()> {
        let p = Self::to_std_path(path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(mode & 0o7777))
        }
        #[cfg(not(unix))]
        {
            let mut perms = fs::metadata(&p)?.permissions();
            perms.set_readonly((mode & 0o200) == 0);
            fs::set_permissions(&p, perms)
        }
    }

    fn create_hard_link(
        &self,
        link_path: &PathFragment,
        original_path: &PathFragment,
    ) -> io::Result<()> {
        if !self.exists(original_path, true) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "File \"{}\" linked from \"{}\" does not exist",
                    original_path.base_name(),
                    link_path.base_name()
                ),
            ));
        }
        let dst = Self::to_std_path(link_path);
        if fs::symlink_metadata(&dst).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("New link path \"{}\" already exists", link_path.base_name()),
            ));
        }
        let canon_orig = fs::canonicalize(Self::to_std_path(original_path))?;
        if fs::metadata(&canon_orig)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("Cannot hard link a directory: {original_path}"),
            ));
        }
        fs::hard_link(canon_orig, dst)
    }
}

/// Creates a directory symlink on Unix, or an NTFS directory junction (with `symlink_dir` fallback)
/// on Windows (`CreateJunction` in `src/main/native/windows/file.cc`).
pub fn create_directory_junction_or_symlink(target: &StdPath, link: &StdPath) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        create_windows_junction(target, link)
            .or_else(|_| std::os::windows::fs::symlink_dir(target, link))
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        let _ = (target, link);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory links not supported on this platform",
        ))
    }
}

#[cfg(windows)]
fn create_windows_junction(target: &StdPath, link: &StdPath) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            lp_file_name: *const u16,
            dw_desired_access: u32,
            dw_share_mode: u32,
            lp_security_attributes: *mut core::ffi::c_void,
            dw_creation_disposition: u32,
            dw_flags_and_attributes: u32,
            h_template_file: *mut core::ffi::c_void,
        ) -> *mut core::ffi::c_void;
        fn DeviceIoControl(
            h_device: *mut core::ffi::c_void,
            dw_io_control_code: u32,
            lp_in_buffer: *const core::ffi::c_void,
            n_in_buffer_size: u32,
            lp_out_buffer: *mut core::ffi::c_void,
            n_out_buffer_size: u32,
            lp_bytes_returned: *mut u32,
            lp_overlapped: *mut core::ffi::c_void,
        ) -> i32;
        fn CloseHandle(h_object: *mut core::ffi::c_void) -> i32;
    }

    const GENERIC_WRITE: u32 = 0x4000_0000;
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

    let abs_target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        std::env::current_dir()?.join(target)
    };
    let raw_target = abs_target.to_string_lossy().replace('/', "\\");
    let stripped = raw_target
        .strip_prefix(r"\\?\")
        .or_else(|| raw_target.strip_prefix(r"\??\"))
        .unwrap_or(&raw_target);
    let subst = format!(r"\??\{}", stripped);
    let subst_utf16: Vec<u16> = std::ffi::OsStr::new(&subst).encode_wide().collect();
    let print_utf16: Vec<u16> = std::ffi::OsStr::new(stripped).encode_wide().collect();

    let subst_bytes = (subst_utf16.len() * 2) as u16;
    let print_offset = subst_bytes + 2;
    let print_bytes = (print_utf16.len() * 2) as u16;
    let data_len = 8u16
        .checked_add(subst_bytes)
        .and_then(|n| n.checked_add(2))
        .and_then(|n| n.checked_add(print_bytes))
        .and_then(|n| n.checked_add(2))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "junction target too long"))?;

    fs::create_dir(link)?;

    let mut link_wide: Vec<u16> = link.as_os_str().encode_wide().collect();
    link_wide.push(0);
    let invalid = (-1isize) as *mut core::ffi::c_void;
    let handle = unsafe {
        CreateFileW(
            link_wide.as_ptr(),
            GENERIC_WRITE,
            0,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle.is_null() || handle == invalid {
        let err = io::Error::last_os_error();
        let _ = fs::remove_dir(link);
        return Err(err);
    }

    let mut buf: Vec<u8> = Vec::with_capacity(8 + data_len as usize);
    buf.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    buf.extend_from_slice(&data_len.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes()); // Reserved
    buf.extend_from_slice(&0u16.to_le_bytes()); // SubstituteNameOffset
    buf.extend_from_slice(&subst_bytes.to_le_bytes()); // SubstituteNameLength
    buf.extend_from_slice(&print_offset.to_le_bytes()); // PrintNameOffset
    buf.extend_from_slice(&print_bytes.to_le_bytes()); // PrintNameLength
    for w in &subst_utf16 {
        buf.extend_from_slice(&w.to_le_bytes());
    }
    buf.extend_from_slice(&0u16.to_le_bytes());
    for w in &print_utf16 {
        buf.extend_from_slice(&w.to_le_bytes());
    }
    buf.extend_from_slice(&0u16.to_le_bytes());

    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_SET_REPARSE_POINT,
            buf.as_ptr() as *const _,
            buf.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    let ioctl_err = io::Error::last_os_error();
    unsafe {
        let _ = CloseHandle(handle);
    }
    if ok == 0 {
        let _ = fs::remove_dir(link);
        return Err(ioctl_err);
    }
    Ok(())
}

#[derive(Debug)]
struct InMemoryFileData {
    content: Vec<u8>,
    mtime_ms: u64,
    permissions: i32,
    node_id: u64,
}

#[derive(Debug, Clone)]
enum InMemoryNode {
    File(Arc<RwLock<InMemoryFileData>>),
    Directory {
        mtime_ms: u64,
        permissions: i32,
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
                permissions: 0o755,
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
        Self::resolve_path_depth(map, path, path, follow_symlinks, &mut depth)
    }

    fn resolve_path_depth(
        map: &BTreeMap<PathFragment, InMemoryNode>,
        orig_path: &PathFragment,
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
                            format!("{orig_path} (Too many levels of symbolic links)"),
                        ));
                    }
                    let next = if target.is_absolute() {
                        target.clone()
                    } else if let Some(parent) = cur.parent_directory() {
                        parent.get_relative(target)
                    } else {
                        cur.get_relative(target)
                    };
                    cur = Self::resolve_path_depth(map, orig_path, &next, true, depth)?;
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
            Some(InMemoryNode::File(data)) => {
                let d = data.read().unwrap();
                Ok(FileStatus {
                    is_file: true,
                    is_directory: false,
                    is_symlink: false,
                    is_special_file: false,
                    size: d.content.len() as u64,
                    last_modified_time_ms: d.mtime_ms,
                    last_change_time_ms: d.mtime_ms,
                    permissions: d.permissions,
                    node_id: d.node_id,
                })
            }
            Some(InMemoryNode::Directory {
                mtime_ms,
                permissions,
                node_id,
            }) => Ok(FileStatus {
                is_file: false,
                is_directory: true,
                is_symlink: false,
                is_special_file: false,
                size: 0,
                last_modified_time_ms: *mtime_ms,
                last_change_time_ms: *mtime_ms,
                permissions: *permissions,
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
                last_change_time_ms: *mtime_ms,
                permissions: 0o777,
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
            Some(InMemoryNode::File(data)) => Ok(data.read().unwrap().content.clone()),
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
        if let Ok(resolved) = Self::resolve_path(&map, path, true) {
            if let Some(InMemoryNode::File(existing)) = map.get(&resolved) {
                existing.write().unwrap().content = content.to_vec();
                return Ok(());
            }
        }
        map.insert(
            path.clone(),
            InMemoryNode::File(Arc::new(RwLock::new(InMemoryFileData {
                content: content.to_vec(),
                mtime_ms: 0,
                permissions: 0o644,
                node_id: id,
            }))),
        );
        Ok(())
    }

    fn create_directory(&self, path: &PathFragment) -> io::Result<bool> {
        let id = self.alloc_id();
        let mut map = self.nodes.write().unwrap();
        if let Some(node) = map.get(path) {
            return match node {
                InMemoryNode::Directory { .. } => Ok(false),
                InMemoryNode::Symlink { .. } => {
                    let resolved = Self::resolve_path(&map, path, true)?;
                    if matches!(map.get(&resolved), Some(InMemoryNode::Directory { .. })) {
                        Ok(false)
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            format!("File exists: {path}"),
                        ))
                    }
                }
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
                permissions: 0o755,
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
                    InMemoryNode::File(_) => DirentType::File,
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

    fn chmod(&self, path: &PathFragment, mode: u32) -> io::Result<()> {
        let mut map = self.nodes.write().unwrap();
        let resolved = Self::resolve_path(&map, path, true)?;
        match map.get_mut(&resolved) {
            Some(InMemoryNode::File(data)) => {
                data.write().unwrap().permissions = (mode & 0o7777) as i32;
                Ok(())
            }
            Some(InMemoryNode::Directory { permissions, .. }) => {
                *permissions = (mode & 0o7777) as i32;
                Ok(())
            }
            _ => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("No such file or directory: {path}"),
            )),
        }
    }

    fn set_last_modified_time(&self, path: &PathFragment, time_ms: i64) -> io::Result<()> {
        let effective_ms = if time_ms == VfsPath::NOW_SENTINEL_TIME {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_millis() as u64
        } else {
            time_ms.max(0) as u64
        };
        let mut map = self.nodes.write().unwrap();
        let resolved = Self::resolve_path(&map, path, true)?;
        match map.get_mut(&resolved) {
            Some(InMemoryNode::File(data)) => {
                data.write().unwrap().mtime_ms = effective_ms;
                Ok(())
            }
            Some(InMemoryNode::Directory { mtime_ms, .. })
            | Some(InMemoryNode::Symlink { mtime_ms, .. }) => {
                *mtime_ms = effective_ms;
                Ok(())
            }
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("No such file or directory: {path}"),
            )),
        }
    }

    fn create_hard_link(
        &self,
        link_path: &PathFragment,
        original_path: &PathFragment,
    ) -> io::Result<()> {
        let mut map = self.nodes.write().unwrap();
        if map.contains_key(link_path) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("New link path \"{}\" already exists", link_path.base_name()),
            ));
        }
        if let Some(parent) = link_path.parent_directory() {
            let resolved_parent = Self::resolve_path(&map, &parent, true)?;
            match map.get(&resolved_parent) {
                Some(InMemoryNode::Directory { .. }) => {}
                Some(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotADirectory,
                        format!("Not a directory: {parent}"),
                    ));
                }
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("Parent directory does not exist: {parent}"),
                    ));
                }
            }
        }
        let resolved_orig = Self::resolve_path(&map, original_path, true).map_err(|_| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "File \"{}\" linked from \"{}\" does not exist",
                    original_path.base_name(),
                    link_path.base_name()
                ),
            )
        })?;
        let shared = match map.get(&resolved_orig) {
            Some(InMemoryNode::File(data)) => Arc::clone(data),
            Some(InMemoryNode::Directory { .. }) => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("Cannot hard link a directory: {original_path}"),
                ));
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "File \"{}\" linked from \"{}\" does not exist",
                        original_path.base_name(),
                        link_path.base_name()
                    ),
                ));
            }
        };
        map.insert(link_path.clone(), InMemoryNode::File(shared));
        Ok(())
    }
}

/// Root + relative `PathFragment` pair matching `com.google.devtools.build.lib.vfs.RootedPath`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RootedPath {
    root: PathFragment,
    root_relative_path: PathFragment,
    is_absolute_root: bool,
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
            is_absolute_root: false,
        })
    }

    /// Constructs a `RootedPath` from a [`Root`] (`PathRoot` or `AbsoluteRoot`).
    pub fn to_rooted_path_from_root(
        root: Root,
        relative_or_abs: PathFragment,
    ) -> Result<Self, String> {
        match root {
            Root::PathRoot(base) => {
                if relative_or_abs.is_absolute() {
                    let rel = relative_or_abs.relative_to(&base)?;
                    Self::to_rooted_path(base, rel)
                } else {
                    Self::to_rooted_path(base, relative_or_abs)
                }
            }
            Root::AbsoluteRoot => {
                if !relative_or_abs.is_absolute() {
                    return Err(format!(
                        "AbsoluteRoot requires an absolute path, got '{relative_or_abs}'"
                    ));
                }
                let drive = PathFragment::create_already_normalized(relative_or_abs.drive_str());
                let rel = relative_or_abs.to_relative();
                Ok(Self {
                    root: drive,
                    root_relative_path: rel,
                    is_absolute_root: true,
                })
            }
        }
    }

    pub fn root(&self) -> &PathFragment {
        &self.root
    }

    pub fn root_value(&self) -> Root {
        if self.is_absolute_root {
            Root::AbsoluteRoot
        } else {
            Root::PathRoot(self.root.clone())
        }
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
    Cycle {
        path_to_cycle: Vec<PathFragment>,
        chain: Vec<PathFragment>,
    },
    InfiniteExpansion {
        path_to_chain: Vec<PathFragment>,
        chain: Vec<PathFragment>,
    },
    Io(String),
}

impl SymlinkError {
    /// Fixed exception message matching `FileSymlinkCycleException.getMessage()` /
    /// `FileSymlinkInfiniteExpansionException.getMessage()`.
    pub fn message(&self) -> &str {
        match self {
            Self::Cycle { .. } => "Symlink cycle",
            Self::InfiniteExpansion { .. } => "Infinite symlink expansion",
            Self::Io(m) => m.as_str(),
        }
    }

    /// Detailed user-facing message matching `getUserFriendlyMessage()`.
    pub fn user_friendly_message(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for SymlinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cycle { chain, .. } => {
                let s: Vec<&str> = chain.iter().map(|p| p.path_string()).collect();
                write!(f, "Symlink cycle: {}", s.join("- > "))
            }
            Self::InfiniteExpansion { chain, .. } => {
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
                path_to_cycle: self.logical_chain[..pos].to_vec(),
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
                let path_to_chain = self.logical_chain[..pos].to_vec();
                let mut chain = self.logical_chain[pos..].to_vec();
                chain.push(path.clone());
                return Err(SymlinkError::InfiniteExpansion {
                    path_to_chain,
                    chain,
                });
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

        // Entering a cycle from outside records both path_to_cycle and cycle.
        fs.create_symbolic_link(
            &PathFragment::create("/ws/pkg/entry"),
            &PathFragment::create("a"),
        )
        .unwrap();
        match resolve_symlink_chain(&fs, &PathFragment::create("/ws/pkg/entry")) {
            Err(SymlinkError::Cycle {
                path_to_cycle,
                chain,
            }) => {
                assert_eq!(path_to_cycle, vec![PathFragment::create("/ws/pkg/entry")]);
                assert_eq!(
                    chain,
                    vec![
                        PathFragment::create("/ws/pkg/a"),
                        PathFragment::create("/ws/pkg/b"),
                    ]
                );
                let cyc = SymlinkError::Cycle {
                    path_to_cycle,
                    chain,
                };
                assert_eq!(cyc.message(), "Symlink cycle");
                assert!(cyc.user_friendly_message().starts_with("Symlink cycle: "));
            }
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    #[test]
    fn test_os_path_policy_and_extended_path_fragment_methods() {
        // On Unix policy, backslashes and `c:/foo` stay literal/relative.
        let unix_pf = PathFragment::create_for_os("a\\b/c:/foo", OsPathPolicy::Unix);
        assert!(!unix_pf.is_absolute());
        assert_eq!(unix_pf.path_string(), "a\\b/c:/foo");
        assert_eq!(
            PathFragment::create_for_os("a", OsPathPolicy::Unix)
                .get_relative_for_os("C:/foo", OsPathPolicy::Unix)
                .path_string(),
            "a/C:/foo"
        );
        // Bare `C:` without slash is relative even on Windows policy.
        let bare_drive = PathFragment::create("C:");
        assert!(!bare_drive.is_absolute());
        assert_eq!(bare_drive.drive_str_length(), 0);

        let p = PathFragment::create("/a/b/c.tar.gz");
        assert_eq!(p.file_extension(), "gz");
        assert!(p.ends_with(&PathFragment::create("b/c.tar.gz")));
        assert!(!p.ends_with(&PathFragment::create("a/c.tar.gz")));
        assert!(p.starts_with_ignoring_case(&PathFragment::create("/A/B")));
        assert_eq!(p.drive_str(), "/");
        assert_eq!(p.to_relative(), PathFragment::create("a/b/c.tar.gz"));
        assert_eq!(
            p.strip_components(2).unwrap(),
            PathFragment::create("c.tar.gz")
        );
        assert!(p.strip_components(-1).is_err());
        assert_eq!(p.strip_components(10).unwrap(), PathFragment::empty());
        assert_eq!(
            p.replace_name("other.txt"),
            Some(PathFragment::create("/a/b/other.txt"))
        );
        assert_eq!(PathFragment::create("/").replace_name("x"), None);
        assert_eq!(PathFragment::empty().safe_path_string(), ".");
        assert_eq!(
            PathFragment::create("tool").callable_path_string_for_os(OsPathPolicy::Unix),
            "./tool"
        );
        assert_eq!(
            PathFragment::create("tool").callable_path_string_for_os(OsPathPolicy::Windows),
            ".\\tool"
        );

        let segs: Vec<&str> = PathFragment::raw_segment_iter("a//b/", 0).collect();
        assert_eq!(segs, vec!["a", "/b"]);

        assert_eq!(OutputPermissions::Readonly.permissions_mode(), 0o555);
        assert_eq!(OutputPermissions::Writable.permissions_mode(), 0o755);
        assert_eq!(
            DigestHashFunction::parse_bazel("sha1").unwrap(),
            DigestHashFunction::Sha1
        );
        assert_eq!(
            DigestHashFunction::parse_bazel("MD5"),
            Err("Not a valid hash function.")
        );
    }

    #[test]
    fn test_hard_links_in_memory_and_local() {
        let fs = InMemoryFileSystem::default();
        let dir = PathFragment::create("/ws/hl");
        fs.create_directory_and_parents(&dir).unwrap();
        let orig = dir.get_relative_str("orig.txt");
        let sym = dir.get_relative_str("sym.txt");
        let link = dir.get_relative_str("link.txt");
        fs.write_file(&orig, b"hello").unwrap();
        fs.create_symbolic_link(&sym, &PathFragment::create("orig.txt"))
            .unwrap();
        // Hard-linking through a symlink links to the underlying file inode.
        fs.create_hard_link(&link, &sym).unwrap();
        assert_eq!(
            fs.stat(&orig, false).unwrap().node_id,
            fs.stat(&link, false).unwrap().node_id
        );
        assert_eq!(fs.read_file(&link).unwrap(), b"hello");
        // Existing target or directory original is rejected.
        assert!(fs.create_hard_link(&link, &orig).is_err());
        assert!(fs
            .create_hard_link(&dir.get_relative_str("dir_link"), &dir)
            .is_err());
        // Deleting the original leaves the hard link intact.
        fs.delete(&orig).unwrap();
        assert!(!fs.exists(&orig, false));
        assert_eq!(fs.read_file(&link).unwrap(), b"hello");

        let tmp = std::env::temp_dir().join(format!("zel_vfs_hl_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let lfs = LocalFileSystem::default();
        let l_orig = PathFragment::create(&tmp.join("a.txt").to_string_lossy());
        let l_link = PathFragment::create(&tmp.join("b.txt").to_string_lossy());
        lfs.write_file(&l_orig, b"world").unwrap();
        lfs.create_hard_link(&l_link, &l_orig).unwrap();
        #[cfg(unix)]
        assert_eq!(
            lfs.stat(&l_orig, false).unwrap().node_id,
            lfs.stat(&l_link, false).unwrap().node_id
        );
        lfs.delete(&l_orig).unwrap();
        assert_eq!(lfs.read_file(&l_link).unwrap(), b"world");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn local_fs_write_file_replaces_symlink_and_junction_helper_works() {
        let dir = std::env::temp_dir().join(format!("zel-vfs-sym-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("target_dir")).unwrap();
        fs::write(dir.join("target_dir/inside"), b"ok").unwrap();
        fs::write(dir.join("protected"), b"keep").unwrap();

        let dir_link = dir.join("dir_link");
        create_directory_junction_or_symlink(&dir.join("target_dir"), &dir_link).unwrap();
        assert_eq!(fs::read(dir_link.join("inside")).unwrap(), b"ok");

        #[cfg(unix)]
        {
            let file_link = dir.join("file_link");
            std::os::unix::fs::symlink(dir.join("protected"), &file_link).unwrap();
            let lfs = LocalFileSystem::default();
            let frag = PathFragment::create(&file_link.to_string_lossy());
            lfs.write_file(&frag, b"replaced").unwrap();
            assert_eq!(fs::read(dir.join("protected")).unwrap(), b"keep");
            assert_eq!(fs::read(&file_link).unwrap(), b"replaced");
            assert!(!fs::symlink_metadata(&file_link)
                .unwrap()
                .file_type()
                .is_symlink());
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
