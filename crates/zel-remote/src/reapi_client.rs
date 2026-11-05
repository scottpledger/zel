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

//! Remote Execution API v2 client over [`crate::grpc`]: Capabilities, ContentAddressableStorage
//! (FindMissingBlobs, BatchUpdateBlobs, BatchReadBlobs, GetTree), ByteStream (Read/Write),
//! ActionCache (Get/Update) and Execution (Execute/WaitExecution with Operations).

use crate::digest::DigestFn;
use crate::grpc::{CallOptions, Channel, GrpcStatus, RetryPolicy};
use crate::merkle::BlobSource;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io;
use std::sync::Mutex;
use std::time::Duration;
use zel_proto::reapi::*;

const CAS: &str = "/build.bazel.remote.execution.v2.ContentAddressableStorage/";
const AC: &str = "/build.bazel.remote.execution.v2.ActionCache/";
const EXEC: &str = "/build.bazel.remote.execution.v2.Execution/";
const CAPS: &str = "/build.bazel.remote.execution.v2.Capabilities/GetCapabilities";
const BS: &str = "/google.bytestream.ByteStream/";
const REQUEST_METADATA_HEADER: &str = "build.bazel.remote.execution.v2.requestmetadata-bin";

/// ByteStream chunk size for uploads.
const WRITE_CHUNK: usize = 64 * 1024;
/// Default maximum total size of a batch when the server does not advertise one (Bazel: 4 MiB,
/// minus protocol overhead).
const DEFAULT_MAX_BATCH: i64 = 4 * 1024 * 1024 - 1024;
const FIND_MISSING_CHUNK: usize = 1000;

#[derive(Debug)]
pub enum RemoteError {
    Grpc(GrpcStatus),
    /// Remote execution finished with a non-OK `ExecuteResponse.status` / `Operation.error`.
    Exec(RpcStatus),
    Io(io::Error),
    NotFound,
    Other(String),
}

impl fmt::Display for RemoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Grpc(s) => write!(f, "{s}"),
            Self::Exec(s) => write!(f, "remote execution failed (code {}): {}", s.code, s.message),
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

pub struct ReapiClient {
    pub cache: std::sync::Arc<Channel>,
    pub executor: Option<std::sync::Arc<Channel>>,
    pub cfg: ReapiConfig,
    caps: Mutex<Option<ServerCapabilities>>,
}

pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

impl ReapiClient {
    pub fn new(
        cache: std::sync::Arc<Channel>,
        executor: Option<std::sync::Arc<Channel>>,
        cfg: ReapiConfig,
    ) -> Self {
        Self { cache, executor, cfg, caps: Mutex::new(None) }
    }

    fn opts(&self, meta: &ActionMeta, with_timeout: bool) -> CallOptions {
        let rm = RequestMetadata {
            tool_details: Some(ToolDetails { tool_name: "bazel".into(), tool_version: "zel".into() }),
            action_id: meta.action_id.clone(),
            tool_invocation_id: self.cfg.tool_invocation_id.clone(),
            correlated_invocations_id: self.cfg.correlated_invocations_id.clone(),
            action_mnemonic: meta.mnemonic.clone(),
            target_id: meta.target_id.clone(),
            configuration_id: String::new(),
        };
        CallOptions {
            timeout: if with_timeout { Some(self.cfg.timeout) } else { None },
            metadata: vec![(REQUEST_METADATA_HEADER.into(), base64_encode(&rm.encode_to_vec()).trim_end_matches('=').to_string())],
        }
    }

    fn unary<M: Message, R: Message>(&self, ch: &Channel, method: &str, req: &M, meta: &ActionMeta) -> Result<R> {
        let bytes = req.encode_to_vec();
        let opts = self.opts(meta, true);
        let resp = self.cfg.retry.run(|| ch.unary(method, &bytes, &opts))?;
        Ok(R::decode(&resp)?)
    }

    fn exec_channel(&self) -> Result<&std::sync::Arc<Channel>> {
        self.executor
            .as_ref()
            .ok_or_else(|| RemoteError::Other("no --remote_executor configured".into()))
    }

    // ------------------------------------------------------------------ capabilities

    /// `Capabilities.GetCapabilities` (cached). Uses the executor endpoint if configured.
    pub fn capabilities(&self) -> Result<ServerCapabilities> {
        if let Some(c) = self.caps.lock().unwrap().clone() {
            return Ok(c);
        }
        let ch = self.executor.as_ref().unwrap_or(&self.cache);
        let req = GetCapabilitiesRequest { instance_name: self.cfg.instance_name.clone() };
        let caps: ServerCapabilities = self.unary(ch, CAPS, &req, &ActionMeta::default())?;
        *self.caps.lock().unwrap() = Some(caps.clone());
        Ok(caps)
    }

    /// `RemoteServerCapabilities.checkClientServerCompatibility`.
    pub fn check_compatibility(&self, need_exec: bool) -> Result<()> {
        let caps = self.capabilities()?;
        let lo = caps.low_api_version.clone().unwrap_or(SemVer { major: 2, ..Default::default() });
        let hi = caps.high_api_version.clone().unwrap_or(SemVer { major: 2, ..Default::default() });
        let cmp = |a: &SemVer, b: (i32, i32)| (a.major, a.minor).cmp(&b);
        if cmp(&lo, (2, 0)) == std::cmp::Ordering::Greater
            || (hi.major, hi.minor) < (2, 0)
        {
            return Err(RemoteError::Other(format!(
                "Server does not support REAPI v2.0 (supports {}.{} - {}.{})",
                lo.major, lo.minor, hi.major, hi.minor
            )));
        }
        let want = self.cfg.digest_fn.enum_value();
        let cc = caps.cache_capabilities.clone().unwrap_or_default();
        if !cc.digest_functions.is_empty() && !cc.digest_functions.contains(&want) {
            return Err(RemoteError::Other(format!(
                "Remote cache does not support the digest function {:?} (supports {:?})",
                self.cfg.digest_fn, cc.digest_functions
            )));
        }
        if need_exec {
            let ec = caps.execution_capabilities.unwrap_or_default();
            if !ec.exec_enabled {
                return Err(RemoteError::Other("Remote execution is not enabled on the server".into()));
            }
            let fns: Vec<i32> = if ec.digest_functions.is_empty() { vec![ec.digest_function] } else { ec.digest_functions.clone() };
            if !fns.is_empty() && !fns.contains(&0) && !fns.contains(&want) {
                return Err(RemoteError::Other("Remote executor does not support the digest function".into()));
            }
        }
        Ok(())
    }

    pub fn remote_cache_supports_update(&self) -> bool {
        self.capabilities()
            .ok()
            .and_then(|c| c.cache_capabilities)
            .and_then(|c| c.action_cache_update_capabilities)
            .map(|u| u.update_enabled)
            .unwrap_or(true)
    }

    /// Server's REAPI high version supports `Command.output_paths` (>= 2.1).
    pub fn supports_output_paths(&self) -> bool {
        match self.capabilities().ok().and_then(|c| c.high_api_version) {
            Some(v) => (v.major, v.minor) >= (2, 1),
            None => true,
        }
    }

    pub fn max_batch_size(&self) -> i64 {
        let m = self
            .capabilities()
            .ok()
            .and_then(|c| c.cache_capabilities)
            .map(|c| c.max_batch_total_size_bytes)
            .unwrap_or(0);
        if m > 0 { m } else { DEFAULT_MAX_BATCH }
    }

    // ------------------------------------------------------------------ resource names

    fn blob_prefix(&self) -> String {
        if self.cfg.instance_name.is_empty() { String::new() } else { format!("{}/", self.cfg.instance_name) }
    }

    pub fn read_resource_name(&self, d: &Digest) -> String {
        match self.cfg.digest_fn.resource_name() {
            Some(f) => format!("{}blobs/{f}/{}/{}", self.blob_prefix(), d.hash, d.size_bytes),
            None => format!("{}blobs/{}/{}", self.blob_prefix(), d.hash, d.size_bytes),
        }
    }

    pub fn write_resource_name(&self, uuid: &str, d: &Digest) -> String {
        match self.cfg.digest_fn.resource_name() {
            Some(f) => format!("{}uploads/{uuid}/blobs/{f}/{}/{}", self.blob_prefix(), d.hash, d.size_bytes),
            None => format!("{}uploads/{uuid}/blobs/{}/{}", self.blob_prefix(), d.hash, d.size_bytes),
        }
    }

    // ------------------------------------------------------------------ CAS

    pub fn find_missing_blobs(&self, digests: &[Digest], meta: &ActionMeta) -> Result<Vec<Digest>> {
        let mut missing = Vec::new();
        for chunk in digests.chunks(FIND_MISSING_CHUNK) {
            let req = FindMissingBlobsRequest {
                instance_name: self.cfg.instance_name.clone(),
                blob_digests: chunk.to_vec(),
                digest_function: self.cfg.digest_fn.proto_value(),
            };
            let r: FindMissingBlobsResponse =
                self.unary(&self.cache, &format!("{CAS}FindMissingBlobs"), &req, meta)?;
            missing.extend(r.missing_blob_digests);
        }
        Ok(missing)
    }

    /// `BatchUpdateBlobs`; the caller guarantees the total size fits the server's limit.
    pub fn batch_update_blobs(&self, blobs: &[(Digest, Vec<u8>)], meta: &ActionMeta) -> Result<()> {
        if blobs.is_empty() {
            return Ok(());
        }
        let req = BatchUpdateBlobsRequest {
            instance_name: self.cfg.instance_name.clone(),
            requests: blobs
                .iter()
                .map(|(d, data)| BatchUpdateBlobsRequestItem { digest: Some(d.clone()), data: data.clone(), compressor: 0 })
                .collect(),
            digest_function: self.cfg.digest_fn.proto_value(),
        };
        let r: BatchUpdateBlobsResponse = self.unary(&self.cache, &format!("{CAS}BatchUpdateBlobs"), &req, meta)?;
        for item in r.responses {
            if let Some(s) = item.status {
                if s.code != code::OK {
                    let d = item.digest.unwrap_or_default();
                    return Err(RemoteError::Grpc(GrpcStatus::new(
                        s.code,
                        format!("failed to upload blob {}/{}: {}", d.hash, d.size_bytes, s.message),
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn batch_read_blobs(&self, digests: &[Digest], meta: &ActionMeta) -> Result<HashMap<Digest, Vec<u8>>> {
        let mut out = HashMap::new();
        for chunk in digests.chunks(FIND_MISSING_CHUNK) {
            let req = BatchReadBlobsRequest {
                instance_name: self.cfg.instance_name.clone(),
                digests: chunk.to_vec(),
                acceptable_compressors: vec![],
                digest_function: self.cfg.digest_fn.proto_value(),
            };
            let r: BatchReadBlobsResponse = self.unary(&self.cache, &format!("{CAS}BatchReadBlobs"), &req, meta)?;
            for item in r.responses {
                let d = item.digest.unwrap_or_default();
                let st = item.status.unwrap_or_default();
                if st.code == code::NOT_FOUND {
                    return Err(RemoteError::NotFound);
                }
                if st.code != code::OK {
                    return Err(RemoteError::Grpc(GrpcStatus::new(st.code, st.message)));
                }
                self.verify(&d, &item.data)?;
                out.insert(d, item.data);
            }
        }
        Ok(out)
    }

    fn verify(&self, d: &Digest, data: &[u8]) -> Result<()> {
        let actual = self.cfg.digest_fn.digest(data);
        if actual.hash != d.hash || actual.size_bytes != d.size_bytes {
            return Err(RemoteError::Other(format!(
                "Download of {}/{} failed: digest mismatch (got {}/{})",
                d.hash, d.size_bytes, actual.hash, actual.size_bytes
            )));
        }
        Ok(())
    }

    /// `ByteStream.Write` of one blob in chunks.
    pub fn write_blob_bytestream(&self, d: &Digest, data: &[u8], meta: &ActionMeta) -> Result<()> {
        let uuid = format!("{:016x}-{:016x}", self.nanos(), d.size_bytes);
        let resource = self.write_resource_name(&uuid, d);
        let opts = self.opts(meta, true);
        self.cfg.retry.run(|| {
            let mut msgs: Vec<Vec<u8>> = Vec::new();
            let mut offset = 0usize;
            loop {
                let end = (offset + WRITE_CHUNK).min(data.len());
                msgs.push(
                    WriteRequest {
                        resource_name: if offset == 0 { resource.clone() } else { String::new() },
                        write_offset: offset as i64,
                        finish_write: end == data.len(),
                        data: data[offset..end].to_vec(),
                    }
                    .encode_to_vec(),
                );
                if end == data.len() {
                    break;
                }
                offset = end;
            }
            let mut chunks = msgs.into_iter();
            let mut committed: Option<i64> = None;
            let r = self.cache.call(&format!("{BS}Write"), &mut chunks, &opts, &mut |m| {
                committed = Some(
                    WriteResponse::decode(&m)
                        .map_err(|e| GrpcStatus::new(code::INTERNAL, e.to_string()))?
                        .committed_size,
                );
                Ok(())
            });
            match r {
                Ok(()) => {
                    // A server may report -1 or the full size for an already-present blob.
                    match committed {
                        Some(c) if c == d.size_bytes || c == -1 => Ok(()),
                        Some(c) => Err(GrpcStatus::new(code::INVALID_ARGUMENT, format!("committed size {c} != blob size {}", d.size_bytes))),
                        None => Err(GrpcStatus::new(code::INTERNAL, "no WriteResponse")),
                    }
                }
                Err(e) if e.code == code::ALREADY_EXISTS => Ok(()),
                Err(e) => Err(e),
            }
        })?;
        Ok(())
    }

    fn nanos(&self) -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        t ^ C.fetch_add(0x9e37_79b9, Ordering::SeqCst).rotate_left(17)
    }

    /// `ByteStream.Read` of a whole blob (digest-verified). Falls back to `BatchReadBlobs` when
    /// the server does not implement ByteStream.
    pub fn download_blob(&self, d: &Digest, meta: &ActionMeta) -> Result<Vec<u8>> {
        if d.size_bytes == 0 {
            return Ok(Vec::new());
        }
        let req = ReadRequest { resource_name: self.read_resource_name(d), read_offset: 0, read_limit: 0 }.encode_to_vec();
        let opts = self.opts(meta, true);
        let res = self.cfg.retry.run(|| {
            let mut data = Vec::with_capacity(d.size_bytes.max(0) as usize);
            self.cache.server_stream(&format!("{BS}Read"), &req, &opts, &mut |m| {
                let r = ReadResponse::decode(&m).map_err(|e| GrpcStatus::new(code::INTERNAL, e.to_string()))?;
                data.extend(r.data);
                Ok(())
            })?;
            Ok(data)
        });
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

    /// Ensures all `blobs` exist in the CAS: `FindMissingBlobs`, then uploads the missing ones
    /// via `BatchUpdateBlobs` (small) or `ByteStream.Write` (larger than a batch).
    pub fn ensure_blobs_present(&self, blobs: &BTreeMap<Digest, BlobSource>, meta: &ActionMeta) -> Result<usize> {
        let digests: Vec<Digest> = blobs.keys().cloned().filter(|d| d.size_bytes > 0).collect();
        let missing = self.find_missing_blobs(&digests, meta)?;
        self.upload_blobs(&missing, blobs, meta)?;
        Ok(missing.len())
    }

    /// Uploads exactly the given digests (no existence check).
    pub fn upload_blobs(&self, digests: &[Digest], blobs: &BTreeMap<Digest, BlobSource>, meta: &ActionMeta) -> Result<()> {
        let max_batch = self.max_batch_size();
        let mut batch: Vec<(Digest, Vec<u8>)> = Vec::new();
        let mut batch_bytes = 0i64;
        let mut large: Vec<(Digest, Vec<u8>)> = Vec::new();
        for d in digests {
            let src = blobs
                .get(d)
                .ok_or_else(|| RemoteError::Other(format!("missing local source for blob {}/{}", d.hash, d.size_bytes)))?;
            let data = src.read()?;
            if d.size_bytes >= max_batch {
                large.push((d.clone(), data));
                continue;
            }
            if batch_bytes + d.size_bytes > max_batch {
                self.batch_update_blobs(&batch, meta)?;
                batch.clear();
                batch_bytes = 0;
            }
            batch_bytes += d.size_bytes;
            batch.push((d.clone(), data));
        }
        self.batch_update_blobs(&batch, meta)?;
        // Large blobs go over ByteStream, a few at a time.
        let err: Mutex<Option<RemoteError>> = Mutex::new(None);
        for group in large.chunks(4) {
            std::thread::scope(|s| {
                for (d, data) in group {
                    let err = &err;
                    s.spawn(move || {
                        if let Err(e) = self.write_blob_bytestream(d, data, meta) {
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

    /// `GetTree` with pagination. Returns a `Tree` whose root is the directory with `root` digest.
    pub fn get_tree(&self, root: &Digest, meta: &ActionMeta) -> Result<Tree> {
        let mut dirs: Vec<Directory> = Vec::new();
        let mut token = String::new();
        loop {
            let req = GetTreeRequest {
                instance_name: self.cfg.instance_name.clone(),
                root_digest: Some(root.clone()),
                page_size: 0,
                page_token: token.clone(),
                digest_function: self.cfg.digest_fn.proto_value(),
            };
            let r: GetTreeResponse = self.unary(&self.cache, &format!("{CAS}GetTree"), &req, meta)?;
            dirs.extend(r.directories);
            if r.next_page_token.is_empty() {
                break;
            }
            token = r.next_page_token;
        }
        let pos = dirs
            .iter()
            .position(|d| self.cfg.digest_fn.hash(&d.encode_to_vec()) == root.hash)
            .ok_or_else(|| RemoteError::Other("GetTree response did not contain the root directory".into()))?;
        let root_dir = dirs.remove(pos);
        Ok(Tree { root: Some(root_dir), children: dirs })
    }

    // ------------------------------------------------------------------ action cache

    pub fn get_action_result(&self, action: &Digest, meta: &ActionMeta) -> Result<Option<ActionResult>> {
        let req = GetActionResultRequest {
            instance_name: self.cfg.instance_name.clone(),
            action_digest: Some(action.clone()),
            digest_function: self.cfg.digest_fn.proto_value(),
            ..Default::default()
        };
        match self.unary::<_, ActionResult>(&self.cache, &format!("{AC}GetActionResult"), &req, meta) {
            Ok(r) => Ok(Some(r)),
            Err(RemoteError::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn update_action_result(&self, action: &Digest, result: &ActionResult, meta: &ActionMeta) -> Result<()> {
        let req = UpdateActionResultRequest {
            instance_name: self.cfg.instance_name.clone(),
            action_digest: Some(action.clone()),
            action_result: Some(result.clone()),
            results_cache_policy: None,
            digest_function: self.cfg.digest_fn.proto_value(),
        };
        let _: ActionResult = self.unary(&self.cache, &format!("{AC}UpdateActionResult"), &req, meta)?;
        Ok(())
    }

    // ------------------------------------------------------------------ execution

    /// `Execution.Execute`, resuming with `WaitExecution` when the stream breaks before the
    /// operation is done. `on_stage` observes `ExecuteOperationMetadata.stage` updates.
    pub fn execute(
        &self,
        action: &Digest,
        skip_cache_lookup: bool,
        meta: &ActionMeta,
        on_stage: &mut dyn FnMut(i32),
    ) -> Result<ExecuteResponse> {
        let ch = self.exec_channel()?.clone();
        let req = ExecuteRequest {
            instance_name: self.cfg.instance_name.clone(),
            skip_cache_lookup,
            action_digest: Some(action.clone()),
            execution_policy: None,
            results_cache_policy: None,
            digest_function: self.cfg.digest_fn.proto_value(),
        }
        .encode_to_vec();
        let opts = self.opts(meta, false);
        let mut op_name: Option<String> = None;
        let mut attempt = 0u32;
        loop {
            let mut done: Option<Operation> = None;
            let current = op_name.clone();
            let mut seen_name: Option<String> = None;
            let mut handle = |m: Vec<u8>| -> std::result::Result<(), GrpcStatus> {
                let op = Operation::decode(&m).map_err(|e| GrpcStatus::new(code::INTERNAL, e.to_string()))?;
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
                (_, Some(op)) => return self.finish_operation(op),
                (Ok(()), None) => {
                    // Stream closed without a final operation; keep waiting.
                    if attempt >= self.cfg.retry.max_retries {
                        return Err(RemoteError::Other("execution stream ended without completing the operation".into()));
                    }
                }
                (Err(e), None) => {
                    // An unknown operation on WaitExecution means we must re-Execute.
                    if e.code == code::NOT_FOUND && op_name.is_some() {
                        op_name = None;
                    } else if !RetryPolicy::is_retriable(&e) || attempt >= self.cfg.retry.max_retries {
                        return Err(e.into());
                    }
                }
            }
            attempt += 1;
            std::thread::sleep(Duration::from_millis(10 * attempt as u64));
        }
    }

    fn finish_operation(&self, op: Operation) -> Result<ExecuteResponse> {
        if let Some(e) = op.error {
            if e.code != code::OK {
                return Err(RemoteError::Exec(e));
            }
        }
        let any = op
            .response
            .ok_or_else(|| RemoteError::Other("done operation has neither response nor error".into()))?;
        if !any.type_url.ends_with("ExecuteResponse") {
            return Err(RemoteError::Other(format!("unexpected operation response type {}", any.type_url)));
        }
        let resp = ExecuteResponse::decode(&any.value)?;
        if let Some(s) = &resp.status {
            if s.code != code::OK {
                return Err(RemoteError::Exec(s.clone()));
            }
        }
        Ok(resp)
    }
}

/// Whether an `Exec` error is the REAPI "missing blobs" precondition failure.
pub fn is_missing_blobs_failure(s: &RpcStatus) -> bool {
    if s.code != code::FAILED_PRECONDITION {
        return false;
    }
    s.details.iter().any(|d| {
        d.type_url.ends_with("PreconditionFailure")
            && PreconditionFailure::decode(&d.value)
                .map(|p| p.violations.iter().any(|v| v.r#type == "MISSING"))
                .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_server::FakeReapiServer;
    use std::sync::Arc;

    pub(crate) fn client_for(server: &FakeReapiServer, exec: bool) -> ReapiClient {
        let ch = Arc::new(Channel::new(server.connector(), 4, vec![]));
        let cfg = ReapiConfig {
            retry: RetryPolicy { initial_delay: Duration::from_millis(1), ..Default::default() },
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
    fn capabilities_and_compatibility() {
        let s = FakeReapiServer::new();
        let c = client_for(&s, true);
        let caps = c.capabilities().unwrap();
        assert!(caps.execution_capabilities.unwrap().exec_enabled);
        c.check_compatibility(true).unwrap();
        assert!(c.supports_output_paths());
        assert_eq!(c.max_batch_size(), 4 << 20);
        assert_eq!(s.request_count("GetCapabilities"), 1);
        c.capabilities().unwrap();
        assert_eq!(s.request_count("GetCapabilities"), 1, "capabilities are cached");

        let s2 = FakeReapiServer::new();
        s2.configure(|c| c.exec_enabled = false);
        assert!(client_for(&s2, true).check_compatibility(true).is_err());
        let s3 = FakeReapiServer::new();
        s3.configure(|c| c.advertise_digest_functions = vec![digest_function::BLAKE3]);
        let e = client_for(&s3, false).check_compatibility(false).unwrap_err();
        assert!(e.to_string().contains("digest function"));
        let s4 = FakeReapiServer::new();
        s4.configure(|c| c.high_api_version = SemVer { major: 2, minor: 0, ..Default::default() });
        assert!(!client_for(&s4, false).supports_output_paths());
    }

    #[test]
    fn cas_roundtrip_batch_and_bytestream() {
        let s = FakeReapiServer::new();
        s.configure(|c| {
            c.max_batch_total_size_bytes = 1000;
            c.read_chunk = 100;
        });
        let c = client_for(&s, false);
        let small = b"small blob".to_vec();
        let big: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 256) as u8).collect();
        let mut blobs = BTreeMap::new();
        let sd = c.cfg.digest_fn.digest(&small);
        let bd = c.cfg.digest_fn.digest(&big);
        blobs.insert(sd.clone(), BlobSource::Bytes(small.clone()));
        blobs.insert(bd.clone(), BlobSource::Bytes(big.clone()));
        assert_eq!(c.ensure_blobs_present(&blobs, &meta()).unwrap(), 2);
        assert_eq!(s.request_count("BatchUpdateBlobs"), 1);
        assert_eq!(s.request_count("ByteStream/Write"), 1);
        assert_eq!(s.get_blob(&bd).unwrap(), big);
        // Second time: nothing missing, nothing uploaded.
        let writes = s.cas_write_count();
        assert_eq!(c.ensure_blobs_present(&blobs, &meta()).unwrap(), 0);
        assert_eq!(s.cas_write_count(), writes);
        assert_eq!(c.download_blob(&bd, &meta()).unwrap(), big);
        assert_eq!(c.download_blob(&sd, &meta()).unwrap(), small);
        let m = c.batch_read_blobs(&[sd.clone()], &meta()).unwrap();
        assert_eq!(m[&sd], small);
        // Missing blob → NotFound.
        let nd = c.cfg.digest_fn.digest(b"never uploaded");
        assert!(matches!(c.download_blob(&nd, &meta()), Err(RemoteError::NotFound)));
        // Empty blob needs no round trip.
        assert!(c.download_blob(&c.cfg.digest_fn.digest(b""), &meta()).unwrap().is_empty());
    }

    #[test]
    fn bytestream_resource_names_and_instance() {
        let s = FakeReapiServer::new();
        let mut c = client_for(&s, false);
        c.cfg.instance_name = "projects/x/instances/y".into();
        let d = Digest { hash: "ab".repeat(32), size_bytes: 5 };
        assert_eq!(c.read_resource_name(&d), format!("projects/x/instances/y/blobs/{}/5", d.hash));
        assert_eq!(c.write_resource_name("u", &d), format!("projects/x/instances/y/uploads/u/blobs/{}/5", d.hash));
        c.cfg.digest_fn = DigestFn::Md5;
        assert!(c.read_resource_name(&d).contains("/blobs/md5/"));
        c.cfg.instance_name.clear();
        assert!(c.read_resource_name(&d).starts_with("blobs/md5/"));
    }

    #[test]
    fn action_cache_get_update() {
        let s = FakeReapiServer::new();
        let c = client_for(&s, false);
        let ad = c.cfg.digest_fn.digest(b"action");
        assert!(c.get_action_result(&ad, &meta()).unwrap().is_none());
        let ar = ActionResult { exit_code: 0, stdout_raw: b"out".to_vec(), ..Default::default() };
        c.update_action_result(&ad, &ar, &meta()).unwrap();
        assert_eq!(c.get_action_result(&ad, &meta()).unwrap().unwrap(), ar);
        s.configure(|c| c.update_enabled = false);
        assert!(!c.remote_cache_supports_update() || c.capabilities().is_ok());
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
        assert!(s.request_count("GetTree") >= 3, "pagination issued multiple requests");
        let entries = crate::merkle::flatten_tree(&tree, DigestFn::Sha256).unwrap();
        assert!(entries.iter().any(|e| matches!(e, crate::merkle::TreeEntry::File { path, .. } if path == "a/b/c.txt")));
        assert_eq!(tree.root.unwrap(), t.root);
    }

    #[test]
    fn retries_transient_errors_but_not_permanent_ones() {
        let s = FakeReapiServer::new();
        let c = client_for(&s, false);
        s.inject_fault("FindMissingBlobs", GrpcStatus::new(code::UNAVAILABLE, "flaky"));
        s.inject_fault("FindMissingBlobs", GrpcStatus::new(code::ABORTED, "flaky"));
        let d = c.cfg.digest_fn.digest(b"zz");
        assert_eq!(c.find_missing_blobs(&[d.clone()], &meta()).unwrap(), vec![d.clone()]);
        assert_eq!(s.request_count("FindMissingBlobs"), 3);
        s.inject_fault("FindMissingBlobs", GrpcStatus::new(code::PERMISSION_DENIED, "no"));
        let e = c.find_missing_blobs(&[d], &meta()).unwrap_err();
        assert!(matches!(e, RemoteError::Grpc(ref g) if g.code == code::PERMISSION_DENIED));
        assert_eq!(s.request_count("FindMissingBlobs"), 4);
    }

    #[test]
    fn execute_streams_operations_and_resumes_with_wait_execution() {
        let s = FakeReapiServer::new();
        s.set_executor(|ctx| {
            let out = ctx.server.put_blob(b"built");
            Ok(ActionResult {
                exit_code: 0,
                output_files: vec![OutputFile { path: "out.txt".into(), digest: Some(out), ..Default::default() }],
                stdout_raw: ctx.command.arguments.join(" ").into_bytes(),
                ..Default::default()
            })
        });
        let c = client_for(&s, true);
        let cmd = Command { arguments: vec!["tool".into(), "arg".into()], output_paths: vec!["out.txt".into()], ..Default::default() };
        let cb = cmd.encode_to_vec();
        let cd = c.cfg.digest_fn.digest(&cb);
        let root = s.put_blob(b"");
        s.put_blob(&cb);
        let action = Action { command_digest: Some(cd), input_root_digest: Some(root), ..Default::default() };
        let ab = action.encode_to_vec();
        let ad = s.put_blob(&ab);
        let mut stages = Vec::new();
        let r = c.execute(&ad, false, &meta(), &mut |st| stages.push(st)).unwrap();
        assert_eq!(r.result.as_ref().unwrap().stdout_raw, b"tool arg");
        assert!(!r.cached_result);
        assert_eq!(stages, vec![execution_stage::QUEUED, execution_stage::EXECUTING, execution_stage::COMPLETED]);
        assert_eq!(s.executions(), 1);
        // Server cached the result: a second Execute is a cache hit without running anything.
        let r2 = c.execute(&ad, false, &meta(), &mut |_| {}).unwrap();
        assert!(r2.cached_result);
        assert_eq!(s.executions(), 1);
        // skip_cache_lookup forces re-execution.
        c.execute(&ad, true, &meta(), &mut |_| {}).unwrap();
        assert_eq!(s.executions(), 2);

        // Stream drops after the first operation: client must call WaitExecution.
        s.configure(|c| c.drop_execute_stream_after_first_op = true);
        let mut stages = Vec::new();
        let r3 = c.execute(&ad, true, &meta(), &mut |st| stages.push(st)).unwrap();
        assert_eq!(r3.result.unwrap().stdout_raw, b"tool arg");
        assert_eq!(s.request_count("WaitExecution"), 1);
        assert_eq!(stages[0], execution_stage::QUEUED);
    }

    #[test]
    fn execute_reports_missing_blobs_and_failures() {
        let s = FakeReapiServer::new();
        s.set_executor(|_| Err(GrpcStatus::new(code::INTERNAL, "worker exploded")));
        let c = client_for(&s, true);
        let missing = c.cfg.digest_fn.digest(b"not uploaded action");
        match c.execute(&missing, false, &meta(), &mut |_| {}) {
            Err(RemoteError::Exec(st)) => assert!(is_missing_blobs_failure(&st), "{st:?}"),
            other => panic!("{other:?}"),
        }
        let root = s.put_blob(b"");
        let cmd = s.put_blob(&Command::default().encode_to_vec());
        let ad = s.put_blob(&Action { command_digest: Some(cmd), input_root_digest: Some(root), ..Default::default() }.encode_to_vec());
        match c.execute(&ad, false, &meta(), &mut |_| {}) {
            Err(RemoteError::Exec(st)) => assert_eq!(st.code, code::INTERNAL),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn remote_headers_and_request_metadata_are_sent() {
        let s = FakeReapiServer::new();
        let ch = Arc::new(Channel::new(s.connector(), 1, vec![("Authorization".into(), "Bearer t0k".into())]));
        let c = ReapiClient::new(ch, None, ReapiConfig { tool_invocation_id: "inv-1".into(), ..Default::default() });
        c.find_missing_blobs(&[], &meta()).unwrap_or_default();
        c.find_missing_blobs(&[c.cfg.digest_fn.digest(b"a")], &ActionMeta { mnemonic: "CppCompile".into(), ..Default::default() }).unwrap();
        let h = s.last_headers("FindMissingBlobs").unwrap();
        assert!(h.contains(&("authorization".to_string(), "Bearer t0k".to_string())));
        assert!(h.iter().any(|(k, _)| k == "build.bazel.remote.execution.v2.requestmetadata-bin"));
        assert!(h.iter().any(|(k, v)| k == "grpc-timeout" && v.ends_with('m')));
        assert!(h.contains(&("content-type".to_string(), "application/grpc".to_string())));
    }
}
