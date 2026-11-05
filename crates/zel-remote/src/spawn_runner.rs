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

//! `RemoteSpawnRunner` + `RemoteExecutionService` + `CombinedCache`: executes a [`Spawn`]
//! through the disk cache, the remote action cache and/or a remote executor, falling back to a
//! local [`SpawnRunner`].
//!
//! The strategy boundary is the [`SpawnRunner`] trait (same shape as
//! `zel_sandbox::execute_spawn_local`). Integrators (the exec phase) construct a
//! [`RemoteSpawnRunner`] from [`RemoteOptions`] and call [`SpawnRunner::exec`] for each spawn.
//!
//! Lazy outputs (`--remote_download_outputs=toplevel|minimal`): outputs of remotely executed or
//! cache-hit actions are not downloaded; they are tracked by digest, passed to dependent remote
//! actions as digest-only Merkle inputs, and materialized on demand before local execution or
//! explicitly through [`RemoteSpawnRunner::materialize`] (used for top-level targets).

use crate::disk_cache::{set_executable, DiskCache};
use crate::merkle::{flatten_tree, tree_from_local_dir, BlobSource, InputKind, MerkleInput, MerkleTree, MerkleTreeComputer, TreeEntry};
use crate::options::{DownloadMode, RemoteOptions};
use crate::reapi_client::{is_missing_blobs_failure, ActionMeta, ReapiClient, RemoteError};
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use zel_proto::reapi::*;
use zel_sandbox::{Spawn, SpawnResult, SpawnStatus};
use zel_vfs::{FileSystem, PathFragment};

/// A strategy that can execute spawns (`SpawnRunner` in Bazel).
pub trait SpawnRunner: Send + Sync {
    fn name(&self) -> &str;
    fn can_exec(&self, spawn: &Spawn) -> bool;
    fn exec(&self, spawn: &Spawn, execroot: &PathFragment, fs: &dyn FileSystem) -> io::Result<SpawnResult>;
}

/// How one `exec` call behaves (the integration points of `RemoteSpawnStrategy`, the
/// `SpawnCache` role of a remote cache wrapped around a local strategy, and the remote branch of
/// dynamic execution).
#[derive(Clone)]
pub struct ExecMode<'a> {
    /// Execute cache misses on the remote executor (when one is configured). `false` gives the
    /// `RemoteSpawnCache` behaviour: lookup, run `fallback` on a miss, upload the result.
    pub allow_remote_exec: bool,
    /// Runner used for local execution of cache misses / fallbacks (default: the runner given at
    /// construction).
    pub fallback: Option<&'a dyn SpawnRunner>,
    /// Permit falling back to local execution when remote execution fails (disabled for the remote
    /// branch of dynamic execution, where the local branch is racing anyway).
    pub allow_local: bool,
    /// Dynamic-execution lock: consulted before any output is written into the exec root.
    pub hooks: zel_sandbox::ExecHooks,
}

impl Default for ExecMode<'_> {
    fn default() -> Self {
        Self { allow_remote_exec: true, fallback: None, allow_local: true, hooks: Default::default() }
    }
}

/// Local execution (`zel_sandbox::execute_spawn_local`).
pub struct LocalSpawnRunner;

impl SpawnRunner for LocalSpawnRunner {
    fn name(&self) -> &str {
        "local"
    }
    fn can_exec(&self, _spawn: &Spawn) -> bool {
        true
    }
    fn exec(&self, spawn: &Spawn, execroot: &PathFragment, fs: &dyn FileSystem) -> io::Result<SpawnResult> {
        zel_sandbox::execute_spawn_local(spawn, execroot, fs)
    }
}

// ---- `Spawns.*` execution-requirement predicates -------------------------------------------

fn has(spawn: &Spawn, key: &str) -> bool {
    spawn.execution_info.contains_key(key)
}

/// `Spawns.mayBeCached`.
pub fn may_be_cached(spawn: &Spawn) -> bool {
    !has(spawn, "no-cache")
}
/// `Spawns.mayBeCachedRemotely`.
pub fn may_be_cached_remotely(spawn: &Spawn) -> bool {
    may_be_cached(spawn) && !has(spawn, "no-remote-cache") && !has(spawn, "no-remote")
}
/// `Spawns.mayBeExecutedRemotely`.
pub fn may_be_executed_remotely(spawn: &Spawn) -> bool {
    !has(spawn, "no-remote") && !has(spawn, "no-remote-exec") && !has(spawn, "local")
}

const TREE_OUTPUT_PREFIX: &str = "zel-tree-output:";

/// Marks `path` (relative to the exec root) as a directory (tree artifact) output of `spawn`.
pub fn mark_tree_output(spawn: &mut Spawn, path: &str) {
    spawn.execution_info.insert(format!("{TREE_OUTPUT_PREFIX}{path}"), "1".into());
}

fn tree_outputs(spawn: &Spawn) -> Vec<String> {
    spawn
        .execution_info
        .keys()
        .filter_map(|k| k.strip_prefix(TREE_OUTPUT_PREFIX).map(str::to_string))
        .collect()
}

/// Counters for the "N processes: ..." summary.
#[derive(Default)]
pub struct RemoteStats {
    pub disk_cache_hits: AtomicUsize,
    pub remote_cache_hits: AtomicUsize,
    pub remote_executions: AtomicUsize,
    pub local_executions: AtomicUsize,
    pub cache_uploads: AtomicUsize,
    pub bytes_downloaded: AtomicUsize,
    pub fallbacks: AtomicUsize,
}

impl RemoteStats {
    /// `INFO: N processes: ...`-style summary (Bazel's SpawnStats output).
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        for (n, label) in [
            (self.disk_cache_hits.load(Ordering::SeqCst), "disk cache hit"),
            (self.remote_cache_hits.load(Ordering::SeqCst), "remote cache hit"),
            (self.remote_executions.load(Ordering::SeqCst), "remote"),
            (self.local_executions.load(Ordering::SeqCst), "local"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {label}"));
            }
        }
        let total: usize = [&self.disk_cache_hits, &self.remote_cache_hits, &self.remote_executions, &self.local_executions]
            .iter()
            .map(|c| c.load(Ordering::SeqCst))
            .sum();
        format!("{total} process{}: {}", if total == 1 { "" } else { "es" }, parts.join(", "))
    }
}

#[derive(Debug, Clone)]
struct RemoteOutput {
    digest: Digest,
    executable: bool,
}

struct RemoteAction {
    command_bytes: Vec<u8>,
    action_bytes: Vec<u8>,
    action_digest: Digest,
    command_digest: Digest,
    tree: MerkleTree,
    meta: ActionMeta,
}

impl RemoteAction {
    /// All blobs the executor needs: input tree + command + action.
    fn all_blobs(&self) -> BTreeMap<Digest, BlobSource> {
        let mut b = self.tree.blobs.clone();
        b.insert(self.command_digest.clone(), BlobSource::Bytes(self.command_bytes.clone()));
        b.insert(self.action_digest.clone(), BlobSource::Bytes(self.action_bytes.clone()));
        b
    }
}

enum CacheHit {
    Disk(ActionResult),
    Remote(ActionResult),
}

pub struct RemoteSpawnRunner {
    pub opts: RemoteOptions,
    client: Option<Arc<ReapiClient>>,
    disk: Option<DiskCache>,
    merkle: MerkleTreeComputer,
    fallback: Arc<dyn SpawnRunner>,
    lazy: Mutex<HashMap<String, RemoteOutput>>,
    warnings: Mutex<Vec<String>>,
    pub stats: RemoteStats,
}

impl RemoteSpawnRunner {
    /// Builds the runner from parsed options (`RemoteModule.beforeCommand` equivalent):
    /// opens the disk cache, connects the REAPI client and checks server capabilities.
    pub fn from_options(
        opts: RemoteOptions,
        invocation_id: &str,
        fallback: Arc<dyn SpawnRunner>,
    ) -> Result<Self, String> {
        let client = opts.build_client(invocation_id)?.map(Arc::new);
        Self::new(opts, client, fallback)
    }

    pub fn new(opts: RemoteOptions, client: Option<Arc<ReapiClient>>, fallback: Arc<dyn SpawnRunner>) -> Result<Self, String> {
        let disk = match &opts.disk_cache {
            Some(p) => Some(DiskCache::open(Path::new(p), opts.digest_fn, true).map_err(|e| format!("Failed to open disk cache {p}: {e}"))?),
            None => None,
        };
        let mut warnings = Vec::new();
        if let Some(c) = &client {
            let need_exec = c.executor.is_some();
            c.check_compatibility(need_exec).map_err(|e| e.to_string())?;
        }
        if opts.cache_compression {
            warnings.push(
                "--remote_cache_compression is not supported by this build (no zstd implementation); blobs are transferred uncompressed".to_string(),
            );
        }
        if !opts.download_regex.is_empty() {
            warnings.push("--remote_download_regex is ignored by this build".to_string());
        }
        Ok(Self {
            merkle: MerkleTreeComputer::new(opts.digest_fn),
            opts,
            client,
            disk,
            fallback,
            lazy: Mutex::new(HashMap::new()),
            warnings: Mutex::new(warnings),
            stats: RemoteStats::default(),
        })
    }

    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut *self.warnings.lock().unwrap())
    }

    fn warn(&self, w: String) {
        self.warnings.lock().unwrap().push(w);
    }

    fn exec_remotely_enabled(&self) -> bool {
        self.client.as_ref().map(|c| c.executor.is_some()).unwrap_or(false)
    }

    /// A disk cache or remote cache is configured (`RemoteSpawnCache` has something to consult).
    pub fn has_cache(&self) -> bool {
        self.client.is_some() || self.disk.is_some()
    }

    /// Whether the exec-root-relative `path` is known only by digest (to be downloaded later).
    pub fn is_pending_output(&self, path: &str) -> bool {
        self.lazy.lock().unwrap().contains_key(path)
    }

    /// Number of outputs currently known only by digest (not downloaded).
    pub fn pending_remote_outputs(&self) -> usize {
        self.lazy.lock().unwrap().len()
    }

    // ------------------------------------------------------------------- action building

    fn collect_inputs(&self, spawn: &Spawn, execroot: &Path) -> Result<Vec<MerkleInput>, Vec<String>> {
        let lazy = self.lazy.lock().unwrap();
        let mut out = Vec::new();
        let mut missing = Vec::new();
        for inp in &spawn.inputs {
            let rel = inp.path_string().to_string();
            if let Some(r) = lazy.get(&rel) {
                out.push(MerkleInput {
                    path: rel,
                    kind: InputKind::Remote { digest: r.digest.clone(), executable: r.executable },
                    node_properties: None,
                });
                continue;
            }
            // Lazy outputs nested under a directory input.
            let prefix = format!("{rel}/");
            let mut had_nested = false;
            for (k, r) in lazy.iter() {
                if k.starts_with(&prefix) {
                    had_nested = true;
                    out.push(MerkleInput {
                        path: k.clone(),
                        kind: InputKind::Remote { digest: r.digest.clone(), executable: r.executable },
                        node_properties: None,
                    });
                }
            }
            let full = execroot.join(&rel);
            match std::fs::metadata(&full) {
                Ok(md) if md.is_dir() => expand_dir(&full, &rel, &mut out, &lazy),
                Ok(md) => {
                    #[cfg(unix)]
                    let exec = {
                        use std::os::unix::fs::PermissionsExt;
                        md.permissions().mode() & 0o111 != 0
                    };
                    #[cfg(not(unix))]
                    let exec = false;
                    out.push(MerkleInput::file(&rel, full, exec));
                }
                Err(_) if had_nested => {}
                Err(_) => missing.push(rel),
            }
        }
        if missing.is_empty() { Ok(out) } else { Err(missing) }
    }

    fn build_action(&self, spawn: &Spawn, execroot: &Path) -> Result<RemoteAction, Vec<String>> {
        let inputs = self.collect_inputs(spawn, execroot)?;
        let tree = self.merkle.build(&inputs).map_err(|e| vec![e.to_string()])?;
        let trees = tree_outputs(spawn);
        let mut files: Vec<String> = Vec::new();
        let mut dirs: Vec<String> = Vec::new();
        for o in &spawn.outputs {
            let p = o.path_string().to_string();
            if trees.contains(&p) { dirs.push(p) } else { files.push(p) }
        }
        files.sort();
        dirs.sort();
        let use_paths = self.client.as_ref().map(|c| c.supports_output_paths()).unwrap_or(true);
        let mut command = Command {
            arguments: spawn.argv.clone(),
            environment_variables: spawn
                .env
                .iter()
                .map(|(k, v)| EnvironmentVariable { name: k.clone(), value: v.clone() })
                .collect(),
            ..Default::default()
        };
        if use_paths {
            let mut all = files.clone();
            all.extend(dirs.clone());
            all.sort();
            command.output_paths = all;
        } else {
            command.output_files = files;
            command.output_directories = dirs;
        }
        let mut props: BTreeMap<String, String> = self.opts.default_exec_properties.clone();
        for (k, v) in &spawn.execution_info {
            if let Some(name) = k.strip_prefix("exec-property:") {
                props.insert(name.to_string(), v.clone());
            }
        }
        if !props.is_empty() {
            command.platform = Some(Platform {
                properties: props.into_iter().map(|(name, value)| PlatformProperty { name, value }).collect(),
            });
        }
        let command_bytes = command.encode_to_vec();
        let command_digest = self.opts.digest_fn.digest(&command_bytes);
        let salt = CacheSalt {
            may_be_executed_remotely: may_be_executed_remotely(spawn),
            workspace: spawn.execution_info.get("diff-workspace-cache").cloned().unwrap_or_default(),
        }
        .encode_to_vec();
        let timeout = spawn.timeout_secs().map(|s| Duration {
            seconds: s.floor() as i64,
            nanos: ((s - s.floor()) * 1e9) as i32,
        });
        let action = Action {
            command_digest: Some(command_digest.clone()),
            input_root_digest: Some(tree.root_digest.clone()),
            timeout,
            do_not_cache: !may_be_cached_remotely(spawn),
            salt,
            platform: None,
        };
        let action_bytes = action.encode_to_vec();
        let action_digest = self.opts.digest_fn.digest(&action_bytes);
        Ok(RemoteAction {
            meta: ActionMeta {
                action_id: action_digest.hash.clone(),
                mnemonic: spawn.mnemonic.clone(),
                target_id: spawn.owner_label.clone(),
            },
            command_bytes,
            action_bytes,
            action_digest,
            command_digest,
            tree,
        })
    }

    // ------------------------------------------------------------------- cache reads

    fn read_policy(&self, spawn: &Spawn) -> (bool, bool) {
        let remote = self.client.is_some() && self.opts.accept_cached && may_be_cached_remotely(spawn);
        let disk = self.disk.is_some() && may_be_cached(spawn);
        (remote, disk)
    }

    fn write_policy(&self, spawn: &Spawn) -> (bool, bool) {
        let remote = self.client.is_some()
            && self.opts.upload_local_results
            && may_be_cached_remotely(spawn)
            && !has(spawn, "no-remote-cache-upload")
            && self.client.as_ref().map(|c| c.remote_cache_supports_update()).unwrap_or(false);
        let disk = self.disk.is_some() && may_be_cached(spawn);
        (remote, disk)
    }

    fn lookup_cache(&self, ra: &RemoteAction, spawn: &Spawn) -> Option<CacheHit> {
        let (remote, disk) = self.read_policy(spawn);
        if disk {
            if let Some(r) = self.disk.as_ref().and_then(|d| d.get_action_result(&ra.action_digest)) {
                return Some(CacheHit::Disk(r));
            }
        }
        if remote {
            match self.client.as_ref().unwrap().get_action_result(&ra.action_digest, &ra.meta) {
                Ok(Some(r)) => return Some(CacheHit::Remote(r)),
                Ok(None) => {}
                Err(e) => self.warn(format!("Remote Cache: {e}")),
            }
        }
        None
    }

    /// Fetches a blob from the disk cache, then the remote cache (populating the disk cache).
    fn fetch_blob(&self, d: &Digest, meta: &ActionMeta) -> Result<Vec<u8>, RemoteError> {
        if let Some(disk) = &self.disk {
            if let Ok(Some(b)) = disk.get_blob(d) {
                return Ok(b);
            }
        }
        if let Some(c) = &self.client {
            let b = c.download_blob(d, meta)?;
            self.stats.bytes_downloaded.fetch_add(b.len(), Ordering::SeqCst);
            if let Some(disk) = &self.disk {
                let _ = disk.put_blob(d, &b);
            }
            return Ok(b);
        }
        Err(RemoteError::NotFound)
    }

    // ------------------------------------------------------------------- output handling

    fn should_download(&self, spawn: &Spawn) -> bool {
        match self.opts.download_mode {
            DownloadMode::All => true,
            DownloadMode::TopLevel | DownloadMode::Minimal => spawn.mnemonic == "TestRunner",
        }
    }

    /// Applies an `ActionResult` to the exec root (downloading or deferring outputs).
    /// Returns (stdout, stderr).
    fn apply_result(
        &self,
        r: &ActionResult,
        spawn: &Spawn,
        execroot: &Path,
        meta: &ActionMeta,
        download: bool,
    ) -> Result<(Vec<u8>, Vec<u8>), RemoteError> {
        let stdout = self.inline_or_fetch(&r.stdout_raw, &r.stdout_digest, meta)?;
        let stderr = self.inline_or_fetch(&r.stderr_raw, &r.stderr_digest, meta)?;
        if r.exit_code != 0 && !download {
            return Ok((stdout, stderr));
        }
        let mut lazy_updates: Vec<(String, RemoteOutput)> = Vec::new();
        for f in &r.output_files {
            let dest = execroot.join(&f.path);
            let _ = std::fs::remove_file(&dest);
            let digest = f.digest.clone().unwrap_or_default();
            if !f.contents.is_empty() {
                write_file(&dest, &f.contents, f.is_executable)?;
                continue;
            }
            if download || digest.size_bytes == 0 {
                if digest.size_bytes == 0 {
                    write_file(&dest, &[], f.is_executable)?;
                } else {
                    let b = self.fetch_blob(&digest, meta)?;
                    write_file(&dest, &b, f.is_executable)?;
                }
            } else {
                lazy_updates.push((f.path.clone(), RemoteOutput { digest, executable: f.is_executable }));
            }
        }
        for s in r.output_symlinks.iter().chain(&r.output_file_symlinks).chain(&r.output_directory_symlinks) {
            make_symlink(&execroot.join(&s.path), &s.target)?;
        }
        for od in &r.output_directories {
            let td = od.tree_digest.clone().ok_or_else(|| RemoteError::Other("output directory without tree digest".into()))?;
            let tree_bytes = self.fetch_blob(&td, meta)?;
            let tree = Tree::decode(&tree_bytes)?;
            let entries = flatten_tree(&tree, self.opts.digest_fn).map_err(RemoteError::Other)?;
            let base = execroot.join(&od.path);
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base)?;
            for e in entries {
                match e {
                    TreeEntry::Directory { path } => std::fs::create_dir_all(base.join(path))?,
                    TreeEntry::Symlink { path, target } => make_symlink(&base.join(path), &target)?,
                    TreeEntry::File { path, digest, executable } => {
                        if download || digest.size_bytes == 0 {
                            let b = if digest.size_bytes == 0 { vec![] } else { self.fetch_blob(&digest, meta)? };
                            write_file(&base.join(&path), &b, executable)?;
                        } else {
                            lazy_updates.push((format!("{}/{path}", od.path), RemoteOutput { digest, executable }));
                        }
                    }
                }
            }
        }
        // Declared outputs the result lacks are an error (Bazel: "output was not created").
        if r.exit_code == 0 {
            let have: std::collections::HashSet<&str> = r
                .output_files
                .iter()
                .map(|f| f.path.as_str())
                .chain(r.output_directories.iter().map(|d| d.path.as_str()))
                .chain(r.output_symlinks.iter().chain(&r.output_file_symlinks).chain(&r.output_directory_symlinks).map(|s| s.path.as_str()))
                .collect();
            for o in &spawn.outputs {
                if !have.contains(o.path_string()) {
                    return Err(RemoteError::Other(format!("Output {} was not created by the remote action", o.path_string())));
                }
            }
        }
        let mut lazy = self.lazy.lock().unwrap();
        // Fresh outputs supersede any stale lazy entries with the same prefix.
        for o in &spawn.outputs {
            let p = o.path_string();
            lazy.remove(p);
            let pre = format!("{p}/");
            lazy.retain(|k, _| !k.starts_with(&pre));
        }
        for (k, v) in lazy_updates {
            lazy.insert(k, v);
        }
        Ok((stdout, stderr))
    }

    fn inline_or_fetch(&self, raw: &[u8], digest: &Option<Digest>, meta: &ActionMeta) -> Result<Vec<u8>, RemoteError> {
        if !raw.is_empty() {
            return Ok(raw.to_vec());
        }
        match digest {
            Some(d) if d.size_bytes > 0 => self.fetch_blob(d, meta),
            _ => Ok(Vec::new()),
        }
    }

    /// Downloads outputs that are only known by digest (e.g. top-level targets in
    /// `--remote_download_outputs=toplevel` mode). Paths are exec-root relative; a directory
    /// path materializes everything below it.
    pub fn materialize(&self, paths: &[String], execroot: &Path) -> Result<usize, String> {
        let mut n = 0;
        let meta = ActionMeta::default();
        for p in paths {
            let pre = format!("{p}/");
            let items: Vec<(String, RemoteOutput)> = self
                .lazy
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _)| *k == p || k.starts_with(&pre))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for (k, v) in items {
                let b = self.fetch_blob(&v.digest, &meta).map_err(|e| format!("Failed to download {k}: {e}"))?;
                write_file(&execroot.join(&k), &b, v.executable).map_err(|e| format!("Failed to write {k}: {e}"))?;
                self.lazy.lock().unwrap().remove(&k);
                n += 1;
            }
        }
        Ok(n)
    }

    fn materialize_inputs(&self, spawn: &Spawn, execroot: &Path) -> Result<(), String> {
        let paths: Vec<String> = spawn.inputs.iter().map(|p| p.path_string().to_string()).collect();
        self.materialize(&paths, execroot).map(|_| ())
    }

    // ------------------------------------------------------------------- cache writes

    /// Uploads a locally produced result to the disk and/or remote cache.
    fn upload_result(&self, ra: &RemoteAction, spawn: &Spawn, execroot: &Path, res: &SpawnResult) {
        let (remote, disk) = self.write_policy(spawn);
        if !remote && !disk {
            return;
        }
        let mut blobs: BTreeMap<Digest, BlobSource> = BTreeMap::new();
        let mut ar = ActionResult { exit_code: res.exit_code, ..Default::default() };
        let f = self.opts.digest_fn;
        let trees = tree_outputs(spawn);
        let mut outs: Vec<String> = spawn.outputs.iter().map(|o| o.path_string().to_string()).collect();
        outs.sort();
        for o in outs {
            let full = execroot.join(&o);
            let Ok(md) = std::fs::symlink_metadata(&full) else { continue };
            if md.file_type().is_symlink() {
                if let Ok(t) = std::fs::read_link(&full) {
                    ar.output_symlinks.push(OutputSymlink { path: o, target: t.to_string_lossy().into_owned(), node_properties: None });
                }
            } else if md.is_dir() || trees.contains(&o) {
                match tree_from_local_dir(&full, f, &mut blobs) {
                    Ok(t) => {
                        let tb = t.encode_to_vec();
                        let td = f.digest(&tb);
                        blobs.insert(td.clone(), BlobSource::Bytes(tb));
                        ar.output_directories.push(OutputDirectory { path: o, tree_digest: Some(td), ..Default::default() });
                    }
                    Err(e) => {
                        self.warn(format!("Failed to upload output directory {o}: {e}"));
                        return;
                    }
                }
            } else {
                let Ok(data) = std::fs::read(&full) else { continue };
                let d = f.digest(&data);
                #[cfg(unix)]
                let exec = {
                    use std::os::unix::fs::PermissionsExt;
                    md.permissions().mode() & 0o111 != 0
                };
                #[cfg(not(unix))]
                let exec = false;
                blobs.insert(d.clone(), BlobSource::File(full));
                ar.output_files.push(OutputFile { path: o, digest: Some(d), is_executable: exec, ..Default::default() });
            }
        }
        for (bytes, slot) in [(&res.stdout, 0), (&res.stderr, 1)] {
            if !bytes.is_empty() {
                let d = f.digest(bytes);
                blobs.insert(d.clone(), BlobSource::Bytes(bytes.clone()));
                if slot == 0 { ar.stdout_digest = Some(d) } else { ar.stderr_digest = Some(d) }
            }
        }
        if disk {
            let dc = self.disk.as_ref().unwrap();
            for (d, s) in &blobs {
                if let Err(e) = dc.put_source(d, s) {
                    self.warn(format!("Disk Cache: {e}"));
                }
            }
            if let Err(e) = dc.put_action_result(&ra.action_digest, &ar) {
                self.warn(format!("Disk Cache: {e}"));
            }
        }
        if remote {
            let c = self.client.as_ref().unwrap();
            let r = c
                .ensure_blobs_present(&blobs, &ra.meta)
                .and_then(|_| c.update_action_result(&ra.action_digest, &ar, &ra.meta));
            match r {
                Ok(()) => {
                    self.stats.cache_uploads.fetch_add(1, Ordering::SeqCst);
                }
                Err(e) => self.warn(format!("Remote Cache: Upload failed: {e}")),
            }
        }
    }

    // ------------------------------------------------------------------- remote execution

    fn execute_remotely(&self, ra: &RemoteAction) -> Result<ExecuteResponse, RemoteError> {
        let c = self.client.as_ref().unwrap();
        let all = ra.all_blobs();
        c.ensure_blobs_present(&all, &ra.meta)?;
        let skip = !self.opts.accept_cached;
        match c.execute(&ra.action_digest, skip, &ra.meta, &mut |_| {}) {
            Err(RemoteError::Exec(st)) if is_missing_blobs_failure(&st) => {
                // The CAS lost some blobs after our check: force re-upload of everything.
                let digests: Vec<Digest> = all.keys().filter(|d| d.size_bytes > 0).cloned().collect();
                c.upload_blobs(&digests, &all, &ra.meta)?;
                c.execute(&ra.action_digest, skip, &ra.meta, &mut |_| {})
            }
            other => other,
        }
    }

    fn result_from_action_result(
        &self,
        r: &ActionResult,
        runner: &str,
        spawn: &Spawn,
        execroot: &Path,
        meta: &ActionMeta,
        download: bool,
        start: Instant,
    ) -> SpawnResult {
        match self.apply_result(r, spawn, execroot, meta, download) {
            Ok((stdout, stderr)) => SpawnResult {
                status: if r.exit_code == 0 { SpawnStatus::Success } else { SpawnStatus::NonZeroExit },
                exit_code: r.exit_code,
                stdout,
                stderr,
                wall_time_ms: start.elapsed().as_millis() as u64,
                runner_name: runner.to_string(),
                missing: vec![],
                usage: None,
                executed_command: None,
            },
            Err(e) => SpawnResult {
                status: SpawnStatus::ExecutionFailed,
                exit_code: 1,
                stdout: vec![],
                stderr: format!("Failed to apply remote result: {e}\n").into_bytes(),
                wall_time_ms: start.elapsed().as_millis() as u64,
                runner_name: runner.to_string(),
                missing: vec![],
                usage: None,
                executed_command: None,
            },
        }
    }

    fn failed(&self, msg: String, start: Instant, status: SpawnStatus) -> SpawnResult {
        SpawnResult {
            status,
            exit_code: 1,
            stdout: vec![],
            stderr: format!("{msg}\n").into_bytes(),
            wall_time_ms: start.elapsed().as_millis() as u64,
            runner_name: "remote".into(),
            missing: vec![],
            usage: None,
            executed_command: None,
        }
    }

    fn run_local(
        &self,
        spawn: &Spawn,
        execroot: &PathFragment,
        fs: &dyn FileSystem,
        ra: Option<&RemoteAction>,
        mode: &ExecMode,
    ) -> io::Result<SpawnResult> {
        if let Err(e) = self.materialize_inputs(spawn, Path::new(execroot.path_string())) {
            return Ok(self.failed(e, Instant::now(), SpawnStatus::ExecutionFailed));
        }
        self.stats.local_executions.fetch_add(1, Ordering::SeqCst);
        let res = match mode.fallback {
            Some(f) => f.exec(spawn, execroot, fs)?,
            None => self.fallback.exec(spawn, execroot, fs)?,
        };
        if res.status == SpawnStatus::Success && res.exit_code == 0 {
            // Locally produced outputs supersede lazy entries.
            {
                let mut lazy = self.lazy.lock().unwrap();
                for o in &spawn.outputs {
                    lazy.remove(o.path_string());
                }
            }
            if let Some(ra) = ra {
                self.upload_result(ra, spawn, Path::new(execroot.path_string()), &res);
            }
        }
        Ok(res)
    }
}

impl SpawnRunner for RemoteSpawnRunner {
    fn name(&self) -> &str {
        if self.exec_remotely_enabled() { "remote" } else { "remote-cache" }
    }

    fn can_exec(&self, spawn: &Spawn) -> bool {
        // The runner handles every spawn: it falls back to local execution where needed.
        let _ = spawn;
        true
    }

    fn exec(&self, spawn: &Spawn, execroot: &PathFragment, fs: &dyn FileSystem) -> io::Result<SpawnResult> {
        self.exec_ext(spawn, execroot, fs, &ExecMode::default())
    }
}

impl RemoteSpawnRunner {
    /// `exec` with an explicit [`ExecMode`].
    pub fn exec_ext(
        &self,
        spawn: &Spawn,
        execroot: &PathFragment,
        fs: &dyn FileSystem,
        mode: &ExecMode,
    ) -> io::Result<SpawnResult> {
        let start = Instant::now();
        let root = PathBuf::from(execroot.path_string());
        let caching = self.client.is_some() || self.disk.is_some();
        if !caching {
            return self.run_local(spawn, execroot, fs, None, mode);
        }
        let ra = match self.build_action(spawn, &root) {
            Ok(ra) => ra,
            Err(missing) => {
                // Let the local runner produce the canonical missing-inputs diagnostics.
                let _ = missing;
                return self.run_local(spawn, execroot, fs, None, mode);
            }
        };
        let download = self.should_download(spawn);
        match self.lookup_cache(&ra, spawn) {
            Some(hit) => {
                let (r, runner) = match hit {
                    CacheHit::Disk(r) => {
                        self.stats.disk_cache_hits.fetch_add(1, Ordering::SeqCst);
                        (r, "disk cache hit")
                    }
                    CacheHit::Remote(r) => {
                        self.stats.remote_cache_hits.fetch_add(1, Ordering::SeqCst);
                        (r, "remote cache hit")
                    }
                };
                if mode.hooks.is_cancelled() || !mode.hooks.lock(r.exit_code) {
                    return Err(zel_sandbox::cancelled_error());
                }
                let res = self.result_from_action_result(&r, runner, spawn, &root, &ra.meta, download, start);
                if res.status != SpawnStatus::ExecutionFailed {
                    return Ok(res);
                }
                // The cached entry was unusable (e.g. evicted blobs): treat as a miss.
                self.warn(format!("Cached result for {} could not be applied: {}", spawn.mnemonic, String::from_utf8_lossy(&res.stderr).trim()));
            }
            None => {}
        }
        if mode.allow_remote_exec && self.exec_remotely_enabled() && may_be_executed_remotely(spawn) {
            return match self.execute_remotely(&ra) {
                Ok(resp) => {
                    let r = resp.result.unwrap_or_default();
                    let cached = resp.cached_result;
                    if mode.hooks.is_cancelled() || !mode.hooks.lock(r.exit_code) {
                        return Err(zel_sandbox::cancelled_error());
                    }
                    if cached {
                        self.stats.remote_cache_hits.fetch_add(1, Ordering::SeqCst);
                    } else {
                        self.stats.remote_executions.fetch_add(1, Ordering::SeqCst);
                    }
                    let res = self.result_from_action_result(
                        &r,
                        if cached { "remote cache hit" } else { "remote" },
                        spawn,
                        &root,
                        &ra.meta,
                        download || r.exit_code != 0 && spawn.mnemonic == "TestRunner",
                        start,
                    );
                    // Populate the disk cache from successful remote results (CombinedCache).
                    if res.status == SpawnStatus::Success && download {
                        if let (Some(dc), true) = (&self.disk, may_be_cached(spawn)) {
                            let _ = dc.put_action_result(&ra.action_digest, &r);
                        }
                    }
                    Ok(res)
                }
                Err(e) => {
                    let timeout = matches!(&e, RemoteError::Exec(s) if s.code == code::DEADLINE_EXCEEDED);
                    if self.opts.local_fallback && !timeout && mode.allow_local {
                        self.stats.fallbacks.fetch_add(1, Ordering::SeqCst);
                        self.warn(format!("Remote execution of {} failed, falling back to local execution: {e}", spawn.mnemonic));
                        return self.run_local(spawn, execroot, fs, Some(&ra), mode);
                    }
                    if mode.hooks.is_cancelled() || !mode.hooks.lock(1) {
                        return Err(zel_sandbox::cancelled_error());
                    }
                    Ok(self.failed(
                        format!("Remote execution failed: {e}"),
                        start,
                        if timeout { SpawnStatus::Timeout } else { SpawnStatus::ExecutionFailed },
                    ))
                }
            };
        }
        self.run_local(spawn, execroot, fs, Some(&ra), mode)
    }
}

fn expand_dir(full: &Path, rel: &str, out: &mut Vec<MerkleInput>, lazy: &HashMap<String, RemoteOutput>) {
    let Ok(rd) = std::fs::read_dir(full) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    if entries.is_empty() {
        out.push(MerkleInput::empty_dir(rel));
    }
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let child_rel = format!("{rel}/{name}");
        let p = e.path();
        if lazy.contains_key(&child_rel) {
            continue;
        }
        match std::fs::metadata(&p) {
            Ok(md) if md.is_dir() => expand_dir(&p, &child_rel, out, lazy),
            Ok(md) => {
                #[cfg(unix)]
                let exec = {
                    use std::os::unix::fs::PermissionsExt;
                    md.permissions().mode() & 0o111 != 0
                };
                #[cfg(not(unix))]
                let exec = false;
                out.push(MerkleInput::file(&child_rel, p, exec));
            }
            Err(_) => {}
        }
    }
}

fn write_file(dest: &Path, data: &[u8], executable: bool) -> io::Result<()> {
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    let _ = std::fs::remove_file(dest);
    std::fs::write(dest, data)?;
    set_executable(dest, executable);
    Ok(())
}

fn make_symlink(link: &Path, target: &str) -> io::Result<()> {
    if let Some(p) = link.parent() {
        std::fs::create_dir_all(p)?;
    }
    let _ = std::fs::remove_file(link);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(not(unix))]
    {
        let _ = target;
        Err(io::Error::new(io::ErrorKind::Unsupported, "symlinks unsupported"))
    }
}
