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

//! A `FileSystem` wrapper that records every path observed (stat, read, readlink, readdir) on
//! the *current thread* while recording is active. SkyFunctions that run opaque loaders (BUILD
//! evaluation, .bzl loading, globbing) wrap their file system in this to discover the exact
//! set of `FILE_STATE` / `DIRECTORY_LISTING_STATE` dependencies of the computation.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io;
use std::sync::Arc;
use zel_vfs::{DigestHashFunction, Dirent, FileStatus, FileSystem, PathFragment, RootedPath};

use crate::SkyKey;

/// Paths observed while recording.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// Paths whose file state was observed (stat/read/readlink), including absent ones.
    pub files: BTreeSet<PathFragment>,
    /// Directories whose listing was observed.
    pub dirs: BTreeSet<PathFragment>,
}

thread_local! {
    static RECORDER: RefCell<Option<Recorded>> = const { RefCell::new(None) };
}

/// Starts recording on this thread (nested calls are not supported; the previous recording is
/// discarded).
pub fn start_recording() {
    RECORDER.with(|r| *r.borrow_mut() = Some(Recorded::default()));
}

/// Stops recording on this thread and returns what was observed.
pub fn finish_recording() -> Recorded {
    RECORDER.with(|r| r.borrow_mut().take().unwrap_or_default())
}

fn record_file(p: &PathFragment) {
    RECORDER.with(|r| {
        if let Some(rec) = r.borrow_mut().as_mut() {
            rec.files.insert(p.clone());
        }
    });
}

fn record_dir(p: &PathFragment) {
    RECORDER.with(|r| {
        if let Some(rec) = r.borrow_mut().as_mut() {
            rec.dirs.insert(p.clone());
        }
    });
}

/// Converts an absolute path into a `RootedPath` rooted at its drive/filesystem root (`/` or `C:/`).
pub fn rooted(p: &PathFragment) -> Option<RootedPath> {
    if !p.is_absolute() {
        return None;
    }
    let dlen = p.drive_str_length();
    let (root, rel) = p.path_string().split_at(dlen);
    RootedPath::to_rooted_path(PathFragment::create(root), PathFragment::create(rel)).ok()
}

/// Skyframe dependency keys for everything in `rec`.
pub fn dependency_keys(rec: &Recorded) -> Vec<SkyKey> {
    let mut keys = Vec::new();
    for f in &rec.files {
        if let Some(r) = rooted(f) {
            keys.push(SkyKey::FileState(r));
        }
    }
    for d in &rec.dirs {
        if let Some(r) = rooted(d) {
            keys.push(SkyKey::DirectoryListingState(r));
        }
    }
    keys
}

pub struct RecordingFileSystem {
    inner: Arc<dyn FileSystem>,
}

impl RecordingFileSystem {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(inner: Arc<dyn FileSystem>) -> Arc<dyn FileSystem> {
        Arc::new(Self { inner })
    }

    fn record_with_symlinks(&self, path: &PathFragment, follow: bool) {
        record_file(path);
        if !follow {
            return;
        }
        let mut cur = path.clone();
        for _ in 0..32 {
            let Ok(st) = self.inner.stat(&cur, false) else {
                break;
            };
            if !st.is_symlink {
                break;
            }
            let Ok(target) = self.inner.read_symbolic_link(&cur) else {
                break;
            };
            let next = if target.is_absolute() {
                target
            } else if let Some(parent) = cur.parent_directory() {
                parent.get_relative(&target)
            } else {
                break;
            };
            record_file(&next);
            cur = next;
        }
    }
}

impl FileSystem for RecordingFileSystem {
    fn digest_hash_function(&self) -> DigestHashFunction {
        self.inner.digest_hash_function()
    }
    fn stat(&self, path: &PathFragment, follow_symlinks: bool) -> io::Result<FileStatus> {
        self.record_with_symlinks(path, follow_symlinks);
        self.inner.stat(path, follow_symlinks)
    }
    fn read_file(&self, path: &PathFragment) -> io::Result<Vec<u8>> {
        self.record_with_symlinks(path, true);
        self.inner.read_file(path)
    }
    fn write_file(&self, path: &PathFragment, content: &[u8]) -> io::Result<()> {
        self.inner.write_file(path, content)
    }
    fn create_directory(&self, path: &PathFragment) -> io::Result<bool> {
        self.inner.create_directory(path)
    }
    fn create_directory_and_parents(&self, path: &PathFragment) -> io::Result<()> {
        self.inner.create_directory_and_parents(path)
    }
    fn readdir(&self, path: &PathFragment) -> io::Result<Vec<Dirent>> {
        record_dir(path);
        self.inner.readdir(path)
    }
    fn delete(&self, path: &PathFragment) -> io::Result<bool> {
        self.inner.delete(path)
    }
    fn rename(&self, source: &PathFragment, target: &PathFragment) -> io::Result<()> {
        self.inner.rename(source, target)
    }
    fn create_symbolic_link(
        &self,
        link_path: &PathFragment,
        target: &PathFragment,
    ) -> io::Result<()> {
        self.inner.create_symbolic_link(link_path, target)
    }
    fn read_symbolic_link(&self, link_path: &PathFragment) -> io::Result<PathFragment> {
        record_file(link_path);
        self.inner.read_symbolic_link(link_path)
    }
    fn create_hard_link(
        &self,
        link_path: &PathFragment,
        original_path: &PathFragment,
    ) -> io::Result<()> {
        self.inner.create_hard_link(link_path, original_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_vfs::InMemoryFileSystem;

    #[test]
    fn records_reads_stats_and_listings_per_thread() {
        let fs = Arc::new(InMemoryFileSystem::default());
        fs.create_directory_and_parents(&PathFragment::create("/w/a"))
            .unwrap();
        fs.write_file(&PathFragment::create("/w/a/f"), b"x")
            .unwrap();
        let rfs = RecordingFileSystem::new(fs);
        // Not recording: nothing is captured.
        let _ = rfs.read_file(&PathFragment::create("/w/a/f"));
        start_recording();
        let _ = rfs.read_file(&PathFragment::create("/w/a/f"));
        let _ = rfs.stat(&PathFragment::create("/w/missing"), true);
        let _ = rfs.readdir(&PathFragment::create("/w/a"));
        // Another thread's activity is not recorded here.
        std::thread::scope(|s| {
            s.spawn(|| {
                let _ = rfs.read_file(&PathFragment::create("/w/a/f2"));
            });
        });
        let rec = finish_recording();
        assert_eq!(rec.files.len(), 2);
        assert!(rec.files.contains(&PathFragment::create("/w/missing")));
        assert_eq!(rec.dirs.len(), 1);
        assert_eq!(dependency_keys(&rec).len(), 3);
        assert!(finish_recording().files.is_empty());
    }

    #[test]
    fn records_symlink_target_as_file_state() {
        let fs = Arc::new(InMemoryFileSystem::default());
        fs.create_directory_and_parents(&PathFragment::create("/w/ext"))
            .unwrap();
        fs.create_directory_and_parents(&PathFragment::create("/w/real"))
            .unwrap();
        fs.write_file(
            &PathFragment::create("/w/real/BUILD.bazel"),
            b"filegroup(name='a')",
        )
        .unwrap();
        fs.create_symbolic_link(
            &PathFragment::create("/w/ext/BUILD.bazel"),
            &PathFragment::create("/w/real/BUILD.bazel"),
        )
        .unwrap();
        let rfs = RecordingFileSystem::new(fs);
        start_recording();
        let _ = rfs
            .read_file(&PathFragment::create("/w/ext/BUILD.bazel"))
            .unwrap();
        let rec = finish_recording();
        assert!(
            rec.files
                .contains(&PathFragment::create("/w/ext/BUILD.bazel")),
            "{:?}",
            rec.files
        );
        assert!(
            rec.files
                .contains(&PathFragment::create("/w/real/BUILD.bazel")),
            "{:?}",
            rec.files
        );
    }
}
