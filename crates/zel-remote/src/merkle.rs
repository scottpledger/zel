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
//! the set of blobs needed in the CAS. Also the inverse (`Tree` flattening) for output directories.

use crate::digest::DigestFn;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::UNIX_EPOCH;
use zel_proto::reapi::*;

#[derive(Debug, Clone)]
pub enum InputKind {
    /// A regular file on the local file system (symlinks already resolved by the caller).
    File { local: PathBuf, executable: bool },
    /// Inline file contents.
    Bytes { data: Vec<u8>, executable: bool },
    /// A file known only by digest (a remote output that was never downloaded); the CAS is
    /// assumed to hold it already, so no blob source is recorded.
    Remote { digest: Digest, executable: bool },
    /// An unresolved symlink artifact.
    Symlink { target: String },
    EmptyDirectory,
}

#[derive(Debug, Clone)]
pub struct MerkleInput {
    /// Path relative to the input root, `/`-separated.
    pub path: String,
    pub kind: InputKind,
    /// Optional REAPI node properties (e.g. `--remote_...` mtime / unix mode).
    pub node_properties: Option<NodeProperties>,
}

impl MerkleInput {
    pub fn file(path: &str, local: PathBuf, executable: bool) -> Self {
        Self { path: path.into(), kind: InputKind::File { local, executable }, node_properties: None }
    }
    pub fn bytes(path: &str, data: Vec<u8>, executable: bool) -> Self {
        Self { path: path.into(), kind: InputKind::Bytes { data, executable }, node_properties: None }
    }
    pub fn symlink(path: &str, target: &str) -> Self {
        Self { path: path.into(), kind: InputKind::Symlink { target: target.into() }, node_properties: None }
    }
    pub fn empty_dir(path: &str) -> Self {
        Self { path: path.into(), kind: InputKind::EmptyDirectory, node_properties: None }
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
    /// All blobs (directories + files) needed in the CAS to execute with this input root.
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

/// Computes Merkle trees, caching file digests by (path, size, mtime).
pub struct MerkleTreeComputer {
    pub digest_fn: DigestFn,
    cache: Mutex<HashMap<(PathBuf, u64, u128), Digest>>,
}

impl MerkleTreeComputer {
    pub fn new(digest_fn: DigestFn) -> Self {
        Self { digest_fn, cache: Mutex::new(HashMap::new()) }
    }

    fn file_digest(&self, path: &PathBuf) -> io::Result<Digest> {
        let md = std::fs::metadata(path)?;
        let mtime = md.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
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
        let mut root = DirBuilder::default();
        let mut blobs: BTreeMap<Digest, BlobSource> = BTreeMap::new();
        let mut input_files = 0usize;
        let mut input_bytes = 0i64;
        let bad = |m: String| io::Error::new(io::ErrorKind::InvalidInput, m);

        for inp in inputs {
            let segs: Vec<&str> = inp.path.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
            if segs.is_empty() {
                if matches!(inp.kind, InputKind::EmptyDirectory) {
                    continue;
                }
                return Err(bad(format!("empty input path for {:?}", inp.kind)));
            }
            if segs.contains(&"..") {
                return Err(bad(format!("input path escapes the input root: {}", inp.path)));
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
                    let node = SymlinkNode { name: name.clone(), target: target.clone(), node_properties: inp.node_properties.clone() };
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
                        InputKind::Remote { digest, executable } => (digest.clone(), *executable, None),
                        InputKind::File { local, executable } => {
                            let d = self.file_digest(local).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", local.display())))?;
                            (d, *executable, Some(BlobSource::File(local.clone())))
                        }
                        InputKind::Bytes { data, executable } => {
                            (self.digest_fn.digest(data), *executable, Some(BlobSource::Bytes(data.clone())))
                        }
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
                    if let Some(src) = src {
                        blobs.entry(digest).or_insert(src);
                    }
                    cur.files.insert(name, node);
                }
            }
        }
        let (root_dir, root_digest) = self.finish(&root, &mut blobs);
        Ok(MerkleTree { root_digest, root: root_dir, blobs, input_files, input_bytes })
    }

    fn finish(&self, b: &DirBuilder, blobs: &mut BTreeMap<Digest, BlobSource>) -> (Directory, Digest) {
        let mut dir = Directory::default();
        for (name, sub) in &b.dirs {
            let (_, d) = self.finish(sub, blobs);
            dir.directories.push(DirectoryNode { name: name.clone(), digest: Some(d) });
        }
        dir.files = b.files.values().cloned().collect();
        dir.symlinks = b.symlinks.values().cloned().collect();
        let bytes = dir.encode_to_vec();
        let digest = self.digest_fn.digest(&bytes);
        blobs.insert(digest.clone(), BlobSource::Bytes(bytes));
        (dir, digest)
    }
}

/// A flattened output directory entry.
#[derive(Debug, Clone, PartialEq)]
pub enum TreeEntry {
    File { path: String, digest: Digest, executable: bool },
    Symlink { path: String, target: String },
    Directory { path: String },
}

/// Flattens a REAPI `Tree` (root + children) into entries with paths relative to the tree root,
/// in depth-first order. Errors on missing children or cycles/over-deep recursion.
pub fn flatten_tree(tree: &Tree, digest_fn: DigestFn) -> Result<Vec<TreeEntry>, String> {
    let mut by_digest: HashMap<String, &Directory> = HashMap::new();
    for c in &tree.children {
        by_digest.insert(digest_fn.hash(&c.encode_to_vec()), c);
    }
    let root = tree.root.as_ref().ok_or("tree has no root")?;
    let mut out = Vec::new();
    fn walk(
        dir: &Directory,
        prefix: &str,
        by: &HashMap<String, &Directory>,
        out: &mut Vec<TreeEntry>,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 256 {
            return Err("tree too deep".into());
        }
        let join = |n: &str| if prefix.is_empty() { n.to_string() } else { format!("{prefix}/{n}") };
        for f in &dir.files {
            out.push(TreeEntry::File {
                path: join(&f.name),
                digest: f.digest.clone().ok_or("file node without digest")?,
                executable: f.is_executable,
            });
        }
        for s in &dir.symlinks {
            out.push(TreeEntry::Symlink { path: join(&s.name), target: s.target.clone() });
        }
        for d in &dir.directories {
            let dd = d.digest.clone().ok_or("directory node without digest")?;
            let p = join(&d.name);
            out.push(TreeEntry::Directory { path: p.clone() });
            let child = by.get(&dd.hash).ok_or_else(|| format!("missing child directory {} in tree", dd.hash))?;
            walk(child, &p, by, out, depth + 1)?;
        }
        Ok(())
    }
    walk(root, "", &by_digest, &mut out, 0)?;
    Ok(out)
}

/// Builds a REAPI `Tree` from a local directory (used when uploading output directories).
pub fn tree_from_local_dir(
    dir: &std::path::Path,
    digest_fn: DigestFn,
    blobs: &mut BTreeMap<Digest, BlobSource>,
) -> io::Result<Tree> {
    fn rec(
        dir: &std::path::Path,
        f: DigestFn,
        blobs: &mut BTreeMap<Digest, BlobSource>,
        children: &mut Vec<Directory>,
    ) -> io::Result<Directory> {
        let mut d = Directory::default();
        let mut names: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
        names.sort_by_key(|e| e.file_name());
        for e in names {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            let ft = e.file_type()?;
            if ft.is_symlink() {
                let target = std::fs::read_link(&path)?.to_string_lossy().into_owned();
                d.symlinks.push(SymlinkNode { name, target, node_properties: None });
            } else if ft.is_dir() {
                let sub = rec(&path, f, blobs, children)?;
                let bytes = sub.encode_to_vec();
                let dg = f.digest(&bytes);
                blobs.insert(dg.clone(), BlobSource::Bytes(bytes));
                children.push(sub);
                d.directories.push(DirectoryNode { name, digest: Some(dg) });
            } else {
                let data = std::fs::read(&path)?;
                let dg = f.digest(&data);
                blobs.entry(dg.clone()).or_insert(BlobSource::File(path.clone()));
                #[cfg(unix)]
                let exec = {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::metadata(&path)?.permissions().mode() & 0o100 != 0
                };
                #[cfg(not(unix))]
                let exec = false;
                d.files.push(FileNode { name, digest: Some(dg), is_executable: exec, node_properties: None });
            }
        }
        Ok(d)
    }
    let mut children = Vec::new();
    let root = rec(dir, digest_fn, blobs, &mut children)?;
    // Deduplicate children by digest, as Bazel does.
    let mut seen = std::collections::HashSet::new();
    children.retain(|c| seen.insert(digest_fn.hash(&c.encode_to_vec())));
    Ok(Tree { root: Some(root), children })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_tree_has_sorted_children_and_stable_digest() {
        let c = MerkleTreeComputer::new(DigestFn::Sha256);
        let inputs = vec![
            MerkleInput::bytes("b/z.txt", b"zz".to_vec(), false),
            MerkleInput::bytes("a.txt", b"a".to_vec(), true),
            MerkleInput::bytes("b/c/d.txt", b"d".to_vec(), false),
            MerkleInput::symlink("lnk", "a.txt"),
            MerkleInput::empty_dir("empty"),
        ];
        let t = c.build(&inputs).unwrap();
        assert_eq!(t.root.files.len(), 1);
        assert!(t.root.files[0].is_executable);
        let names: Vec<_> = t.root.directories.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["b", "empty"]);
        assert_eq!(t.root.symlinks[0].target, "a.txt");
        assert_eq!(t.input_files, 3);
        // Order of inputs must not change the digest.
        let mut rev = inputs.clone();
        rev.reverse();
        assert_eq!(c.build(&rev).unwrap().root_digest, t.root_digest);
        // Root digest equals the digest of the serialized root directory.
        assert_eq!(t.root_digest, DigestFn::Sha256.digest(&t.root.encode_to_vec()));
        assert!(t.blobs.contains_key(&t.root_digest));
        // Empty directory blob is the empty message.
        assert!(t.blobs.contains_key(&DigestFn::Sha256.digest(b"")));
    }

    #[test]
    fn conflicts_are_errors_and_duplicates_dedupe() {
        let c = MerkleTreeComputer::new(DigestFn::Sha256);
        let same = vec![MerkleInput::bytes("x", b"1".to_vec(), false), MerkleInput::bytes("x", b"1".to_vec(), false)];
        assert_eq!(c.build(&same).unwrap().input_files, 1);
        let diff = vec![MerkleInput::bytes("x", b"1".to_vec(), false), MerkleInput::bytes("x", b"2".to_vec(), false)];
        assert!(c.build(&diff).is_err());
        let fd = vec![MerkleInput::bytes("x", b"1".to_vec(), false), MerkleInput::bytes("x/y", b"2".to_vec(), false)];
        assert!(c.build(&fd).is_err());
        assert!(c.build(&[MerkleInput::bytes("../x", vec![], false)]).is_err());
    }

    #[test]
    fn flatten_roundtrip_through_local_dir() {
        let tmp = std::env::temp_dir().join(format!("zel-merkle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub/deep")).unwrap();
        std::fs::write(tmp.join("a"), b"A").unwrap();
        std::fs::write(tmp.join("sub/deep/b"), b"B").unwrap();
        std::fs::write(tmp.join("sub/b2"), b"B").unwrap(); // same content twice
        #[cfg(unix)]
        std::os::unix::fs::symlink("a", tmp.join("link")).unwrap();
        let mut blobs = BTreeMap::new();
        let tree = tree_from_local_dir(&tmp, DigestFn::Sha256, &mut blobs).unwrap();
        let entries = flatten_tree(&tree, DigestFn::Sha256).unwrap();
        assert!(entries.iter().any(|e| matches!(e, TreeEntry::File { path, .. } if path == "sub/deep/b")));
        assert!(entries.iter().any(|e| matches!(e, TreeEntry::Directory { path } if path == "sub/deep")));
        #[cfg(unix)]
        assert!(entries.iter().any(|e| matches!(e, TreeEntry::Symlink { path, target } if path == "link" && target == "a")));
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
        let t = c.build(&[MerkleInput::file("src/f.txt", p.clone(), false)]).unwrap();
        let d = DigestFn::Sha256.digest(b"hello");
        assert_eq!(t.blobs.get(&d), Some(&BlobSource::File(p)));
        assert_eq!(t.input_bytes, 5);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
