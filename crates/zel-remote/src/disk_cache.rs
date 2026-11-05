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

//! `--disk_cache` (`remote/disk/DiskCacheClient.java`): `<root>[/<digest fn>]/{ac,cas}/<first two
//! hex chars>/<hash>` plus `<root>/tmp`. Entries are written to a temp file and renamed; the
//! mtime of an entry records the last store or retrieval (for LRU garbage collection). The
//! `<digest fn>` directory is omitted for SHA-256 ("old style" layout).

use crate::digest::DigestFn;
use crate::merkle::{flatten_tree, BlobSource};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
use zel_proto::reapi::{ActionResult, Digest, Message, Tree};
use zel_vfs::PathFragment;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Store {
    Ac,
    Cas,
}

#[derive(Debug)]
pub struct DiskCache {
    root: PathBuf,
    ac_root: PathBuf,
    cas_root: PathBuf,
    tmp_root: PathBuf,
    pub digest_fn: DigestFn,
    check_ac_integrity: bool,
    tmp_counter: AtomicU64,
}

impl DiskCache {
    /// SHA-256 layout, with action-result integrity checking (as Bazel enables by default
    /// when `--remote_download_outputs` is not `all`... here always on).
    pub fn new(root: PathFragment) -> Self {
        Self::open(Path::new(root.path_string()), DigestFn::Sha256, true)
            .unwrap_or_else(|_| Self::unopened(Path::new(root.path_string()), DigestFn::Sha256, true))
    }

    fn unopened(root: &Path, f: DigestFn, check: bool) -> Self {
        let fn_root = match f.resource_name() {
            Some(n) => root.join(n),
            None => root.to_path_buf(),
        };
        Self {
            root: root.to_path_buf(),
            ac_root: fn_root.join("ac"),
            cas_root: fn_root.join("cas"),
            tmp_root: root.join("tmp"),
            digest_fn: f,
            check_ac_integrity: check,
            tmp_counter: AtomicU64::new(0),
        }
    }

    pub fn open(root: &Path, f: DigestFn, check_ac_integrity: bool) -> io::Result<Self> {
        let c = Self::unopened(root, f, check_ac_integrity);
        std::fs::create_dir_all(&c.ac_root)?;
        std::fs::create_dir_all(&c.cas_root)?;
        std::fs::create_dir_all(&c.tmp_root)?;
        Ok(c)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, store: Store, hash: &str) -> PathBuf {
        let base = match store {
            Store::Ac => &self.ac_root,
            Store::Cas => &self.cas_root,
        };
        let prefix = if hash.len() >= 2 { &hash[..2] } else { "00" };
        base.join(prefix).join(hash)
    }

    fn temp_path(&self) -> PathBuf {
        let n = self.tmp_counter.fetch_add(1, Ordering::SeqCst);
        let t = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        self.tmp_root.join(format!("{}-{t}-{n}", std::process::id()))
    }

    /// Updates the entry's mtime; returns whether it exists.
    pub fn refresh(&self, path: &Path) -> io::Result<bool> {
        match std::fs::File::open(path) {
            Ok(f) => {
                // Best effort: a shared cache may be owned by another user.
                let _ = f.set_modified(SystemTime::now());
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub fn contains_blob(&self, d: &Digest) -> bool {
        if d.size_bytes == 0 {
            return true;
        }
        self.refresh(&self.path(Store::Cas, &d.hash)).unwrap_or(false)
    }

    fn save(&self, store: Store, hash: &str, write: impl FnOnce(&Path) -> io::Result<()>) -> io::Result<()> {
        let path = self.path(store, hash);
        if store == Store::Cas && self.refresh(&path)? {
            return Ok(());
        }
        let tmp = self.temp_path();
        if let Some(p) = tmp.parent() {
            std::fs::create_dir_all(p)?;
        }
        let r = (|| {
            write(&tmp)?;
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p)?;
            }
            // Tolerates concurrent creation: rename atomically replaces with identical content.
            std::fs::rename(&tmp, &path)
        })();
        if r.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        r
    }

    pub fn put_blob(&self, d: &Digest, data: &[u8]) -> io::Result<()> {
        if d.size_bytes == 0 {
            return Ok(());
        }
        self.save(Store::Cas, &d.hash, |tmp| {
            std::fs::write(tmp, data)?;
            set_mode_0644(tmp);
            Ok(())
        })
    }

    pub fn put_file(&self, d: &Digest, file: &Path) -> io::Result<()> {
        if d.size_bytes == 0 {
            return Ok(());
        }
        self.save(Store::Cas, &d.hash, |tmp| {
            std::fs::copy(file, tmp)?;
            set_mode_0644(tmp);
            Ok(())
        })
    }

    pub fn put_source(&self, d: &Digest, s: &BlobSource) -> io::Result<()> {
        match s {
            BlobSource::Bytes(b) => self.put_blob(d, b),
            BlobSource::File(p) => self.put_file(d, p),
        }
    }

    /// Reads a blob (verifying its size) and refreshes its mtime.
    pub fn get_blob(&self, d: &Digest) -> io::Result<Option<Vec<u8>>> {
        if d.size_bytes == 0 {
            return Ok(Some(Vec::new()));
        }
        let p = self.path(Store::Cas, &d.hash);
        match std::fs::read(&p) {
            Ok(b) => {
                if b.len() as i64 != d.size_bytes {
                    return Ok(None); // corrupted entry: treat as a miss
                }
                let _ = self.refresh(&p);
                Ok(Some(b))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Copies a blob to `dest` (creating parents), optionally marking it executable.
    pub fn copy_blob_to(&self, d: &Digest, dest: &Path, executable: bool) -> io::Result<bool> {
        let Some(data) = self.get_blob(d)? else { return Ok(false) };
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p)?;
        }
        let _ = std::fs::remove_file(dest);
        std::fs::write(dest, data)?;
        set_executable(dest, executable);
        Ok(true)
    }

    pub fn put_action_result(&self, action: &Digest, r: &ActionResult) -> io::Result<()> {
        let bytes = r.encode_to_vec();
        self.save(Store::Ac, &action.hash, |tmp| {
            std::fs::write(tmp, &bytes)?;
            set_mode_0644(tmp);
            Ok(())
        })
    }

    /// Looks up an action result; with integrity checking the result is only returned if all
    /// blobs it references are present in the CAS (a partially evicted entry is a miss).
    pub fn get_action_result(&self, action: &Digest) -> Option<ActionResult> {
        let p = self.path(Store::Ac, &action.hash);
        let bytes = std::fs::read(&p).ok()?;
        let r = ActionResult::decode(&bytes).ok()?;
        if self.check_ac_integrity && !self.action_result_complete(&r) {
            return None;
        }
        let _ = self.refresh(&p);
        Some(r)
    }

    fn action_result_complete(&self, r: &ActionResult) -> bool {
        for f in &r.output_files {
            if let Some(d) = &f.digest {
                if !self.contains_blob(d) {
                    return false;
                }
            }
        }
        for d in [&r.stdout_digest, &r.stderr_digest].into_iter().flatten() {
            if !self.contains_blob(d) {
                return false;
            }
        }
        for od in &r.output_directories {
            let Some(td) = &od.tree_digest else { return false };
            let Ok(Some(bytes)) = self.get_blob(td) else { return false };
            let Ok(tree) = Tree::decode(&bytes) else { return false };
            let Ok(entries) = flatten_tree(&tree, self.digest_fn) else { return false };
            for e in entries {
                if let crate::merkle::TreeEntry::File { digest, .. } = e {
                    if !self.contains_blob(&digest) {
                        return false;
                    }
                }
            }
        }
        true
    }
}

#[cfg(unix)]
fn set_mode_0644(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o644));
}
#[cfg(not(unix))]
fn set_mode_0644(_p: &Path) {}

#[cfg(unix)]
pub fn set_executable(p: &Path, exec: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if exec { 0o755 } else { 0o644 };
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode));
}
#[cfg(not(unix))]
pub fn set_executable(_p: &Path, _exec: bool) {}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_proto::reapi::OutputFile;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zel-dc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn layout_matches_bazel_and_roundtrips() {
        let root = tmpdir("layout");
        let dc = DiskCache::open(&root, DigestFn::Sha256, true).unwrap();
        let d = DigestFn::Sha256.digest(b"hello world");
        dc.put_blob(&d, b"hello world").unwrap();
        let expect = root.join("cas").join(&d.hash[..2]).join(&d.hash);
        assert_eq!(std::fs::read(&expect).unwrap(), b"hello world");
        assert!(root.join("tmp").is_dir());
        assert_eq!(dc.get_blob(&d).unwrap().unwrap(), b"hello world");
        assert!(dc.get_blob(&DigestFn::Sha256.digest(b"nope")).unwrap().is_none());

        let ad = DigestFn::Sha256.digest(b"action");
        let ar = ActionResult {
            exit_code: 0,
            output_files: vec![OutputFile { path: "o".into(), digest: Some(d.clone()), ..Default::default() }],
            ..Default::default()
        };
        dc.put_action_result(&ad, &ar).unwrap();
        assert!(root.join("ac").join(&ad.hash[..2]).join(&ad.hash).is_file());
        assert_eq!(dc.get_action_result(&ad).unwrap(), ar);
        // Evict the blob: integrity check turns the AC entry into a miss.
        std::fs::remove_file(&expect).unwrap();
        assert!(dc.get_action_result(&ad).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn non_sha256_uses_function_subdirectory_and_empty_blob_is_free() {
        let root = tmpdir("md5");
        let dc = DiskCache::open(&root, DigestFn::Md5, true).unwrap();
        let d = DigestFn::Md5.digest(b"x");
        dc.put_blob(&d, b"x").unwrap();
        assert!(root.join("md5").join("cas").join(&d.hash[..2]).join(&d.hash).is_file());
        let empty = DigestFn::Md5.digest(b"");
        assert!(dc.contains_blob(&empty));
        assert_eq!(dc.get_blob(&empty).unwrap().unwrap(), Vec::<u8>::new());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn put_file_copy_and_refresh_mtime() {
        let root = tmpdir("file");
        let dc = DiskCache::open(&root, DigestFn::Sha256, true).unwrap();
        let src = root.join("src.txt");
        std::fs::write(&src, b"content").unwrap();
        let d = DigestFn::Sha256.digest(b"content");
        dc.put_file(&d, &src).unwrap();
        let p = dc.path(Store::Cas, &d.hash);
        let old = SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::open(&p).unwrap().set_modified(old).unwrap();
        assert!(dc.contains_blob(&d));
        let m = std::fs::metadata(&p).unwrap().modified().unwrap();
        assert!(m > old + std::time::Duration::from_secs(60), "mtime must be refreshed on access");
        let out = root.join("out/x.bin");
        assert!(dc.copy_blob_to(&d, &out, true).unwrap());
        assert_eq!(std::fs::read(&out).unwrap(), b"content");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&out).unwrap().permissions().mode() & 0o111, 0o111);
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
