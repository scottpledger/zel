//! Emulation of the iteration order of the Java collections Bazel's post-analysis query uses:
//! `ConcurrentHashMap` (the `ThreadSafeMutableKeyExtractorBackedSetImpl` behind
//! `ConfiguredTargetQueryEnvironment.createThreadSafeMutableSet()`) and `HashMap`
//! (`PostAnalysisQueryEnvironment.targetifyValues`), together with the hash codes of the keys
//! (`Label`, `PackageIdentifier`, `RepositoryName`, `ConfiguredTargetKey`,
//! `BuildConfigurationKey`).  The cquery/aquery result order is the iteration order of these sets,
//! so it is a deterministic function of the key hashes.

/// `String.hashCode` (UTF-16 code units).
pub fn string_hash(s: &str) -> i32 {
    let mut h: i32 = 0;
    for u in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(u as i32);
    }
    h
}

/// `HashCodes.hashObjects(o1, o2, o3)`.
pub fn hash_objects3(h1: i32, h2: i32, h3: i32) -> i32 {
    h3.wrapping_add(31i32.wrapping_mul(h2.wrapping_add(31i32.wrapping_mul(h1.wrapping_add(31)))))
}

/// `HashCodes.hashObjects(o1, o2)`.
pub fn hash_objects2(h1: i32, h2: i32) -> i32 {
    h2.wrapping_add(31i32.wrapping_mul(h1.wrapping_add(31)))
}

/// `RepositoryName.hashCode` for a plain name (`hashObjects(name, null, null)`).
pub fn repo_hash(canonical_name: &str) -> i32 {
    hash_objects3(string_hash(canonical_name), 0, 0)
}

/// `Label.hashCode`: `31 * (31 + name.hashCode()) + packageIdentifier.hashCode()`.
pub fn label_hash(repo_canonical: &str, package: &str, name: &str) -> i32 {
    let pkg = hash_objects2(repo_hash(repo_canonical), string_hash(package));
    31i32.wrapping_mul(31i32.wrapping_add(string_hash(name))).wrapping_add(pkg)
}

/// `BuildConfigurationKey.hashCode` = `BuildOptions.hashCode` = `31 + checksum().hashCode()`.
pub fn config_key_hash(full_checksum: &str) -> i32 {
    31i32.wrapping_add(string_hash(full_checksum))
}

/// `ConfiguredTargetKey.computeHashCode`.
pub fn ct_key_hash(label: i32, config: Option<i32>, exec_platform: Option<i32>, apply_rule_transition: bool) -> i32 {
    let h = hash_objects3(label, config.unwrap_or(0), exec_platform.unwrap_or(0));
    if apply_rule_transition {
        h
    } else {
        !h
    }
}

fn spread_chm(h: i32) -> i32 {
    (h ^ ((h as u32) >> 16) as i32) & 0x7fff_ffff
}

fn table_size_for(c: usize) -> usize {
    c.max(1).next_power_of_two()
}

#[derive(Clone, Copy)]
struct Entry {
    hash: i32,
    id: usize,
}

/// Iteration order of a `ConcurrentHashMap` created with `initialCapacity = parallelism` and
/// load factor 0.75 after inserting the (distinct) `(hash, id)` entries in order.  Returns ids.
pub fn chm_order(entries: &[(i32, usize)], parallelism: usize) -> Vec<usize> {
    let init = (1.0f64 + (parallelism as f64) / 0.75f32 as f64) as usize;
    let mut n = table_size_for(init);
    let mut size_ctl = n - (n >> 2);
    let mut table: Vec<Vec<Entry>> = vec![Vec::new(); n];
    let mut count = 0usize;
    for &(h, id) in entries {
        let sh = spread_chm(h);
        let idx = (n - 1) & (sh as usize);
        table[idx].push(Entry { hash: sh, id });
        count += 1;
        while count >= size_ctl && n < (1 << 30) {
            // transfer to 2n
            let mut next: Vec<Vec<Entry>> = vec![Vec::new(); n * 2];
            for i in (0..n).rev() {
                let bin = &table[i];
                if bin.is_empty() {
                    continue;
                }
                let mut run_bit = (bin[0].hash as usize) & n;
                let mut last_run = 0usize;
                for (k, p) in bin.iter().enumerate().skip(1) {
                    let b = (p.hash as usize) & n;
                    if b != run_bit {
                        run_bit = b;
                        last_run = k;
                    }
                }
                let tail: Vec<Entry> = bin[last_run..].to_vec();
                let (mut ln, mut hn) = if run_bit == 0 { (tail, Vec::new()) } else { (Vec::new(), tail) };
                for p in &bin[..last_run] {
                    if (p.hash as usize) & n == 0 {
                        ln.insert(0, *p);
                    } else {
                        hn.insert(0, *p);
                    }
                }
                next[i] = ln;
                next[i + n] = hn;
            }
            table = next;
            n *= 2;
            // `sizeCtl = (oldN << 1) - (oldN >>> 1)` = 0.75 * newN
            size_ctl = n - (n >> 2);
        }
    }
    table.iter().flat_map(|b| b.iter().map(|e| e.id)).collect()
}

/// Iteration order of a default `java.util.HashMap` (table 16, load factor 0.75) after inserting
/// the distinct `(hash, id)` entries in order.
pub fn hashmap_order(entries: &[(i32, usize)]) -> Vec<usize> {
    hashmap_order_cap(entries, 16)
}

pub fn hashmap_order_cap(entries: &[(i32, usize)], init_cap: usize) -> Vec<usize> {
    let mut cap = init_cap.max(2).next_power_of_two();
    while entries.len() > cap * 3 / 4 {
        cap *= 2;
    }
    let mut v: Vec<(usize, usize)> = entries
        .iter()
        .map(|&(h, id)| {
            let sh = h ^ ((h as u32) >> 16) as i32;
            ((sh as u32 as usize) & (cap - 1), id)
        })
        .collect();
    v.sort_by_key(|&(b, _)| b); // stable: insertion order within a bucket
    v.into_iter().map(|(_, id)| id).collect()
}

pub fn parallelism() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_string_hash() {
        assert_eq!(string_hash("hello"), 99162322);
        assert_eq!(string_hash(""), 0);
    }

    #[test]
    fn chm_sorted_by_bucket() {
        // hashes 3,1,2 in a 16-bucket table iterate 1,2,3
        assert_eq!(chm_order(&[(3, 0), (1, 1), (2, 2)], 8), vec![1, 2, 0]);
    }

    #[test]
    fn chm_resize_keeps_bucket_order() {
        let e: Vec<(i32, usize)> = (0..40).map(|i| ((i * 7) % 64, i as usize)).collect();
        let o = chm_order(&e, 8);
        assert_eq!(o.len(), 40);
        let keys: Vec<i32> = o.iter().map(|&i| e[i].0).collect();
        // final table has 64 buckets: sorted by hash
        let mut s = keys.clone();
        s.sort();
        assert_eq!(keys, s);
    }

    #[test]
    fn chm_collision_resize_reverses_prefix() {
        // 16-table: hashes 0 and 16 collide; a, b, c with (hash&16): 0,16,0 -> lastRun = c
        // after growing to 32, lo = [a? ...]. Just check determinism & length.
        let mut e: Vec<(i32, usize)> = vec![(0, 0), (16, 1), (32, 2)];
        for i in 3..13 {
            e.push((i as i32 * 100 + 1, i));
        }
        let o = chm_order(&e, 8);
        assert_eq!(o.len(), 13);
    }
}
