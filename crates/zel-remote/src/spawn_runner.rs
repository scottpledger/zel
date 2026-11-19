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
//! through the disk cache, the remote action cache (gRPC REAPI or HTTP/1.1 REST) and/or a remote
//! executor, falling back to a local [`SpawnRunner`].

use crate::disk_cache::{set_executable, DiskCache};
use crate::http_cache::HttpCacheClient;
use crate::merkle::{
    flatten_tree, tree_from_local_dir_ext, BlobSource, InputKind, MerkleInput, MerkleTree,
    MerkleTreeComputer, TreeEntry,
};
use crate::options::{DownloadMode, RemoteOptions};
use crate::reapi_client::{is_missing_blobs_failure, ActionMeta, ReapiClient, RemoteError};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, UNIX_EPOCH};
use zel_proto::reapi::*;
use zel_sandbox::{Spawn, SpawnResult, SpawnStatus};
use zel_util::java_regex::Regex;
use zel_vfs::{FileSystem, PathFragment};

/// Bazel `ExitCode.REMOTE_ERROR`.
pub const REMOTE_ERROR_EXIT_CODE: i32 = 34;
/// POSIX `128 + SIGALRM` timeout exit code returned for timed-out spawns.
pub const TIMEOUT_EXIT_CODE: i32 = 142;
/// Maximum symlink dereference hops during BwoB output materialization (`32` in Bazel).
const MAX_SYMLINK_HOPS: usize = 32;

/// A strategy that can execute spawns (`SpawnRunner` in Bazel).
pub trait SpawnRunner: Send + Sync {
    fn name(&self) -> &str;
    fn can_exec(&self, spawn: &Spawn) -> bool;
    fn exec(
        &self,
        spawn: &Spawn,
        execroot: &PathFragment,
        fs: &dyn FileSystem,
    ) -> io::Result<SpawnResult>;
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
        Self {
            allow_remote_exec: true,
            fallback: None,
            allow_local: true,
            hooks: Default::default(),
        }
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
    fn exec(
        &self,
        spawn: &Spawn,
        execroot: &PathFragment,
        fs: &dyn FileSystem,
    ) -> io::Result<SpawnResult> {
        zel_sandbox::execute_spawn_local(spawn, execroot, fs)
    }
}

// ---- `Spawns.*` execution-requirement predicates -------------------------------------------

fn has(spawn: &Spawn, key: &str) -> bool {
    spawn.execution_info.contains_key(key)
}

/// `Spawns.mayBeCached`.
pub fn may_be_cached(spawn: &Spawn) -> bool {
    !has(spawn, "no-cache") && !has(spawn, "local")
}
/// `Spawns.mayBeCachedRemotely` (`!no-cache && !no-remote-cache && !no-remote && !local`).
pub fn may_be_cached_remotely(spawn: &Spawn) -> bool {
    may_be_cached(spawn)
        && !has(spawn, "no-remote-cache")
        && !has(spawn, "no-remote")
        && !has(spawn, "local")
}
/// `Spawns.mayBeExecutedRemotely`.
pub fn may_be_executed_remotely(spawn: &Spawn) -> bool {
    !has(spawn, "no-remote") && !has(spawn, "no-remote-exec") && !has(spawn, "local")
}

const TREE_OUTPUT_PREFIX: &str = "zel-tree-output:";

/// Marks `path` (relative to the exec root) as a directory (tree artifact) output of `spawn`.
pub fn mark_tree_output(spawn: &mut Spawn, path: &str) {
    spawn
        .execution_info
        .insert(format!("{TREE_OUTPUT_PREFIX}{path}"), "1".into());
}

fn tree_outputs(spawn: &Spawn) -> Vec<String> {
    let mut out: Vec<String> = spawn
        .execution_info
        .keys()
        .filter_map(|k| k.strip_prefix(TREE_OUTPUT_PREFIX).map(str::to_string))
        .collect();
    for d in &spawn.output_dirs {
        let s = d.path_string().to_string();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

// ---- `--experimental_remote_scrubbing_config` (`scrubber/ConfigParser.java`) ----------------

#[derive(Debug)]
struct CompiledArgReplacement {
    source: Regex,
    target: String,
}

#[derive(Debug)]
struct CompiledScrubRule {
    kind: Option<Regex>,
    label: Option<Regex>,
    mnemonic: Option<Regex>,
    match_tools: bool,
    omitted_inputs: Vec<Regex>,
    arg_replacements: Vec<CompiledArgReplacement>,
    salt: String,
}

#[derive(Debug, Default)]
struct Scrubber {
    rules: Vec<CompiledScrubRule>,
}

impl Scrubber {
    fn from_path(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            format!(
                "Failed to read --experimental_remote_scrubbing_config {}: {e}",
                path.display()
            )
        })?;
        Self::parse_textproto(&text).map_err(|e| {
            format!(
                "Failed to parse --experimental_remote_scrubbing_config {}: {e}",
                path.display()
            )
        })
    }

    fn parse_textproto(input: &str) -> Result<Self, String> {
        let mut p = TextProtoScanner::new(input);
        let mut rules = Vec::new();
        while let Some(ident) = p.next_ident()? {
            if ident != "rules" {
                return Err(format!("unknown top-level field `{ident}`"));
            }
            p.skip_optional_colon();
            let open = p.expect_block_open()?;
            rules.push(parse_scrub_rule(&mut p, close_delim(open))?);
        }
        Ok(Self { rules })
    }

    /// Evaluates rules in reverse order and returns the last matching rule.
    fn match_spawn(&self, spawn: &Spawn) -> Option<&CompiledScrubRule> {
        let target_kind = spawn
            .execution_info
            .get("target-kind")
            .map(String::as_str)
            .unwrap_or("");
        let for_tool = spawn
            .execution_info
            .get("for-tool")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        self.rules.iter().rev().find(|r| {
            if for_tool && !r.match_tools {
                return false;
            }
            if let Some(re) = &r.kind {
                if !re.is_match(target_kind) {
                    return false;
                }
            }
            if let Some(re) = &r.label {
                if !re.is_match(&spawn.owner_label) {
                    return false;
                }
            }
            if let Some(re) = &r.mnemonic {
                if !re.is_match(&spawn.mnemonic) {
                    return false;
                }
            }
            true
        })
    }
}

impl CompiledScrubRule {
    fn omits_input(&self, path: &str) -> bool {
        self.omitted_inputs.iter().any(|re| re.is_match(path))
    }

    fn scrub_arg(&self, arg: &str) -> String {
        let mut cur = arg.to_string();
        for rep in &self.arg_replacements {
            cur = rep.source.replace_all(&cur, &rep.target);
        }
        cur
    }
}

fn compile_full_match_regex(pat: &str) -> Result<Regex, String> {
    Regex::new(&format!("(?s)^(?:{pat})$")).map_err(|e| format!("invalid regex `{pat}`: {e}"))
}

fn close_delim(open: u8) -> u8 {
    if open == b'<' {
        b'>'
    } else {
        b'}'
    }
}

fn parse_scrub_rule(p: &mut TextProtoScanner<'_>, end: u8) -> Result<CompiledScrubRule, String> {
    let mut kind = None;
    let mut label = None;
    let mut mnemonic = None;
    let mut match_tools = false;
    let mut omitted_inputs = Vec::new();
    let mut arg_replacements = Vec::new();
    let mut salt = String::new();

    while !p.consume_char(end) {
        let field = p
            .next_ident()?
            .ok_or_else(|| "unexpected EOF in `rules` block".to_string())?;
        p.skip_optional_colon();
        match field.as_str() {
            "matcher" => {
                let open = p.expect_block_open()?;
                let c = close_delim(open);
                while !p.consume_char(c) {
                    let mf = p
                        .next_ident()?
                        .ok_or_else(|| "unexpected EOF in `matcher` block".to_string())?;
                    p.skip_optional_colon();
                    match mf.as_str() {
                        "kind" => {
                            let s = p.expect_string()?;
                            if !s.is_empty() {
                                kind = Some(compile_full_match_regex(&s)?);
                            }
                        }
                        "label" => {
                            let s = p.expect_string()?;
                            if !s.is_empty() {
                                label = Some(compile_full_match_regex(&s)?);
                            }
                        }
                        "mnemonic" => {
                            let s = p.expect_string()?;
                            if !s.is_empty() {
                                mnemonic = Some(compile_full_match_regex(&s)?);
                            }
                        }
                        "match_tools" => {
                            match_tools = p.expect_bool()?;
                        }
                        other => return Err(format!("unknown matcher field `{other}`")),
                    }
                }
            }
            "transform" => {
                let open = p.expect_block_open()?;
                let c = close_delim(open);
                while !p.consume_char(c) {
                    let tf = p
                        .next_ident()?
                        .ok_or_else(|| "unexpected EOF in `transform` block".to_string())?;
                    p.skip_optional_colon();
                    match tf.as_str() {
                        "omitted_inputs" => {
                            if p.consume_char(b'[') {
                                while !p.consume_char(b']') {
                                    let s = p.expect_string()?;
                                    omitted_inputs.push(compile_full_match_regex(&s)?);
                                    let _ = p.consume_char(b',');
                                }
                            } else {
                                let s = p.expect_string()?;
                                omitted_inputs.push(compile_full_match_regex(&s)?);
                            }
                        }
                        "arg_replacements" => {
                            let ro = p.expect_block_open()?;
                            let rc = close_delim(ro);
                            let mut source = String::new();
                            let mut target = String::new();
                            while !p.consume_char(rc) {
                                let rf = p.next_ident()?.ok_or_else(|| {
                                    "unexpected EOF in `arg_replacements`".to_string()
                                })?;
                                p.skip_optional_colon();
                                match rf.as_str() {
                                    "source" => source = p.expect_string()?,
                                    "target" => target = p.expect_string()?,
                                    other => {
                                        return Err(format!(
                                            "unknown arg_replacements field `{other}`"
                                        ))
                                    }
                                }
                            }
                            let re = Regex::new(&source)
                                .map_err(|e| format!("invalid regex `{source}`: {e}"))?;
                            arg_replacements.push(CompiledArgReplacement { source: re, target });
                        }
                        "salt" => {
                            salt = p.expect_string()?;
                        }
                        other => return Err(format!("unknown transform field `{other}`")),
                    }
                }
            }
            other => return Err(format!("unknown rule field `{other}`")),
        }
    }

    Ok(CompiledScrubRule {
        kind,
        label,
        mnemonic,
        match_tools,
        omitted_inputs,
        arg_replacements,
        salt,
    })
}

struct TextProtoScanner<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> TextProtoScanner<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            b: s.as_bytes(),
            i: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if c.is_ascii_whitespace() || c == b',' || c == b';' {
                self.i += 1;
            } else if c == b'#' {
                while self.i < self.b.len() && self.b[self.i] != b'\n' {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
    }

    fn consume_char(&mut self, ch: u8) -> bool {
        self.skip_ws();
        if self.i < self.b.len() && self.b[self.i] == ch {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn skip_optional_colon(&mut self) {
        let _ = self.consume_char(b':');
    }

    fn expect_block_open(&mut self) -> Result<u8, String> {
        self.skip_ws();
        if self.i < self.b.len() && (self.b[self.i] == b'{' || self.b[self.i] == b'<') {
            let ch = self.b[self.i];
            self.i += 1;
            Ok(ch)
        } else {
            Err("expected `{` or `<`".into())
        }
    }

    fn next_ident(&mut self) -> Result<Option<String>, String> {
        self.skip_ws();
        if self.i >= self.b.len() {
            return Ok(None);
        }
        let start = self.i;
        while self.i < self.b.len()
            && (self.b[self.i].is_ascii_alphanumeric() || self.b[self.i] == b'_')
        {
            self.i += 1;
        }
        if self.i == start {
            return Err(format!("unexpected character `{}`", self.b[self.i] as char));
        }
        Ok(Some(
            std::str::from_utf8(&self.b[start..self.i])
                .unwrap()
                .to_string(),
        ))
    }

    fn expect_bool(&mut self) -> Result<bool, String> {
        match self.next_ident()?.as_deref() {
            Some("true" | "True" | "1") => Ok(true),
            Some("false" | "False" | "0") => Ok(false),
            other => Err(format!("expected boolean, got {other:?}")),
        }
    }

    fn expect_string(&mut self) -> Result<String, String> {
        self.skip_ws();
        let mut out = Vec::new();
        let mut saw_any = false;
        while self.i < self.b.len() && (self.b[self.i] == b'"' || self.b[self.i] == b'\'') {
            saw_any = true;
            let quote = self.b[self.i];
            self.i += 1;
            while self.i < self.b.len() && self.b[self.i] != quote {
                if self.b[self.i] == b'\\' {
                    self.i += 1;
                    if self.i >= self.b.len() {
                        return Err("unterminated escape in string literal".into());
                    }
                    match self.b[self.i] {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'\\' => out.push(b'\\'),
                        b'"' => out.push(b'"'),
                        b'\'' => out.push(b'\''),
                        other => {
                            out.push(b'\\');
                            out.push(other);
                        }
                    }
                    self.i += 1;
                } else {
                    out.push(self.b[self.i]);
                    self.i += 1;
                }
            }
            if self.i >= self.b.len() {
                return Err("unterminated string literal".into());
            }
            self.i += 1;
            self.skip_ws();
        }
        if !saw_any {
            return Err("expected quoted string literal".into());
        }
        String::from_utf8(out).map_err(|e| format!("invalid UTF-8 in string literal: {e}"))
    }
}

// ---------------------------------------------------------------------------

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
            (
                self.disk_cache_hits.load(Ordering::SeqCst),
                "disk cache hit",
            ),
            (
                self.remote_cache_hits.load(Ordering::SeqCst),
                "remote cache hit",
            ),
            (self.remote_executions.load(Ordering::SeqCst), "remote"),
            (self.local_executions.load(Ordering::SeqCst), "local"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {label}"));
            }
        }
        let total: usize = [
            &self.disk_cache_hits,
            &self.remote_cache_hits,
            &self.remote_executions,
            &self.local_executions,
        ]
        .iter()
        .map(|c| c.load(Ordering::SeqCst))
        .sum();
        format!(
            "{total} process{}: {}",
            if total == 1 { "" } else { "es" },
            parts.join(", ")
        )
    }
}

#[derive(Debug, Clone)]
struct RemoteOutput {
    digest: Digest,
    executable: bool,
    expires_at: Instant,
}

struct RemoteAction {
    command_bytes: Vec<u8>,
    action_bytes: Vec<u8>,
    action_digest: Digest,
    command_digest: Digest,
    tree: MerkleTree,
    meta: ActionMeta,
    scrubbed: bool,
}

impl RemoteAction {
    /// All positive-size blobs the executor/cache needs: input tree + command + action.
    fn all_blobs(&self) -> BTreeMap<Digest, BlobSource> {
        let mut b = self.tree.blobs.clone();
        if self.command_digest.size_bytes > 0 {
            b.insert(
                self.command_digest.clone(),
                BlobSource::Bytes(self.command_bytes.clone()),
            );
        }
        if self.action_digest.size_bytes > 0 {
            b.insert(
                self.action_digest.clone(),
                BlobSource::Bytes(self.action_bytes.clone()),
            );
        }
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
    http_cache: Option<Arc<HttpCacheClient>>,
    disk: Option<DiskCache>,
    merkle: MerkleTreeComputer,
    scrubber: Scrubber,
    download_regexes: Vec<Regex>,
    fallback: Arc<dyn SpawnRunner>,
    lazy: Mutex<HashMap<String, RemoteOutput>>,
    toplevel_paths: Mutex<HashSet<String>>,
    skip_download: Mutex<HashSet<String>>,
    command_mode: Mutex<Option<String>>,
    warnings: Mutex<Vec<String>>,
    pub stats: RemoteStats,
}

impl RemoteSpawnRunner {
    /// Builds the runner from parsed options (`RemoteModule.beforeCommand` equivalent):
    /// opens the disk cache, connects the REAPI or HTTP REST cache client, and checks server
    /// capabilities.
    pub fn from_options(
        opts: RemoteOptions,
        invocation_id: &str,
        fallback: Arc<dyn SpawnRunner>,
    ) -> Result<Self, String> {
        let http_cache = opts.build_http_cache()?;
        let client = if http_cache.is_some() {
            None
        } else {
            opts.build_client(invocation_id)?.map(Arc::new)
        };
        Self::with_backends(opts, client, http_cache, fallback)
    }

    pub fn new(
        opts: RemoteOptions,
        client: Option<Arc<ReapiClient>>,
        fallback: Arc<dyn SpawnRunner>,
    ) -> Result<Self, String> {
        Self::with_backends(opts, client, None, fallback)
    }

    pub fn with_http_cache(
        opts: RemoteOptions,
        http_cache: Option<Arc<HttpCacheClient>>,
        fallback: Arc<dyn SpawnRunner>,
    ) -> Result<Self, String> {
        Self::with_backends(opts, None, http_cache, fallback)
    }

    fn with_backends(
        opts: RemoteOptions,
        client: Option<Arc<ReapiClient>>,
        http_cache: Option<Arc<HttpCacheClient>>,
        fallback: Arc<dyn SpawnRunner>,
    ) -> Result<Self, String> {
        let disk = match &opts.disk_cache {
            Some(p) => Some(
                DiskCache::open(Path::new(p), opts.digest_fn, true)
                    .map_err(|e| format!("Failed to open disk cache {p}: {e}"))?,
            ),
            None => None,
        };
        let scrubber = match &opts.scrubbing_config {
            Some(p) => Scrubber::from_path(Path::new(p))?,
            None => Scrubber::default(),
        };
        let mut download_regexes = Vec::with_capacity(opts.download_regex.len());
        for pat in &opts.download_regex {
            let re = Regex::new(&format!("(?s)^(?:{pat})$"))
                .map_err(|e| format!("Invalid --remote_download_regex '{pat}': {e}"))?;
            download_regexes.push(re);
        }
        let mut warnings = Vec::new();
        if let Some(c) = &client {
            let need_exec = c.executor.is_some();
            c.check_compatibility(need_exec)
                .map_err(|e| e.to_string())?;
            warnings.extend(c.take_warnings());
        }
        if let Some(hc) = &http_cache {
            warnings.extend(hc.take_warnings());
        }
        Ok(Self {
            merkle: MerkleTreeComputer::new(opts.digest_fn),
            opts,
            client,
            http_cache,
            disk,
            scrubber,
            download_regexes,
            fallback,
            lazy: Mutex::new(HashMap::new()),
            toplevel_paths: Mutex::new(HashSet::new()),
            skip_download: Mutex::new(HashSet::new()),
            command_mode: Mutex::new(None),
            warnings: Mutex::new(warnings),
            stats: RemoteStats::default(),
        })
    }

    pub fn client(&self) -> Option<&Arc<ReapiClient>> {
        self.client.as_ref()
    }

    pub fn close(&self) {
        if let Some(c) = &self.client {
            c.close();
        }
    }

    pub fn take_warnings(&self) -> Vec<String> {
        let mut out = std::mem::take(&mut *self.warnings.lock().unwrap());
        if let Some(c) = &self.client {
            out.extend(c.take_warnings());
        }
        if let Some(hc) = &self.http_cache {
            out.extend(hc.take_warnings());
        }
        out
    }

    pub fn warn(&self, w: String) {
        self.warnings.lock().unwrap().push(w);
    }

    pub fn set_command_mode(&self, cmd: &str) {
        *self.command_mode.lock().unwrap() = Some(cmd.to_string());
    }

    pub fn mark_toplevel_outputs(&self, paths: impl IntoIterator<Item = String>) {
        self.toplevel_paths.lock().unwrap().extend(paths);
    }

    pub fn skip_download_outputs(&self, paths: impl IntoIterator<Item = String>) {
        self.skip_download.lock().unwrap().extend(paths);
    }

    pub fn is_scrubbed(&self, spawn: &Spawn) -> bool {
        self.scrubber.match_spawn(spawn).is_some()
    }

    fn exec_remotely_enabled(&self) -> bool {
        self.client
            .as_ref()
            .map(|c| c.executor.is_some())
            .unwrap_or(false)
    }

    fn has_remote_cache(&self) -> bool {
        self.client.is_some() || self.http_cache.is_some()
    }

    /// A disk cache or remote cache is configured (`RemoteSpawnCache` has something to consult).
    pub fn has_cache(&self) -> bool {
        self.has_remote_cache() || self.disk.is_some()
    }

    /// Whether the exec-root-relative `path` (or a child under `path/` for a tree artifact) is
    /// known by remote metadata (`RemoteActionFileSystem.exists`), independent of TTL.
    pub fn is_pending_output(&self, path: &str) -> bool {
        let lazy = self.lazy.lock().unwrap();
        if lazy.contains_key(path) {
            return true;
        }
        let prefix = format!("{path}/");
        lazy.keys().any(|k| k.starts_with(&prefix))
    }

    /// Returns `(digest_hash, ttl_valid)` for an unmaterialized regular-file output at `path`
    /// (`RemoteActionFileSystem.statIfFound` + `RemoteOutputChecker.shouldTrustMetadata`).
    pub fn remote_file_entry(&self, path: &str) -> Option<(String, bool)> {
        let now = Instant::now();
        self.lazy
            .lock()
            .unwrap()
            .get(path)
            .map(|o| (o.digest.hash.clone(), now < o.expires_at))
    }

    /// Returns `(tree_relative_path, digest_hash, ttl_valid)` for all unmaterialized remote files
    /// nested under directory `path/` (`RemoteActionFileSystem.getDirectoryContents`).
    pub fn remote_tree_entries(&self, path: &str) -> Vec<(String, String, bool)> {
        let now = Instant::now();
        let prefix = format!("{path}/");
        self.lazy
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(k, o)| {
                k.strip_prefix(&prefix)
                    .map(|rel| (rel.to_string(), o.digest.hash.clone(), now < o.expires_at))
            })
            .collect()
    }

    /// Number of outputs currently known only by digest (not downloaded).
    pub fn pending_remote_outputs(&self) -> usize {
        self.lazy.lock().unwrap().len()
    }

    // ------------------------------------------------------------------- remote backend dispatch

    fn remote_supports_update(&self) -> bool {
        if let Some(c) = &self.client {
            c.remote_cache_supports_update()
        } else {
            self.http_cache.is_some()
        }
    }

    fn allows_absolute_symlinks(&self) -> bool {
        self.client
            .as_ref()
            .map(|c| c.allows_absolute_symlinks())
            .unwrap_or(false)
    }

    fn remote_get_action_result(
        &self,
        action_digest: &Digest,
        meta: &ActionMeta,
    ) -> Result<Option<ActionResult>, RemoteError> {
        if let Some(c) = &self.client {
            c.get_action_result(action_digest, meta)
        } else if let Some(hc) = &self.http_cache {
            hc.get_action_result(action_digest)
        } else {
            Ok(None)
        }
    }

    fn remote_download_blob(&self, d: &Digest, meta: &ActionMeta) -> Result<Vec<u8>, RemoteError> {
        if let Some(c) = &self.client {
            c.download_blob(d, meta)
        } else if let Some(hc) = &self.http_cache {
            hc.download_blob(d)
        } else {
            Err(RemoteError::NotFound)
        }
    }

    fn remote_upload_action_result(
        &self,
        action_digest: &Digest,
        ar: &ActionResult,
        blobs: &BTreeMap<Digest, BlobSource>,
        meta: &ActionMeta,
    ) -> Result<(), RemoteError> {
        if let Some(c) = &self.client {
            c.ensure_blobs_present(blobs, meta)?;
            c.update_action_result(action_digest, ar, meta)
        } else if let Some(hc) = &self.http_cache {
            for (d, src) in blobs {
                if d.size_bytes > 0 {
                    let bytes = src.read()?;
                    hc.upload_blob(d, &bytes)?;
                }
            }
            hc.update_action_result(action_digest, ar)
        } else {
            Ok(())
        }
    }

    // ------------------------------------------------------------------- action building

    fn collect_inputs(
        &self,
        spawn: &Spawn,
        execroot: &Path,
        scrub_rule: Option<&CompiledScrubRule>,
    ) -> Result<Vec<MerkleInput>, Vec<String>> {
        let lazy = self.lazy.lock().unwrap();
        let mut out = Vec::new();
        let mut missing = Vec::new();
        let mut covered_roots: HashSet<String> = HashSet::new();
        for inp in &spawn.inputs {
            let rel = inp.path_string().to_string();
            if scrub_rule.is_some_and(|r| r.omits_input(&rel)) {
                continue;
            }
            covered_roots.insert(rel.clone());
            if let Some(r) = lazy.get(&rel) {
                out.push(MerkleInput {
                    path: rel,
                    kind: InputKind::Remote {
                        digest: r.digest.clone(),
                        executable: r.executable,
                    },
                    node_properties: None,
                });
                continue;
            }
            // Lazy outputs nested under a directory input.
            let prefix = format!("{rel}/");
            let mut had_nested = false;
            for (k, r) in lazy.iter() {
                if k.starts_with(&prefix) {
                    if scrub_rule.is_some_and(|sr| sr.omits_input(k)) {
                        continue;
                    }
                    had_nested = true;
                    out.push(MerkleInput {
                        path: k.clone(),
                        kind: InputKind::Remote {
                            digest: r.digest.clone(),
                            executable: r.executable,
                        },
                        node_properties: None,
                    });
                }
            }
            let full = execroot.join(&rel);
            if spawn.symlink_inputs.iter().any(|s| s.path_string() == rel) {
                match std::fs::read_link(&full) {
                    Ok(target) => {
                        out.push(MerkleInput::symlink(&rel, &target.to_string_lossy()));
                        continue;
                    }
                    Err(_) => {
                        missing.push(rel);
                        continue;
                    }
                }
            }
            match std::fs::metadata(&full) {
                Ok(md) if md.is_dir() => expand_dir(&full, &rel, &mut out, &lazy, scrub_rule),
                Ok(md) => {
                    #[cfg(unix)]
                    let exec = {
                        use std::os::unix::fs::PermissionsExt;
                        md.permissions().mode() & 0o111 != 0
                    };
                    #[cfg(not(unix))]
                    let exec = {
                        let _ = md;
                        false
                    };
                    out.push(MerkleInput::file(&rel, full, exec));
                }
                Err(_) if had_nested => {}
                Err(_) => missing.push(rel),
            }
        }
        // Declared output directories (tree artifacts) are staged as empty directories in the
        // input Merkle tree (`MerkleTreeComputer.buildForSpawn`).
        for tree_out in tree_outputs(spawn) {
            if !covered_roots.contains(&tree_out)
                && !out
                    .iter()
                    .any(|i| i.path == tree_out || i.path.starts_with(&format!("{tree_out}/")))
            {
                out.push(MerkleInput::empty_dir(&tree_out));
            }
        }
        if missing.is_empty() {
            Ok(out)
        } else {
            Err(missing)
        }
    }

    fn build_action(&self, spawn: &Spawn, execroot: &Path) -> Result<RemoteAction, Vec<String>> {
        let scrub_rule = self.scrubber.match_spawn(spawn);
        let inputs = self.collect_inputs(spawn, execroot, scrub_rule)?;
        let tree = self
            .merkle
            .build(&inputs)
            .map_err(|e| vec![e.to_string()])?;
        let trees = tree_outputs(spawn);
        let out_dir_set: std::collections::BTreeSet<String> = spawn
            .output_dirs
            .iter()
            .map(|d| d.path_string().to_string())
            .chain(trees)
            .collect();
        let mut files: Vec<String> = Vec::new();
        let mut dirs: Vec<String> = Vec::new();
        for o in &spawn.outputs {
            let p = o.path_string().to_string();
            if out_dir_set.contains(&p) {
                dirs.push(p)
            } else {
                files.push(p)
            }
        }
        for d in out_dir_set {
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
        files.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        dirs.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        let use_paths = self
            .client
            .as_ref()
            .map(|c| c.supports_output_paths())
            .unwrap_or(true);
        let arguments: Vec<String> = match scrub_rule {
            Some(sr) => spawn.argv.iter().map(|a| sr.scrub_arg(a)).collect(),
            None => spawn.argv.clone(),
        };
        let mut env_vars: Vec<EnvironmentVariable> = spawn
            .env
            .iter()
            .map(|(k, v)| EnvironmentVariable {
                name: k.clone(),
                value: v.clone(),
            })
            .collect();
        env_vars.sort_by(|a, b| a.name.encode_utf16().cmp(b.name.encode_utf16()));
        let working_directory = spawn
            .execution_info
            .get("working-directory")
            .cloned()
            .unwrap_or_default();
        let mut command = Command {
            arguments,
            environment_variables: env_vars,
            working_directory,
            ..Default::default()
        };
        if use_paths {
            let mut all = files.clone();
            all.extend(dirs.clone());
            all.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
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
        let platform = if !props.is_empty() {
            let mut properties: Vec<PlatformProperty> = props
                .into_iter()
                .map(|(name, value)| PlatformProperty { name, value })
                .collect();
            // Sort by UTF-16 code units (Java String.compareTo).
            properties.sort_by(|a, b| a.name.encode_utf16().cmp(b.name.encode_utf16()));
            Some(Platform { properties })
        } else {
            None
        };
        command.platform = platform.clone();
        let command_bytes = command.encode_to_vec();
        let command_digest = self.opts.digest_fn.digest(&command_bytes);
        let salt = CacheSalt {
            may_be_executed_remotely: may_be_executed_remotely(spawn),
            workspace: spawn
                .execution_info
                .get("diff-workspace-cache")
                .cloned()
                .unwrap_or_default(),
            scrub_salt: scrub_rule.map(|sr| ScrubSalt {
                salt: sr.salt.clone(),
            }),
        }
        .encode_to_vec();
        // Bazel sets whole-second Duration (`Duration.fromSeconds(timeout.toSeconds())`), omitting
        // timeout when `<= 0`.
        let timeout = spawn
            .timeout_secs()
            .map(|s| s.floor() as i64)
            .filter(|&secs| secs > 0)
            .map(|seconds| Duration { seconds, nanos: 0 });
        let action = Action {
            command_digest: Some(command_digest.clone()),
            input_root_digest: Some(tree.root_digest.clone()),
            timeout,
            do_not_cache: !may_be_cached_remotely(spawn),
            salt,
            platform,
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
            scrubbed: scrub_rule.is_some(),
        })
    }

    // ------------------------------------------------------------------- cache reads

    fn read_policy(&self, spawn: &Spawn) -> (bool, bool) {
        let remote =
            self.has_remote_cache() && self.opts.accept_cached && may_be_cached_remotely(spawn);
        let disk = self.disk.is_some() && may_be_cached(spawn);
        (remote, disk)
    }

    fn write_policy(&self, spawn: &Spawn) -> (bool, bool) {
        let remote = self.has_remote_cache()
            && self.opts.upload_local_results
            && may_be_cached_remotely(spawn)
            && !has(spawn, "no-remote-cache-upload")
            && self.remote_supports_update();
        let disk = self.disk.is_some() && may_be_cached(spawn);
        (remote, disk)
    }

    fn lookup_cache(
        &self,
        ra: &RemoteAction,
        spawn: &Spawn,
    ) -> Result<Option<CacheHit>, RemoteError> {
        let (remote, disk) = self.read_policy(spawn);
        if disk {
            if let Some(dc) = &self.disk {
                match dc.try_get_action_result(&ra.action_digest) {
                    Ok(Some(r)) => return Ok(Some(CacheHit::Disk(r))),
                    Ok(None) => {}
                    Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                        // Corrupted disk AC entry was evicted; continue to remote cache.
                    }
                    Err(e) => return Err(RemoteError::Io(e)),
                }
            }
        }
        if remote {
            if let Some(r) = self.remote_get_action_result(&ra.action_digest, &ra.meta)? {
                return Ok(Some(CacheHit::Remote(r)));
            }
        }
        Ok(None)
    }

    /// Fetches a blob from the disk cache first, trying the remote cache only on disk cache
    /// `NotFound`, and staging downloaded remote blobs into the disk cache (`CombinedCache`).
    fn fetch_blob(&self, d: &Digest, meta: &ActionMeta) -> Result<Vec<u8>, RemoteError> {
        if d.size_bytes == 0 {
            return Ok(Vec::new());
        }
        if let Some(disk) = &self.disk {
            match disk.get_blob_strict(d) {
                Ok(Some(b)) => return Ok(b),
                Ok(None) => {}
                Err(e) => return Err(RemoteError::Io(e)),
            }
        }
        if self.has_remote_cache() {
            let b = self.remote_download_blob(d, meta)?;
            self.stats
                .bytes_downloaded
                .fetch_add(b.len(), Ordering::SeqCst);
            if let Some(disk) = &self.disk {
                disk.put_blob(d, &b).map_err(RemoteError::Io)?;
            }
            return Ok(b);
        }
        Err(RemoteError::NotFound)
    }

    // ------------------------------------------------------------------- output handling

    fn matches_download_regex(&self, path: &str, tree_root: Option<&str>) -> bool {
        if self.download_regexes.is_empty() {
            return false;
        }
        if self.download_regexes.iter().any(|re| re.is_match(path)) {
            return true;
        }
        if let Some(root) = tree_root {
            if self.download_regexes.iter().any(|re| re.is_match(root)) {
                return true;
            }
        }
        false
    }

    fn should_download_output(
        &self,
        spawn: &Spawn,
        path: &str,
        tree_root: Option<&str>,
        exit_code: i32,
        cached: bool,
    ) -> bool {
        // A non-cached failed remote action downloads every output (`RemoteExecutionService`).
        if exit_code != 0 && !cached {
            return true;
        }
        if self.skip_download.lock().unwrap().contains(path) {
            return false;
        }
        match self.opts.download_mode {
            DownloadMode::All => true,
            DownloadMode::TopLevel => {
                let cmd = self.command_mode.lock().unwrap().clone();
                let is_test_cmd = matches!(cmd.as_deref(), None | Some("test") | Some("coverage"));
                if spawn.mnemonic == "TestRunner" && is_test_cmd {
                    return true;
                }
                if self.toplevel_paths.lock().unwrap().contains(path) {
                    return true;
                }
                if let Some(root) = tree_root {
                    if self.toplevel_paths.lock().unwrap().contains(root) {
                        return true;
                    }
                }
                self.matches_download_regex(path, tree_root)
            }
            DownloadMode::Minimal => {
                let cmd = self.command_mode.lock().unwrap().clone();
                if spawn.mnemonic == "TestRunner"
                    && cmd.as_deref() == Some("coverage")
                    && (path == "coverage.dat" || path.ends_with("/coverage.dat"))
                {
                    return true;
                }
                self.matches_download_regex(path, tree_root)
            }
        }
    }

    fn validate_mandatory_outputs(r: &ActionResult, spawn: &Spawn) -> Result<(), String> {
        if r.exit_code != 0 {
            return Ok(());
        }
        let have: HashSet<&str> = r
            .output_files
            .iter()
            .map(|f| f.path.as_str())
            .chain(r.output_directories.iter().map(|d| d.path.as_str()))
            .chain(
                r.output_symlinks
                    .iter()
                    .chain(&r.output_file_symlinks)
                    .chain(&r.output_directory_symlinks)
                    .map(|s| s.path.as_str()),
            )
            .collect();
        let optional: HashSet<&str> = spawn
            .optional_outputs
            .iter()
            .map(|p| p.path_string())
            .collect();
        for o in &spawn.outputs {
            let p = o.path_string();
            if !optional.contains(p) && !have.contains(p) {
                return Err(format!("Output {p} was not created by the remote action"));
            }
        }
        Ok(())
    }
}

struct StagedTreeOutput {
    rel_dir: String,
    subdirs: Vec<String>,
    files: Vec<(String, Vec<u8>, bool)>,
    symlinks: Vec<(String, String)>,
}

struct StagedResult {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    all_downloaded: bool,
    files: Vec<(String, Vec<u8>, bool)>,
    unmaterialized_files: Vec<String>,
    trees: Vec<StagedTreeOutput>,
    symlinks: BTreeMap<String, String>,
    lazy_updates: Vec<(String, RemoteOutput)>,
}

impl RemoteSpawnRunner {
    /// Downloads and validates all required blobs for an `ActionResult` without modifying
    /// `execroot` or `self.lazy` yet (so dynamic-execution output locking or mid-download CAS
    /// `NOT_FOUND` leaves `execroot` untouched).
    fn stage_result(
        &self,
        r: &ActionResult,
        spawn: &Spawn,
        meta: &ActionMeta,
        cached: bool,
    ) -> Result<StagedResult, RemoteError> {
        Self::validate_mandatory_outputs(r, spawn).map_err(RemoteError::Other)?;

        let stdout = self.inline_or_fetch(&r.stdout_raw, &r.stdout_digest, meta)?;
        let stderr = self.inline_or_fetch(&r.stderr_raw, &r.stderr_digest, meta)?;

        // Collect and deduplicate symlinks first so conflicting symlink paths fail before writes.
        let mut symlinks_to_plant: BTreeMap<String, String> = BTreeMap::new();
        for s in r
            .output_symlinks
            .iter()
            .chain(&r.output_file_symlinks)
            .chain(&r.output_directory_symlinks)
        {
            if let Some(prev) = symlinks_to_plant.get(&s.path) {
                if prev != &s.target {
                    return Err(RemoteError::Other(format!(
                        "Conflicting output symlinks for {}: '{}' vs '{}'",
                        s.path, prev, s.target
                    )));
                }
            } else {
                symlinks_to_plant.insert(s.path.clone(), s.target.clone());
            }
        }

        let expires_at = Instant::now() + self.opts.remote_cache_ttl;
        let mut lazy_updates: Vec<(String, RemoteOutput)> = Vec::new();
        let mut all_downloaded = true;
        let mut files: Vec<(String, Vec<u8>, bool)> = Vec::new();
        let mut unmaterialized_files: Vec<String> = Vec::new();
        let mut trees: Vec<StagedTreeOutput> = Vec::new();

        let inline_outputs: HashSet<&str> = spawn
            .execution_info
            .get("internal-inline-outputs")
            .map(|s| s.split(',').filter(|p| !p.is_empty()).collect())
            .unwrap_or_default();

        for f in &r.output_files {
            let digest = f.digest.clone().unwrap_or_default();
            let dl = inline_outputs.contains(f.path.as_str())
                || self.should_download_output(spawn, &f.path, None, r.exit_code, cached);
            if !f.contents.is_empty() {
                files.push((f.path.clone(), f.contents.clone(), f.is_executable));
                continue;
            }
            if dl || digest.size_bytes == 0 {
                let b = if digest.size_bytes == 0 {
                    Vec::new()
                } else {
                    self.fetch_blob(&digest, meta)?
                };
                files.push((f.path.clone(), b, f.is_executable));
            } else {
                all_downloaded = false;
                unmaterialized_files.push(f.path.clone());
                lazy_updates.push((
                    f.path.clone(),
                    RemoteOutput {
                        digest,
                        executable: f.is_executable,
                        expires_at,
                    },
                ));
            }
        }

        for od in &r.output_directories {
            let td = od
                .tree_digest
                .clone()
                .ok_or_else(|| RemoteError::Other("output directory without tree digest".into()))?;
            let tree_bytes = self.fetch_blob(&td, meta)?;
            let tree = Tree::decode(&tree_bytes)?;
            let entries = flatten_tree(&tree, self.opts.digest_fn).map_err(RemoteError::Other)?;
            let mut subdirs = Vec::new();
            let mut tree_files = Vec::new();
            let mut tree_symlinks = Vec::new();
            for e in entries {
                match e {
                    TreeEntry::Directory { path } => subdirs.push(path),
                    TreeEntry::Symlink { path, target } => {
                        tree_symlinks.push((path, target));
                    }
                    TreeEntry::File {
                        path,
                        digest,
                        executable,
                    } => {
                        let full_rel = format!("{}/{path}", od.path);
                        let dl = self.should_download_output(
                            spawn,
                            &full_rel,
                            Some(&od.path),
                            r.exit_code,
                            cached,
                        );
                        if dl || digest.size_bytes == 0 {
                            let b = if digest.size_bytes == 0 {
                                Vec::new()
                            } else {
                                self.fetch_blob(&digest, meta)?
                            };
                            tree_files.push((path, b, executable));
                        } else {
                            all_downloaded = false;
                            lazy_updates.push((
                                full_rel,
                                RemoteOutput {
                                    digest,
                                    executable,
                                    expires_at,
                                },
                            ));
                        }
                    }
                }
            }
            trees.push(StagedTreeOutput {
                rel_dir: od.path.clone(),
                subdirs,
                files: tree_files,
                symlinks: tree_symlinks,
            });
        }

        Ok(StagedResult {
            stdout,
            stderr,
            all_downloaded,
            files,
            unmaterialized_files,
            trees,
            symlinks: symlinks_to_plant,
            lazy_updates,
        })
    }

    /// Commits a previously staged `ActionResult` to `execroot` and `self.lazy` after output
    /// locks have been acquired.
    fn commit_staged_result(
        &self,
        staged: StagedResult,
        spawn: &Spawn,
        execroot: &Path,
    ) -> Result<(Vec<u8>, Vec<u8>, bool), RemoteError> {
        for (rel, bytes, executable) in &staged.files {
            write_atomic_output(&execroot.join(rel), bytes, *executable)?;
        }
        for rel in &staged.unmaterialized_files {
            let _ = std::fs::remove_file(execroot.join(rel));
        }
        for t in &staged.trees {
            let base = execroot.join(&t.rel_dir);
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base)?;
            for sd in &t.subdirs {
                std::fs::create_dir_all(base.join(sd))?;
            }
            for (path, bytes, executable) in &t.files {
                write_atomic_output(&base.join(path), bytes, *executable)?;
            }
            for (path, target) in &t.symlinks {
                make_symlink(&base.join(path), target)?;
            }
        }
        // Plant top-level symlinks AFTER all regular files and directories are materialized.
        for (rel_path, target) in &staged.symlinks {
            make_symlink(&execroot.join(rel_path), target)?;
        }

        let mut lazy = self.lazy.lock().unwrap();
        // Fresh outputs supersede any stale lazy entries with the same prefix.
        for o in &spawn.outputs {
            let p = o.path_string();
            lazy.remove(p);
            let pre = format!("{p}/");
            lazy.retain(|k, _| !k.starts_with(&pre));
        }
        for (k, v) in staged.lazy_updates {
            lazy.insert(k, v);
        }
        Ok((staged.stdout, staged.stderr, staged.all_downloaded))
    }

    fn inline_or_fetch(
        &self,
        raw: &[u8],
        digest: &Option<Digest>,
        meta: &ActionMeta,
    ) -> Result<Vec<u8>, RemoteError> {
        if !raw.is_empty() {
            return Ok(raw.to_vec());
        }
        match digest {
            Some(d) if d.size_bytes > 0 => self.fetch_blob(d, meta),
            _ => Ok(Vec::new()),
        }
    }

    /// Downloads outputs that are only known by digest (e.g. top-level targets in
    /// `--remote_download_outputs=toplevel` mode), iteratively following symlink chains up to
    /// 32 hops within `execroot`.
    pub fn materialize(&self, paths: &[String], execroot: &Path) -> Result<usize, String> {
        let mut n = 0;
        let meta = ActionMeta::default();
        for p in paths {
            n += self.materialize_one(p, execroot, &meta, 0)?;
        }
        Ok(n)
    }

    fn materialize_one(
        &self,
        p: &str,
        execroot: &Path,
        meta: &ActionMeta,
        hops: usize,
    ) -> Result<usize, String> {
        if hops > MAX_SYMLINK_HOPS {
            return Err(format!(
                "Too many levels of symbolic links while materializing {p} (> {MAX_SYMLINK_HOPS})"
            ));
        }
        let mut n = 0;
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
            let b = self
                .fetch_blob(&v.digest, meta)
                .map_err(|e| format!("Failed to download {k}: {e}"))?;
            write_atomic_output(&execroot.join(&k), &b, v.executable)
                .map_err(|e| format!("Failed to write {k}: {e}"))?;
            self.lazy.lock().unwrap().remove(&k);
            n += 1;
        }
        // If `execroot/p` is a symlink on disk, follow its target within `execroot` up to 32 hops.
        let full = execroot.join(p);
        if let Ok(md) = std::fs::symlink_metadata(&full) {
            if md.file_type().is_symlink() {
                if let Ok(target) = std::fs::read_link(&full) {
                    if let Some(next_rel) = resolve_execroot_symlink(p, &target, execroot) {
                        n += self.materialize_one(&next_rel, execroot, meta, hops + 1)?;
                    }
                }
            }
        }
        Ok(n)
    }

    fn materialize_inputs(&self, spawn: &Spawn, execroot: &Path) -> Result<(), String> {
        let paths: Vec<String> = spawn
            .inputs
            .iter()
            .map(|p| p.path_string().to_string())
            .collect();
        self.materialize(&paths, execroot).map(|_| ())
    }

    // ------------------------------------------------------------------- cache writes

    /// Uploads a locally produced result (`exit_code == 0`) to the disk and/or remote cache
    /// (`UploadManifest` + `CombinedCache`).
    fn upload_result(&self, ra: &RemoteAction, spawn: &Spawn, execroot: &Path, res: &SpawnResult) {
        if res.exit_code != 0 || res.status != SpawnStatus::Success {
            return;
        }
        let (remote, disk) = self.write_policy(spawn);
        if !remote && !disk {
            return;
        }
        let mut blobs: BTreeMap<Digest, BlobSource> = BTreeMap::new();
        if ra.command_digest.size_bytes > 0 {
            blobs.insert(
                ra.command_digest.clone(),
                BlobSource::Bytes(ra.command_bytes.clone()),
            );
        }
        if ra.action_digest.size_bytes > 0 {
            blobs.insert(
                ra.action_digest.clone(),
                BlobSource::Bytes(ra.action_bytes.clone()),
            );
        }
        let mut ar = ActionResult {
            exit_code: res.exit_code,
            ..Default::default()
        };
        let f = self.opts.digest_fn;
        let allow_abs = self.allows_absolute_symlinks();
        let trees = tree_outputs(spawn);
        let mut outs: Vec<String> = spawn
            .outputs
            .iter()
            .map(|o| o.path_string().to_string())
            .collect();
        outs.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));

        for o in outs {
            let full = execroot.join(&o);
            let Ok(md) = std::fs::symlink_metadata(&full) else {
                continue;
            };
            let ft = md.file_type();
            if ft.is_symlink() {
                let Ok(t_pb) = std::fs::read_link(&full) else {
                    continue;
                };
                let t_str = t_pb.to_string_lossy().into_owned();
                if t_pb.is_absolute() {
                    match std::fs::metadata(&full) {
                        Ok(t_md) if t_md.is_dir() => {
                            match tree_from_local_dir_ext(&full, f, allow_abs, &mut blobs) {
                                Ok(t) => {
                                    let tb = t.encode_to_vec();
                                    let td = f.digest(&tb);
                                    if td.size_bytes > 0 {
                                        blobs.insert(td.clone(), BlobSource::Bytes(tb));
                                    }
                                    ar.output_directories.push(OutputDirectory {
                                        path: o,
                                        tree_digest: Some(td),
                                        ..Default::default()
                                    });
                                }
                                Err(e) => {
                                    self.warn(format!(
                                        "Failed to upload output directory {o}: {e}"
                                    ));
                                    return;
                                }
                            }
                            continue;
                        }
                        Ok(t_md) if t_md.is_file() => {
                            let Some((data, d)) = self.read_file_for_upload(
                                &full,
                                &o,
                                self.opts.guard_against_concurrent_changes,
                            ) else {
                                return;
                            };
                            let _ = data;
                            if d.size_bytes > 0 {
                                blobs.insert(d.clone(), BlobSource::File(full));
                            }
                            ar.output_files.push(OutputFile {
                                path: o,
                                digest: Some(d),
                                is_executable: true,
                                ..Default::default()
                            });
                            continue;
                        }
                        Ok(_) => {
                            self.warn(format!("Illegal special output file {o}"));
                            return;
                        }
                        Err(_) if !allow_abs => {
                            self.warn(format!(
                                "Output {o} is a dangling absolute symbolic link (-> {t_str}), disallowed by remote cache"
                            ));
                            return;
                        }
                        Err(_) => {}
                    }
                }
                let sym = OutputSymlink {
                    path: o.clone(),
                    target: t_str,
                    node_properties: None,
                };
                let is_dir_sym = trees.contains(&o)
                    || std::fs::metadata(&full)
                        .map(|m| m.is_dir())
                        .unwrap_or(false);
                if is_dir_sym {
                    ar.output_directory_symlinks.push(sym.clone());
                } else {
                    ar.output_file_symlinks.push(sym.clone());
                }
                ar.output_symlinks.push(sym);
            } else if ft.is_dir() {
                match tree_from_local_dir_ext(&full, f, allow_abs, &mut blobs) {
                    Ok(t) => {
                        let tb = t.encode_to_vec();
                        let td = f.digest(&tb);
                        if td.size_bytes > 0 {
                            blobs.insert(td.clone(), BlobSource::Bytes(tb));
                        }
                        ar.output_directories.push(OutputDirectory {
                            path: o,
                            tree_digest: Some(td),
                            ..Default::default()
                        });
                    }
                    Err(e) => {
                        self.warn(format!("Failed to upload output directory {o}: {e}"));
                        return;
                    }
                }
            } else if ft.is_file() {
                let Some((_data, d)) = self.read_file_for_upload(
                    &full,
                    &o,
                    self.opts.guard_against_concurrent_changes,
                ) else {
                    return;
                };
                if d.size_bytes > 0 {
                    blobs.insert(d.clone(), BlobSource::File(full));
                }
                // UploadManifest sets `is_executable = true` on all uploaded regular files.
                ar.output_files.push(OutputFile {
                    path: o,
                    digest: Some(d),
                    is_executable: true,
                    ..Default::default()
                });
            } else {
                self.warn(format!("Illegal special output file {o}"));
                return;
            }
        }
        for (bytes, slot) in [(&res.stdout, 0), (&res.stderr, 1)] {
            if !bytes.is_empty() {
                let d = f.digest(bytes);
                if d.size_bytes > 0 {
                    blobs.insert(d.clone(), BlobSource::Bytes(bytes.clone()));
                }
                if slot == 0 {
                    ar.stdout_digest = Some(d)
                } else {
                    ar.stderr_digest = Some(d)
                }
            }
        }
        if disk {
            let dc = self.disk.as_ref().unwrap();
            let mut disk_ok = true;
            for (d, s) in &blobs {
                if let Err(e) = dc.put_source(d, s) {
                    self.warn(format!("Disk Cache: {e}"));
                    disk_ok = false;
                }
            }
            if disk_ok {
                if let Err(e) = dc.put_action_result(&ra.action_digest, &ar) {
                    self.warn(format!("Disk Cache: {e}"));
                }
            }
        }
        if remote {
            match self.remote_upload_action_result(&ra.action_digest, &ar, &blobs, &ra.meta) {
                Ok(()) => {
                    self.stats.cache_uploads.fetch_add(1, Ordering::SeqCst);
                }
                Err(e) => self.warn(format!("Remote Cache: Upload failed: {e}")),
            }
        }
    }

    fn read_file_for_upload(
        &self,
        full: &Path,
        rel: &str,
        guard_concurrent_changes: bool,
    ) -> Option<(Vec<u8>, Digest)> {
        let stat_stamp = |p: &Path| -> Option<(u64, u128)> {
            let md = std::fs::metadata(p).ok()?;
            let mtime = md
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            Some((md.len(), mtime))
        };
        let before = if guard_concurrent_changes {
            stat_stamp(full)
        } else {
            None
        };
        let Ok(data) = std::fs::read(full) else {
            return None;
        };
        if guard_concurrent_changes {
            let after = stat_stamp(full);
            if before.is_some() && before != after {
                self.warn(format!(
                    "Skipping upload of {rel}: file was modified during upload"
                ));
                return None;
            }
        }
        let d = self.opts.digest_fn.digest(&data);
        Some((data, d))
    }

    // ------------------------------------------------------------------- remote execution

    fn execute_remotely(
        &self,
        ra: &RemoteAction,
        force_skip_cache_lookup: bool,
    ) -> Result<ExecuteResponse, RemoteError> {
        let c = self.client.as_ref().unwrap();
        let all = ra.all_blobs();
        c.ensure_blobs_present(&all, &ra.meta)?;
        let skip = !self.opts.accept_cached || force_skip_cache_lookup;
        match c.execute(&ra.action_digest, skip, &ra.meta, &mut |_| {}) {
            Err(RemoteError::Exec(st)) if is_missing_blobs_failure(&st) => {
                // The CAS lost some blobs after our check: force re-upload of everything.
                let digests: Vec<Digest> =
                    all.keys().filter(|d| d.size_bytes > 0).cloned().collect();
                c.upload_blobs(&digests, &all, &ra.meta)?;
                c.execute(&ra.action_digest, skip, &ra.meta, &mut |_| {})
            }
            other => other,
        }
    }

    fn failed_with_code(
        &self,
        msg: String,
        start: Instant,
        status: SpawnStatus,
        exit_code: i32,
    ) -> SpawnResult {
        SpawnResult {
            status,
            exit_code,
            stderr: format!("{msg}\n").into_bytes(),
            wall_time_ms: start.elapsed().as_millis() as u64,
            runner_name: "remote".into(),
            failure_message: Some(msg),
            ..Default::default()
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
            return Ok(self.failed_with_code(
                e,
                Instant::now(),
                SpawnStatus::ExecutionFailed,
                REMOTE_ERROR_EXIT_CODE,
            ));
        }
        self.stats.local_executions.fetch_add(1, Ordering::SeqCst);
        let res = match mode.fallback {
            Some(f) => f.exec(spawn, execroot, fs)?,
            None => self.fallback.exec(spawn, execroot, fs)?,
        };
        if res.status == SpawnStatus::Success && res.exit_code == 0 {
            {
                let mut lazy = self.lazy.lock().unwrap();
                for o in &spawn.outputs {
                    let p = o.path_string();
                    lazy.remove(p);
                    let pre = format!("{p}/");
                    lazy.retain(|k, _| !k.starts_with(&pre));
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
        if self.exec_remotely_enabled() {
            "remote"
        } else {
            "remote-cache"
        }
    }

    fn can_exec(&self, spawn: &Spawn) -> bool {
        if self.exec_remotely_enabled() {
            may_be_executed_remotely(spawn) && !self.is_scrubbed(spawn)
        } else {
            true
        }
    }

    fn exec(
        &self,
        spawn: &Spawn,
        execroot: &PathFragment,
        fs: &dyn FileSystem,
    ) -> io::Result<SpawnResult> {
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
        if !self.has_cache() {
            return self.run_local(spawn, execroot, fs, None, mode);
        }
        let ra = match self.build_action(spawn, &root) {
            Ok(ra) => ra,
            Err(_missing) => {
                // Let the local runner produce the canonical missing-inputs diagnostics.
                return self.run_local(spawn, execroot, fs, None, mode);
            }
        };
        let will_exec_remotely = mode.allow_remote_exec
            && self.exec_remotely_enabled()
            && may_be_executed_remotely(spawn)
            && !ra.scrubbed;

        if ra.scrubbed
            && mode.allow_remote_exec
            && self.exec_remotely_enabled()
            && may_be_executed_remotely(spawn)
        {
            self.warn(format!(
                "Action {} ({}) was scrubbed and cannot be executed remotely; running locally",
                spawn.mnemonic, spawn.owner_label
            ));
        }

        let mut force_skip_cache_lookup = false;

        // 1. ActionCache lookup (disk first, then remote).
        match self.lookup_cache(&ra, spawn) {
            Ok(Some(hit)) => {
                let (r, is_remote_hit, runner) = match hit {
                    CacheHit::Disk(r) => (r, false, "disk cache hit"),
                    CacheHit::Remote(r) => (r, true, "remote cache hit"),
                };
                // Cached non-zero exit codes or missing mandatory outputs are silent cache misses!
                if r.exit_code != 0 || Self::validate_mandatory_outputs(&r, spawn).is_err() {
                    force_skip_cache_lookup = true;
                } else {
                    match self.stage_result(&r, spawn, &ra.meta, true) {
                        Ok(staged) => {
                            // CombinedCache: populate disk AC on remote AC hit when disk writes
                            // are allowed (failing the lookup if disk write errors).
                            let mut disk_sync_err = None;
                            if is_remote_hit && staged.all_downloaded && self.write_policy(spawn).1
                            {
                                if let Some(dc) = &self.disk {
                                    if let Err(e) = dc.put_action_result(&ra.action_digest, &r) {
                                        disk_sync_err = Some(RemoteError::Io(e));
                                    }
                                }
                            }
                            if let Some(err) = disk_sync_err {
                                if will_exec_remotely {
                                    if self.opts.local_fallback && mode.allow_local {
                                        self.warn(format!("Remote Cache: {err}"));
                                        return self.run_local(
                                            spawn,
                                            execroot,
                                            fs,
                                            Some(&ra),
                                            mode,
                                        );
                                    }
                                    return Ok(self.failed_with_code(
                                        format!("Remote cache error: {err}"),
                                        start,
                                        SpawnStatus::ExecutionFailed,
                                        REMOTE_ERROR_EXIT_CODE,
                                    ));
                                } else {
                                    self.warn(format!("Remote Cache: {err}"));
                                    return self.run_local(spawn, execroot, fs, Some(&ra), mode);
                                }
                            }
                            if mode.hooks.is_cancelled() || !mode.hooks.lock(r.exit_code) {
                                return Err(zel_sandbox::cancelled_error());
                            }
                            let (stdout, stderr, _all_downloaded) =
                                match self.commit_staged_result(staged, spawn, &root) {
                                    Ok(v) => v,
                                    Err(e) => {
                                        if self.opts.local_fallback && mode.allow_local {
                                            self.warn(format!("Remote Cache: {e}"));
                                            return self.run_local(
                                                spawn,
                                                execroot,
                                                fs,
                                                Some(&ra),
                                                mode,
                                            );
                                        }
                                        return Ok(self.failed_with_code(
                                            format!("Failed to write cached outputs: {e}"),
                                            start,
                                            SpawnStatus::ExecutionFailed,
                                            REMOTE_ERROR_EXIT_CODE,
                                        ));
                                    }
                                };
                            if let Some(cb) = &mode.hooks.stream_out {
                                if !stdout.is_empty() {
                                    cb(&stdout);
                                }
                                if !stderr.is_empty() {
                                    cb(&stderr);
                                }
                            }
                            if is_remote_hit {
                                self.stats.remote_cache_hits.fetch_add(1, Ordering::SeqCst);
                            } else {
                                self.stats.disk_cache_hits.fetch_add(1, Ordering::SeqCst);
                            }
                            return Ok(SpawnResult {
                                status: SpawnStatus::Success,
                                exit_code: 0,
                                stdout,
                                stderr,
                                wall_time_ms: start.elapsed().as_millis() as u64,
                                runner_name: runner.to_string(),
                                ..Default::default()
                            });
                        }
                        Err(e) if e.is_credential_helper_error() => {
                            return Ok(self.failed_with_code(
                                format!("Credential helper failed: {e}"),
                                start,
                                SpawnStatus::ExecutionFailed,
                                REMOTE_ERROR_EXIT_CODE,
                            ));
                        }
                        Err(e) if e.is_cache_not_found() => {
                            // Evicted CAS blob on cache hit: silent miss -> re-execute with
                            // `skip_cache_lookup = true`.
                            force_skip_cache_lookup = true;
                        }
                        Err(e) => {
                            if will_exec_remotely {
                                if self.opts.local_fallback && mode.allow_local {
                                    self.stats.fallbacks.fetch_add(1, Ordering::SeqCst);
                                    self.warn(format!(
                                        "Remote cache download for {} failed, falling back to local execution: {e}",
                                        spawn.mnemonic
                                    ));
                                    return self.run_local(spawn, execroot, fs, Some(&ra), mode);
                                }
                                return Ok(self.failed_with_code(
                                    format!("Failed to download cached outputs: {e}"),
                                    start,
                                    SpawnStatus::ExecutionFailed,
                                    REMOTE_ERROR_EXIT_CODE,
                                ));
                            } else {
                                self.warn(format!("Remote Cache: {e}"));
                                return self.run_local(spawn, execroot, fs, Some(&ra), mode);
                            }
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(e) if e.is_credential_helper_error() => {
                return Ok(self.failed_with_code(
                    format!("Credential helper failed: {e}"),
                    start,
                    SpawnStatus::ExecutionFailed,
                    REMOTE_ERROR_EXIT_CODE,
                ));
            }
            Err(e) => {
                self.warn(format!("Remote Cache: {e}"));
            }
        }

        // 2. Remote execution (if enabled and permitted for this spawn).
        if will_exec_remotely {
            let mut skip_cache = force_skip_cache_lookup || !may_be_cached_remotely(spawn);
            let mut retried_stale_exec_cache = false;
            loop {
                match self.execute_remotely(&ra, skip_cache) {
                    Ok(resp) => {
                        let Some(r) = resp.result.clone() else {
                            let e = RemoteError::Other(
                                "Unexpected result of remote execution: no result".into(),
                            );
                            if self.opts.local_fallback && mode.allow_local {
                                self.stats.fallbacks.fetch_add(1, Ordering::SeqCst);
                                self.warn(format!(
                                    "Remote execution of {} failed, falling back to local execution: {e}",
                                    spawn.mnemonic
                                ));
                                return self.run_local(spawn, execroot, fs, Some(&ra), mode);
                            }
                            if mode.hooks.is_cancelled() || !mode.hooks.lock(REMOTE_ERROR_EXIT_CODE)
                            {
                                return Err(zel_sandbox::cancelled_error());
                            }
                            return Ok(self.failed_with_code(
                                format!("Remote execution failed: {e}"),
                                start,
                                SpawnStatus::ExecutionFailed,
                                REMOTE_ERROR_EXIT_CODE,
                            ));
                        };
                        if !resp.message.is_empty() {
                            self.warn(resp.message.clone());
                        }
                        let cached = resp.cached_result;
                        // Cached non-zero exit code or missing mandatory output from Execute
                        // (`cached_result == true`) -> re-run once with `skip_cache_lookup = true`.
                        if cached
                            && !retried_stale_exec_cache
                            && (r.exit_code != 0
                                || Self::validate_mandatory_outputs(&r, spawn).is_err())
                        {
                            retried_stale_exec_cache = true;
                            skip_cache = true;
                            continue;
                        }
                        // Best-effort download of server_logs on failed non-cached execution.
                        if r.exit_code != 0 && !resp.server_logs.is_empty() {
                            for log_entry in &resp.server_logs {
                                if let Some(lf) = &log_entry.value {
                                    if let Some(d) = &lf.digest {
                                        let _ = self.fetch_blob(d, &ra.meta);
                                    }
                                }
                            }
                        }
                        match self.stage_result(&r, spawn, &ra.meta, cached) {
                            Ok(staged) => {
                                if mode.hooks.is_cancelled() || !mode.hooks.lock(r.exit_code) {
                                    return Err(zel_sandbox::cancelled_error());
                                }
                                let (stdout, stderr, all_downloaded) =
                                    match self.commit_staged_result(staged, spawn, &root) {
                                        Ok(v) => v,
                                        Err(e) => {
                                            return Ok(self.failed_with_code(
                                                format!("Failed to write remote outputs: {e}"),
                                                start,
                                                SpawnStatus::ExecutionFailed,
                                                REMOTE_ERROR_EXIT_CODE,
                                            ));
                                        }
                                    };
                                if let Some(cb) = &mode.hooks.stream_out {
                                    if !stdout.is_empty() {
                                        cb(&stdout);
                                    }
                                    if !stderr.is_empty() {
                                        cb(&stderr);
                                    }
                                }
                                if cached {
                                    self.stats.remote_cache_hits.fetch_add(1, Ordering::SeqCst);
                                } else {
                                    self.stats.remote_executions.fetch_add(1, Ordering::SeqCst);
                                }
                                // Populate the disk cache from successful remote results when all
                                // outputs were downloaded (`CombinedCache`).
                                if r.exit_code == 0 && all_downloaded && self.write_policy(spawn).1
                                {
                                    if let Some(dc) = &self.disk {
                                        let _ = dc.put_action_result(&ra.action_digest, &r);
                                    }
                                }
                                return Ok(SpawnResult {
                                    status: if r.exit_code == 0 {
                                        SpawnStatus::Success
                                    } else {
                                        SpawnStatus::NonZeroExit
                                    },
                                    exit_code: r.exit_code,
                                    stdout,
                                    stderr,
                                    wall_time_ms: start.elapsed().as_millis() as u64,
                                    runner_name: if cached {
                                        "remote cache hit".to_string()
                                    } else {
                                        "remote".to_string()
                                    },
                                    ..Default::default()
                                });
                            }
                            Err(e)
                                if cached
                                    && !retried_stale_exec_cache
                                    && e.is_cache_not_found() =>
                            {
                                retried_stale_exec_cache = true;
                                skip_cache = true;
                                continue;
                            }
                            Err(e) => {
                                if self.opts.local_fallback
                                    && !e.is_credential_helper_error()
                                    && mode.allow_local
                                {
                                    self.stats.fallbacks.fetch_add(1, Ordering::SeqCst);
                                    self.warn(format!(
                                        "Remote execution of {} failed, falling back to local execution: {e}",
                                        spawn.mnemonic
                                    ));
                                    return self.run_local(spawn, execroot, fs, Some(&ra), mode);
                                }
                                let evicted = e.is_cache_not_found();
                                let exit_code = if evicted { 39 } else { REMOTE_ERROR_EXIT_CODE };
                                if mode.hooks.is_cancelled() || !mode.hooks.lock(exit_code) {
                                    return Err(zel_sandbox::cancelled_error());
                                }
                                let mut sr = self.failed_with_code(
                                    format!("Failed to apply remote result: {e}"),
                                    start,
                                    if evicted {
                                        SpawnStatus::RemoteCacheEvicted
                                    } else {
                                        SpawnStatus::ExecutionFailed
                                    },
                                    exit_code,
                                );
                                sr.catastrophic = evicted;
                                return Ok(sr);
                            }
                        }
                    }
                    Err(e) => {
                        let timeout = e.is_action_timeout();
                        if self.opts.local_fallback
                            && !timeout
                            && !e.is_credential_helper_error()
                            && mode.allow_local
                        {
                            self.stats.fallbacks.fetch_add(1, Ordering::SeqCst);
                            self.warn(format!(
                                "Remote execution of {} failed, falling back to local execution: {e}",
                                spawn.mnemonic
                            ));
                            return self.run_local(spawn, execroot, fs, Some(&ra), mode);
                        }
                        let exit_code = if timeout {
                            TIMEOUT_EXIT_CODE
                        } else {
                            REMOTE_ERROR_EXIT_CODE
                        };
                        if mode.hooks.is_cancelled() || !mode.hooks.lock(exit_code) {
                            return Err(zel_sandbox::cancelled_error());
                        }
                        return Ok(self.failed_with_code(
                            format!("Remote execution failed: {e}"),
                            start,
                            if timeout {
                                SpawnStatus::Timeout
                            } else {
                                SpawnStatus::ExecutionFailed
                            },
                            exit_code,
                        ));
                    }
                }
            }
        }

        // 3. Local execution (`RemoteSpawnCache` or local-only spawn).
        self.run_local(spawn, execroot, fs, Some(&ra), mode)
    }
}

fn expand_dir(
    full: &Path,
    rel: &str,
    out: &mut Vec<MerkleInput>,
    lazy: &HashMap<String, RemoteOutput>,
    scrub_rule: Option<&CompiledScrubRule>,
) {
    let Ok(rd) = std::fs::read_dir(full) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    if entries.is_empty() {
        out.push(MerkleInput::empty_dir(rel));
    }
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let child_rel = format!("{rel}/{name}");
        if scrub_rule.is_some_and(|sr| sr.omits_input(&child_rel)) {
            continue;
        }
        let p = e.path();
        if lazy.contains_key(&child_rel) {
            continue;
        }
        match std::fs::metadata(&p) {
            Ok(md) if md.is_dir() => expand_dir(&p, &child_rel, out, lazy, scrub_rule),
            Ok(md) => {
                #[cfg(unix)]
                let exec = {
                    use std::os::unix::fs::PermissionsExt;
                    md.permissions().mode() & 0o111 != 0
                };
                #[cfg(not(unix))]
                let exec = {
                    let _ = md;
                    false
                };
                out.push(MerkleInput::file(&child_rel, p, exec));
            }
            Err(_) => {}
        }
    }
}

static ATOMIC_OUT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Writes `dest` atomically via a sibling temporary file + permissions + `rename`.
fn write_atomic_output(dest: &Path, data: &[u8], executable: bool) -> io::Result<()> {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let n = ATOMIC_OUT_SEQ.fetch_add(1, Ordering::Relaxed);
    let fname = dest
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".into());
    let tmp = parent.join(format!(".zel-dl-{fname}-{}-{n}.tmp", std::process::id()));
    std::fs::write(&tmp, data)?;
    set_executable(&tmp, executable);
    let _ = std::fs::remove_file(dest);
    if let Err(e) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
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
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "symlinks unsupported",
        ))
    }
}

/// Resolves `target` relative to `link_rel`'s parent directory, returning an execroot-relative
/// `/`-separated path if it remains inside `execroot`.
fn resolve_execroot_symlink(link_rel: &str, target: &Path, execroot: &Path) -> Option<String> {
    if target.is_absolute() {
        let rel = target.strip_prefix(execroot).ok()?;
        return normalize_rel_components(rel);
    }
    let parent = Path::new(link_rel)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    normalize_rel_components(&parent.join(target))
}

fn normalize_rel_components(p: &Path) -> Option<String> {
    let mut stack: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::Normal(s) => stack.push(s.to_string_lossy().into_owned()),
            Component::ParentDir => {
                stack.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if stack.is_empty() {
        None
    } else {
        Some(stack.join("/"))
    }
}
