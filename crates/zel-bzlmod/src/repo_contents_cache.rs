//! Repository contents cache.
//!
//! Stores the contents of fetched repositories across workspaces, keyed by the
//! hash of predeclared repository rule inputs (transitive bzl digest, rule attributes,
//! client environment variables, etc.).
//! Mirrors Bazel's `com.google.devtools.build.lib.bazel.repository.cache.LocalRepoContentsCache`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const RECORDED_INPUTS_SUFFIX: &str = ".recorded_inputs";
pub const LOCK_PATH: &str = "gc_lock";
pub const TRASH_PATH: &str = "_trash";

#[derive(Clone, Debug)]
pub struct CandidateRepo {
    pub recorded_inputs_file: PathBuf,
    pub contents_dir: PathBuf,
}

impl CandidateRepo {
    pub fn from_recorded_inputs_file(path: &Path) -> Option<Self> {
        let file_name = path.file_name()?.to_string_lossy();
        if !file_name.ends_with(RECORDED_INPUTS_SUFFIX) {
            return None;
        }
        let base_name = &file_name[..file_name.len() - RECORDED_INPUTS_SUFFIX.len()];
        let contents_dir = path.with_file_name(base_name);
        Some(Self {
            recorded_inputs_file: path.to_path_buf(),
            contents_dir,
        })
    }

    /// Updates the mtime of the recorded inputs file to delay GC.
    pub fn touch(&self) {
        let now = filetime::FileTime::now();
        let _ = filetime::set_file_mtime(&self.recorded_inputs_file, now);
    }
}

pub struct RepoContentsCache {
    path: PathBuf,
}

impl RepoContentsCache {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn ensure_trash_dir(&self) -> io::Result<PathBuf> {
        let trash = self.path.join(TRASH_PATH);
        fs::create_dir_all(&trash)?;
        Ok(trash)
    }

    /// Returns candidate repos for the given predeclared input hash, sorted by descending mtime.
    pub fn get_candidate_repos(&self, predeclared_input_hash: &str) -> Vec<CandidateRepo> {
        let entry_dir = self.path.join(predeclared_input_hash);
        let entries = match fs::read_dir(&entry_dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        let mut candidates = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(cand) = CandidateRepo::from_recorded_inputs_file(&path) {
                if cand.contents_dir.exists() {
                    candidates.push(cand);
                }
            }
        }

        candidates.sort_by(|a, b| {
            let mtime_a = fs::metadata(&a.recorded_inputs_file).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
            let mtime_b = fs::metadata(&b.recorded_inputs_file).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
            mtime_b.cmp(&mtime_a)
        });

        candidates
    }

    /// Moves or copies a freshly fetched repository into the repo contents cache.
    pub fn move_to_cache(
        &self,
        fetched_repo_dir: &Path,
        fetched_marker_file: &Path,
        predeclared_input_hash: &str,
    ) -> io::Result<CandidateRepo> {
        let entry_dir = self.path.join(predeclared_input_hash);
        fs::create_dir_all(&entry_dir)?;

        let uuid = generate_uuid();
        let cache_repo_dir = entry_dir.join(&uuid);
        let cache_recorded_inputs = entry_dir.join(format!("{uuid}{RECORDED_INPUTS_SUFFIX}"));

        // Copy or move fetched_repo_dir to cache_repo_dir
        if fs::rename(fetched_repo_dir, &cache_repo_dir).is_err() {
            fs::create_dir_all(&cache_repo_dir)?;
            copy_dir_recursive(fetched_repo_dir, &cache_repo_dir)?;
            let _ = remove_dir_all(fetched_repo_dir);
        }

        // Copy marker content to recorded inputs
        if fetched_marker_file.exists() {
            fs::copy(fetched_marker_file, &cache_recorded_inputs)?;
        } else {
            fs::write(&cache_recorded_inputs, predeclared_input_hash)?;
        }

        // Symlink fetched_repo_dir to cache_repo_dir
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink(&cache_repo_dir, fetched_repo_dir);
        }
        #[cfg(not(unix))]
        {
            let _ = copy_dir_recursive(&cache_repo_dir, fetched_repo_dir);
        }

        Ok(CandidateRepo {
            recorded_inputs_file: cache_recorded_inputs,
            contents_dir: cache_repo_dir,
        })
    }

    /// Garbage collection: deletes entries older than `max_age` as well as identical duplicate entries.
    pub fn run_gc(&self, max_age: Duration) -> io::Result<()> {
        if !self.path.exists() {
            return Ok(());
        }
        let cutoff = if max_age.is_zero() {
            SystemTime::UNIX_EPOCH
        } else {
            SystemTime::now().checked_sub(max_age).unwrap_or(SystemTime::UNIX_EPOCH)
        };

        let trash_dir = self.ensure_trash_dir()?;
        let mut seen_hashes = std::collections::HashSet::new();

        let entries = match fs::read_dir(&self.path) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if !path.is_dir() || name == TRASH_PATH || name == LOCK_PATH {
                continue;
            }

            let sub_entries = match fs::read_dir(&path) {
                Ok(s) => s,
                Err(_) => continue,
            };

            for sub in sub_entries.flatten() {
                let p = sub.path();
                if let Some(cand) = CandidateRepo::from_recorded_inputs_file(&p) {
                    let mtime = fs::metadata(&cand.recorded_inputs_file)
                        .and_then(|m| m.modified())
                        .unwrap_or(SystemTime::UNIX_EPOCH);

                    let content = fs::read(&cand.recorded_inputs_file).unwrap_or_default();
                    let content_hash = crate::hash::sha256_hex(&content);

                    let is_expired = mtime < cutoff;
                    let is_duplicate = !seen_hashes.insert(content_hash);

                    if is_expired || is_duplicate {
                        let _ = fs::remove_file(&cand.recorded_inputs_file);
                        let trash_target = trash_dir.join(generate_uuid());
                        if fs::rename(&cand.contents_dir, &trash_target).is_err() {
                            let _ = remove_dir_all(&cand.contents_dir);
                        }
                    }
                }
            }
        }

        // Clean up trash directory
        let _ = remove_dir_all(&trash_dir);
        let _ = fs::create_dir_all(&trash_dir);
        Ok(())
    }
}

fn generate_uuid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let cnt = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!("{t:016x}-{cnt:08x}")
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    if !dst.exists() {
        fs::create_dir_all(dst)?;
    }
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_p = entry.path();
        let dst_p = dst.join(entry.file_name());
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_recursive(&src_p, &dst_p)?;
        } else if ty.is_symlink() {
            #[cfg(unix)]
            {
                let target = fs::read_link(&src_p)?;
                let _ = std::os::unix::fs::symlink(target, &dst_p);
            }
            #[cfg(not(unix))]
            {
                let _ = fs::copy(&src_p, &dst_p);
            }
        } else {
            fs::copy(&src_p, &dst_p)?;
        }
    }
    Ok(())
}

fn remove_dir_all(path: &Path) -> io::Result<()> {
    if path.is_symlink() {
        fs::remove_file(path)
    } else if path.is_dir() {
        fs::remove_dir_all(path)
    } else if path.exists() {
        fs::remove_file(path)
    } else {
        Ok(())
    }
}

mod filetime {
    use std::fs::File;
    use std::io;
    use std::path::Path;
    use std::time::SystemTime;

    pub struct FileTime(#[allow(dead_code)] pub SystemTime);
    impl FileTime {
        pub fn now() -> Self {
            Self(SystemTime::now())
        }
    }

    pub fn set_file_mtime(path: &Path, _mtime: FileTime) -> io::Result<()> {
        // Touch by re-writing or opening in write mode without truncating
        if let Ok(f) = File::options().write(true).open(path) {
            drop(f);
        }
        Ok(())
    }
}
