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

//! Port of `com.google.devtools.build.lib.exec.SingleBuildFileCache`:
//! caches followed file stats/digests for source artifacts (at the source root) and non-derived
//! inputs (at the exec root). Non-regular files (directories, FIFOs, sockets, devices) are cached
//! as directory/non-file exceptions and never read or hashed.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};
use zel_analysis::Artifact;
use zel_vfs::{FileSystem, PathFragment};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedFileMetadata {
    pub digest: Vec<u8>,
    pub size: u64,
    pub mtime_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileCacheError {
    Directory(String),
    IllegalState(String),
    Io(String),
}

impl std::fmt::Display for FileCacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Directory(m) | Self::IllegalState(m) | Self::Io(m) => f.write_str(m),
        }
    }
}

impl From<FileCacheError> for io::Error {
    fn from(e: FileCacheError) -> Self {
        io::Error::new(io::ErrorKind::Other, e.to_string())
    }
}

enum FsRef<'a> {
    Borrowed(&'a dyn FileSystem),
    Owned(Arc<dyn FileSystem>),
}

impl FsRef<'_> {
    fn as_fs(&self) -> &dyn FileSystem {
        match self {
            Self::Borrowed(f) => *f,
            Self::Owned(f) => f.as_ref(),
        }
    }
}

/// Caches metadata per exec path within a single build (`SingleBuildFileCache.java`).
pub struct SingleBuildFileCache<'a> {
    exec_root: PathFragment,
    source_root: Option<PathFragment>,
    relative_output_path: PathFragment,
    fs: FsRef<'a>,
    digest_fn: Option<crate::DigestFunction>,
    computed_digest_cache_limit: usize,
    cache: Mutex<HashMap<PathFragment, Result<CachedFileMetadata, FileCacheError>>>,
}

impl std::fmt::Debug for SingleBuildFileCache<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SingleBuildFileCache")
            .field("exec_root", &self.exec_root)
            .field("source_root", &self.source_root)
            .field("relative_output_path", &self.relative_output_path)
            .finish()
    }
}

impl SingleBuildFileCache<'static> {
    pub fn new_owned(
        exec_root: PathFragment,
        source_root: Option<PathFragment>,
        relative_output_path: PathFragment,
        fs: Arc<dyn FileSystem>,
        digest_fn: crate::DigestFunction,
    ) -> Self {
        Self {
            exec_root,
            source_root,
            relative_output_path,
            fs: FsRef::Owned(fs),
            digest_fn: Some(digest_fn),
            computed_digest_cache_limit: 0,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_computed_digest_cache(mut self, max_entries: usize) -> Self {
        self.computed_digest_cache_limit = max_entries;
        self
    }
}

impl<'a> SingleBuildFileCache<'a> {
    pub fn new(
        exec_root: PathFragment,
        source_root: Option<PathFragment>,
        relative_output_path: PathFragment,
        fs: &'a dyn FileSystem,
    ) -> Self {
        Self {
            exec_root,
            source_root,
            relative_output_path,
            fs: FsRef::Borrowed(fs),
            digest_fn: None,
            computed_digest_cache_limit: 0,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn get_artifact_metadata(
        &self,
        artifact: &Artifact,
    ) -> Result<CachedFileMetadata, FileCacheError> {
        if !artifact.is_source {
            return Err(FileCacheError::IllegalState(format!(
                "SingleBuildFileCache does not support derived artifact '{}'",
                artifact.exec_path
            )));
        }
        let disk_path = match &self.source_root {
            Some(sr) if artifact.owner_label.repository().is_main() => {
                sr.get_relative(&artifact.root_relative_path)
            }
            _ => self.exec_root.get_relative(&artifact.exec_path),
        };
        self.lookup_or_stat(&artifact.exec_path, &disk_path)
    }

    pub fn get_input_metadata(
        &self,
        exec_path: &PathFragment,
    ) -> Result<CachedFileMetadata, FileCacheError> {
        if !self.relative_output_path.is_empty()
            && exec_path.starts_with(&self.relative_output_path)
        {
            return Err(FileCacheError::IllegalState(format!(
                "SingleBuildFileCache does not support action input '{}' in the output tree",
                exec_path
            )));
        }
        let disk_path = self.exec_root.get_relative(exec_path);
        self.lookup_or_stat(exec_path, &disk_path)
    }

    fn lookup_or_stat(
        &self,
        exec_path: &PathFragment,
        disk_path: &PathFragment,
    ) -> Result<CachedFileMetadata, FileCacheError> {
        {
            let guard = self.cache.lock().unwrap();
            if let Some(cached) = guard.get(exec_path) {
                return cached.clone();
            }
        }
        let fs = self.fs.as_fs();
        let computed = match fs.stat(disk_path, true) {
            Ok(st) => {
                if !st.is_file {
                    Err(FileCacheError::Directory(format!(
                        "Input is a directory: {}",
                        exec_path
                    )))
                } else {
                    let digest_res = match self.digest_fn {
                        Some(df) => crate::cached_file_digest_bytes(
                            df,
                            fs,
                            disk_path,
                            &st,
                            self.computed_digest_cache_limit,
                        ),
                        None => fs.get_digest(disk_path),
                    };
                    match digest_res {
                        Ok(digest) => Ok(CachedFileMetadata {
                            digest,
                            size: st.size,
                            mtime_ms: st.last_modified_time_ms,
                        }),
                        Err(e) => Err(FileCacheError::Io(e.to_string())),
                    }
                }
            }
            Err(e) => Err(FileCacheError::Io(e.to_string())),
        };
        let mut guard = self.cache.lock().unwrap();
        guard
            .entry(exec_path.clone())
            .or_insert_with(|| computed.clone());
        computed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_cmdline::Label;
    use zel_vfs::LocalFileSystem;

    #[test]
    fn caches_regular_files_at_source_root_and_rejects_directories_and_special_files() {
        let d = std::env::temp_dir().join(format!("zel-sbfc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let ws = d.join("ws");
        let er = d.join("ob/execroot/_main");
        std::fs::create_dir_all(ws.join("pkg/dir")).unwrap();
        std::fs::create_dir_all(&er).unwrap();
        // Source file exists in ws, NOT linked into execroot.
        std::fs::write(ws.join("pkg/src.txt"), b"hello").unwrap();
        std::fs::write(ws.join("pkg/dir/inner.txt"), b"inner").unwrap();

        let fs = LocalFileSystem::default();
        let cache = SingleBuildFileCache::new(
            PathFragment::create(&er.to_string_lossy()),
            Some(PathFragment::create(&ws.to_string_lossy())),
            PathFragment::create("bazel-out"),
            &fs,
        );

        let src_art = Artifact::source(
            PathFragment::create("pkg/src.txt"),
            Label::parse_canonical("//pkg:t").unwrap(),
        );
        let meta = cache.get_artifact_metadata(&src_art).unwrap();
        assert_eq!(meta.size, 5);

        let dir_art = Artifact::source(
            PathFragment::create("pkg/dir"),
            Label::parse_canonical("//pkg:t").unwrap(),
        );
        let err = cache.get_artifact_metadata(&dir_art).unwrap_err();
        assert!(matches!(err, FileCacheError::Directory(_)), "{err:?}");

        // Special file (FIFO) must not block or read bytes.
        #[cfg(unix)]
        {
            let fifo = er.join("my.fifo");
            let c_fifo = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
            extern "C" {
                fn mkfifo(pathname: *const std::ffi::c_char, mode: u32) -> i32;
            }
            assert_eq!(unsafe { mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
            let err = cache
                .get_input_metadata(&PathFragment::create("my.fifo"))
                .unwrap_err();
            assert!(matches!(err, FileCacheError::Directory(_)), "{err:?}");
        }

        // Derived artifact throws IllegalState.
        let derived = Artifact::derived(
            "k8-fastbuild",
            PathFragment::create("pkg/out"),
            Label::parse_canonical("//pkg:t").unwrap(),
        );
        assert!(matches!(
            cache.get_artifact_metadata(&derived),
            Err(FileCacheError::IllegalState(_))
        ));
    }

    #[test]
    fn computed_digest_cache_reuses_unchanged_files_and_invalidates_on_edit() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingFs {
            inner: LocalFileSystem,
            reads: Arc<AtomicUsize>,
        }
        impl FileSystem for CountingFs {
            fn stat(&self, path: &PathFragment, follow: bool) -> io::Result<zel_vfs::FileStatus> {
                self.inner.stat(path, follow)
            }
            fn read_file(&self, path: &PathFragment) -> io::Result<Vec<u8>> {
                self.reads.fetch_add(1, Ordering::SeqCst);
                self.inner.read_file(path)
            }
            fn write_file(&self, path: &PathFragment, content: &[u8]) -> io::Result<()> {
                self.inner.write_file(path, content)
            }
            fn create_directory(&self, path: &PathFragment) -> io::Result<bool> {
                self.inner.create_directory(path)
            }
            fn delete(&self, path: &PathFragment) -> io::Result<bool> {
                self.inner.delete(path)
            }
            fn readdir(&self, path: &PathFragment) -> io::Result<Vec<zel_vfs::Dirent>> {
                self.inner.readdir(path)
            }
            fn read_symbolic_link(&self, path: &PathFragment) -> io::Result<PathFragment> {
                self.inner.read_symbolic_link(path)
            }
            fn create_symbolic_link(
                &self,
                link: &PathFragment,
                target: &PathFragment,
            ) -> io::Result<()> {
                self.inner.create_symbolic_link(link, target)
            }
            fn rename(&self, src: &PathFragment, dst: &PathFragment) -> io::Result<()> {
                self.inner.rename(src, dst)
            }
            fn digest_hash_function(&self) -> zel_vfs::DigestHashFunction {
                self.inner.digest_hash_function()
            }
            fn create_directory_and_parents(&self, path: &PathFragment) -> io::Result<()> {
                self.inner.create_directory_and_parents(path)
            }
            fn set_last_modified_time(&self, path: &PathFragment, mtime_ms: i64) -> io::Result<()> {
                self.inner.set_last_modified_time(path, mtime_ms)
            }
        }

        let d = std::env::temp_dir().join(format!("zel-sbfc-digest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let ws = d.join("ws");
        let er = d.join("ob/execroot/_main");
        std::fs::create_dir_all(ws.join("pkg")).unwrap();
        std::fs::create_dir_all(&er).unwrap();
        let file = ws.join("pkg/stable.txt");
        std::fs::write(&file, b"version-one").unwrap();

        // Set mtime > 2s in the past so the file is outside the racy-mtime window.
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(10);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(past)
            .unwrap();

        let reads = Arc::new(AtomicUsize::new(0));
        let fs: Arc<dyn FileSystem> = Arc::new(CountingFs {
            inner: LocalFileSystem::default(),
            reads: Arc::clone(&reads),
        });
        let art = Artifact::source(
            PathFragment::create("pkg/stable.txt"),
            Label::parse_canonical("//pkg:t").unwrap(),
        );

        let mk_cache = || {
            SingleBuildFileCache::new_owned(
                PathFragment::create(&er.to_string_lossy()),
                Some(PathFragment::create(&ws.to_string_lossy())),
                PathFragment::create("bazel-out"),
                Arc::clone(&fs),
                crate::DigestFunction::Sha256,
            )
            .with_computed_digest_cache(50_000)
        };

        let m1 = mk_cache().get_artifact_metadata(&art).unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 1);

        // Fresh SingleBuildFileCache for the next build must reuse the cached digest without reading.
        let m2 = mk_cache().get_artifact_metadata(&art).unwrap();
        assert_eq!(m2.digest, m1.digest);
        assert_eq!(reads.load(Ordering::SeqCst), 1);

        // Editing the file (even within the racy window) must bypass/invalidate the cached digest.
        std::fs::write(&file, b"version-two-edited").unwrap();
        let m3 = mk_cache().get_artifact_metadata(&art).unwrap();
        assert_ne!(m3.digest, m1.digest);
        assert_eq!(reads.load(Ordering::SeqCst), 2);

        let _ = std::fs::remove_dir_all(&d);
    }
}
