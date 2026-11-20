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

//! `--disk_cache` (`remote/disk/DiskCacheClient.java`) and `CachePolicy`:
//! `<root>[/<digest fn>]/{ac,cas}/<first two hex chars>/<hash>` plus `<root>/tmp`.
//! Only `<root>` and `<root>/tmp` are created on startup; `ac/` and `cas/` shard directories are
//! created lazily when entries are written via fsynced temp files and atomic rename.
//! The `<digest fn>` directory is omitted for old-style digest functions (`enum <= 7`: SHA-256,
//! SHA-1, MD5, SHA-384, SHA-512) and included for newer functions (`blake3`).

use crate::digest::DigestFn;
use crate::merkle::{flatten_tree, BlobSource};
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use zel_proto::reapi::{ActionResult, Digest, Message, Tree};
use zel_vfs::PathFragment;

/// Mirrors Bazel's `CombinedCache.CachePolicy` (`NO_CACHE`, `REMOTE_CACHE_ONLY`,
/// `DISK_CACHE_ONLY`, `ANY_CACHE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CachePolicy {
    pub allow_remote_cache: bool,
    pub allow_disk_cache: bool,
}

impl CachePolicy {
    pub const NO_CACHE: Self = Self {
        allow_remote_cache: false,
        allow_disk_cache: false,
    };
    pub const REMOTE_CACHE_ONLY: Self = Self {
        allow_remote_cache: true,
        allow_disk_cache: false,
    };
    pub const DISK_CACHE_ONLY: Self = Self {
        allow_remote_cache: false,
        allow_disk_cache: true,
    };
    pub const ANY_CACHE: Self = Self {
        allow_remote_cache: true,
        allow_disk_cache: true,
    };

    pub const fn create(allow_remote_cache: bool, allow_disk_cache: bool) -> Self {
        Self {
            allow_remote_cache,
            allow_disk_cache,
        }
    }

    pub const fn add_remote_cache(self) -> Self {
        Self {
            allow_remote_cache: true,
            allow_disk_cache: self.allow_disk_cache,
        }
    }

    pub const fn allow_any_cache(self) -> bool {
        self.allow_remote_cache || self.allow_disk_cache
    }
}

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
    verify_downloads: bool,
    capture_corrupted_dir: Option<PathBuf>,
}

impl DiskCache {
    /// SHA-256 layout, with action-result integrity checking.
    pub fn new(root: PathFragment) -> Self {
        Self::open(Path::new(root.path_string()), DigestFn::Sha256, true).unwrap_or_else(|_| {
            Self::unopened(Path::new(root.path_string()), DigestFn::Sha256, true)
        })
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
            verify_downloads: true,
            capture_corrupted_dir: None,
        }
    }

    pub fn open(root: &Path, f: DigestFn, check_ac_integrity: bool) -> io::Result<Self> {
        let c = Self::unopened(root, f, check_ac_integrity);
        // Bazel creates only `<root>` and `<root>/tmp` on startup; `ac/` and `cas/` are created lazily.
        std::fs::create_dir_all(&c.root)?;
        std::fs::create_dir_all(&c.tmp_root)?;
        Ok(c)
    }

    pub fn with_verification(
        mut self,
        verify_downloads: bool,
        capture_corrupted_dir: Option<PathBuf>,
    ) -> Self {
        self.verify_downloads = verify_downloads;
        self.capture_corrupted_dir = capture_corrupted_dir;
        self
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

    pub fn temp_path(&self) -> PathBuf {
        self.tmp_root.join(zel_util::uuid_v4())
    }

    /// Updates the entry's mtime; returns whether it exists.
    pub fn refresh(&self, path: &Path) -> io::Result<bool> {
        #[cfg(windows)]
        {
            match std::fs::symlink_metadata(path) {
                Ok(_) => {
                    if let Ok(f) = open_for_mtime(path) {
                        let _ = f.set_modified(SystemTime::now());
                    }
                    Ok(true)
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e),
            }
        }
        #[cfg(not(windows))]
        match open_for_mtime(path) {
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
        self.refresh(&self.path(Store::Cas, &d.hash))
            .unwrap_or(false)
    }

    fn save(
        &self,
        store: Store,
        hash: &str,
        write_fn: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<()> {
        let path = self.path(store, hash);
        if store == Store::Cas && self.refresh(&path)? {
            return Ok(());
        }
        let tmp = self.temp_path();
        if let Some(p) = tmp.parent() {
            std::fs::create_dir_all(p)?;
        }
        let r = (|| {
            write_fn(&tmp)?;
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p)?;
            }
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
            let mut f = File::create(tmp)?;
            f.write_all(data)?;
            f.sync_all()?;
            drop(f);
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
            if let Ok(f) = File::open(tmp) {
                let _ = f.sync_all();
            }
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

    fn maybe_capture_corrupted(&self, hash: &str, data: &[u8]) {
        if let Some(dir) = &self.capture_corrupted_dir {
            if std::fs::create_dir_all(dir).is_ok() {
                let dest = dir.join(format!("{hash}-{}", zel_util::uuid_v4()));
                let _ = std::fs::write(dest, data);
            }
        }
    }

    /// Reads a blob (verifying size and optionally digest) and refreshes its mtime.
    /// Corrupted disk entries are evicted and reported as an error so callers know disk was corrupt
    /// (or can distinguish missing vs corrupt).
    pub fn get_blob_strict(&self, d: &Digest) -> io::Result<Option<Vec<u8>>> {
        if d.size_bytes == 0 {
            return Ok(Some(Vec::new()));
        }
        let p = self.path(Store::Cas, &d.hash);
        match std::fs::read(&p) {
            Ok(b) => {
                if b.len() as i64 != d.size_bytes {
                    self.maybe_capture_corrupted(&d.hash, &b);
                    let _ = std::fs::remove_file(&p);
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "Disk cache entry {}/{} size mismatch (got {})",
                            d.hash,
                            d.size_bytes,
                            b.len()
                        ),
                    ));
                }
                if self.verify_downloads {
                    let actual = self.digest_fn.hash(&b);
                    if actual != d.hash {
                        self.maybe_capture_corrupted(&d.hash, &b);
                        let _ = std::fs::remove_file(&p);
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "Disk cache entry {}/{} digest mismatch (got {actual})",
                                d.hash, d.size_bytes
                            ),
                        ));
                    }
                }
                let _ = self.refresh(&p);
                Ok(Some(b))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Reads a blob (treating size/digest mismatch as a miss `Ok(None)`).
    pub fn get_blob(&self, d: &Digest) -> io::Result<Option<Vec<u8>>> {
        match self.get_blob_strict(d) {
            Ok(v) => Ok(v),
            Err(e) if e.kind() == io::ErrorKind::InvalidData => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Copies a blob to `dest` via an atomic temp file, optionally marking it executable.
    pub fn copy_blob_to(&self, d: &Digest, dest: &Path, executable: bool) -> io::Result<bool> {
        let Some(data) = self.get_blob(d)? else {
            return Ok(false);
        };
        write_atomic_output(dest, &data, executable)?;
        Ok(true)
    }

    pub fn put_action_result(&self, action: &Digest, r: &ActionResult) -> io::Result<()> {
        let bytes = r.encode_to_vec();
        self.save(Store::Ac, &action.hash, |tmp| {
            let mut f = File::create(tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            drop(f);
            set_mode_0644(tmp);
            Ok(())
        })
    }

    /// Strict AC lookup that surfaces disk I/O / protobuf corruption errors.
    pub fn try_get_action_result(&self, action: &Digest) -> io::Result<Option<ActionResult>> {
        let p = self.path(Store::Ac, &action.hash);
        let bytes = match std::fs::read(&p) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let r = ActionResult::decode(&bytes).map_err(|e| {
            let _ = std::fs::remove_file(&p);
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Corrupt ActionResult in disk cache {}: {e}", action.hash),
            )
        })?;
        if self.check_ac_integrity && !self.action_result_complete(&r) {
            return Ok(None);
        }
        let _ = self.refresh(&p);
        Ok(Some(r))
    }

    /// Looks up an action result; with integrity checking the result is only returned if all
    /// blobs it references are present in the CAS (a partially evicted entry is a miss).
    pub fn get_action_result(&self, action: &Digest) -> Option<ActionResult> {
        self.try_get_action_result(action).ok().flatten()
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
            let Some(td) = &od.tree_digest else {
                return false;
            };
            let Ok(Some(bytes)) = self.get_blob(td) else {
                return false;
            };
            let Ok(tree) = Tree::decode(&bytes) else {
                return false;
            };
            let Ok(entries) = flatten_tree(&tree, self.digest_fn) else {
                return false;
            };
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

/// Writes `data` to a sibling temp file, applies `0755` / `0644` permissions (Bazel `0555` /
/// `0644`-compatible executable bit), and atomically renames over `dest`.
pub fn write_atomic_output(dest: &Path, data: &[u8], executable: bool) -> io::Result<()> {
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    let file_name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("out");
    let tmp = dest.with_file_name(format!(".{file_name}.zel-tmp-{}", zel_util::uuid_v4()));
    let res = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        drop(f);
        set_executable(&tmp, executable);
        let _ = crate::merkle::remove_existing_entry(dest);
        std::fs::rename(&tmp, dest)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

#[cfg(windows)]
fn open_for_mtime(path: &Path) -> io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    std::fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .open(path)
}

#[cfg(not(windows))]
fn open_for_mtime(path: &Path) -> io::Result<std::fs::File> {
    std::fs::File::open(path)
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
    fn layout_matches_bazel_lazy_dirs_and_roundtrips() {
        let root = tmpdir("layout");
        let dc = DiskCache::open(&root, DigestFn::Sha256, true).unwrap();
        // Only root and tmp exist before any writes.
        assert!(root.join("tmp").is_dir());
        assert!(!root.join("ac").exists());
        assert!(!root.join("cas").exists());

        let d = DigestFn::Sha256.digest(b"hello world");
        dc.put_blob(&d, b"hello world").unwrap();
        let expect = root.join("cas").join(&d.hash[..2]).join(&d.hash);
        assert_eq!(std::fs::read(&expect).unwrap(), b"hello world");
        assert_eq!(dc.get_blob(&d).unwrap().unwrap(), b"hello world");
        assert!(dc
            .get_blob(&DigestFn::Sha256.digest(b"nope"))
            .unwrap()
            .is_none());

        let ad = DigestFn::Sha256.digest(b"action");
        let ar = ActionResult {
            exit_code: 0,
            output_files: vec![OutputFile {
                path: "o".into(),
                digest: Some(d.clone()),
                ..Default::default()
            }],
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
    fn old_style_md5_omits_subdir_and_blake3_uses_function_subdirectory() {
        let root_md5 = tmpdir("md5");
        let dc_md5 = DiskCache::open(&root_md5, DigestFn::Md5, true).unwrap();
        let d_md5 = DigestFn::Md5.digest(b"x");
        dc_md5.put_blob(&d_md5, b"x").unwrap();
        // MD5 is an old-style hash function (<= 7) in Bazel: stored directly under <root>/cas/!
        assert!(root_md5
            .join("cas")
            .join(&d_md5.hash[..2])
            .join(&d_md5.hash)
            .is_file());
        let empty = DigestFn::Md5.digest(b"");
        assert!(dc_md5.contains_blob(&empty));
        assert_eq!(dc_md5.get_blob(&empty).unwrap().unwrap(), Vec::<u8>::new());
        let _ = std::fs::remove_dir_all(&root_md5);

        let root_b3 = tmpdir("blake3");
        let dc_b3 = DiskCache::open(&root_b3, DigestFn::Blake3, true).unwrap();
        let d_b3 = DigestFn::Blake3.digest(b"x");
        dc_b3.put_blob(&d_b3, b"x").unwrap();
        // BLAKE3 (> 7) uses <root>/blake3/cas/!
        assert!(root_b3
            .join("blake3")
            .join("cas")
            .join(&d_b3.hash[..2])
            .join(&d_b3.hash)
            .is_file());
        let _ = std::fs::remove_dir_all(&root_b3);
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
        open_for_mtime(&p).unwrap().set_modified(old).unwrap();
        assert!(dc.contains_blob(&d));
        let m = std::fs::metadata(&p).unwrap().modified().unwrap();
        assert!(
            m > old + std::time::Duration::from_secs(60),
            "mtime must be refreshed on access"
        );
        let out = root.join("out/x.bin");
        assert!(dc.copy_blob_to(&d, &out, true).unwrap());
        assert_eq!(std::fs::read(&out).unwrap(), b"content");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&out).unwrap().permissions().mode() & 0o111,
                0o111
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
