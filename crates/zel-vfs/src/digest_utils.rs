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

//! Digest caching, fast-digest fallback, and order-independent digest combination matching
//! `com.google.devtools.build.lib.vfs.DigestUtils`.

use crate::{FileStatus, FileSystem, PathFragment};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::{OnceLock, RwLock};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DigestCacheKey {
    path: PathFragment,
    node_id: u64,
    ctime_ms: u64,
    mtime_ms: u64,
    size: u64,
}

impl DigestCacheKey {
    fn from_status(path: &PathFragment, status: &FileStatus) -> Self {
        Self {
            path: path.clone(),
            node_id: status.node_id,
            ctime_ms: status.last_change_time_ms,
            mtime_ms: status.last_modified_time_ms,
            size: status.size,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DigestCacheStats {
    pub hit_count: u64,
    pub miss_count: u64,
    pub eviction_count: u64,
    pub size: usize,
}

#[derive(Debug)]
struct DigestCacheInner {
    max_entries: usize,
    entries: HashMap<DigestCacheKey, Vec<u8>>,
    order: VecDeque<DigestCacheKey>,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl DigestCacheInner {
    fn new(max_entries: usize) -> Self {
        Self {
            max_entries,
            entries: HashMap::new(),
            order: VecDeque::new(),
            hits: 0,
            misses: 0,
            evictions: 0,
        }
    }

    fn get(&mut self, key: &DigestCacheKey) -> Option<Vec<u8>> {
        if let Some(val) = self.entries.get(key) {
            self.hits += 1;
            Some(val.clone())
        } else {
            self.misses += 1;
            None
        }
    }

    fn put(&mut self, key: DigestCacheKey, digest: Vec<u8>) {
        if self.max_entries == 0 {
            return;
        }
        if let std::collections::hash_map::Entry::Occupied(mut occupied) =
            self.entries.entry(key.clone())
        {
            occupied.insert(digest);
            return;
        }
        while self.entries.len() >= self.max_entries {
            if let Some(oldest) = self.order.pop_front() {
                if self.entries.remove(&oldest).is_some() {
                    self.evictions += 1;
                }
            } else {
                break;
            }
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, digest);
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    fn stats(&self) -> DigestCacheStats {
        DigestCacheStats {
            hit_count: self.hits,
            miss_count: self.misses,
            eviction_count: self.evictions,
            size: self.entries.len(),
        }
    }
}

static DIGEST_CACHE: OnceLock<RwLock<Option<DigestCacheInner>>> = OnceLock::new();

fn cache_lock() -> &'static RwLock<Option<DigestCacheInner>> {
    DIGEST_CACHE.get_or_init(|| RwLock::new(None))
}

/// Configures the process-wide digest cache (`DigestUtils.configureCache(long)`).
/// Passing `0` disables caching (`null` cache); any positive size rebuilds an empty cache.
pub fn configure_digest_cache(maximum_size: usize) {
    let mut guard = cache_lock().write().unwrap();
    if maximum_size == 0 {
        *guard = None;
    } else {
        *guard = Some(DigestCacheInner::new(maximum_size));
    }
}

/// Clears entries when a cache is configured (`DigestUtils.clearCache()`).
pub fn clear_digest_cache() {
    let mut guard = cache_lock().write().unwrap();
    if let Some(cache) = guard.as_mut() {
        cache.clear();
    }
}

/// Returns cache statistics, or `Err` when caching is disabled (`DigestUtils.getCacheStats()`).
pub fn get_digest_cache_stats() -> Result<DigestCacheStats, &'static str> {
    let guard = cache_lock().read().unwrap();
    match guard.as_ref() {
        Some(cache) => Ok(cache.stats()),
        None => Err("Digest cache is not enabled"),
    }
}

/// Computes a file digest using `get_fast_digest` first, then the stat-keyed cache (stat'ing if
/// `status` is `None`), and finally `fs.get_digest(path)` (`DigestUtils.getDigestWithManualFallback`).
pub fn get_digest_with_manual_fallback(
    fs: &dyn FileSystem,
    path: &PathFragment,
    status: Option<&FileStatus>,
) -> io::Result<Vec<u8>> {
    if let Ok(Some(fast)) = fs.get_fast_digest(path) {
        return Ok(fast);
    }
    manually_compute_digest(fs, path, status)
}

/// Computes a manual digest using the stat-keyed cache when enabled (`DigestUtils.manuallyComputeDigest`).
pub fn manually_compute_digest(
    fs: &dyn FileSystem,
    path: &PathFragment,
    status: Option<&FileStatus>,
) -> io::Result<Vec<u8>> {
    let owned_status;
    let st = match status {
        Some(s) => s,
        None => {
            owned_status = fs.stat(path, true)?;
            &owned_status
        }
    };
    let key = DigestCacheKey::from_status(path, st);
    {
        let mut guard = cache_lock().write().unwrap();
        if let Some(cache) = guard.as_mut() {
            if let Some(hit) = cache.get(&key) {
                return Ok(hit);
            }
        }
    }
    let digest = fs.get_digest(path)?;
    {
        let mut guard = cache_lock().write().unwrap();
        if let Some(cache) = guard.as_mut() {
            cache.put(key, digest.clone());
        }
    }
    Ok(digest)
}

/// Order-independently combines two digests (`DigestUtils.combineUnordered`): copies the longer
/// slice (or `lhs` when lengths are equal) and adds each byte of the shorter slice modulo 256.
pub fn combine_digests_unordered(lhs: &[u8], rhs: &[u8]) -> Vec<u8> {
    let (mut base, other) = if lhs.len() >= rhs.len() {
        (lhs.to_vec(), rhs)
    } else {
        (rhs.to_vec(), lhs)
    };
    for (i, &b) in other.iter().enumerate() {
        base[i] = base[i].wrapping_add(b);
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryFileSystem;

    #[test]
    fn combine_unordered_is_commutative() {
        let a = [0xfe_u8, 0x05, 0x10, 0x99];
        let b = [0x04_u8, 0xff];
        let ab = combine_digests_unordered(&a, &b);
        let ba = combine_digests_unordered(&b, &a);
        assert_eq!(ab, ba);
        assert_eq!(ab, vec![0x02, 0x04, 0x10, 0x99]);
    }

    #[test]
    fn digest_cache_lifecycle_and_invalidation() {
        configure_digest_cache(0);
        assert!(get_digest_cache_stats().is_err());

        configure_digest_cache(2);
        let fs = InMemoryFileSystem::default();
        let p = PathFragment::create("/file.txt");
        fs.write_file(&p, b"hello").unwrap();

        let d1 = get_digest_with_manual_fallback(&fs, &p, None).unwrap();
        let d2 = get_digest_with_manual_fallback(&fs, &p, None).unwrap();
        assert_eq!(d1, d2);
        let stats = get_digest_cache_stats().unwrap();
        assert_eq!(stats.miss_count, 1);
        assert_eq!(stats.hit_count, 1);

        // Overwriting changes node_id/size so cache misses and recomputes.
        fs.write_file(&p, b"world!").unwrap();
        let d3 = get_digest_with_manual_fallback(&fs, &p, None).unwrap();
        assert_ne!(d1, d3);

        clear_digest_cache();
        assert_eq!(get_digest_cache_stats().unwrap().size, 0);
        configure_digest_cache(0);
    }
}
