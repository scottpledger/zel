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

//! Merkle tree construction (`merkletree/MerkleTreeComputer.java`): turns a set of spawn inputs
//! into REAPI `Directory` protos whose children are sorted by name, with the input root digest and
//! the set of positive-size blobs needed in the CAS. Also the inverse (`Tree` flattening) and
//! `UploadManifest`-compatible output directory `Tree` construction.

use crate::digest::DigestFn;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;
use zel_proto::reapi::*;

#[derive(Debug, Clone)]
pub enum InputKind {
    /// A regular file on the local file system (symlinks already resolved by the caller).
    File {
        local: PathBuf,
        executable: bool,
    },
    /// Inline file contents.
    Bytes {
        data: Vec<u8>,
        executable: bool,
    },
    /// A file known only by digest (a remote output that was never downloaded); the CAS is
    /// assumed to hold it already, so no blob source is recorded.
    Remote {
        digest: Digest,
        executable: bool,
    },
    /// An unresolved symlink artifact.
    Symlink {
        target: String,
    },
    EmptyDirectory,
}

#[derive(Debug, Clone)]
pub struct MerkleInput {
    /// Path relative to the input root, `/`-separated.
    pub path: String,
    pub kind: InputKind,
    /// Optional REAPI node properties (e.g. mtime / unix mode).
    pub node_properties: Option<NodeProperties>,
}

impl MerkleInput {
    pub fn file(path: &str, local: PathBuf, executable: bool) -> Self {
        Self {
            path: path.into(),
            kind: InputKind::File { local, executable },
            node_properties: None,
        }
    }
    pub fn bytes(path: &str, data: Vec<u8>, executable: bool) -> Self {
        Self {
            path: path.into(),
            kind: InputKind::Bytes { data, executable },
            node_properties: None,
        }
    }
    pub fn symlink(path: &str, target: &str) -> Self {
        Self {
            path: path.into(),
            kind: InputKind::Symlink {
                target: target.into(),
            },
            node_properties: None,
        }
    }
    pub fn empty_dir(path: &str) -> Self {
        Self {
            path: path.into(),
            kind: InputKind::EmptyDirectory,
            node_properties: None,
        }
    }
}

/// Where the bytes of a blob come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobSource {
    Bytes(Vec<u8>),
    File(PathBuf),
}

impl BlobSource {
    pub fn read(&self) -> io::Result<Vec<u8>> {
        match self {
            Self::Bytes(b) => Ok(b.clone()),
            Self::File(p) => std::fs::read(p),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MerkleTree {
    pub root_digest: Digest,
    pub root: Directory,
    /// All non-empty blobs (directories + files with `size_bytes > 0`) needed in the CAS.
    pub blobs: BTreeMap<Digest, BlobSource>,
    pub input_files: usize,
    pub input_bytes: i64,
}

#[derive(Default)]
struct DirBuilder {
    dirs: BTreeMap<String, DirBuilder>,
    files: BTreeMap<String, FileNode>,
    symlinks: BTreeMap<String, SymlinkNode>,
}

/// Computes Merkle trees, caching file digests by `(path, size, mtime)`.
pub struct MerkleTreeComputer {
    pub digest_fn: DigestFn,
    cache: Mutex<HashMap<(PathBuf, u64, u128), Digest>>,
}

impl MerkleTreeComputer {
    pub fn new(digest_fn: DigestFn) -> Self {
        Self {
            digest_fn,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn file_digest(&self, path: &PathBuf) -> io::Result<Digest> {
        let md = std::fs::metadata(path)?;
        let mtime = md
            .modified()
            .ok()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let key = (path.clone(), md.len(), mtime);
        if let Some(d) = self.cache.lock().unwrap().get(&key) {
            return Ok(d.clone());
        }
        let data = std::fs::read(path)?;
        let d = self.digest_fn.digest(&data);
        self.cache.lock().unwrap().insert(key, d.clone());
        Ok(d)
    }

    pub fn build(&self, inputs: &[MerkleInput]) -> io::Result<MerkleTree> {
        if inputs.is_empty() {
            return Ok(MerkleTree {
                root_digest: self.digest_fn.digest(b""),
                root: Directory::default(),
                blobs: BTreeMap::new(),
                input_files: 0,
                input_bytes: 0,
            });
        }
        let mut root = DirBuilder::default();
        let mut blobs: BTreeMap<Digest, BlobSource> = BTreeMap::new();
        let mut input_files = 0usize;
        let mut input_bytes = 0i64;
        let bad = |m: String| io::Error::new(io::ErrorKind::InvalidInput, m);

        for inp in inputs {
            let segs: Vec<&str> = inp
                .path
                .split('/')
                .filter(|s| !s.is_empty() && *s != ".")
                .collect();
            if segs.is_empty() {
                if matches!(inp.kind, InputKind::EmptyDirectory) {
                    continue;
                }
                return Err(bad(format!("empty input path for {:?}", inp.kind)));
            }
            if segs.contains(&"..") {
                return Err(bad(format!(
                    "input path escapes the input root: {}",
                    inp.path
                )));
            }
            let (name, parents) = segs.split_last().unwrap();
            let mut cur = &mut root;
            for p in parents {
                if cur.files.contains_key(*p) || cur.symlinks.contains_key(*p) {
                    return Err(bad(format!("{} is both a file and a directory", inp.path)));
                }
                cur = cur.dirs.entry(p.to_string()).or_default();
            }
            let name = name.to_string();
            match &inp.kind {
                InputKind::EmptyDirectory => {
                    if cur.files.contains_key(&name) || cur.symlinks.contains_key(&name) {
                        return Err(bad(format!("{} is both a file and a directory", inp.path)));
                    }
                    cur.dirs.entry(name).or_default();
                }
                InputKind::Symlink { target } => {
                    let node = SymlinkNode {
                        name: name.clone(),
                        target: target.clone(),
                        node_properties: inp.node_properties.clone(),
                    };
                    if let Some(old) = cur.symlinks.get(&name) {
                        if *old != node {
                            return Err(bad(format!("conflicting symlink inputs at {}", inp.path)));
                        }
                    }
                    if cur.dirs.contains_key(&name) || cur.files.contains_key(&name) {
                        return Err(bad(format!("conflicting inputs at {}", inp.path)));
                    }
                    cur.symlinks.insert(name, node);
                }
                InputKind::File { .. } | InputKind::Bytes { .. } | InputKind::Remote { .. } => {
                    let (digest, exec, src) = match &inp.kind {
                        InputKind::Remote { digest, executable } => {
                            (digest.clone(), *executable, None)
                        }
                        InputKind::File { local, executable } => {
                            let d = self.file_digest(local).map_err(|e| {
                                io::Error::new(e.kind(), format!("{}: {e}", local.display()))
                            })?;
                            (d, *executable, Some(BlobSource::File(local.clone())))
                        }
                        InputKind::Bytes { data, executable } => (
                            self.digest_fn.digest(data),
                            *executable,
                            Some(BlobSource::Bytes(data.clone())),
                        ),
                        _ => unreachable!(),
                    };
                    let node = FileNode {
                        name: name.clone(),
                        digest: Some(digest.clone()),
                        is_executable: exec,
                        node_properties: inp.node_properties.clone(),
                    };
                    if let Some(old) = cur.files.get(&name) {
                        if *old != node {
                            return Err(bad(format!("conflicting file inputs at {}", inp.path)));
                        }
                    } else {
                        input_files += 1;
                        input_bytes += digest.size_bytes;
                    }
                    if cur.dirs.contains_key(&name) || cur.symlinks.contains_key(&name) {
                        return Err(bad(format!("conflicting inputs at {}", inp.path)));
                    }
                    // Empty blobs (size 0) are implicitly present in every REAPI CAS and are
                    // omitted from `MerkleTree.blobs` (matching Bazel's MerkleTreeComputer).
                    if digest.size_bytes > 0 {
                        if let Some(src) = src {
                            blobs.entry(digest).or_insert(src);
                        }
                    }
                    cur.files.insert(name, node);
                }
            }
        }
        let (root_dir, root_digest) = self.finish(&root, &mut blobs);
        Ok(MerkleTree {
            root_digest,
            root: root_dir,
            blobs,
            input_files,
            input_bytes,
        })
    }

    fn finish(
        &self,
        b: &DirBuilder,
        blobs: &mut BTreeMap<Digest, BlobSource>,
    ) -> (Directory, Digest) {
        let mut dir = Directory::default();
        for (name, sub) in &b.dirs {
            let (_, d) = self.finish(sub, blobs);
            dir.directories.push(DirectoryNode {
                name: name.clone(),
                digest: Some(d),
            });
        }
        dir.files = b.files.values().cloned().collect();
        dir.symlinks = b.symlinks.values().cloned().collect();
        let bytes = dir.encode_to_vec();
        let digest = self.digest_fn.digest(&bytes);
        if digest.size_bytes > 0 {
            blobs.insert(digest.clone(), BlobSource::Bytes(bytes));
        }
        (dir, digest)
    }
}

/// A flattened output directory entry.
#[derive(Debug, Clone, PartialEq)]
pub enum TreeEntry {
    File {
        path: String,
        digest: Digest,
        executable: bool,
    },
    Symlink {
        path: String,
        target: String,
    },
    Directory {
        path: String,
    },
}

/// Flattens a REAPI `Tree` (root + children) into entries with paths relative to the tree root.
/// Emits all `Directory` and `File` entries before `Symlink` entries so callers materializing
/// entries in order always have intra-tree symlink targets on disk before planting symlinks.
/// Errors on missing children or cycles/over-deep recursion.
pub fn flatten_tree(tree: &Tree, digest_fn: DigestFn) -> Result<Vec<TreeEntry>, String> {
    let mut by_digest: HashMap<String, &Directory> = HashMap::new();
    let empty_dir = Directory::default();
    by_digest.insert(digest_fn.hash(b""), &empty_dir);
    for c in &tree.children {
        by_digest.insert(digest_fn.hash(&c.encode_to_vec()), c);
    }
    let root = tree.root.as_ref().ok_or("tree has no root")?;
    let mut out = Vec::new();
    let mut symlinks = Vec::new();
    fn walk(
        dir: &Directory,
        prefix: &str,
        by: &HashMap<String, &Directory>,
        out: &mut Vec<TreeEntry>,
        symlinks: &mut Vec<TreeEntry>,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 256 {
            return Err("tree too deep".into());
        }
        let join = |n: &str| {
            if prefix.is_empty() {
                n.to_string()
            } else {
                format!("{prefix}/{n}")
            }
        };
        for f in &dir.files {
            out.push(TreeEntry::File {
                path: join(&f.name),
                digest: f.digest.clone().ok_or("file node without digest")?,
                executable: f.is_executable,
            });
        }
        for s in &dir.symlinks {
            symlinks.push(TreeEntry::Symlink {
                path: join(&s.name),
                target: s.target.clone(),
            });
        }
        for d in &dir.directories {
            let dd = d.digest.clone().ok_or("directory node without digest")?;
            let p = join(&d.name);
            out.push(TreeEntry::Directory { path: p.clone() });
            let child = by
                .get(&dd.hash)
                .ok_or_else(|| format!("missing child directory {} in tree", dd.hash))?;
            walk(child, &p, by, out, symlinks, depth + 1)?;
        }
        Ok(())
    }
    walk(root, "", &by_digest, &mut out, &mut symlinks, 0)?;
    out.extend(symlinks);
    Ok(out)
}

/// Materializes a REAPI `Tree` into `dest` in two passes (matching Bazel's
/// `RemoteExecutionService.downloadOutputs`): subdirectories and regular files
/// across the whole tree are written first, and all `SymlinkNode`s are planted
/// second so cross-subtree directory targets exist when Windows `plant_symlink`
/// classifies directory vs file symlinks.
pub fn flatten_to_local_dir(
    tree: &Tree,
    digest_fn: DigestFn,
    dest: &Path,
    mut fetch_blob: impl FnMut(&Digest) -> io::Result<Vec<u8>>,
) -> io::Result<()> {
    let entries =
        flatten_tree(tree, digest_fn).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let _ = std::fs::remove_dir_all(dest);
    std::fs::create_dir_all(dest)?;

    let mut known_dirs: HashSet<String> = HashSet::new();
    known_dirs.insert(String::new());
    let mut known_files: HashSet<String> = HashSet::new();
    let mut symlinks: Vec<(String, String)> = Vec::new();

    for entry in entries {
        match entry {
            TreeEntry::Directory { path } => {
                record_ancestors_as_dirs(&path, &mut known_dirs);
                known_dirs.insert(path.clone());
                std::fs::create_dir_all(dest.join(&path))?;
            }
            TreeEntry::File {
                path,
                digest,
                executable,
            } => {
                record_ancestors_as_dirs(&path, &mut known_dirs);
                known_files.insert(path.clone());
                let data = if digest.size_bytes == 0 {
                    Vec::new()
                } else {
                    fetch_blob(&digest)?
                };
                crate::disk_cache::write_atomic_output(&dest.join(&path), &data, executable)?;
            }
            TreeEntry::Symlink { path, target } => {
                record_ancestors_as_dirs(&path, &mut known_dirs);
                symlinks.push((path, target));
            }
        }
    }

    propagate_symlink_kinds(&symlinks, dest, &mut known_dirs, &mut known_files);
    for (path, target) in &symlinks {
        let hint = infer_root_relative_symlink_hint(path, target, dest, &known_dirs, &known_files);
        plant_symlink(&dest.join(path), target, hint)?;
    }
    Ok(())
}

/// Builds a REAPI `Tree` from a local directory (`UploadManifest` rules: `is_executable = true`
/// on every regular file, non-dangling absolute symlinks followed, dangling absolute symlinks
/// rejected unless `allow_absolute_symlinks`, special files rejected as `InvalidData`).
pub fn tree_from_local_dir(
    dir: &Path,
    digest_fn: DigestFn,
    blobs: &mut BTreeMap<Digest, BlobSource>,
) -> io::Result<Tree> {
    tree_from_local_dir_ext(dir, digest_fn, false, blobs)
}

/// Target type hint when planting a symbolic link on platforms (Windows) where
/// directory and file symlinks use distinct reparse point flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymlinkKindHint {
    Auto,
    File,
    Dir,
}

pub(crate) fn record_ancestors_as_dirs(rel_path: &str, known_dirs: &mut HashSet<String>) {
    let mut cur = rel_path;
    while let Some((parent, _)) = cur.rsplit_once('/') {
        if !parent.is_empty() {
            known_dirs.insert(parent.to_string());
        }
        cur = parent;
    }
}

/// Resolves `target` from a symlink at `link_rel` inside `root`, returning
/// `Some("")` when it resolves to `root` itself or `Some("a/b")` inside `root`,
/// and `None` when it escapes `root` or points outside `root`.
pub(crate) fn resolve_root_or_rel_symlink(
    link_rel: &str,
    target: &str,
    root: &Path,
) -> Option<String> {
    #[cfg(windows)]
    {
        let clean_target = strip_windows_device_prefix(target);
        if is_windows_path_str_absolute(&clean_target) {
            let clean_root = strip_windows_device_prefix(&root.to_string_lossy());
            let rel = strip_windows_prefix_case_insensitive(&clean_target, &clean_root)?;
            return normalize_rel_allowing_empty(&rel);
        }
        let parent = link_rel.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        let joined = if parent.is_empty() {
            clean_target.replace('\\', "/")
        } else {
            format!("{parent}/{}", clean_target.replace('\\', "/"))
        };
        normalize_rel_allowing_empty(&joined)
    }
    #[cfg(not(windows))]
    {
        let tp = Path::new(target);
        if tp.is_absolute() {
            let rel = tp.strip_prefix(root).ok()?;
            return normalize_rel_allowing_empty(&rel.to_string_lossy());
        }
        let parent = link_rel.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        let joined = if parent.is_empty() {
            target.to_string()
        } else {
            format!("{parent}/{target}")
        };
        normalize_rel_allowing_empty(&joined)
    }
}

fn normalize_rel_allowing_empty(raw: &str) -> Option<String> {
    let mut stack: Vec<&str> = Vec::new();
    for seg in raw.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop()?;
            }
            other => stack.push(other),
        }
    }
    Some(stack.join("/"))
}

#[cfg(any(windows, test))]
pub(crate) fn strip_windows_prefix_case_insensitive(path: &str, root: &str) -> Option<String> {
    let p_norm = path.replace('/', r"\");
    let mut r_norm = root.replace('/', r"\");
    while r_norm.len() > 3 && r_norm.ends_with('\\') && !r_norm.ends_with(r":\") {
        r_norm.pop();
    }
    let rest = strip_prefix_ignore_ascii_case(&p_norm, &r_norm)?;
    if rest.is_empty() {
        Some(String::new())
    } else if r_norm.ends_with('\\') {
        Some(rest.to_string())
    } else {
        rest.strip_prefix('\\').map(|s| s.to_string())
    }
}

pub(crate) fn propagate_symlink_kinds(
    symlinks: &[(String, String)],
    root: &Path,
    known_dirs: &mut HashSet<String>,
    known_files: &mut HashSet<String>,
) {
    for _ in 0..32 {
        let mut changed = false;
        for (link_rel, target) in symlinks {
            if known_dirs.contains(link_rel) || known_files.contains(link_rel) {
                continue;
            }
            if target.ends_with('/') || target.ends_with('\\') {
                known_dirs.insert(link_rel.clone());
                changed = true;
                continue;
            }
            if let Some(resolved_rel) = resolve_root_or_rel_symlink(link_rel, target, root) {
                if known_dirs.contains(&resolved_rel) {
                    known_dirs.insert(link_rel.clone());
                    changed = true;
                } else if known_files.contains(&resolved_rel) {
                    known_files.insert(link_rel.clone());
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

pub(crate) fn infer_root_relative_symlink_hint(
    link_rel: &str,
    target: &str,
    root: &Path,
    known_dirs: &HashSet<String>,
    known_files: &HashSet<String>,
) -> SymlinkKindHint {
    if known_dirs.contains(link_rel) || target.ends_with('/') || target.ends_with('\\') {
        return SymlinkKindHint::Dir;
    }
    if known_files.contains(link_rel) {
        return SymlinkKindHint::File;
    }
    if let Some(resolved_rel) = resolve_root_or_rel_symlink(link_rel, target, root) {
        if known_dirs.contains(&resolved_rel) {
            return SymlinkKindHint::Dir;
        }
        if known_files.contains(&resolved_rel) {
            return SymlinkKindHint::File;
        }
    }
    SymlinkKindHint::Auto
}

/// Removes an existing file, file symlink, directory symlink/junction, or empty
/// directory at `p` without recursing into non-empty directories. On Windows,
/// directory symlinks and NTFS junctions require `remove_dir` rather than `remove_file`.
pub(crate) fn remove_existing_entry(p: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        if let Ok(md) = std::fs::symlink_metadata(p) {
            use std::os::windows::fs::MetadataExt;
            let mut perms = md.permissions();
            if perms.readonly()
                && !zel_vfs::is_windows_reparse_or_symlink(
                    md.file_attributes(),
                    md.file_type().is_symlink(),
                )
            {
                #[allow(clippy::permissions_set_readonly_false)]
                perms.set_readonly(false);
                let _ = std::fs::set_permissions(p, perms);
            }
        }
        std::fs::remove_dir(p).or_else(|_| std::fs::remove_file(p))
    }
    #[cfg(not(windows))]
    {
        std::fs::remove_file(p).or_else(|_| std::fs::remove_dir(p))
    }
}

#[cfg(any(not(unix), test))]
fn strip_prefix_ignore_ascii_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.as_bytes()
        .get(..prefix.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(prefix.as_bytes()))
        .then(|| &s[prefix.len()..])
}

/// Strips Windows verbatim (`\\?\`, `\\?\UNC\`) and NT object (`\??\`, `\??\UNC\`)
/// prefixes into standard Win32 drive or UNC paths while preserving original casing.
#[cfg(any(not(unix), test))]
pub(crate) fn strip_windows_device_prefix(raw: &str) -> String {
    let norm_sep = raw.replace('/', r"\");
    if let Some(unc) = strip_prefix_ignore_ascii_case(&norm_sep, r"\\?\UNC\")
        .or_else(|| strip_prefix_ignore_ascii_case(&norm_sep, r"\??\UNC\"))
    {
        let start = raw.len() - unc.len();
        return format!(r"\\{}", &raw[start..]);
    }
    if let Some(rest) = norm_sep
        .strip_prefix(r"\\?\")
        .or_else(|| norm_sep.strip_prefix(r"\??\"))
    {
        let start = raw.len() - rest.len();
        return raw[start..].to_string();
    }
    raw.to_string()
}

#[cfg(any(not(unix), test))]
pub(crate) fn is_windows_path_str_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || s.starts_with(r"\\")
        || s.starts_with("//")
}

pub(crate) fn is_absolute_symlink_target(target: &Path) -> bool {
    #[cfg(windows)]
    {
        let stripped = strip_windows_device_prefix(&target.to_string_lossy());
        Path::new(&stripped).is_absolute() || is_windows_path_str_absolute(&stripped)
    }
    #[cfg(not(windows))]
    {
        target.is_absolute()
    }
}

#[cfg(any(windows, test))]
pub(crate) fn format_windows_symlink_target(raw: &str) -> String {
    let stripped = strip_windows_device_prefix(raw);
    let replaced = stripped.replace('/', r"\");
    let is_unc = replaced.starts_with(r"\\");
    let body = if is_unc {
        &replaced[2..]
    } else {
        &replaced[..]
    };
    let mut collapsed = String::with_capacity(replaced.len());
    if is_unc {
        collapsed.push_str(r"\\");
    }
    let mut prev_backslash = false;
    for ch in body.chars() {
        if ch == '\\' {
            if !prev_backslash {
                collapsed.push('\\');
            }
            prev_backslash = true;
        } else {
            collapsed.push(ch);
            prev_backslash = false;
        }
    }
    while collapsed.ends_with('\\') {
        let b = collapsed.as_bytes();
        let is_drive_root =
            b.len() == 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
        if collapsed == r"\" || collapsed == r"\\" || is_drive_root {
            break;
        }
        collapsed.pop();
    }
    collapsed
}

#[cfg(any(windows, test))]
pub(crate) fn normalize_windows_junction_path(raw: &str) -> String {
    let s = format_windows_symlink_target(raw);
    if let Some(after_unc) = s.strip_prefix(r"\\") {
        let mut parts = after_unc.split('\\').filter(|p| !p.is_empty());
        let Some(server) = parts.next() else {
            return s;
        };
        let Some(share) = parts.next() else {
            return format!(r"\\{server}");
        };
        let mut stack: Vec<&str> = Vec::new();
        for seg in parts {
            match seg {
                "." => {}
                ".." => {
                    stack.pop();
                }
                other => stack.push(other),
            }
        }
        if stack.is_empty() {
            return format!(r"\\{server}\{share}");
        }
        return format!(r"\\{server}\{share}\{}", stack.join(r"\"));
    }
    let b = s.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        let drive = b[0] as char;
        let mut stack: Vec<&str> = Vec::new();
        for seg in s[2..].split('\\') {
            match seg {
                "" | "." => {}
                ".." => {
                    stack.pop();
                }
                other => stack.push(other),
            }
        }
        if stack.is_empty() {
            return format!(r"{drive}:\");
        }
        return format!(r"{drive}:\{}", stack.join(r"\"));
    }
    s
}

#[cfg(any(windows, test))]
pub(crate) fn normalize_windows_read_link_target(raw: &str) -> String {
    strip_windows_device_prefix(raw).replace('\\', "/")
}

pub(crate) fn normalize_read_link_target(target: &Path) -> String {
    #[cfg(windows)]
    {
        normalize_windows_read_link_target(&target.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        target.to_string_lossy().into_owned()
    }
}

#[cfg(any(windows, test))]
pub(crate) fn classify_windows_symlink(
    link: &Path,
    target: &str,
    hint: SymlinkKindHint,
    existing_dir: bool,
    existing_file: bool,
) -> (String, PathBuf, bool) {
    let trailing_sep = target.ends_with('/') || target.ends_with('\\');
    let win_target = format_windows_symlink_target(target);
    let win_link = PathBuf::from(link.to_string_lossy().replace('/', r"\"));
    let resolved = if is_windows_path_str_absolute(&win_target) {
        PathBuf::from(&win_target)
    } else {
        win_link
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(&win_target)
    };
    let is_dir_link = match hint {
        SymlinkKindHint::Dir => true,
        SymlinkKindHint::File => false,
        SymlinkKindHint::Auto => {
            if existing_dir {
                true
            } else if existing_file {
                false
            } else {
                trailing_sep
            }
        }
    };
    (win_target, resolved, is_dir_link)
}

/// Plants a symbolic link at `link -> target`, honoring `hint` and Bazel
/// `WindowsFileSystem.createSymbolicLink` fallback rules on Windows (normalizing
/// NT `\??\` prefixes and `/` -> `\` separators so NTFS reparse targets never
/// fail traversal with `ERROR_INVALID_NAME`, using `symlink_dir` / NTFS junction
/// `mklink /J` for directories, and `symlink_file` / copy fallback for files).
pub fn plant_symlink(link: &Path, target: &str, hint: SymlinkKindHint) -> io::Result<()> {
    #[cfg(windows)]
    let win_link = PathBuf::from(link.to_string_lossy().replace('/', r"\"));
    #[cfg(windows)]
    let link: &Path = &win_link;
    if let Some(p) = link.parent() {
        std::fs::create_dir_all(p)?;
    }
    let _ = remove_existing_entry(link);
    #[cfg(unix)]
    {
        let _ = hint;
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        let probe_target = format_windows_symlink_target(target);
        let probe_resolved = if is_windows_path_str_absolute(&probe_target) {
            PathBuf::from(&probe_target)
        } else {
            link.parent()
                .unwrap_or_else(|| Path::new("."))
                .join(&probe_target)
        };
        let stat_follow = std::fs::metadata(&probe_resolved).ok();
        let existing_dir = stat_follow.as_ref().is_some_and(|m| m.is_dir());
        let existing_file = stat_follow.as_ref().is_some_and(|m| m.is_file());
        let (win_target, resolved, is_dir_link) =
            classify_windows_symlink(link, target, hint, existing_dir, existing_file);
        if is_dir_link {
            match std::os::windows::fs::symlink_dir(&win_target, link) {
                Ok(()) => Ok(()),
                Err(sym_err) => {
                    let cwd = std::env::current_dir().ok();
                    let abs_link = if link.is_absolute() {
                        link.to_path_buf()
                    } else if let Some(c) = &cwd {
                        c.join(link)
                    } else {
                        link.to_path_buf()
                    };
                    let abs_target =
                        if resolved.is_absolute() || is_windows_path_str_absolute(&win_target) {
                            resolved
                        } else if let Some(c) = &cwd {
                            c.join(&resolved)
                        } else {
                            resolved
                        };
                    let junc_link = normalize_windows_junction_path(&abs_link.to_string_lossy());
                    let junc_target =
                        normalize_windows_junction_path(&abs_target.to_string_lossy());
                    let ok = std::process::Command::new("cmd")
                        .args(["/c", "mklink", "/J"])
                        .arg(&junc_link)
                        .arg(&junc_target)
                        .output()
                        .map(|o| o.status.success())
                        .unwrap_or(false);
                    if ok {
                        Ok(())
                    } else {
                        Err(sym_err)
                    }
                }
            }
        } else {
            match std::os::windows::fs::symlink_file(&win_target, link) {
                Ok(()) => Ok(()),
                Err(sym_err) if sym_err.raw_os_error() == Some(1314) && existing_file => {
                    std::fs::copy(&resolved, link).map(|_| ())
                }
                Err(sym_err) => Err(sym_err),
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, hint);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "symlinks unsupported",
        ))
    }
}

#[cfg(any(not(unix), test))]
fn normalize_windows_canonical_path(raw: &str) -> String {
    let replaced = raw.replace('/', r"\");
    let mut s = strip_windows_device_prefix(&replaced);
    while s.len() > 3 && s.ends_with('\\') && !s.ends_with(r":\") {
        s.pop();
    }
    s.to_lowercase()
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DirIdentity {
    dev: u64,
    ino: u64,
}

#[cfg(not(unix))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DirIdentity(String);

pub(crate) fn dir_identity(path: &Path) -> io::Result<DirIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let md = std::fs::metadata(path)?;
        Ok(DirIdentity {
            dev: md.dev(),
            ino: md.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        let canon = std::fs::canonicalize(path)?;
        Ok(DirIdentity(normalize_windows_canonical_path(
            &canon.to_string_lossy(),
        )))
    }
}

/// Tracks the active directory recursion stack so symlink cycles to `root` or
/// any ancestor directory fail immediately with `InvalidData`, while DAG
/// diamonds (sibling symlinks pointing to the same non-ancestor directory)
/// succeed cleanly.
pub(crate) struct AncestorDirs {
    active: HashSet<DirIdentity>,
}

impl AncestorDirs {
    pub(crate) fn for_root(root: &Path) -> io::Result<Self> {
        let mut active = HashSet::new();
        active.insert(dir_identity(root)?);
        if let Ok(canon) = std::fs::canonicalize(root) {
            for anc in canon.ancestors() {
                if let Ok(id) = dir_identity(anc) {
                    active.insert(id);
                }
            }
        }
        Ok(Self { active })
    }

    pub(crate) fn enter(&mut self, dir: &Path) -> io::Result<DirIdentity> {
        let id = dir_identity(dir)?;
        if !self.active.insert(id.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Symlink cycle detected while traversing directory {}",
                    dir.display()
                ),
            ));
        }
        Ok(id)
    }

    pub(crate) fn leave(&mut self, id: &DirIdentity) {
        self.active.remove(id);
    }
}

pub fn tree_from_local_dir_ext(
    dir: &Path,
    digest_fn: DigestFn,
    allow_absolute_symlinks: bool,
    blobs: &mut BTreeMap<Digest, BlobSource>,
) -> io::Result<Tree> {
    fn rec(
        dir: &Path,
        f: DigestFn,
        allow_abs: bool,
        blobs: &mut BTreeMap<Digest, BlobSource>,
        children: &mut Vec<Directory>,
        ancestors: &mut AncestorDirs,
    ) -> io::Result<Directory> {
        let mut d = Directory::default();
        let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
        // Sort by UTF-16 code units of entry name (matching Java String.compareTo).
        entries.sort_by(|a, b| {
            let na = a.file_name().to_string_lossy().into_owned();
            let nb = b.file_name().to_string_lossy().into_owned();
            na.encode_utf16().cmp(nb.encode_utf16())
        });
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            let ft = e.file_type()?;
            if ft.is_symlink() || zel_vfs::is_junction_or_symlink(&path) {
                let target_pb = std::fs::read_link(&path)?;
                let target_str = normalize_read_link_target(&target_pb);
                if is_absolute_symlink_target(&target_pb) {
                    match std::fs::metadata(&path) {
                        Ok(target_md) if target_md.is_dir() => {
                            let guard_id = ancestors.enter(&path)?;
                            let res = rec(&path, f, allow_abs, blobs, children, ancestors);
                            ancestors.leave(&guard_id);
                            let sub = res?;
                            let bytes = sub.encode_to_vec();
                            let dg = f.digest(&bytes);
                            if dg.size_bytes > 0 {
                                blobs.insert(dg.clone(), BlobSource::Bytes(bytes));
                            }
                            children.push(sub);
                            d.directories.push(DirectoryNode {
                                name,
                                digest: Some(dg),
                            });
                            continue;
                        }
                        Ok(target_md) if target_md.is_file() => {
                            let data = std::fs::read(&path)?;
                            let dg = f.digest(&data);
                            if dg.size_bytes > 0 {
                                blobs
                                    .entry(dg.clone())
                                    .or_insert(BlobSource::File(path.clone()));
                            }
                            d.files.push(FileNode {
                                name,
                                digest: Some(dg),
                                is_executable: true,
                                node_properties: None,
                            });
                            continue;
                        }
                        Ok(_) => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("Illegal special output file {}", path.display()),
                            ));
                        }
                        Err(_) if !allow_abs => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "{} is a dangling absolute symbolic link (-> {}), disallowed by remote cache",
                                    path.display(),
                                    target_str
                                ),
                            ));
                        }
                        Err(_) => {}
                    }
                }
                d.symlinks.push(SymlinkNode {
                    name,
                    target: target_str,
                    node_properties: None,
                });
            } else if ft.is_dir() {
                let guard_id = ancestors.enter(&path)?;
                let res = rec(&path, f, allow_abs, blobs, children, ancestors);
                ancestors.leave(&guard_id);
                let sub = res?;
                let bytes = sub.encode_to_vec();
                let dg = f.digest(&bytes);
                if dg.size_bytes > 0 {
                    blobs.insert(dg.clone(), BlobSource::Bytes(bytes));
                }
                children.push(sub);
                d.directories.push(DirectoryNode {
                    name,
                    digest: Some(dg),
                });
            } else if ft.is_file() {
                let data = std::fs::read(&path)?;
                let dg = f.digest(&data);
                if dg.size_bytes > 0 {
                    blobs
                        .entry(dg.clone())
                        .or_insert(BlobSource::File(path.clone()));
                }
                // UploadManifest sets `is_executable = true` on all uploaded regular files.
                d.files.push(FileNode {
                    name,
                    digest: Some(dg),
                    is_executable: true,
                    node_properties: None,
                });
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Illegal special output file {}", path.display()),
                ));
            }
        }
        Ok(d)
    }
    let mut ancestors = AncestorDirs::for_root(dir)?;
    let mut children = Vec::new();
    let root = rec(
        dir,
        digest_fn,
        allow_absolute_symlinks,
        blobs,
        &mut children,
        &mut ancestors,
    )?;
    let mut seen = HashSet::new();
    children.retain(|c| seen.insert(digest_fn.hash(&c.encode_to_vec())));
    Ok(Tree {
        root: Some(root),
        children,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_tree_has_sorted_children_omits_empty_blobs_and_has_stable_digest() {
        let c = MerkleTreeComputer::new(DigestFn::Sha256);
        assert!(c.build(&[]).unwrap().blobs.is_empty());

        let inputs = vec![
            MerkleInput::bytes("b/z.txt", b"zz".to_vec(), false),
            MerkleInput::bytes("a.txt", b"a".to_vec(), true),
            MerkleInput::bytes("empty_file.txt", Vec::new(), false),
            MerkleInput::bytes("b/c/d.txt", b"d".to_vec(), false),
            MerkleInput::symlink("lnk", "a.txt"),
            MerkleInput::empty_dir("empty"),
        ];
        let t = c.build(&inputs).unwrap();
        assert_eq!(t.root.files.len(), 2);
        assert!(t.root.files[0].is_executable);
        let names: Vec<_> = t.root.directories.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["b", "empty"]);
        assert_eq!(t.root.symlinks[0].target, "a.txt");
        assert_eq!(t.input_files, 4);
        // Order of inputs must not change the digest.
        let mut rev = inputs.clone();
        rev.reverse();
        assert_eq!(c.build(&rev).unwrap().root_digest, t.root_digest);
        assert_eq!(
            t.root_digest,
            DigestFn::Sha256.digest(&t.root.encode_to_vec())
        );
        assert!(t.blobs.contains_key(&t.root_digest));
        // Empty blob (size 0) is omitted from MerkleTree.blobs!
        assert!(!t.blobs.contains_key(&DigestFn::Sha256.digest(b"")));
    }

    #[test]
    fn conflicts_are_errors_and_duplicates_dedupe() {
        let c = MerkleTreeComputer::new(DigestFn::Sha256);
        let same = vec![
            MerkleInput::bytes("x", b"1".to_vec(), false),
            MerkleInput::bytes("x", b"1".to_vec(), false),
        ];
        assert_eq!(c.build(&same).unwrap().input_files, 1);
        let diff = vec![
            MerkleInput::bytes("x", b"1".to_vec(), false),
            MerkleInput::bytes("x", b"2".to_vec(), false),
        ];
        assert!(c.build(&diff).is_err());
        let fd = vec![
            MerkleInput::bytes("x", b"1".to_vec(), false),
            MerkleInput::bytes("x/y", b"2".to_vec(), false),
        ];
        assert!(c.build(&fd).is_err());
        assert!(c
            .build(&[MerkleInput::bytes("../x", vec![], false)])
            .is_err());
    }

    #[test]
    fn flatten_roundtrip_through_local_dir() {
        let tmp = std::env::temp_dir().join(format!("zel-merkle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub/deep")).unwrap();
        std::fs::write(tmp.join("a"), b"A").unwrap();
        std::fs::write(tmp.join("sub/deep/b"), b"B").unwrap();
        std::fs::write(tmp.join("sub/b2"), b"B").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("a", tmp.join("link")).unwrap();
        let mut blobs = BTreeMap::new();
        let tree = tree_from_local_dir(&tmp, DigestFn::Sha256, &mut blobs).unwrap();
        let entries = flatten_tree(&tree, DigestFn::Sha256).unwrap();
        assert!(entries.iter().any(
            |e| matches!(e, TreeEntry::File { path, executable: true, .. } if path == "sub/deep/b")
        ));
        assert!(entries
            .iter()
            .any(|e| matches!(e, TreeEntry::Directory { path } if path == "sub/deep")));
        #[cfg(unix)]
        assert!(entries.iter().any(
            |e| matches!(e, TreeEntry::Symlink { path, target } if path == "link" && target == "a")
        ));
        let mut bad = tree.clone();
        bad.children.clear();
        assert!(flatten_tree(&bad, DigestFn::Sha256).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn local_file_inputs_use_file_blob_source() {
        let tmp = std::env::temp_dir().join(format!("zel-merkle-f-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let p = tmp.join("f.txt");
        std::fs::write(&p, b"hello").unwrap();
        let c = MerkleTreeComputer::new(DigestFn::Sha256);
        let t = c
            .build(&[MerkleInput::file("src/f.txt", p.clone(), false)])
            .unwrap();
        let d = DigestFn::Sha256.digest(b"hello");
        assert_eq!(t.blobs.get(&d), Some(&BlobSource::File(p)));
        assert_eq!(t.input_bytes, 5);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn make_dir_symlink(target: &Path, link: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(target, link)
        }
    }

    #[test]
    fn tree_from_local_dir_ext_rejects_absolute_symlink_cycles_to_root_or_ancestor() {
        let base = std::env::temp_dir().join(format!("zel-merkle-cycle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let ancestor = base.join("ws");
        let out_root = ancestor.join("sub").join("out");
        std::fs::create_dir_all(out_root.join("nested")).unwrap();
        std::fs::write(out_root.join("file.txt"), b"ok").unwrap();

        let abs_out_root = std::fs::canonicalize(&out_root).unwrap();
        let abs_ancestor = std::fs::canonicalize(&ancestor).unwrap();

        // 1. Absolute symlink pointing directly back to the output tree root.
        let self_link = out_root.join("nested").join("self_loop");
        make_dir_symlink(&abs_out_root, &self_link).unwrap();
        let mut blobs = BTreeMap::new();
        let err = tree_from_local_dir_ext(&out_root, DigestFn::Sha256, true, &mut blobs)
            .expect_err("symlink pointing to tree root must fail without stack overflow");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        remove_existing_entry(&self_link).unwrap();

        // 2. Absolute symlink pointing to an ancestor above the output tree root.
        let anc_link = out_root.join("nested").join("ancestor_loop");
        make_dir_symlink(&abs_ancestor, &anc_link).unwrap();
        let mut blobs = BTreeMap::new();
        let err = tree_from_local_dir_ext(&out_root, DigestFn::Sha256, true, &mut blobs)
            .expect_err("symlink pointing to ancestor directory must fail without stack overflow");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        remove_existing_entry(&anc_link).unwrap();

        // 3. Mutual cycle between two external directories (`ext_a -> ext_b -> ext_a`).
        let ext_a = base.join("ext_a");
        let ext_b = base.join("ext_b");
        std::fs::create_dir_all(&ext_a).unwrap();
        std::fs::create_dir_all(&ext_b).unwrap();
        let abs_ext_a = std::fs::canonicalize(&ext_a).unwrap();
        let abs_ext_b = std::fs::canonicalize(&ext_b).unwrap();
        let ext_a_to_b = ext_a.join("to_b");
        let ext_b_to_a = ext_b.join("to_a");
        let ext_entry = out_root.join("ext_entry");
        make_dir_symlink(&abs_ext_b, &ext_a_to_b).unwrap();
        make_dir_symlink(&abs_ext_a, &ext_b_to_a).unwrap();
        make_dir_symlink(&abs_ext_a, &ext_entry).unwrap();
        let mut blobs = BTreeMap::new();
        let err = tree_from_local_dir_ext(&out_root, DigestFn::Sha256, true, &mut blobs)
            .expect_err("mutual external directory symlink cycle must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        // Unlink cyclic directory symlinks explicitly before recursive temp cleanup.
        remove_existing_entry(&ext_entry).unwrap();
        remove_existing_entry(&ext_a_to_b).unwrap();
        remove_existing_entry(&ext_b_to_a).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn tree_from_local_dir_ext_preserves_sibling_dag_diamond_symlinks() {
        let base = std::env::temp_dir().join(format!("zel-merkle-diamond-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let shared = base.join("shared_external");
        let out_root = base.join("out");
        std::fs::create_dir_all(shared.join("inner")).unwrap();
        std::fs::write(shared.join("hello.txt"), b"shared-payload").unwrap();
        std::fs::write(shared.join("inner/deep.txt"), b"deep-payload").unwrap();
        std::fs::create_dir_all(&out_root).unwrap();

        let abs_shared = std::fs::canonicalize(&shared).unwrap();
        let first_link = out_root.join("first");
        let second_link = out_root.join("second");
        make_dir_symlink(&abs_shared, &first_link).unwrap();
        make_dir_symlink(&abs_shared, &second_link).unwrap();

        let mut blobs = BTreeMap::new();
        let tree = tree_from_local_dir_ext(&out_root, DigestFn::Sha256, false, &mut blobs)
            .expect("sibling symlinks to the same non-ancestor directory form a DAG diamond");

        let root = tree.root.as_ref().unwrap();
        let dir_names: Vec<_> = root.directories.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(dir_names, ["first", "second"]);
        assert_eq!(root.directories[0].digest, root.directories[1].digest);
        // Shared child Directory protos are deduplicated in `Tree.children`.
        assert_eq!(tree.children.len(), 2);

        let entries = flatten_tree(&tree, DigestFn::Sha256).unwrap();
        assert!(entries
            .iter()
            .any(|e| matches!(e, TreeEntry::File { path, .. } if path == "first/inner/deep.txt")));
        assert!(entries
            .iter()
            .any(|e| matches!(e, TreeEntry::File { path, .. } if path == "second/inner/deep.txt")));

        remove_existing_entry(&first_link).unwrap();
        remove_existing_entry(&second_link).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn windows_canonical_path_normalization_unifies_verbatim_nt_and_casing() {
        assert_eq!(
            normalize_windows_canonical_path(r"\\?\C:\Users\Runner\Out\"),
            normalize_windows_canonical_path(r"c:/users/runner/out")
        );
        assert_eq!(
            normalize_windows_canonical_path(r"\??\C:\Users\Runner\Out"),
            r"c:\users\runner\out"
        );
        assert_eq!(normalize_windows_canonical_path(r"\\?\C:\"), r"c:\");
        // Verbatim UNC (`\\?\UNC\...`), NT UNC (`\??\UNC\...`), and plain UNC (`\\...`)
        // must all normalize to the identical `\\server\share\dir` identity.
        let unc_verbatim = normalize_windows_canonical_path(r"\\?\UNC\Server\Share\Dir\");
        let unc_nt = normalize_windows_canonical_path(r"\??\UNC\server\share\dir");
        let unc_nt_mixed = normalize_windows_canonical_path(r"\??\Unc\Server\Share\Dir\");
        let unc_plain = normalize_windows_canonical_path(r"\\server\share\dir");
        assert_eq!(unc_verbatim, r"\\server\share\dir");
        assert_eq!(unc_nt, unc_verbatim);
        assert_eq!(unc_nt_mixed, unc_verbatim);
        assert_eq!(unc_plain, unc_verbatim);
        assert_eq!(
            strip_windows_prefix_case_insensitive(r"C:\Ws\ExecRoot\out\a", r"c:/ws/execroot/"),
            Some(r"out\a".to_string())
        );
    }

    #[test]
    fn windows_symlink_classification_handles_nt_prefixes_hints_and_dangling_targets() {
        let link = Path::new(r"C:\ws\execroot\out\link_node");

        // NT `\??\C:\...` junction target is stripped to `C:\...` and treated as absolute
        // (never joined onto `link.parent()`), classifying as a directory symlink when target exists.
        let (win_t, resolved, is_dir) = classify_windows_symlink(
            link,
            r"\??\C:\ext\shared_dir",
            SymlinkKindHint::Auto,
            true,
            false,
        );
        assert_eq!(win_t, r"C:\ext\shared_dir");
        assert_eq!(resolved, PathBuf::from(r"C:\ext\shared_dir"));
        assert!(is_dir);

        // NT `\??\UNC\...` target rewrites to `\\server\share\...` and is treated as absolute.
        let (win_unc, resolved_unc, is_dir_unc) = classify_windows_symlink(
            link,
            r"\??\UNC\Server\Share\dir",
            SymlinkKindHint::Auto,
            true,
            false,
        );
        assert_eq!(win_unc, r"\\Server\Share\dir");
        assert_eq!(resolved_unc, PathBuf::from(r"\\Server\Share\dir"));
        assert!(is_dir_unc);

        // Explicit `SymlinkKindHint::Dir` (`output_directory_symlinks` or declared tree output)
        // classifies even a dangling relative target as a directory symlink and rewrites `/` -> `\`.
        let (win_dangling_dir, _, is_dangling_dir) = classify_windows_symlink(
            link,
            "../dangling_future_dir",
            SymlinkKindHint::Dir,
            false,
            false,
        );
        assert_eq!(win_dangling_dir, r"..\dangling_future_dir");
        assert!(is_dangling_dir);

        // Explicit `SymlinkKindHint::File` (`output_file_symlinks`) classifies as file symlink
        // and rewrites `/` -> `\`.
        let (win_dangling_file, _, is_dangling_file) = classify_windows_symlink(
            link,
            "../dangling_file",
            SymlinkKindHint::File,
            false,
            false,
        );
        assert_eq!(win_dangling_file, r"..\dangling_file");
        assert!(!is_dangling_file);

        // `Auto` with trailing `/` or `\` on a dangling target classifies as a directory symlink
        // while stripping the trailing separator before Win32 `CreateSymbolicLinkW`.
        let (win_trailing, _, is_trailing_slash_dir) = classify_windows_symlink(
            link,
            "../dangling_dir/",
            SymlinkKindHint::Auto,
            false,
            false,
        );
        assert_eq!(win_trailing, r"..\dangling_dir");
        assert!(is_trailing_slash_dir);
    }

    #[test]
    fn windows_symlink_target_and_read_link_helpers_roundtrip_posix_and_win32_forms() {
        // POSIX relative targets must contain zero `/` forward slashes or trailing `\` when
        // passed to Win32 `CreateSymbolicLinkW` (preventing NTFS `ERROR_INVALID_NAME` / os error 123).
        assert_eq!(
            format_windows_symlink_target("../z_late/nested"),
            r"..\z_late\nested"
        );
        assert_eq!(
            format_windows_symlink_target("../dangling_dir/"),
            r"..\dangling_dir"
        );
        assert_eq!(format_windows_symlink_target("./sub//leaf/"), r".\sub\leaf");
        assert_eq!(format_windows_symlink_target("./"), ".");
        assert_eq!(format_windows_symlink_target("/"), r"\");
        assert_eq!(
            format_windows_symlink_target(r"\??\C:\Users\runner\dir/"),
            r"C:\Users\runner\dir"
        );
        assert_eq!(format_windows_symlink_target(r"\??\C:/"), r"C:\");
        assert_eq!(
            format_windows_symlink_target(r"\??\UNC\Server\Share\dir//sub/"),
            r"\\Server\Share\dir\sub"
        );

        // Junction path helper resolves `.` and `..` lexically into clean Win32 absolute paths.
        assert_eq!(
            normalize_windows_junction_path(r"C:\ws\execroot\out_a\..\out_b\sub\"),
            r"C:\ws\execroot\out_b\sub"
        );
        assert_eq!(
            normalize_windows_junction_path(r"\\Server\Share\ws\a\.\..\b/"),
            r"\\Server\Share\ws\b"
        );
        assert_eq!(normalize_windows_junction_path(r"C:\ws\.."), r"C:\");

        // Reading back a Windows symlink normalizes `\` -> `/` and strips NT `\??\` prefixes
        // for REAPI `SymlinkNode.target` / `OutputSymlink.target` upload parity.
        assert_eq!(
            normalize_windows_read_link_target(r"..\out_b\sub"),
            "../out_b/sub"
        );
        assert_eq!(
            normalize_windows_read_link_target(r"\??\C:\ws\out_b\sub"),
            "C:/ws/out_b/sub"
        );
        assert_eq!(
            normalize_windows_read_link_target(r"\??\UNC\Server\Share\dir"),
            "//Server/Share/dir"
        );
    }

    #[test]
    fn flatten_to_local_dir_plants_cross_subtree_directory_symlinks_traversably() {
        // `a_early/link_to_z -> ../z_late/nested` appears alphabetically BEFORE `z_late/nested`
        // in the `Tree`; two-pass materialization ensures `z_late/nested` already exists on disk
        // when `a_early/link_to_z` is planted, so Windows creates a directory symlink (`symlink_dir`)
        // that can be traversed both by `std::fs::read_dir` and `tree_from_local_dir`.
        let base = std::env::temp_dir().join(format!("zel-merkle-xtree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let c = MerkleTreeComputer::new(DigestFn::Sha256);
        let built = c
            .build(&[
                MerkleInput::symlink("a_early/link_to_z", "../z_late/nested"),
                MerkleInput::symlink("a_early/chain_to_z", "link_to_z"),
                MerkleInput::bytes(
                    "z_late/nested/payload.txt",
                    b"cross-tree-ok".to_vec(),
                    false,
                ),
            ])
            .unwrap();
        let mut children = Vec::new();
        for (dg, src) in &built.blobs {
            if *dg != built.root_digest {
                if let BlobSource::Bytes(b) = src {
                    if let Ok(dir_proto) = Directory::decode(b) {
                        if !dir_proto.files.is_empty()
                            || !dir_proto.directories.is_empty()
                            || !dir_proto.symlinks.is_empty()
                        {
                            children.push(dir_proto);
                        }
                    }
                }
            }
        }
        let tree = Tree {
            root: Some(built.root.clone()),
            children,
        };

        let dest = base.join("materialized");
        flatten_to_local_dir(&tree, DigestFn::Sha256, &dest, |dg| {
            match built.blobs.get(dg) {
                Some(BlobSource::Bytes(b)) => Ok(b.clone()),
                _ => Err(io::Error::new(io::ErrorKind::NotFound, "missing test blob")),
            }
        })
        .unwrap();

        assert_eq!(
            std::fs::read(dest.join("a_early/link_to_z/payload.txt")).unwrap(),
            b"cross-tree-ok"
        );
        assert_eq!(
            std::fs::read(dest.join("a_early/chain_to_z/payload.txt")).unwrap(),
            b"cross-tree-ok"
        );

        let mut reupload_blobs = BTreeMap::new();
        let reuploaded_early =
            tree_from_local_dir(&dest.join("a_early"), DigestFn::Sha256, &mut reupload_blobs)
                .unwrap();
        let early_root = reuploaded_early.root.unwrap();
        if !early_root.symlinks.is_empty() {
            let link_node = early_root
                .symlinks
                .iter()
                .find(|s| s.name == "link_to_z")
                .unwrap();
            assert_eq!(link_node.target, "../z_late/nested");
        } else {
            assert_eq!(early_root.directories.len(), 2);
        }

        let _ = remove_existing_entry(&dest.join("a_early/chain_to_z"));
        let _ = remove_existing_entry(&dest.join("a_early/link_to_z"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
