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

//! Remote Execution API v2 client over [`crate::grpc`]: Capabilities (`ClientApiVersion`,
//! `RemoteServerCapabilities`), ContentAddressableStorage (`FindMissingBlobs`,
//! `BatchUpdateBlobs`, `BatchReadBlobs`, `GetTree`), ByteStream (`Read`/`Write`, `Chunker`, zstd
//! `compressed-blobs`), ActionCache (`Get`/`Update`), and Execution (`Execute`/`WaitExecution`).

use crate::digest::DigestFn;
use crate::grpc::{CallOptions, Channel, GrpcStatus, RetryPolicy};
use crate::merkle::BlobSource;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use zel_proto::reapi::*;

const CAS: &str = "/build.bazel.remote.execution.v2.ContentAddressableStorage/";
const AC: &str = "/build.bazel.remote.execution.v2.ActionCache/";
const EXEC: &str = "/build.bazel.remote.execution.v2.Execution/";
const CAPS: &str = "/build.bazel.remote.execution.v2.Capabilities/GetCapabilities";
const BS: &str = "/google.bytestream.ByteStream/";
pub const REQUEST_METADATA_HEADER: &str = "build.bazel.remote.execution.v2.requestmetadata-bin";

/// ByteStream chunk size for uploads (64 KiB).
pub const WRITE_CHUNK: usize = 64 * 1024;
/// Default maximum total size of a batch when the server does not advertise one (Bazel: 4 MiB
/// minus protocol overhead).
const DEFAULT_MAX_BATCH: i64 = 4 * 1024 * 1024 - 1024;
const FIND_MISSING_CHUNK: usize = 1000;

/// ZSTD enum value in `build.bazel.remote.execution.v2.Compressor.Value`.
pub const COMPRESSOR_ZSTD: i32 = 1;

#[derive(Debug)]
pub enum RemoteError {
    Grpc(GrpcStatus),
    /// Remote execution finished with a non-OK `ExecuteResponse.status` (excluding action timeout).
    Exec(RpcStatus),
    /// `ExecuteResponse.status` had `DEADLINE_EXCEEDED` (action timed out on worker; never retried).
    ActionTimeout(RpcStatus),
    /// Capabilities check or query failed (`RemoteExecutionCapabilitiesException`).
    Capabilities(String),
    Io(io::Error),
    NotFound,
    Other(String),
}

impl RemoteError {
    pub fn is_credential_helper_error(&self) -> bool {
        matches!(self, Self::Grpc(g) if g.credential_helper_error)
    }

    pub fn is_capabilities_error(&self) -> bool {
        matches!(self, Self::Capabilities(_))
    }

    pub fn is_action_timeout(&self) -> bool {
        matches!(self, Self::ActionTimeout(_))
    }

    pub fn is_cache_not_found(&self) -> bool {
        matches!(self, Self::NotFound) || matches!(self, Self::Grpc(g) if g.code == code::NOT_FOUND)
    }
}

impl fmt::Display for RemoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Grpc(s) => write!(f, "{s}"),
            Self::Exec(s) => write!(
                f,
                "remote execution failed (code {}): {}",
                s.code, s.message
            ),
            Self::ActionTimeout(s) => {
                if s.message.is_empty() {
                    write!(f, "Action timed out")
                } else {
                    write!(f, "Action timed out: {}", s.message)
                }
            }
            Self::Capabilities(m) => write!(f, "{m}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::NotFound => write!(f, "not found in remote cache"),
            Self::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for RemoteError {}

impl From<GrpcStatus> for RemoteError {
    fn from(s: GrpcStatus) -> Self {
        if s.code == code::NOT_FOUND {
            Self::NotFound
        } else {
            Self::Grpc(s)
        }
    }
}
impl From<io::Error> for RemoteError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<zel_proto::DecodeError> for RemoteError {
    fn from(e: zel_proto::DecodeError) -> Self {
        Self::Other(format!("malformed response: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, RemoteError>;

// ---------------------------------------------------------------------------
// Chunker (mirrors Bazel's `com.google.devtools.build.lib.remote.Chunker`)
// ---------------------------------------------------------------------------

enum ChunkerState {
    Unopened,
    Memory { buf: Vec<u8>, pos: usize },
    FileStream { file: File, remaining: u64 },
    Exhausted,
}

/// Lazily-opened chunk iterator over in-memory bytes or a file on disk, optionally zstd-compressed.
pub struct Chunker {
    source: BlobSource,
    chunk_size: usize,
    compressed: bool,
    offset: i64,
    state: ChunkerState,
}

impl Chunker {
    pub fn new(source: BlobSource, chunk_size: usize, compressed: bool) -> Self {
        Self {
            source,
            chunk_size: chunk_size.max(1),
            compressed,
            offset: 0,
            state: ChunkerState::Unopened,
        }
    }

    pub fn is_compressed(&self) -> bool {
        self.compressed
    }

    pub fn offset(&self) -> i64 {
        self.offset
    }

    pub fn reset(&mut self) {
        self.offset = 0;
        self.state = ChunkerState::Unopened;
    }

    fn ensure_open(&mut self) -> io::Result<()> {
        if !matches!(self.state, ChunkerState::Unopened) {
            return Ok(());
        }
        if self.compressed {
            let raw = self.source.read()?;
            let z = zel_util::zstd::compress(&raw);
            self.state = if z.is_empty() {
                ChunkerState::Exhausted
            } else {
                ChunkerState::Memory { buf: z, pos: 0 }
            };
            return Ok(());
        }
        match &self.source {
            BlobSource::Bytes(b) => {
                self.state = if b.is_empty() {
                    ChunkerState::Exhausted
                } else {
                    ChunkerState::Memory {
                        buf: b.clone(),
                        pos: 0,
                    }
                };
            }
            BlobSource::File(p) => {
                let f = File::open(p)?;
                let len = f.metadata()?.len();
                self.state = if len == 0 {
                    ChunkerState::Exhausted
                } else {
                    ChunkerState::FileStream {
                        file: f,
                        remaining: len,
                    }
                };
            }
        }
        Ok(())
    }

    pub fn seek(&mut self, to_offset: i64) -> io::Result<()> {
        if to_offset < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "negative seek"));
        }
        if to_offset < self.offset || matches!(self.state, ChunkerState::Exhausted) {
            self.reset();
        }
        self.ensure_open()?;
        let mut to_skip = (to_offset - self.offset) as usize;
        while to_skip > 0 {
            match &mut self.state {
                ChunkerState::Memory { buf, pos } => {
                    let avail = buf.len().saturating_sub(*pos);
                    if to_skip > avail {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "seek past end of chunker",
                        ));
                    }
                    *pos += to_skip;
                    self.offset += to_skip as i64;
                    if *pos >= buf.len() {
                        self.state = ChunkerState::Exhausted;
                    }
                    break;
                }
                ChunkerState::FileStream { file, remaining } => {
                    let mut scratch = vec![0u8; to_skip.min(64 * 1024)];
                    let n = file.read(&mut scratch)?;
                    if n == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "seek past EOF",
                        ));
                    }
                    to_skip -= n;
                    *remaining = remaining.saturating_sub(n as u64);
                    self.offset += n as i64;
                    if *remaining == 0 {
                        self.state = ChunkerState::Exhausted;
                    }
                }
                ChunkerState::Exhausted => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "seek past exhausted chunker",
                    ));
                }
                ChunkerState::Unopened => unreachable!(),
            }
        }
        Ok(())
    }

    /// Returns `Ok(Some((chunk_offset, data, is_last)))`, or `Ok(None)` once finished.
    pub fn next_chunk(&mut self) -> io::Result<Option<(i64, Vec<u8>, bool)>> {
        if matches!(self.state, ChunkerState::Exhausted) {
            return Ok(None);
        }
        let was_unopened = matches!(self.state, ChunkerState::Unopened);
        self.ensure_open()?;
        if matches!(self.state, ChunkerState::Exhausted) {
            if was_unopened && self.offset == 0 {
                return Ok(Some((0, Vec::new(), true)));
            }
            return Ok(None);
        }

        let start_offset = self.offset;
        match &mut self.state {
            ChunkerState::Memory { buf, pos } => {
                let end = (*pos + self.chunk_size).min(buf.len());
                let slice = buf[*pos..end].to_vec();
                *pos = end;
                self.offset += slice.len() as i64;
                let last = *pos >= buf.len();
                if last {
                    self.state = ChunkerState::Exhausted;
                }
                Ok(Some((start_offset, slice, last)))
            }
            ChunkerState::FileStream { file, remaining } => {
                let want = self.chunk_size.min(*remaining as usize);
                let mut out = vec![0u8; want];
                let mut read_total = 0usize;
                while read_total < want {
                    let n = file.read(&mut out[read_total..])?;
                    if n == 0 {
                        break;
                    }
                    read_total += n;
                }
                out.truncate(read_total);
                *remaining = remaining.saturating_sub(read_total as u64);
                self.offset += read_total as i64;
                let last = *remaining == 0 || read_total == 0;
                if last {
                    // Close underlying file handle immediately upon exhaustion.
                    self.state = ChunkerState::Exhausted;
                }
                Ok(Some((start_offset, out, last)))
            }
            ChunkerState::Exhausted | ChunkerState::Unopened => unreachable!(),
        }
    }
}

// ---------------------------------------------------------------------------
// ClientApiVersion (`[2.0.0, 2.11.0]`, UTF-16 prerelease ordering)
// ---------------------------------------------------------------------------

pub const CLIENT_LOW_API: SemVer = SemVer {
    major: 2,
    minor: 0,
    patch: 0,
    prerelease: String::new(),
};
pub const CLIENT_HIGH_API: SemVer = SemVer {
    major: 2,
    minor: 11,
    patch: 0,
    prerelease: String::new(),
};

/// Compares two `SemVer` instances matching Bazel's `ApiVersion.compareTo`:
/// - any non-empty `prerelease` sorts older than every release (`prerelease.is_empty()`);
/// - two non-empty `prerelease` values compare by UTF-16 code units (ignoring major/minor/patch);
/// - two releases compare by `(major, minor, patch)`.
pub fn compare_semver(a: &SemVer, b: &SemVer) -> Ordering {
    match (a.prerelease.is_empty(), b.prerelease.is_empty()) {
        (false, true) => Ordering::Less,
        (true, false) => Ordering::Greater,
        (false, false) => a.prerelease.encode_utf16().cmp(b.prerelease.encode_utf16()),
        (true, true) => (a.major, a.minor, a.patch).cmp(&(b.major, b.minor, b.patch)),
    }
}

pub fn format_semver(v: &SemVer) -> String {
    if !v.prerelease.is_empty() {
        v.prerelease.clone()
    } else if v.patch != 0 {
        format!("{}.{}.{}", v.major, v.minor, v.patch)
    } else {
        format!("{}.{}", v.major, v.minor)
    }
}

fn max_semver<'a>(a: &'a SemVer, b: &'a SemVer) -> &'a SemVer {
    if compare_semver(a, b) >= Ordering::Equal {
        a
    } else {
        b
    }
}

fn min_semver<'a>(a: &'a SemVer, b: &'a SemVer) -> &'a SemVer {
    if compare_semver(a, b) <= Ordering::Equal {
        a
    } else {
        b
    }
}

/// Checks `ServerCapabilities` API range against Bazel's `[2.0.0, 2.11.0]` window.
/// Returns `Ok((negotiated_high, optional_deprecation_warning))` or `Err(unsupported_msg)`.
pub fn check_api_version(
    caps: &ServerCapabilities,
) -> std::result::Result<(SemVer, Option<String>), String> {
    let default_zero = SemVer::default();
    let client_low = CLIENT_LOW_API;
    let client_high = CLIENT_HIGH_API;
    let st_low = caps.low_api_version.as_ref().unwrap_or(&default_zero);
    let st_high = caps.high_api_version.as_ref().unwrap_or(&default_zero);

    let overlap_low = max_semver(&client_low, st_low);
    let overlap_high = min_semver(&client_high, st_high);
    if compare_semver(overlap_low, overlap_high) <= Ordering::Equal {
        return Ok((overlap_high.clone(), None));
    }

    if let Some(dep) = &caps.deprecated_api_version {
        let dep_low = max_semver(&client_low, dep);
        let dep_high = min_semver(&client_high, st_high);
        if compare_semver(dep_low, dep_high) <= Ordering::Equal {
            let warn = format!(
                "The highest API version Bazel support {} is deprecated by the server. Please upgrade to server's recommended version: {} to {}.",
                format_semver(dep_high),
                format_semver(st_low),
                format_semver(st_high),
            );
            return Ok((dep_high.clone(), Some(warn)));
        }
    }

    Err(format!(
        "The client supported API versions, {} to {}, is not supported by the server, {} to {}. Please switch to a different server or upgrade Bazel.",
        format_semver(&client_low),
        format_semver(&client_high),
        format_semver(st_low),
        format_semver(st_high),
    ))
}

/// Client configuration.
#[derive(Debug, Clone)]
pub struct ReapiConfig {
    pub instance_name: String,
    pub digest_fn: DigestFn,
    /// `--remote_timeout` (applied to every non-execution call).
    pub timeout: Duration,
    pub retry: RetryPolicy,
    pub tool_invocation_id: String,
    pub correlated_invocations_id: String,
    pub cache_compression: bool,
    pub compression_threshold: usize,
    pub verify_downloads: bool,
    pub execution_priority: i32,
    pub result_cache_priority: i32,
}

impl Default for ReapiConfig {
    fn default() -> Self {
        Self {
            instance_name: String::new(),
            digest_fn: DigestFn::Sha256,
            timeout: Duration::from_secs(60),
            retry: RetryPolicy::default(),
            tool_invocation_id: String::new(),
            correlated_invocations_id: String::new(),
            cache_compression: false,
            compression_threshold: 100,
            verify_downloads: true,
            execution_priority: 0,
            result_cache_priority: 0,
        }
    }
}

/// Metadata attached to calls about the action they belong to.
#[derive(Debug, Clone, Default)]
pub struct ActionMeta {
    pub action_id: String,
    pub mnemonic: String,
    pub target_id: String,
}

#[derive(Default)]
struct UploadTracker {
    present: HashSet<Digest>,
    inflight: HashSet<Digest>,
}

pub struct ReapiClient {
    pub cache: Arc<Channel>,
    pub executor: Option<Arc<Channel>>,
    pub cfg: ReapiConfig,
    cache_caps: Mutex<Option<ServerCapabilities>>,
    exec_caps: Mutex<Option<ServerCapabilities>>,
    warnings: Mutex<Vec<String>>,
    uploads: Mutex<UploadTracker>,
    upload_cv: Condvar,
}

pub fn base64_encode(data: &[u8]) -> String {
    zel_util::base64_encode(data)
}

impl ReapiClient {
    pub fn new(cache: Arc<Channel>, executor: Option<Arc<Channel>>, cfg: ReapiConfig) -> Self {
        Self {
            cache,
            executor,
            cfg,
            cache_caps: Mutex::new(None),
            exec_caps: Mutex::new(None),
            warnings: Mutex::new(Vec::new()),
            uploads: Mutex::new(UploadTracker::default()),
            upload_cv: Condvar::new(),
        }
    }

    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut *self.warnings.lock().unwrap())
    }

    pub fn opts(&self, meta: &ActionMeta, with_timeout: bool) -> CallOptions {
        let rm = RequestMetadata {
            tool_details: Some(ToolDetails {
                tool_name: "bazel".into(),
                tool_version: "zel".into(),
            }),
            action_id: meta.action_id.clone(),
            tool_invocation_id: self.cfg.tool_invocation_id.clone(),
            correlated_invocations_id: self.cfg.correlated_invocations_id.clone(),
            action_mnemonic: meta.mnemonic.clone(),
            target_id: meta.target_id.clone(),
            configuration_id: String::new(),
        };
        CallOptions {
            timeout: if with_timeout {
                Some(self.cfg.timeout)
            } else {
                None
            },
            metadata: vec![(
                REQUEST_METADATA_HEADER.into(),
                base64_encode(&rm.encode_to_vec()),
            )],
        }
    }

    fn unary<M: Message, R: Message>(
        &self,
        ch: &Channel,
        method: &str,
        req: &M,
        meta: &ActionMeta,
    ) -> Result<R> {
        let bytes = req.encode_to_vec();
        let opts = self.opts(meta, true);
        let resp = self.cfg.retry.run(|| ch.unary(method, &bytes, &opts))?;
        Ok(R::decode(&resp)?)
    }

    fn exec_channel(&self) -> Result<&Arc<Channel>> {
        self.executor
            .as_ref()
            .ok_or_else(|| RemoteError::Other("no --remote_executor configured".into()))
    }

    pub fn close(&self) {
        self.cache.close();
        if let Some(e) = &self.executor {
            e.close();
        }
    }

    // ------------------------------------------------------------------ capabilities

    fn fetch_caps(&self, ch: &Channel) -> Result<ServerCapabilities> {
        let req = GetCapabilitiesRequest {
            instance_name: self.cfg.instance_name.clone(),
        };
        let meta = ActionMeta {
            action_id: "capabilities".into(),
            ..Default::default()
        };
        self.unary(ch, CAPS, &req, &meta).map_err(|e| match e {
            RemoteError::Grpc(g) if g.credential_helper_error => RemoteError::Grpc(g),
            other => RemoteError::Capabilities(other.to_string()),
        })
    }

    /// Queries and caches capabilities for the cache channel.
    pub fn cache_capabilities(&self) -> Result<ServerCapabilities> {
        if let Some(c) = self.cache_caps.lock().unwrap().clone() {
            return Ok(c);
        }
        let caps = self.fetch_caps(&self.cache)?;
        if let Some(ex) = &self.executor {
            if Arc::ptr_eq(&self.cache, ex) {
                *self.exec_caps.lock().unwrap() = Some(caps.clone());
            }
        }
        *self.cache_caps.lock().unwrap() = Some(caps.clone());
        Ok(caps)
    }

    /// Queries and caches capabilities for the executor channel (or cache channel when unset).
    pub fn capabilities(&self) -> Result<ServerCapabilities> {
        if let Some(ex) = &self.executor {
            if Arc::ptr_eq(&self.cache, ex) {
                return self.cache_capabilities();
            }
            if let Some(c) = self.exec_caps.lock().unwrap().clone() {
                return Ok(c);
            }
            let caps = self.fetch_caps(ex)?;
            *self.exec_caps.lock().unwrap() = Some(caps.clone());
            Ok(caps)
        } else {
            self.cache_capabilities()
        }
    }

    fn check_cache_caps(&self, caps: &ServerCapabilities, want_upload: bool) -> Result<()> {
        let (_ver, warn) = check_api_version(caps).map_err(RemoteError::Capabilities)?;
        if let Some(w) = warn {
            self.warnings.lock().unwrap().push(w);
        }
        let want = self.cfg.digest_fn.enum_value();
        let cc = caps.cache_capabilities.clone().unwrap_or_default();
        if !cc.digest_functions.contains(&want) {
            return Err(RemoteError::Capabilities(format!(
                "Remote cache does not support the digest function {:?} (supports {:?})",
                self.cfg.digest_fn, cc.digest_functions
            )));
        }
        if self.cfg.cache_compression && !cc.supported_compressors.contains(&COMPRESSOR_ZSTD) {
            return Err(RemoteError::Capabilities(
                "Remote cache does not support ZSTD compression (--remote_cache_compression)"
                    .into(),
            ));
        }
        if cc.action_cache_update_capabilities.is_none() && want_upload {
            self.warnings.lock().unwrap().push(
                "Remote Action Cache is set to read-only (action_cache_update_capabilities omitted by server); local results will not be uploaded."
                    .into(),
            );
        }
        if self.cfg.result_cache_priority != 0 {
            if let Some(pr) = &cc.cache_priority_capabilities {
                if !pr.priorities.is_empty()
                    && !pr.priorities.iter().any(|r| {
                        self.cfg.result_cache_priority >= r.min_priority
                            && self.cfg.result_cache_priority <= r.max_priority
                    })
                {
                    return Err(RemoteError::Capabilities(format!(
                        "--remote_result_cache_priority={} is outside server's supported range",
                        self.cfg.result_cache_priority
                    )));
                }
            }
        }
        Ok(())
    }

    fn check_exec_caps(&self, caps: &ServerCapabilities) -> Result<()> {
        let (_ver, warn) = check_api_version(caps).map_err(RemoteError::Capabilities)?;
        if let Some(w) = warn {
            self.warnings.lock().unwrap().push(w);
        }
        let ec = caps.execution_capabilities.clone().unwrap_or_default();
        if !ec.exec_enabled {
            return Err(RemoteError::Capabilities(
                "Remote execution is not enabled on the server".into(),
            ));
        }
        let want = self.cfg.digest_fn.enum_value();
        let fns: Vec<i32> = if ec.digest_functions.is_empty() {
            if ec.digest_function == 0 {
                Vec::new()
            } else {
                vec![ec.digest_function]
            }
        } else {
            ec.digest_functions
        };
        if !fns.contains(&want) {
            return Err(RemoteError::Capabilities(format!(
                "Remote executor does not support the digest function {:?}",
                self.cfg.digest_fn
            )));
        }
        if self.cfg.execution_priority != 0 {
            if let Some(pr) = &ec.execution_priority_capabilities {
                if !pr.priorities.is_empty()
                    && !pr.priorities.iter().any(|r| {
                        self.cfg.execution_priority >= r.min_priority
                            && self.cfg.execution_priority <= r.max_priority
                    })
                {
                    return Err(RemoteError::Capabilities(format!(
                        "--remote_execution_priority={} is outside server's supported range",
                        self.cfg.execution_priority
                    )));
                }
            }
        }
        Ok(())
    }

    /// `RemoteServerCapabilities.checkClientServerCompatibility`:
    /// checks the cache endpoint with cache rules, and when `need_exec` is true also checks the
    /// executor endpoint (sharing a single `GetCapabilities` RPC when both endpoints share a
    /// channel, or checking each independently when separate).
    pub fn check_compatibility(&self, need_exec: bool) -> Result<()> {
        self.check_compatibility_ext(need_exec, true)
    }

    pub fn check_compatibility_ext(&self, need_exec: bool, want_upload: bool) -> Result<()> {
        let c_caps = self.cache_capabilities()?;
        if need_exec {
            if let Some(ex) = &self.executor {
                if Arc::ptr_eq(&self.cache, ex) {
                    // Shared channel: check execution enabled first, then cache rules.
                    self.check_exec_caps(&c_caps)?;
                    self.check_cache_caps(&c_caps, want_upload)?;
                    return Ok(());
                }
            }
            self.check_cache_caps(&c_caps, want_upload)?;
            let e_caps = self.capabilities()?;
            self.check_exec_caps(&e_caps)?;
        } else {
            self.check_cache_caps(&c_caps, want_upload)?;
        }
        Ok(())
    }

    /// Returns `true` only when `cache_capabilities.action_cache_update_capabilities.update_enabled`
    /// is explicitly `true` (missing `action_cache_update_capabilities` defaults to `false`).
    pub fn remote_cache_supports_update(&self) -> bool {
        // Re-read fresh capabilities if uncached; never default to true on missing message or error.
        let caps = self
            .cache_caps
            .lock()
            .unwrap()
            .clone()
            .or_else(|| self.cache_capabilities().ok());
        caps.and_then(|c| c.cache_capabilities)
            .and_then(|c| c.action_cache_update_capabilities)
            .map(|u| u.update_enabled)
            .unwrap_or(false)
    }

    pub fn allows_absolute_symlinks(&self) -> bool {
        self.cache_capabilities()
            .ok()
            .and_then(|c| c.cache_capabilities)
            .map(|c| c.symlink_absolute_path_strategy == symlink_absolute_path_strategy::ALLOWED)
            .unwrap_or(false)
    }

    /// Server's negotiated REAPI version supports `Command.output_paths` (>= 2.1).
    pub fn supports_output_paths(&self) -> bool {
        match self
            .capabilities()
            .ok()
            .and_then(|c| check_api_version(&c).ok())
        {
            Some((negotiated, _)) => (negotiated.major, negotiated.minor) >= (2, 1),
            None => true,
        }
    }

    pub fn max_batch_size(&self) -> i64 {
        let m = self
            .cache_capabilities()
            .ok()
            .and_then(|c| c.cache_capabilities)
            .map(|c| c.max_batch_total_size_bytes)
            .unwrap_or(0);
        if m > 0 {
            m
        } else {
            DEFAULT_MAX_BATCH
        }
    }

    fn should_compress(&self, uncompressed_size: i64) -> bool {
        self.cfg.cache_compression
            && uncompressed_size > 0
            && (uncompressed_size as usize) >= self.cfg.compression_threshold
    }

    // ------------------------------------------------------------------ resource names

    fn blob_prefix(&self) -> String {
        if self.cfg.instance_name.is_empty() {
            String::new()
        } else {
            format!("{}/", self.cfg.instance_name)
        }
    }

    pub fn read_resource_name(&self, d: &Digest) -> String {
        self.read_resource_name_ext(d, false)
    }

    pub fn read_resource_name_ext(&self, d: &Digest, compressed: bool) -> String {
        let kind = if compressed {
            "compressed-blobs/zstd"
        } else {
            "blobs"
        };
        match self.cfg.digest_fn.resource_name() {
            Some(f) => format!(
                "{}{kind}/{f}/{}/{}",
                self.blob_prefix(),
                d.hash,
                d.size_bytes
            ),
            None => format!("{}{kind}/{}/{}", self.blob_prefix(), d.hash, d.size_bytes),
        }
    }

    pub fn write_resource_name(&self, uuid: &str, d: &Digest) -> String {
        self.write_resource_name_ext(uuid, d, false)
    }

    pub fn write_resource_name_ext(&self, uuid: &str, d: &Digest, compressed: bool) -> String {
        let kind = if compressed {
            "compressed-blobs/zstd"
        } else {
            "blobs"
        };
        match self.cfg.digest_fn.resource_name() {
            Some(f) => format!(
                "{}uploads/{uuid}/{kind}/{f}/{}/{}",
                self.blob_prefix(),
                d.hash,
                d.size_bytes
            ),
            None => format!(
                "{}uploads/{uuid}/{kind}/{}/{}",
                self.blob_prefix(),
                d.hash,
                d.size_bytes
            ),
        }
    }

    // ------------------------------------------------------------------ CAS

    pub fn find_missing_blobs(&self, digests: &[Digest], meta: &ActionMeta) -> Result<Vec<Digest>> {
        let non_empty: Vec<Digest> = digests
            .iter()
            .filter(|d| d.size_bytes > 0)
            .cloned()
            .collect();
        if non_empty.is_empty() {
            return Ok(Vec::new());
        }
        let mut missing = Vec::new();
        for chunk in non_empty.chunks(FIND_MISSING_CHUNK) {
            let req = FindMissingBlobsRequest {
                instance_name: self.cfg.instance_name.clone(),
                blob_digests: chunk.to_vec(),
                digest_function: self.cfg.digest_fn.proto_value(),
            };
            let r: FindMissingBlobsResponse =
                self.unary(&self.cache, &format!("{CAS}FindMissingBlobs"), &req, meta)?;
            missing.extend(r.missing_blob_digests);
        }
        // Any queried digest NOT reported missing is currently present on the server.
        let missing_set: HashSet<&Digest> = missing.iter().collect();
        let mut trk = self.uploads.lock().unwrap();
        for d in &non_empty {
            if !missing_set.contains(d) {
                trk.present.insert(d.clone());
            }
        }
        Ok(missing)
    }

    /// `BatchUpdateBlobs`; the caller guarantees the total size fits the server's limit.
    pub fn batch_update_blobs(&self, blobs: &[(Digest, Vec<u8>)], meta: &ActionMeta) -> Result<()> {
        let non_empty: Vec<&(Digest, Vec<u8>)> =
            blobs.iter().filter(|(d, _)| d.size_bytes > 0).collect();
        if non_empty.is_empty() {
            return Ok(());
        }
        let req = BatchUpdateBlobsRequest {
            instance_name: self.cfg.instance_name.clone(),
            requests: non_empty
                .iter()
                .map(|(d, data)| {
                    if self.should_compress(d.size_bytes) {
                        BatchUpdateBlobsRequestItem {
                            digest: Some(d.clone()),
                            data: zel_util::zstd::compress(data),
                            compressor: COMPRESSOR_ZSTD,
                        }
                    } else {
                        BatchUpdateBlobsRequestItem {
                            digest: Some(d.clone()),
                            data: data.clone(),
                            compressor: 0,
                        }
                    }
                })
                .collect(),
            digest_function: self.cfg.digest_fn.proto_value(),
        };
        let r: BatchUpdateBlobsResponse =
            self.unary(&self.cache, &format!("{CAS}BatchUpdateBlobs"), &req, meta)?;
        for item in r.responses {
            if let Some(s) = item.status {
                if s.code != code::OK {
                    let d = item.digest.unwrap_or_default();
                    self.uploads.lock().unwrap().present.remove(&d);
                    return Err(RemoteError::Grpc(GrpcStatus::new(
                        s.code,
                        format!(
                            "failed to upload blob {}/{}: {}",
                            d.hash, d.size_bytes, s.message
                        ),
                    )));
                }
            }
        }
        let mut trk = self.uploads.lock().unwrap();
        for (d, _) in non_empty {
            trk.present.insert(d.clone());
        }
        Ok(())
    }

    pub fn batch_read_blobs(
        &self,
        digests: &[Digest],
        meta: &ActionMeta,
    ) -> Result<HashMap<Digest, Vec<u8>>> {
        let mut out = HashMap::new();
        let mut non_empty = Vec::new();
        for d in digests {
            if d.size_bytes == 0 {
                out.insert(d.clone(), Vec::new());
            } else {
                non_empty.push(d.clone());
            }
        }
        let accept = if self.cfg.cache_compression {
            vec![0, COMPRESSOR_ZSTD]
        } else {
            vec![]
        };
        for chunk in non_empty.chunks(FIND_MISSING_CHUNK) {
            let req = BatchReadBlobsRequest {
                instance_name: self.cfg.instance_name.clone(),
                digests: chunk.to_vec(),
                acceptable_compressors: accept.clone(),
                digest_function: self.cfg.digest_fn.proto_value(),
            };
            let r: BatchReadBlobsResponse =
                self.unary(&self.cache, &format!("{CAS}BatchReadBlobs"), &req, meta)?;
            for item in r.responses {
                let d = item.digest.unwrap_or_default();
                let st = item.status.unwrap_or_default();
                if st.code == code::NOT_FOUND {
                    return Err(RemoteError::NotFound);
                }
                if st.code != code::OK {
                    return Err(RemoteError::Grpc(GrpcStatus::new(st.code, st.message)));
                }
                let raw = if item.compressor == COMPRESSOR_ZSTD {
                    zel_util::zstd::decompress(&item.data).map_err(|e| {
                        RemoteError::Other(format!(
                            "Failed to decompress zstd BatchReadBlobs item {}/{}: {e}",
                            d.hash, d.size_bytes
                        ))
                    })?
                } else {
                    item.data
                };
                self.verify(&d, &raw)?;
                out.insert(d, raw);
            }
        }
        Ok(out)
    }

    fn verify(&self, d: &Digest, data: &[u8]) -> Result<()> {
        if (data.len() as i64) != d.size_bytes {
            return Err(RemoteError::Other(format!(
                "Download of {}/{} failed: size mismatch (got {})",
                d.hash,
                d.size_bytes,
                data.len()
            )));
        }
        if !self.cfg.verify_downloads {
            return Ok(());
        }
        let actual = self.cfg.digest_fn.digest(data);
        if actual.hash != d.hash || actual.size_bytes != d.size_bytes {
            return Err(RemoteError::Other(format!(
                "Download of {}/{} failed: digest mismatch (got {}/{})",
                d.hash, d.size_bytes, actual.hash, actual.size_bytes
            )));
        }
        Ok(())
    }

    /// `ByteStream.Write` of a blob source via [`Chunker`], supporting zstd `compressed-blobs`
    /// and Bazel's `committed_size` rules (`-1` allowed only when compressed).
    pub fn write_source_bytestream(
        &self,
        d: &Digest,
        source: &BlobSource,
        meta: &ActionMeta,
    ) -> Result<()> {
        if d.size_bytes == 0 {
            return Ok(());
        }
        let compress = self.should_compress(d.size_bytes);
        let opts = self.opts(meta, true);
        let mut refreshed_auth = false;
        let res = self.cfg.retry.run(|| {
            let uuid = zel_util::uuid_v4();
            let resource = self.write_resource_name_ext(&uuid, d, compress);
            let mut chunker = Chunker::new(source.clone(), WRITE_CHUNK, compress);
            let mut msgs: Vec<Vec<u8>> = Vec::new();
            let mut total_wire: i64 = 0;
            while let Some((off, bytes, is_last)) = chunker
                .next_chunk()
                .map_err(|e| GrpcStatus::new(code::INTERNAL, e.to_string()))?
            {
                total_wire += bytes.len() as i64;
                msgs.push(
                    WriteRequest {
                        resource_name: if off == 0 {
                            resource.clone()
                        } else {
                            String::new()
                        },
                        write_offset: off,
                        finish_write: is_last,
                        data: bytes,
                    }
                    .encode_to_vec(),
                );
                if is_last {
                    break;
                }
            }
            let mut chunks = msgs.into_iter();
            let mut committed: Option<i64> = None;
            let r = self
                .cache
                .call(&format!("{BS}Write"), &mut chunks, &opts, &mut |m| {
                    committed = Some(
                        WriteResponse::decode(&m)
                            .map_err(|e| GrpcStatus::new(code::INTERNAL, e.to_string()))?
                            .committed_size,
                    );
                    Ok(())
                });
            match r {
                Ok(()) => match committed {
                    Some(c) if compress && (c == -1 || c == total_wire || c == d.size_bytes) => {
                        Ok(())
                    }
                    Some(c) if !compress && c == d.size_bytes => Ok(()),
                    Some(c) => Err(GrpcStatus::new(
                        code::INTERNAL,
                        format!(
                            "ByteStream.Write committed_size {c} did not match expected size {}",
                            d.size_bytes
                        ),
                    )),
                    None => Err(GrpcStatus::new(code::INTERNAL, "no WriteResponse")),
                },
                Err(e) if e.code == code::ALREADY_EXISTS => Ok(()),
                Err(e)
                    if e.code == code::UNAUTHENTICATED
                        && !e.credential_helper_error
                        && !refreshed_auth
                        && self.cache.credentials().is_some() =>
                {
                    refreshed_auth = true;
                    self.cache.refresh_credentials()?;
                    // Trigger immediate retry via ABORTED/UNAVAILABLE? No, rerun once inline!
                    let uuid2 = zel_util::uuid_v4();
                    let resource2 = self.write_resource_name_ext(&uuid2, d, compress);
                    chunker.reset();
                    let mut msgs2 = Vec::new();
                    while let Some((off, bytes, is_last)) = chunker
                        .next_chunk()
                        .map_err(|ie| GrpcStatus::new(code::INTERNAL, ie.to_string()))?
                    {
                        msgs2.push(
                            WriteRequest {
                                resource_name: if off == 0 {
                                    resource2.clone()
                                } else {
                                    String::new()
                                },
                                write_offset: off,
                                finish_write: is_last,
                                data: bytes,
                            }
                            .encode_to_vec(),
                        );
                        if is_last {
                            break;
                        }
                    }
                    let mut it2 = msgs2.into_iter();
                    let mut c2 = None;
                    self.cache
                        .call(&format!("{BS}Write"), &mut it2, &opts, &mut |m| {
                            c2 = Some(
                                WriteResponse::decode(&m)
                                    .map_err(|de| GrpcStatus::new(code::INTERNAL, de.to_string()))?
                                    .committed_size,
                            );
                            Ok(())
                        })
                }
                Err(e) => Err(e),
            }
        });
        match res {
            Ok(()) => {
                self.uploads.lock().unwrap().present.insert(d.clone());
                Ok(())
            }
            Err(e) => {
                self.uploads.lock().unwrap().present.remove(d);
                Err(e.into())
            }
        }
    }

    /// `ByteStream.Write` of an in-memory blob.
    pub fn write_blob_bytestream(&self, d: &Digest, data: &[u8], meta: &ActionMeta) -> Result<()> {
        self.write_source_bytestream(d, &BlobSource::Bytes(data.to_vec()), meta)
    }

    /// `ByteStream.Read` of a whole blob (resuming from `read_offset` across retries when
    /// uncompressed, supporting zstd `compressed-blobs`, rejecting extra trailing bytes, and
    /// falling back to `BatchReadBlobs` on `UNIMPLEMENTED`).
    pub fn download_blob(&self, d: &Digest, meta: &ActionMeta) -> Result<Vec<u8>> {
        if d.size_bytes == 0 {
            return Ok(Vec::new());
        }
        let compress = self.should_compress(d.size_bytes);
        let opts = self.opts(meta, true);

        let res = if compress {
            let req = ReadRequest {
                resource_name: self.read_resource_name_ext(d, true),
                read_offset: 0,
                read_limit: 0,
            }
            .encode_to_vec();
            let mut wire = Vec::new();
            let stream_res = self.cfg.retry.run(|| {
                let req_now = ReadRequest {
                    resource_name: self.read_resource_name_ext(d, true),
                    read_offset: wire.len() as i64,
                    read_limit: 0,
                }
                .encode_to_vec();
                self.cache
                    .server_stream(&format!("{BS}Read"), &req_now, &opts, &mut |m| {
                        let r = ReadResponse::decode(&m)
                            .map_err(|e| GrpcStatus::new(code::INTERNAL, e.to_string()))?;
                        wire.extend(r.data);
                        Ok(())
                    })
            });
            let _ = req;
            stream_res.and_then(|()| {
                zel_util::zstd::decompress(&wire).map_err(|e| {
                    GrpcStatus::new(
                        code::DATA_LOSS,
                        format!(
                            "zstd decompression failed for {}/{}: {e}",
                            d.hash, d.size_bytes
                        ),
                    )
                })
            })
        } else {
            let expected = d.size_bytes.max(0) as usize;
            let mut data = Vec::with_capacity(expected);
            let stream_res = self.cfg.retry.run(|| {
                let req = ReadRequest {
                    resource_name: self.read_resource_name_ext(d, false),
                    read_offset: data.len() as i64,
                    read_limit: 0,
                }
                .encode_to_vec();
                let r = self
                    .cache
                    .server_stream(&format!("{BS}Read"), &req, &opts, &mut |m| {
                        let r = ReadResponse::decode(&m)
                            .map_err(|e| GrpcStatus::new(code::INTERNAL, e.to_string()))?;
                        if data.len() + r.data.len() > expected {
                            return Err(GrpcStatus::new(
                                code::DATA_LOSS,
                                format!(
                                    "ByteStream.Read returned more than {} bytes for {}",
                                    expected, d.hash
                                ),
                            ));
                        }
                        data.extend(r.data);
                        Ok(())
                    });
                match r {
                    // Bazel ignores a trailing stream error once all expected bytes were received.
                    Err(_) if data.len() == expected => Ok(()),
                    other => other,
                }
            });
            stream_res.map(|()| data)
        };

        match res {
            Ok(data) => {
                self.verify(d, &data)?;
                Ok(data)
            }
            Err(e) if e.code == code::UNIMPLEMENTED => {
                let mut m = self.batch_read_blobs(std::slice::from_ref(d), meta)?;
                m.remove(d).ok_or(RemoteError::NotFound)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Ensures all `blobs` exist in the CAS, skipping size-0 and already-known-present digests
    /// (unless `force` is true), coalescing concurrent uploads of the same digest.
    pub fn ensure_blobs_present(
        &self,
        blobs: &BTreeMap<Digest, BlobSource>,
        meta: &ActionMeta,
    ) -> Result<usize> {
        self.ensure_blobs_present_ext(blobs, meta, false)
    }

    pub fn ensure_blobs_present_ext(
        &self,
        blobs: &BTreeMap<Digest, BlobSource>,
        meta: &ActionMeta,
        force: bool,
    ) -> Result<usize> {
        let mut to_query = Vec::new();
        {
            let mut trk = self.uploads.lock().unwrap();
            for d in blobs.keys() {
                if d.size_bytes <= 0 {
                    continue;
                }
                if force {
                    trk.present.remove(d);
                    to_query.push(d.clone());
                } else if !trk.present.contains(d) {
                    to_query.push(d.clone());
                }
            }
        }
        if to_query.is_empty() {
            return Ok(0);
        }
        let missing = if force {
            to_query
        } else {
            self.find_missing_blobs(&to_query, meta)?
        };
        if missing.is_empty() {
            return Ok(0);
        }
        self.upload_blobs_coalesced(&missing, blobs, meta, force)?;
        Ok(missing.len())
    }

    fn upload_blobs_coalesced(
        &self,
        digests: &[Digest],
        blobs: &BTreeMap<Digest, BlobSource>,
        meta: &ActionMeta,
        force: bool,
    ) -> Result<()> {
        let mut claimed = Vec::new();
        {
            let mut trk = self.uploads.lock().unwrap();
            for d in digests {
                if d.size_bytes <= 0 {
                    continue;
                }
                while trk.inflight.contains(d) {
                    trk = self.upload_cv.wait(trk).unwrap();
                }
                if !force && trk.present.contains(d) {
                    continue;
                }
                trk.inflight.insert(d.clone());
                claimed.push(d.clone());
            }
        }
        let res = self.upload_blobs(&claimed, blobs, meta);
        {
            let mut trk = self.uploads.lock().unwrap();
            for d in &claimed {
                trk.inflight.remove(d);
            }
            self.upload_cv.notify_all();
        }
        res
    }

    /// Uploads the given digests directly (`BatchUpdateBlobs` for small, `ByteStream.Write` for
    /// large).
    pub fn upload_blobs(
        &self,
        digests: &[Digest],
        blobs: &BTreeMap<Digest, BlobSource>,
        meta: &ActionMeta,
    ) -> Result<()> {
        let max_batch = self.max_batch_size();
        let mut batch: Vec<(Digest, Vec<u8>)> = Vec::new();
        let mut batch_bytes = 0i64;
        let mut large: Vec<(Digest, BlobSource)> = Vec::new();
        for d in digests {
            if d.size_bytes <= 0 {
                continue;
            }
            let src = blobs.get(d).ok_or_else(|| {
                RemoteError::Other(format!(
                    "missing local source for blob {}/{}",
                    d.hash, d.size_bytes
                ))
            })?;
            if d.size_bytes >= max_batch {
                large.push((d.clone(), src.clone()));
                continue;
            }
            let data = src.read()?;
            if batch_bytes + d.size_bytes > max_batch && !batch.is_empty() {
                self.batch_update_blobs(&batch, meta)?;
                batch.clear();
                batch_bytes = 0;
            }
            batch_bytes += d.size_bytes;
            batch.push((d.clone(), data));
        }
        self.batch_update_blobs(&batch, meta)?;
        let err: Mutex<Option<RemoteError>> = Mutex::new(None);
        for group in large.chunks(4) {
            std::thread::scope(|s| {
                for (d, src) in group {
                    let err = &err;
                    s.spawn(move || {
                        if let Err(e) = self.write_source_bytestream(d, src, meta) {
                            err.lock().unwrap().get_or_insert(e);
                        }
                    });
                }
            });
            if let Some(e) = err.lock().unwrap().take() {
                return Err(e);
            }
        }
        Ok(())
    }

    /// `GetTree` server-streaming RPC with pagination. Returns a `Tree` whose root is the
    /// directory with `root` digest.
    pub fn get_tree(&self, root: &Digest, meta: &ActionMeta) -> Result<Tree> {
        if root.size_bytes == 0 {
            return Ok(Tree {
                root: Some(Directory::default()),
                children: Vec::new(),
            });
        }
        let opts = self.opts(meta, true);
        let mut dirs: Vec<Directory> = Vec::new();
        let mut token = String::new();
        loop {
            let req = GetTreeRequest {
                instance_name: self.cfg.instance_name.clone(),
                root_digest: Some(root.clone()),
                page_size: 0,
                page_token: token.clone(),
                digest_function: self.cfg.digest_fn.proto_value(),
            }
            .encode_to_vec();
            let (call_dirs, next_token) = self.cfg.retry.run(|| {
                let mut stream_dirs = Vec::new();
                let mut last_token = String::new();
                self.cache
                    .server_stream(&format!("{CAS}GetTree"), &req, &opts, &mut |m| {
                        let r = GetTreeResponse::decode(&m)
                            .map_err(|e| GrpcStatus::new(code::INTERNAL, e.to_string()))?;
                        stream_dirs.extend(r.directories);
                        if !r.next_page_token.is_empty() {
                            last_token = r.next_page_token;
                        }
                        Ok(())
                    })?;
                Ok((stream_dirs, last_token))
            })?;
            let made_progress = !call_dirs.is_empty();
            dirs.extend(call_dirs);
            if next_token.is_empty() || (!made_progress && next_token == token) {
                break;
            }
            token = next_token;
        }
        let pos = dirs
            .iter()
            .position(|d| self.cfg.digest_fn.hash(&d.encode_to_vec()) == root.hash)
            .ok_or_else(|| {
                RemoteError::Other("GetTree response did not contain the root directory".into())
            })?;
        let root_dir = dirs.remove(pos);
        Ok(Tree {
            root: Some(root_dir),
            children: dirs,
        })
    }

    // ------------------------------------------------------------------ action cache

    pub fn get_action_result(
        &self,
        action: &Digest,
        meta: &ActionMeta,
    ) -> Result<Option<ActionResult>> {
        self.get_action_result_inlined(action, &[], meta)
    }

    pub fn get_action_result_inlined(
        &self,
        action: &Digest,
        inline_output_files: &[String],
        meta: &ActionMeta,
    ) -> Result<Option<ActionResult>> {
        let req = GetActionResultRequest {
            instance_name: self.cfg.instance_name.clone(),
            action_digest: Some(action.clone()),
            inline_stdout: false,
            inline_stderr: false,
            inline_output_files: inline_output_files.to_vec(),
            digest_function: self.cfg.digest_fn.proto_value(),
        };
        match self.unary::<_, ActionResult>(
            &self.cache,
            &format!("{AC}GetActionResult"),
            &req,
            meta,
        ) {
            Ok(r) => Ok(Some(r)),
            Err(RemoteError::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn update_action_result(
        &self,
        action: &Digest,
        result: &ActionResult,
        meta: &ActionMeta,
    ) -> Result<()> {
        let results_cache_policy =
            (self.cfg.result_cache_priority != 0).then_some(ResultsCachePolicy {
                priority: self.cfg.result_cache_priority,
            });
        let req = UpdateActionResultRequest {
            instance_name: self.cfg.instance_name.clone(),
            action_digest: Some(action.clone()),
            action_result: Some(result.clone()),
            results_cache_policy,
            digest_function: self.cfg.digest_fn.proto_value(),
        };
        let _: ActionResult =
            self.unary(&self.cache, &format!("{AC}UpdateActionResult"), &req, meta)?;
        Ok(())
    }

    // ------------------------------------------------------------------ execution

    /// `Execution.Execute`, resuming with `WaitExecution` when the stream breaks before the
    /// operation is done, and retrying transient `ExecuteResponse.status` / `Operation.error`
    /// via fresh `Execute` (matching `GrpcRemoteExecutor` + `ExecuteRetrier`).
    /// `ExecuteResponse.status` with `DEADLINE_EXCEEDED` is an action timeout and is NEVER retried.
    pub fn execute(
        &self,
        action: &Digest,
        skip_cache_lookup: bool,
        meta: &ActionMeta,
        on_stage: &mut dyn FnMut(i32),
    ) -> Result<ExecuteResponse> {
        let ch = self.exec_channel()?.clone();
        let execution_policy = (self.cfg.execution_priority != 0).then_some(ExecutionPolicy {
            priority: self.cfg.execution_priority,
        });
        let results_cache_policy =
            (self.cfg.result_cache_priority != 0).then_some(ResultsCachePolicy {
                priority: self.cfg.result_cache_priority,
            });
        let req = ExecuteRequest {
            instance_name: self.cfg.instance_name.clone(),
            skip_cache_lookup,
            action_digest: Some(action.clone()),
            execution_policy,
            results_cache_policy,
            digest_function: self.cfg.digest_fn.proto_value(),
        }
        .encode_to_vec();
        let opts = self.opts(meta, false);
        let mut op_name: Option<String> = None;
        let mut attempt = 0u32;
        let mut empty_wait_eofs = 0u32;
        loop {
            if self.cfg.retry.is_caller_cancelled() {
                return Err(RemoteError::Grpc(GrpcStatus::new(
                    code::CANCELLED,
                    "Interrupted",
                )));
            }
            let mut done: Option<Operation> = None;
            let current = op_name.clone();
            let mut seen_name: Option<String> = None;
            let mut saw_any_op = false;
            let mut handle = |m: Vec<u8>| -> std::result::Result<(), GrpcStatus> {
                let op = Operation::decode(&m)
                    .map_err(|e| GrpcStatus::new(code::INTERNAL, e.to_string()))?;
                saw_any_op = true;
                if !op.name.is_empty() {
                    seen_name = Some(op.name.clone());
                }
                if let Some(md) = &op.metadata {
                    if md.type_url.ends_with("ExecuteOperationMetadata") {
                        if let Ok(m) = ExecuteOperationMetadata::decode(&md.value) {
                            on_stage(m.stage);
                        }
                    }
                }
                if op.done {
                    done = Some(op);
                }
                Ok(())
            };
            let res = match &current {
                None => ch.server_stream(&format!("{EXEC}Execute"), &req, &opts, &mut handle),
                Some(n) => {
                    let wait = WaitExecutionRequest { name: n.clone() }.encode_to_vec();
                    ch.server_stream(&format!("{EXEC}WaitExecution"), &wait, &opts, &mut handle)
                }
            };
            if seen_name.is_some() {
                op_name = seen_name.take();
            }
            match (res, done) {
                (_, Some(op)) => {
                    match self.finish_operation(op) {
                        Ok(resp) => return Ok(resp),
                        Err(RemoteError::ActionTimeout(st)) => {
                            // Action timeout on worker: NEVER retried.
                            return Err(RemoteError::ActionTimeout(st));
                        }
                        Err(RemoteError::Exec(st)) => {
                            // Missing blobs must surface immediately so RemoteSpawnRunner can
                            // force-upload missing inputs and re-run.
                            if is_missing_blobs_failure(&st) {
                                return Err(RemoteError::Exec(st));
                            }
                            let as_grpc = GrpcStatus::from_rpc_status(st.clone());
                            if RetryPolicy::is_retriable_with_cancel(
                                &as_grpc,
                                self.cfg.retry.is_caller_cancelled(),
                            ) && attempt < self.cfg.retry.max_retries
                            {
                                op_name = None;
                                std::thread::sleep(self.cfg.retry.delay(attempt));
                                attempt += 1;
                                continue;
                            }
                            return Err(RemoteError::Exec(st));
                        }
                        Err(RemoteError::Grpc(g)) => {
                            // Operation.error (LRO RPC error, including DEADLINE_EXCEEDED) is
                            // retried via fresh Execute!
                            if RetryPolicy::is_retriable_with_cancel(
                                &g,
                                self.cfg.retry.is_caller_cancelled(),
                            ) && attempt < self.cfg.retry.max_retries
                            {
                                op_name = None;
                                std::thread::sleep(self.cfg.retry.delay(attempt));
                                attempt += 1;
                                continue;
                            }
                            return Err(RemoteError::Grpc(g));
                        }
                        Err(other) => return Err(other),
                    }
                }
                (Ok(()), None) => {
                    // Clean EOF on initial Execute with no Operation fails immediately
                    // (`GrpcRemoteExecutor`: no retry).
                    if op_name.is_none() {
                        return Err(RemoteError::Other(
                            "Execution stream closed without returning an Operation".into(),
                        ));
                    }
                    // Clean EOF after receiving an in-progress Operation resumes via
                    // WaitExecution without consuming retry budget!
                    if saw_any_op {
                        empty_wait_eofs = 0;
                    } else {
                        empty_wait_eofs += 1;
                        if empty_wait_eofs > 32 {
                            return Err(RemoteError::Other(
                                "WaitExecution repeatedly closed without yielding an Operation"
                                    .into(),
                            ));
                        }
                    }
                    continue;
                }
                (Err(e), None) => {
                    // WaitExecution NOT_FOUND clears op_name and retries fresh Execute!
                    if e.code == code::NOT_FOUND && current.is_some() {
                        op_name = None;
                        if attempt >= self.cfg.retry.max_retries {
                            return Err(e.into());
                        }
                    } else if !RetryPolicy::is_retriable_with_cancel(
                        &e,
                        self.cfg.retry.is_caller_cancelled(),
                    ) || attempt >= self.cfg.retry.max_retries
                    {
                        return Err(e.into());
                    }
                    std::thread::sleep(self.cfg.retry.delay(attempt));
                    attempt += 1;
                }
            }
        }
    }

    fn finish_operation(&self, op: Operation) -> Result<ExecuteResponse> {
        if let Some(e) = op.error {
            if e.code != code::OK {
                // Operation.error is an LRO RPC-level error (retriable even for DEADLINE_EXCEEDED).
                return Err(RemoteError::Grpc(GrpcStatus::from_rpc_status(e)));
            }
        }
        let any = op.response.ok_or_else(|| {
            RemoteError::Other("done operation has neither response nor error".into())
        })?;
        if !any.type_url.ends_with("ExecuteResponse") {
            return Err(RemoteError::Other(format!(
                "unexpected operation response type {}",
                any.type_url
            )));
        }
        let resp = ExecuteResponse::decode(&any.value)?;
        if let Some(s) = &resp.status {
            if s.code == code::DEADLINE_EXCEEDED {
                return Err(RemoteError::ActionTimeout(s.clone()));
            }
            if s.code != code::OK {
                return Err(RemoteError::Exec(s.clone()));
            }
        }
        if resp.result.is_none() {
            return Err(RemoteError::Other(
                "Unexpected result of remote execution: no result".into(),
            ));
        }
        Ok(resp)
    }
}

/// Strict REAPI "missing blobs" precondition check matching `GrpcRemoteExecutor.isMissingBlobs`:
/// `FAILED_PRECONDITION` with non-empty `PreconditionFailure.violations` all of type `"MISSING"`,
/// and no unexpected sibling detail types.
pub fn is_missing_blobs_failure(s: &RpcStatus) -> bool {
    GrpcStatus::from_rpc_status(s.clone()).is_missing_blobs_error()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_server::FakeReapiServer;

    pub(crate) fn client_for(server: &FakeReapiServer, exec: bool) -> ReapiClient {
        let ch = Arc::new(Channel::new(server.connector(), 4, vec![]));
        let cfg = ReapiConfig {
            retry: RetryPolicy {
                initial_delay: Duration::from_millis(1),
                ..Default::default()
            },
            ..Default::default()
        };
        ReapiClient::new(ch.clone(), if exec { Some(ch) } else { None }, cfg)
    }

    fn meta() -> ActionMeta {
        ActionMeta::default()
    }

    #[test]
    fn base64_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn capabilities_semver_deprecation_and_compatibility() {
        let s = FakeReapiServer::new();
        let c = client_for(&s, true);
        let caps = c.capabilities().unwrap();
        assert!(caps.execution_capabilities.unwrap().exec_enabled);
        c.check_compatibility(true).unwrap();
        assert!(c.supports_output_paths());
        assert_eq!(c.max_batch_size(), 4 << 20);
        assert_eq!(s.request_count("GetCapabilities"), 1);
        c.capabilities().unwrap();
        assert_eq!(
            s.request_count("GetCapabilities"),
            1,
            "capabilities are cached per channel"
        );

        // Server speaking 2.1..2.11 (having dropped 2.0) IS supported!
        let s_21 = FakeReapiServer::new();
        s_21.configure(|c| {
            c.low_api_version = SemVer {
                major: 2,
                minor: 1,
                patch: 0,
                prerelease: String::new(),
            };
            c.high_api_version = SemVer {
                major: 2,
                minor: 11,
                patch: 0,
                prerelease: String::new(),
            };
        });
        assert!(client_for(&s_21, false).check_compatibility(false).is_ok());

        // Missing low/high API versions default to 0.0.0 -> unsupported!
        let s_empty = FakeReapiServer::new();
        s_empty.configure(|c| c.omit_api_versions = true);
        assert!(client_for(&s_empty, false)
            .check_compatibility(false)
            .is_err());

        // Missing action_cache_update_capabilities defaults to read-only (false) + warning!
        let s_ro = FakeReapiServer::new();
        s_ro.configure(|c| c.omit_update_capabilities = true);
        let c_ro = client_for(&s_ro, false);
        c_ro.check_compatibility(false).unwrap();
        assert!(!c_ro.remote_cache_supports_update());
        assert_eq!(c_ro.take_warnings().len(), 1);

        let s2 = FakeReapiServer::new();
        s2.configure(|c| c.exec_enabled = false);
        assert!(client_for(&s2, true).check_compatibility(true).is_err());
        let s3 = FakeReapiServer::new();
        s3.configure(|c| c.advertise_digest_functions = vec![digest_function::BLAKE3]);
        let e = client_for(&s3, false)
            .check_compatibility(false)
            .unwrap_err();
        assert!(e.to_string().contains("digest function"));
        let s4 = FakeReapiServer::new();
        s4.configure(|c| {
            c.high_api_version = SemVer {
                major: 2,
                minor: 0,
                ..Default::default()
            }
        });
        assert!(!client_for(&s4, false).supports_output_paths());
    }

    #[test]
    fn cas_roundtrip_batch_bytestream_and_zstd_compressed_blobs() {
        let s = FakeReapiServer::new();
        s.configure(|c| {
            c.max_batch_total_size_bytes = 1000;
            c.read_chunk = 100;
        });
        let mut c = client_for(&s, false);
        c.cfg.cache_compression = true;
        c.cfg.compression_threshold = 10;
        c.check_compatibility(false).unwrap();

        let small = b"small compressible blob small compressible blob".to_vec();
        let big: Vec<u8> = (0..200_000u32).map(|i| (i / 16 % 256) as u8).collect();
        let mut blobs = BTreeMap::new();
        let sd = c.cfg.digest_fn.digest(&small);
        let bd = c.cfg.digest_fn.digest(&big);
        blobs.insert(sd.clone(), BlobSource::Bytes(small.clone()));
        blobs.insert(bd.clone(), BlobSource::Bytes(big.clone()));
        assert_eq!(c.ensure_blobs_present(&blobs, &meta()).unwrap(), 2);
        assert_eq!(s.request_count("BatchUpdateBlobs"), 1);
        assert_eq!(s.request_count("ByteStream/Write"), 1);
        assert_eq!(s.compressed_bytestream_writes(), 1);
        assert_eq!(s.get_blob(&bd).unwrap(), big);
        // Second time: tracked in present set -> zero FindMissingBlobs and zero writes!
        let writes = s.cas_write_count();
        assert_eq!(c.ensure_blobs_present(&blobs, &meta()).unwrap(), 0);
        assert_eq!(s.cas_write_count(), writes);
        assert_eq!(c.download_blob(&bd, &meta()).unwrap(), big);
        assert_eq!(s.compressed_bytestream_reads(), 1);
        assert_eq!(c.download_blob(&sd, &meta()).unwrap(), small);
        let m = c.batch_read_blobs(&[sd.clone()], &meta()).unwrap();
        assert_eq!(m[&sd], small);
        // Missing blob -> NotFound.
        let nd = c.cfg.digest_fn.digest(b"never uploaded");
        assert!(matches!(
            c.download_blob(&nd, &meta()),
            Err(RemoteError::NotFound)
        ));
        // Empty blob needs no round trip.
        assert!(c
            .download_blob(&c.cfg.digest_fn.digest(b""), &meta())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn bytestream_resource_names_and_instance() {
        let s = FakeReapiServer::new();
        let mut c = client_for(&s, false);
        c.cfg.instance_name = "projects/x/instances/y".into();
        let d = Digest {
            hash: "ab".repeat(32),
            size_bytes: 5,
        };
        assert_eq!(
            c.read_resource_name(&d),
            format!("projects/x/instances/y/blobs/{}/5", d.hash)
        );
        assert_eq!(
            c.read_resource_name_ext(&d, true),
            format!("projects/x/instances/y/compressed-blobs/zstd/{}/5", d.hash)
        );
        assert_eq!(
            c.write_resource_name("u", &d),
            format!("projects/x/instances/y/uploads/u/blobs/{}/5", d.hash)
        );
        // MD5 is old-style in Bazel: NO /md5/ segment!
        c.cfg.digest_fn = DigestFn::Md5;
        assert_eq!(
            c.read_resource_name(&d),
            format!("projects/x/instances/y/blobs/{}/5", d.hash)
        );
        // BLAKE3 (> 7) includes /blake3/ segment!
        c.cfg.digest_fn = DigestFn::Blake3;
        c.cfg.instance_name.clear();
        assert!(c.read_resource_name(&d).starts_with("blobs/blake3/"));
        assert!(c
            .read_resource_name_ext(&d, true)
            .starts_with("compressed-blobs/zstd/blake3/"));
    }

    #[test]
    fn action_cache_get_update() {
        let s = FakeReapiServer::new();
        let c = client_for(&s, false);
        let ad = c.cfg.digest_fn.digest(b"action");
        assert!(c.get_action_result(&ad, &meta()).unwrap().is_none());
        let ar = ActionResult {
            exit_code: 0,
            stdout_raw: b"out".to_vec(),
            ..Default::default()
        };
        c.update_action_result(&ad, &ar, &meta()).unwrap();
        assert_eq!(c.get_action_result(&ad, &meta()).unwrap().unwrap(), ar);
        s.configure(|c| c.update_enabled = false);
        assert!(c.update_action_result(&ad, &ar, &meta()).is_err());
    }

    #[test]
    fn get_tree_with_pagination() {
        let s = FakeReapiServer::new();
        s.configure(|c| c.get_tree_page_size = 1);
        let c = client_for(&s, false);
        let comp = crate::merkle::MerkleTreeComputer::new(DigestFn::Sha256);
        let t = comp
            .build(&[
                crate::merkle::MerkleInput::bytes("a/b/c.txt", b"c".to_vec(), false),
                crate::merkle::MerkleInput::bytes("a/d.txt", b"d".to_vec(), false),
            ])
            .unwrap();
        c.ensure_blobs_present(&t.blobs, &meta()).unwrap();
        let tree = c.get_tree(&t.root_digest, &meta()).unwrap();
        assert!(
            s.request_count("GetTree") >= 3,
            "pagination issued multiple requests"
        );
        let entries = crate::merkle::flatten_tree(&tree, DigestFn::Sha256).unwrap();
        assert!(entries.iter().any(
            |e| matches!(e, crate::merkle::TreeEntry::File { path, .. } if path == "a/b/c.txt")
        ));
        assert_eq!(tree.root.unwrap(), t.root);
    }

    #[test]
    fn get_tree_streams_multiple_responses_per_call_and_paginates() {
        let s = FakeReapiServer::new();
        // Single RPC stream yielding multiple GetTreeResponse frames (no next_page_token).
        s.configure(|c| {
            c.get_tree_stream_batch_size = 1;
            c.get_tree_page_size = 0;
        });
        let c = client_for(&s, false);
        let comp = crate::merkle::MerkleTreeComputer::new(DigestFn::Sha256);
        let t = comp
            .build(&[
                crate::merkle::MerkleInput::bytes("a/b/c.txt", b"c".to_vec(), false),
                crate::merkle::MerkleInput::bytes("a/d/e.txt", b"e".to_vec(), false),
                crate::merkle::MerkleInput::bytes("f.txt", b"f".to_vec(), false),
            ])
            .unwrap();
        c.ensure_blobs_present(&t.blobs, &meta()).unwrap();
        let tree = c.get_tree(&t.root_digest, &meta()).unwrap();
        assert_eq!(
            s.request_count("GetTree"),
            1,
            "single server-streaming RPC delivered all directories across multiple frames"
        );
        assert_eq!(tree.root.as_ref().unwrap(), &t.root);
        let entries = crate::merkle::flatten_tree(&tree, DigestFn::Sha256).unwrap();
        let file_count = entries
            .iter()
            .filter(|e| matches!(e, crate::merkle::TreeEntry::File { .. }))
            .count();
        assert_eq!(file_count, 3);
        assert_eq!(tree.children.len(), 3);

        // Combined multi-frame streaming + next_page_token pagination + transient retry.
        s.configure(|c| {
            c.get_tree_stream_batch_size = 1;
            c.get_tree_page_size = 2;
        });
        s.inject_fault("GetTree", GrpcStatus::new(code::UNAVAILABLE, "flaky"));
        let tree2 = c.get_tree(&t.root_digest, &meta()).unwrap();
        assert!(
            s.request_count("GetTree") >= 4,
            "multi-frame streaming combined with next_page_token pagination and retry"
        );
        let entries2 = crate::merkle::flatten_tree(&tree2, DigestFn::Sha256).unwrap();
        assert_eq!(entries2, entries);
    }

    #[test]
    fn retries_transient_errors_but_not_permanent_ones() {
        let s = FakeReapiServer::new();
        let c = client_for(&s, false);
        s.inject_fault(
            "FindMissingBlobs",
            GrpcStatus::new(code::UNAVAILABLE, "flaky"),
        );
        s.inject_fault("FindMissingBlobs", GrpcStatus::new(code::ABORTED, "flaky"));
        let d = c.cfg.digest_fn.digest(b"zz");
        assert_eq!(
            c.find_missing_blobs(&[d.clone()], &meta()).unwrap(),
            vec![d.clone()]
        );
        assert_eq!(s.request_count("FindMissingBlobs"), 3);
        s.inject_fault(
            "FindMissingBlobs",
            GrpcStatus::new(code::PERMISSION_DENIED, "no"),
        );
        let e = c.find_missing_blobs(&[d], &meta()).unwrap_err();
        assert!(matches!(e, RemoteError::Grpc(ref g) if g.code == code::PERMISSION_DENIED));
        assert_eq!(s.request_count("FindMissingBlobs"), 4);
    }

    #[test]
    fn execute_streams_operations_resumes_wait_execution_and_never_retries_action_timeout() {
        let s = FakeReapiServer::new();
        s.configure(|c| c.drop_execute_stream_after_first_op = true);
        s.set_executor(|ctx| {
            let out = ctx.server.put_blob(b"built");
            Ok(ActionResult {
                exit_code: 0,
                output_files: vec![OutputFile {
                    path: "out.txt".into(),
                    digest: Some(out),
                    ..Default::default()
                }],
                stdout_raw: ctx.command.arguments.join(" ").into_bytes(),
                ..Default::default()
            })
        });
        let c = client_for(&s, true);
        let cmd = Command {
            arguments: vec!["tool".into(), "arg".into()],
            output_paths: vec!["out.txt".into()],
            ..Default::default()
        };
        let cb = cmd.encode_to_vec();
        let cd = c.cfg.digest_fn.digest(&cb);
        let root = s.put_blob(b"");
        s.put_blob(&cb);
        let action = Action {
            command_digest: Some(cd),
            input_root_digest: Some(root),
            ..Default::default()
        };
        let ab = action.encode_to_vec();
        let ad = c.cfg.digest_fn.digest(&ab);
        s.put_blob(&ab);

        let mut stages = Vec::new();
        let resp = c
            .execute(&ad, true, &meta(), &mut |st| stages.push(st))
            .unwrap();
        assert_eq!(resp.result.unwrap().stdout_raw, b"tool arg");
        assert!(s.request_count("WaitExecution") >= 1);

        // Clean EOF on initial Execute with zero operations fails immediately without retrying!
        let s_eof = FakeReapiServer::new();
        s_eof.configure(|c| c.clean_eof_without_op = true);
        let c_eof = client_for(&s_eof, true);
        assert!(c_eof.execute(&ad, true, &meta(), &mut |_| {}).is_err());
        assert_eq!(s_eof.request_count("Execute"), 1);

        // Action timeout (ExecuteResponse.status == DEADLINE_EXCEEDED) is NEVER retried!
        let s_to = FakeReapiServer::new();
        s_to.set_executor(|_| Err(GrpcStatus::new(code::DEADLINE_EXCEEDED, "timed out")));
        let d_root = s_to.put_blob(b"");
        let cmd_b = cmd.encode_to_vec();
        let cmd_d = DigestFn::Sha256.digest(&cmd_b);
        s_to.put_blob(&cmd_b);
        let act2 = Action {
            command_digest: Some(cmd_d),
            input_root_digest: Some(d_root),
            ..Default::default()
        };
        let act2_b = act2.encode_to_vec();
        let act2_d = DigestFn::Sha256.digest(&act2_b);
        s_to.put_blob(&act2_b);
        let c_to = client_for(&s_to, true);
        let err = c_to
            .execute(&act2_d, true, &meta(), &mut |_| {})
            .unwrap_err();
        assert!(matches!(err, RemoteError::ActionTimeout(_)));
        assert_eq!(s_to.executions(), 1);
    }
}
