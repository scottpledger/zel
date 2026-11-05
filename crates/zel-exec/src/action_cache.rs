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

//! `CompactPersistentActionCache` / `ActionCacheChecker` semantics.
//!
//! One entry per *primary output* exec path (`ActionCache.Entry`):
//!
//! * `action_key`: fingerprint of everything that defines the action itself — mnemonic, command
//!   line, environment, execution requirements, declared inputs/outputs, and for non-spawn
//!   actions their payload — computed with the configured `--digest_function`,
//! * `used_client_env_digest`: digest of the client environment variables the action used,
//! * `md_digest`: digest over `(input path, input content digest)` for all inputs *including
//!   discovered inputs* (so input content changes re-execute the action),
//! * `files`: digest of every output file as it was after the action ran (a modified output is
//!   re-created),
//! * `discovered_inputs`: paths discovered during execution (e.g. from `.d` files).
//!
//! `ActionCacheChecker.mustExecute` reports why an action must run (`MustExecuteReason`).
//! Persistence: a versioned binary file with an interned string table, written atomically on
//! `save`; a file that is unreadable, from another version or written with a different digest
//! function is discarded (Bazel: "Failed to load action cache ... cache will be rebuilt").

use crate::digest::DigestFunction;
use std::collections::BTreeMap;
use std::io;
use std::sync::Mutex;
use zel_vfs::{FileSystem, PathFragment};

const MAGIC: &[u8; 8] = b"ZELACv1\0";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionCacheEntry {
    pub action_key: String,
    pub used_client_env_digest: String,
    pub md_digest: String,
    pub files: BTreeMap<String, String>,
    pub discovered_inputs: Vec<String>,
}

/// Why `ActionCacheChecker.mustExecute` decided to run an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MustExecuteReason {
    NotCached,
    DifferentActionKey,
    DifferentEnvironment,
    DifferentFiles,
    DifferentDeps,
    OutputsMissing,
    /// Cache hit: nothing to execute.
    UpToDate,
}

impl MustExecuteReason {
    /// The `--explain` text (`ActionCacheChecker.Token` / `MissReason` descriptions).
    pub fn explanation(&self) -> &'static str {
        match self {
            MustExecuteReason::NotCached => "no entry in the cache (action is new)",
            MustExecuteReason::DifferentActionKey => "action command has changed",
            MustExecuteReason::DifferentEnvironment => "action environment has changed",
            MustExecuteReason::DifferentFiles => "one of the files has changed",
            MustExecuteReason::DifferentDeps => "the set of input files has changed",
            MustExecuteReason::OutputsMissing => "output file(s) missing",
            MustExecuteReason::UpToDate => "up to date",
        }
    }
}

#[derive(Debug)]
pub struct CompactPersistentActionCache {
    entries: Mutex<BTreeMap<String, ActionCacheEntry>>,
    digest: DigestFunction,
    /// Set when `load_from_disk` discarded an unusable cache file.
    pub load_warning: Mutex<Option<String>>,
}

impl Default for CompactPersistentActionCache {
    fn default() -> Self {
        Self::with_digest(DigestFunction::default())
    }
}

// ----- binary encoding helpers -----

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        if v < 0x80 {
            out.push(v as u8);
            return;
        }
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
}

fn get_varint(b: &[u8], pos: &mut usize) -> Option<u64> {
    let mut r = 0u64;
    let mut shift = 0;
    loop {
        let c = *b.get(*pos)?;
        *pos += 1;
        r |= ((c & 0x7f) as u64) << shift;
        if c & 0x80 == 0 {
            return Some(r);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

struct Indexer {
    ids: BTreeMap<String, u64>,
    list: Vec<String>,
}

impl Indexer {
    fn id(&mut self, s: &str) -> u64 {
        if let Some(i) = self.ids.get(s) {
            return *i;
        }
        let i = self.list.len() as u64;
        self.ids.insert(s.to_string(), i);
        self.list.push(s.to_string());
        i
    }
}

impl CompactPersistentActionCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_digest(digest: DigestFunction) -> Self {
        Self { entries: Mutex::new(BTreeMap::new()), digest, load_warning: Mutex::new(None) }
    }

    pub fn digest_function(&self) -> DigestFunction {
        self.digest
    }

    /// Loads the cache; an unusable file yields an empty cache and sets `load_warning`.
    pub fn load_from_disk(fs: &dyn FileSystem, cache_file: &PathFragment) -> Self {
        Self::load_from_disk_with(fs, cache_file, DigestFunction::default())
    }

    pub fn load_from_disk_with(fs: &dyn FileSystem, cache_file: &PathFragment, digest: DigestFunction) -> Self {
        let cache = Self::with_digest(digest);
        let Ok(bytes) = fs.read_file(cache_file) else { return cache };
        match Self::decode(&bytes, digest) {
            Ok(map) => *cache.entries.lock().unwrap() = map,
            Err(e) => {
                *cache.load_warning.lock().unwrap() = Some(format!(
                    "Failed to load action cache: {e}. Cache will be rebuilt (all actions will re-execute)."
                ));
            }
        }
        cache
    }

    fn decode(b: &[u8], digest: DigestFunction) -> Result<BTreeMap<String, ActionCacheEntry>, String> {
        if b.len() < MAGIC.len() || &b[..MAGIC.len()] != MAGIC {
            return Err("bad magic / unsupported action cache version".into());
        }
        let mut pos = MAGIC.len();
        let trunc = || "truncated action cache".to_string();
        let name_len = get_varint(b, &mut pos).ok_or_else(trunc)? as usize;
        let name = std::str::from_utf8(b.get(pos..pos + name_len).ok_or_else(trunc)?).map_err(|e| e.to_string())?;
        pos += name_len;
        if name != digest.name() {
            return Err(format!("action cache was written with digest function {name}, current is {}", digest.name()));
        }
        let n_strings = get_varint(b, &mut pos).ok_or_else(trunc)? as usize;
        let mut strings = Vec::with_capacity(n_strings);
        for _ in 0..n_strings {
            let l = get_varint(b, &mut pos).ok_or_else(trunc)? as usize;
            let s = std::str::from_utf8(b.get(pos..pos + l).ok_or_else(trunc)?).map_err(|e| e.to_string())?;
            pos += l;
            strings.push(s.to_string());
        }
        let s = |i: u64| strings.get(i as usize).cloned().ok_or_else(|| "bad string index".to_string());
        let n_entries = get_varint(b, &mut pos).ok_or_else(trunc)? as usize;
        let mut map = BTreeMap::new();
        for _ in 0..n_entries {
            let key = s(get_varint(b, &mut pos).ok_or_else(trunc)?)?;
            let mut e = ActionCacheEntry {
                action_key: s(get_varint(b, &mut pos).ok_or_else(trunc)?)?,
                used_client_env_digest: s(get_varint(b, &mut pos).ok_or_else(trunc)?)?,
                md_digest: s(get_varint(b, &mut pos).ok_or_else(trunc)?)?,
                ..Default::default()
            };
            let nf = get_varint(b, &mut pos).ok_or_else(trunc)?;
            for _ in 0..nf {
                let p = s(get_varint(b, &mut pos).ok_or_else(trunc)?)?;
                let d = s(get_varint(b, &mut pos).ok_or_else(trunc)?)?;
                e.files.insert(p, d);
            }
            let nd = get_varint(b, &mut pos).ok_or_else(trunc)?;
            for _ in 0..nd {
                e.discovered_inputs.push(s(get_varint(b, &mut pos).ok_or_else(trunc)?)?);
            }
            map.insert(key, e);
        }
        // Trailing checksum guards against partial writes.
        let body_end = pos;
        let want = b.get(pos..pos + 8).ok_or_else(trunc)?;
        let got = crate::digest::sha1(&b[..body_end]);
        if want != &got[..8] {
            return Err("checksum mismatch".into());
        }
        Ok(map)
    }

    pub fn encode(&self) -> Vec<u8> {
        let entries = self.entries.lock().unwrap();
        let mut ix = Indexer { ids: BTreeMap::new(), list: Vec::new() };
        let mut body = Vec::new();
        put_varint(&mut body, entries.len() as u64);
        for (k, e) in entries.iter() {
            put_varint(&mut body, ix.id(k));
            put_varint(&mut body, ix.id(&e.action_key));
            put_varint(&mut body, ix.id(&e.used_client_env_digest));
            put_varint(&mut body, ix.id(&e.md_digest));
            put_varint(&mut body, e.files.len() as u64);
            for (p, d) in &e.files {
                put_varint(&mut body, ix.id(p));
                put_varint(&mut body, ix.id(d));
            }
            put_varint(&mut body, e.discovered_inputs.len() as u64);
            for d in &e.discovered_inputs {
                put_varint(&mut body, ix.id(d));
            }
        }
        let mut out = MAGIC.to_vec();
        put_varint(&mut out, self.digest.name().len() as u64);
        out.extend_from_slice(self.digest.name().as_bytes());
        put_varint(&mut out, ix.list.len() as u64);
        for s in &ix.list {
            put_varint(&mut out, s.len() as u64);
            out.extend_from_slice(s.as_bytes());
        }
        out.extend_from_slice(&body);
        let sum = crate::digest::sha1(&out);
        out.extend_from_slice(&sum[..8]);
        out
    }

    /// Atomic save (write to a temp file, then rename).
    pub fn save_to_disk(&self, fs: &dyn FileSystem, cache_file: &PathFragment) -> io::Result<()> {
        if let Some(parent) = cache_file.parent_directory() {
            fs.create_directory_and_parents(&parent)?;
        }
        let tmp = PathFragment::create(&format!("{}.tmp", cache_file.path_string()));
        fs.write_file(&tmp, &self.encode())?;
        fs.rename(&tmp, cache_file)
    }

    /// `bazel dump --action_cache` (`CompactPersistentActionCache.dump`): the record count and
    /// every record sorted by key, in Bazel's textual layout.
    pub fn dump(&self, out: &mut Vec<u8>) {
        use std::io::Write;
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(out, "Action cache ({} records):", entries.len());
        for (i, (key, e)) in entries.iter().enumerate() {
            let _ = writeln!(out, "  {i} -> {key}");
            let _ = writeln!(out, "  digest = {}", e.action_key);
            if !e.discovered_inputs.is_empty() {
                let _ = writeln!(out, "  discoveredInputPaths =");
                let mut d = e.discovered_inputs.clone();
                d.sort();
                for p in d {
                    let _ = writeln!(out, "    {p}");
                }
            }
            if !e.files.is_empty() {
                let _ = writeln!(out, "  outputFileMetadata =");
                for (p, m) in &e.files {
                    let _ = writeln!(out, "    {p} = {m}");
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get_entry(&self, primary_output: &str) -> Option<ActionCacheEntry> {
        self.entries.lock().unwrap().get(primary_output).cloned()
    }

    pub fn put_entry(&self, primary_output: &str, entry: ActionCacheEntry) {
        self.entries.lock().unwrap().insert(primary_output.to_string(), entry);
    }

    pub fn remove(&self, output_exec_path: &str) {
        self.entries.lock().unwrap().remove(output_exec_path);
    }

    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }

    // -- legacy simple-string API (kept for existing callers) --

    pub fn get(&self, output_exec_path: &str) -> Option<String> {
        self.get_entry(output_exec_path).map(|e| e.action_key)
    }

    pub fn put(&self, output_exec_path: String, fingerprint: String) {
        let mut e = self.get_entry(&output_exec_path).unwrap_or_default();
        e.action_key = fingerprint;
        self.put_entry(&output_exec_path, e);
    }

    // ------------------------------------------------------------------ ActionCacheChecker

    /// Fingerprints the parts of an action that define it (`Action.computeKey`).
    pub fn compute_action_key(&self, parts: &ActionKeyParts) -> String {
        let mut b = Vec::new();
        let mut add = |s: &str| {
            b.extend_from_slice(&(s.len() as u64).to_le_bytes());
            b.extend_from_slice(s.as_bytes());
        };
        add("zel-action-key-v1");
        add(&parts.mnemonic);
        add(&parts.kind);
        for a in &parts.command_line {
            add(a);
        }
        add("|env");
        for (k, v) in &parts.env {
            add(k);
            add(v);
        }
        if !parts.inherited_env.is_empty() || !parts.unset_env.is_empty() {
            add("|inherited_env");
            for n in &parts.inherited_env {
                add(n);
            }
            add("|unset_env");
            for n in &parts.unset_env {
                add(n);
            }
        }
        add("|exec_info");
        for (k, v) in &parts.execution_info {
            add(k);
            add(v);
        }
        add("|inputs");
        for i in &parts.input_paths {
            add(i);
        }
        add("|outputs");
        for o in &parts.output_paths {
            add(o);
        }
        add("|payload");
        b.extend_from_slice(&parts.payload);
        self.digest.hex(&b)
    }

    /// `ActionCacheChecker` metadata digest: over sorted `(path, content digest)` of all inputs
    /// (declared + discovered). `digest_of` returns `None` for unreadable inputs.
    pub fn compute_md_digest(&self, inputs: &[(String, Option<String>)]) -> String {
        let mut v: Vec<&(String, Option<String>)> = inputs.iter().collect();
        v.sort();
        let mut b = Vec::new();
        for (p, d) in v {
            b.extend_from_slice(p.as_bytes());
            b.push(0);
            b.extend_from_slice(d.as_deref().unwrap_or("<missing>").as_bytes());
            b.push(0);
        }
        self.digest.hex(&b)
    }

    pub fn client_env_digest(&self, used: &BTreeMap<String, String>) -> String {
        let mut b = Vec::new();
        for (k, v) in used {
            b.extend_from_slice(k.as_bytes());
            b.push(b'=');
            b.extend_from_slice(v.as_bytes());
            b.push(0);
        }
        self.digest.hex(&b)
    }

    /// `ActionCacheChecker.mustExecute`. `current_inputs` must contain declared inputs and (from
    /// the recorded entry) discovered inputs; `current_outputs` maps each output path to its
    /// current digest (`None` = missing).
    pub fn must_execute(
        &self,
        primary_output: &str,
        action_key: &str,
        client_env_digest: &str,
        md_digest_of: &dyn Fn(&[String]) -> String,
        current_outputs: &[(String, Option<String>)],
    ) -> MustExecuteReason {
        let Some(e) = self.get_entry(primary_output) else { return MustExecuteReason::NotCached };
        if e.action_key != action_key {
            return MustExecuteReason::DifferentActionKey;
        }
        if e.used_client_env_digest != client_env_digest {
            return MustExecuteReason::DifferentEnvironment;
        }
        if current_outputs.iter().any(|(_, d)| d.is_none()) {
            return MustExecuteReason::OutputsMissing;
        }
        if md_digest_of(&e.discovered_inputs) != e.md_digest {
            return MustExecuteReason::DifferentDeps;
        }
        for (p, d) in current_outputs {
            if e.files.get(p) != d.as_ref() {
                return MustExecuteReason::DifferentFiles;
            }
        }
        MustExecuteReason::UpToDate
    }
}

/// Inputs to `compute_action_key`.
#[derive(Debug, Clone, Default)]
pub struct ActionKeyParts {
    pub mnemonic: String,
    pub kind: String,
    pub command_line: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Names of the variables inherited from the client environment (their values are tracked by
    /// the used-client-env digest) and of the explicitly unset ones (`ActionEnvironment.addTo`).
    pub inherited_env: Vec<String>,
    pub unset_env: Vec<String>,
    pub execution_info: BTreeMap<String, String>,
    pub input_paths: Vec<String>,
    pub output_paths: Vec<String>,
    pub payload: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_vfs::InMemoryFileSystem;

    fn parts() -> ActionKeyParts {
        ActionKeyParts {
            mnemonic: "CppCompile".into(),
            kind: "spawn".into(),
            command_line: vec!["gcc".into(), "-c".into(), "a.c".into()],
            env: [("A".to_string(), "1".to_string())].into(),
            inherited_env: vec![],
            unset_env: vec![],
            execution_info: BTreeMap::new(),
            input_paths: vec!["a.c".into()],
            output_paths: vec!["a.o".into()],
            payload: vec![],
        }
    }

    #[test]
    fn action_key_covers_command_line_env_exec_info_and_digest_function() {
        let c = CompactPersistentActionCache::new();
        let base = c.compute_action_key(&parts());
        let mut p = parts();
        p.command_line.push("-O2".into());
        assert_ne!(c.compute_action_key(&p), base, "command line");
        let mut p = parts();
        p.env.insert("A".into(), "2".into());
        assert_ne!(c.compute_action_key(&p), base, "env");
        let mut p = parts();
        p.execution_info.insert("local".into(), "1".into());
        assert_ne!(c.compute_action_key(&p), base, "execution requirements");
        let mut p = parts();
        p.mnemonic = "X".into();
        assert_ne!(c.compute_action_key(&p), base);
        assert_eq!(c.compute_action_key(&parts()), base, "deterministic");
        for f in DigestFunction::ALL {
            let k = CompactPersistentActionCache::with_digest(f).compute_action_key(&parts());
            assert_eq!(k.len(), f.digest_length() * 2, "{f:?}");
        }
        assert_ne!(
            CompactPersistentActionCache::with_digest(DigestFunction::Sha1).compute_action_key(&parts()),
            base
        );
    }

    #[test]
    fn md_digest_depends_on_discovered_inputs_and_contents() {
        let c = CompactPersistentActionCache::new();
        let a = c.compute_md_digest(&[("a.c".into(), Some("d1".into())), ("a.h".into(), Some("d2".into()))]);
        let reordered = c.compute_md_digest(&[("a.h".into(), Some("d2".into())), ("a.c".into(), Some("d1".into()))]);
        assert_eq!(a, reordered);
        let changed = c.compute_md_digest(&[("a.c".into(), Some("d1".into())), ("a.h".into(), Some("d3".into()))]);
        assert_ne!(a, changed);
        let fewer = c.compute_md_digest(&[("a.c".into(), Some("d1".into()))]);
        assert_ne!(a, fewer);
    }

    #[test]
    fn must_execute_reasons() {
        let c = CompactPersistentActionCache::new();
        let md = |paths: &[String]| paths.join(",");
        let out = vec![("a.o".to_string(), Some("od".to_string()))];
        assert_eq!(c.must_execute("a.o", "k", "e", &md, &out), MustExecuteReason::NotCached);
        c.put_entry(
            "a.o",
            ActionCacheEntry {
                action_key: "k".into(),
                used_client_env_digest: "e".into(),
                md_digest: "a.h".into(),
                files: [("a.o".to_string(), "od".to_string())].into(),
                discovered_inputs: vec!["a.h".into()],
            },
        );
        assert_eq!(c.must_execute("a.o", "k", "e", &md, &out), MustExecuteReason::UpToDate);
        assert_eq!(c.must_execute("a.o", "k2", "e", &md, &out), MustExecuteReason::DifferentActionKey);
        assert_eq!(c.must_execute("a.o", "k", "e2", &md, &out), MustExecuteReason::DifferentEnvironment);
        assert_eq!(
            c.must_execute("a.o", "k", "e", &|_| "other".to_string(), &out),
            MustExecuteReason::DifferentDeps
        );
        assert_eq!(
            c.must_execute("a.o", "k", "e", &md, &[("a.o".into(), Some("tampered".into()))]),
            MustExecuteReason::DifferentFiles
        );
        assert_eq!(
            c.must_execute("a.o", "k", "e", &md, &[("a.o".into(), None)]),
            MustExecuteReason::OutputsMissing
        );
    }

    #[test]
    fn persistence_round_trip_and_corruption_handling() {
        let fs = InMemoryFileSystem::default();
        let file = PathFragment::create("/ob/action_cache/action_cache.blaze");
        let c = CompactPersistentActionCache::new();
        c.put_entry(
            "bazel-out/x/bin/a.o",
            ActionCacheEntry {
                action_key: "key".into(),
                used_client_env_digest: "env".into(),
                md_digest: "md".into(),
                files: [("bazel-out/x/bin/a.o".to_string(), "dig".to_string()), ("bazel-out/x/bin/b.d".to_string(), "dig2".to_string())].into(),
                discovered_inputs: vec!["x.h".into(), "y.h".into()],
            },
        );
        c.save_to_disk(&fs, &file).unwrap();
        let loaded = CompactPersistentActionCache::load_from_disk(&fs, &file);
        assert_eq!(loaded.get_entry("bazel-out/x/bin/a.o"), c.get_entry("bazel-out/x/bin/a.o"));
        assert!(loaded.load_warning.lock().unwrap().is_none());
        // Wrong digest function discards the cache.
        let other = CompactPersistentActionCache::load_from_disk_with(&fs, &file, DigestFunction::Blake3);
        assert!(other.is_empty());
        assert!(other.load_warning.lock().unwrap().as_ref().unwrap().contains("digest function SHA-256"));
        // Corrupt file.
        let mut bytes = fs.read_file(&file).unwrap();
        let n = bytes.len();
        bytes[n / 2] ^= 0xff;
        fs.write_file(&file, &bytes).unwrap();
        let bad = CompactPersistentActionCache::load_from_disk(&fs, &file);
        assert!(bad.is_empty());
        assert!(bad.load_warning.lock().unwrap().is_some());
        fs.write_file(&file, b"junk").unwrap();
        assert!(CompactPersistentActionCache::load_from_disk(&fs, &file).load_warning.lock().unwrap().is_some());
    }
}
