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

//! An in-process fake Remote Execution API v2 server (Capabilities, ContentAddressableStorage,
//! ByteStream, ActionCache, Execution, Operations) speaking real gRPC over real HTTP/2 framing,
//! via either an in-memory pipe (no network permissions needed) or TCP. Intended for tests of
//! the REAPI client and of anything built on it (e.g. the remote spawn runner).

use crate::grpc::{frame_message, percent_encode, GrpcStatus, MessageParser};
use crate::hpack::Header;
use crate::http2::{Conn, Event, LocalSettings, Stream};
use crate::transport::{memory_pair, Connector, Transport};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::{Arc, Mutex};
use zel_proto::reapi::*;
use zel_util::sha256;

pub const CAS: &str = "/build.bazel.remote.execution.v2.ContentAddressableStorage/";
pub const AC: &str = "/build.bazel.remote.execution.v2.ActionCache/";
pub const EXEC: &str = "/build.bazel.remote.execution.v2.Execution/";
pub const CAPS: &str = "/build.bazel.remote.execution.v2.Capabilities/GetCapabilities";
pub const BYTESTREAM: &str = "/google.bytestream.ByteStream/";

pub const EXECUTE_RESPONSE_TYPE: &str =
    "type.googleapis.com/build.bazel.remote.execution.v2.ExecuteResponse";
pub const EXECUTE_METADATA_TYPE: &str =
    "type.googleapis.com/build.bazel.remote.execution.v2.ExecuteOperationMetadata";
pub const PRECONDITION_FAILURE_TYPE: &str = "type.googleapis.com/google.rpc.PreconditionFailure";

/// Inputs handed to a fake executor.
pub struct ExecContext<'a> {
    pub server: &'a FakeReapiServer,
    pub action: &'a Action,
    pub command: &'a Command,
    pub action_digest: &'a Digest,
}

pub type Executor = dyn Fn(&ExecContext<'_>) -> Result<ActionResult, GrpcStatus> + Send + Sync;

/// Server behaviour knobs.
#[derive(Clone)]
pub struct FakeConfig {
    pub exec_enabled: bool,
    pub update_enabled: bool,
    pub max_batch_total_size_bytes: i64,
    pub high_api_version: SemVer,
    pub low_api_version: SemVer,
    pub instance_name: String,
    /// ByteStream read chunk size.
    pub read_chunk: usize,
    /// When set, Execute closes its stream (UNAVAILABLE) after sending the first operation so
    /// the client has to resume with WaitExecution.
    pub drop_execute_stream_after_first_op: bool,
    pub get_tree_page_size: usize,
    pub advertise_digest_functions: Vec<i32>,
}

impl Default for FakeConfig {
    fn default() -> Self {
        Self {
            exec_enabled: true,
            update_enabled: true,
            max_batch_total_size_bytes: 4 << 20,
            high_api_version: SemVer { major: 2, minor: 3, patch: 0, prerelease: String::new() },
            low_api_version: SemVer { major: 2, minor: 0, patch: 0, prerelease: String::new() },
            instance_name: String::new(),
            read_chunk: 64 * 1024,
            drop_execute_stream_after_first_op: false,
            get_tree_page_size: 0,
            advertise_digest_functions: vec![digest_function::SHA256],
        }
    }
}

#[derive(Default)]
struct Inner {
    cas: HashMap<String, Vec<u8>>,
    ac: HashMap<String, ActionResult>,
    ops: HashMap<String, Vec<Operation>>,
    counts: HashMap<String, usize>,
    last_headers: HashMap<String, Vec<Header>>,
    faults: VecDeque<(String, GrpcStatus)>,
    next_op: u64,
    executions: usize,
    cas_writes: usize,
    /// Digests (hash) whose next BatchRead/Read is reported missing (to simulate eviction).
    evict_on_read: Vec<String>,
    /// Blobs reported present by the next FindMissingBlobs but dropped right afterwards.
    forget_after_find_missing: Vec<String>,
}

struct Shared {
    inner: Mutex<Inner>,
    cfg: Mutex<FakeConfig>,
    executor: Mutex<Option<Arc<Executor>>>,
}

/// Handle to the fake server.
#[derive(Clone)]
pub struct FakeReapiServer {
    sh: Arc<Shared>,
}

impl Default for FakeReapiServer {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeReapiServer {
    pub fn new() -> Self {
        Self {
            sh: Arc::new(Shared {
                inner: Mutex::new(Inner::default()),
                cfg: Mutex::new(FakeConfig::default()),
                executor: Mutex::new(None),
            }),
        }
    }

    pub fn configure(&self, f: impl FnOnce(&mut FakeConfig)) {
        f(&mut self.sh.cfg.lock().unwrap());
    }

    pub fn set_executor(
        &self,
        f: impl Fn(&ExecContext<'_>) -> Result<ActionResult, GrpcStatus> + Send + Sync + 'static,
    ) {
        *self.sh.executor.lock().unwrap() = Some(Arc::new(f));
    }

    /// Makes the next call whose method path contains `method_substr` fail with `status`.
    pub fn inject_fault(&self, method_substr: &str, status: GrpcStatus) {
        self.sh.inner.lock().unwrap().faults.push_back((method_substr.to_string(), status));
    }

    pub fn evict_on_next_read(&self, hash: &str) {
        self.sh.inner.lock().unwrap().evict_on_read.push(hash.to_string());
    }

    /// The next FindMissingBlobs reports `hash` as present, then the CAS silently drops it.
    pub fn forget_after_next_find_missing(&self, hash: &str) {
        self.sh.inner.lock().unwrap().forget_after_find_missing.push(hash.to_string());
    }

    pub fn put_blob(&self, data: &[u8]) -> Digest {
        let d = Digest { hash: sha256::hex_digest(data), size_bytes: data.len() as i64 };
        self.sh.inner.lock().unwrap().cas.insert(d.hash.clone(), data.to_vec());
        d
    }

    pub fn get_blob(&self, d: &Digest) -> Option<Vec<u8>> {
        self.sh.inner.lock().unwrap().cas.get(&d.hash).cloned()
    }

    pub fn remove_blob(&self, d: &Digest) {
        self.sh.inner.lock().unwrap().cas.remove(&d.hash);
    }

    pub fn blob_count(&self) -> usize {
        self.sh.inner.lock().unwrap().cas.len()
    }

    pub fn ac_count(&self) -> usize {
        self.sh.inner.lock().unwrap().ac.len()
    }

    pub fn get_action_result(&self, d: &Digest) -> Option<ActionResult> {
        self.sh.inner.lock().unwrap().ac.get(&d.hash).cloned()
    }

    pub fn put_action_result(&self, d: &Digest, r: ActionResult) {
        self.sh.inner.lock().unwrap().ac.insert(d.hash.clone(), r);
    }

    /// Number of calls seen for a method (full path or suffix, e.g. "FindMissingBlobs").
    pub fn request_count(&self, method_suffix: &str) -> usize {
        self.sh
            .inner
            .lock()
            .unwrap()
            .counts
            .iter()
            .filter(|(k, _)| k.ends_with(method_suffix))
            .map(|(_, v)| *v)
            .sum()
    }

    pub fn executions(&self) -> usize {
        self.sh.inner.lock().unwrap().executions
    }

    pub fn cas_write_count(&self) -> usize {
        self.sh.inner.lock().unwrap().cas_writes
    }

    /// Request headers of the last call to `method_suffix`.
    pub fn last_headers(&self, method_suffix: &str) -> Option<Vec<Header>> {
        self.sh
            .inner
            .lock()
            .unwrap()
            .last_headers
            .iter()
            .find(|(k, _)| k.ends_with(method_suffix))
            .map(|(_, v)| v.clone())
    }

    /// Creates a new in-memory connection served by this server.
    pub fn connect_memory(&self) -> Transport {
        let (c, s) = memory_pair();
        self.serve_transport(s);
        c
    }

    /// Serves one accepted transport on a background thread.
    pub fn serve_transport(&self, t: Transport) {
        let me = self.clone();
        let _ = std::thread::Builder::new().name("fake-reapi-conn".into()).spawn(move || {
            let Ok((_conn, rx)) = Conn::server_from(t, LocalSettings::default()) else {
                return;
            };
            while let Ok(stream) = rx.recv() {
                let me = me.clone();
                let _ = std::thread::spawn(move || me.handle_stream(stream));
            }
        });
    }

    /// Accepts TCP connections forever on `listener` (spawns a thread).
    pub fn serve_tcp(&self, listener: std::net::TcpListener) {
        let me = self.clone();
        std::thread::spawn(move || {
            for s in listener.incoming().flatten() {
                let _ = s.set_nodelay(true);
                let (Ok(r), Ok(w)) = (s.try_clone(), s.try_clone()) else { continue };
                me.serve_transport(Transport {
                    reader: Box::new(r),
                    writer: Box::new(w),
                    shutdown: Box::new(move || {
                        let _ = s.shutdown(std::net::Shutdown::Both);
                    }),
                });
            }
        });
    }

    /// A ready-to-use REAPI client talking to this server over in-memory connections (the
    /// executor endpoint is the same server when `exec` is true).
    pub fn client(&self, exec: bool) -> crate::reapi_client::ReapiClient {
        use crate::grpc::{Channel, RetryPolicy};
        use crate::reapi_client::{ReapiClient, ReapiConfig};
        let ch = Arc::new(Channel::new(self.connector(), 4, vec![]));
        let cfg = ReapiConfig {
            retry: RetryPolicy { initial_delay: std::time::Duration::from_millis(1), max_retries: 3, ..Default::default() },
            ..Default::default()
        };
        ReapiClient::new(ch.clone(), if exec { Some(ch) } else { None }, cfg)
    }

    /// A [`Connector`] producing in-memory connections to this server.
    pub fn connector(&self) -> Arc<dyn Connector> {
        Arc::new(FakeConnector(self.clone()))
    }

    // ---------------------------------------------------------------- request handling

    fn handle_stream(&self, st: Stream) {
        let mut headers: Vec<Header> = Vec::new();
        let mut parser = MessageParser::default();
        let mut msgs: Vec<Vec<u8>> = Vec::new();
        loop {
            match st.recv(None) {
                Ok(Event::Headers(h)) => headers = h,
                Ok(Event::Data(d)) => {
                    parser.push(&d);
                    while let Ok(Some(m)) = parser.next() {
                        msgs.push(m);
                    }
                }
                Ok(Event::End) => break,
                _ => return,
            }
        }
        let path = headers.iter().find(|h| h.0 == ":path").map(|h| h.1.clone()).unwrap_or_default();
        let fault = {
            let mut g = self.sh.inner.lock().unwrap();
            *g.counts.entry(path.clone()).or_insert(0) += 1;
            g.last_headers.insert(path.clone(), headers.clone());
            match g.faults.iter().position(|(m, _)| path.contains(m.as_str())) {
                Some(i) => g.faults.remove(i).map(|f| f.1),
                None => None,
            }
        };
        if let Some(f) = fault {
            let _ = respond(&st, Err(f));
            return;
        }
        let res = self.dispatch(&path, &msgs, &st);
        let _ = respond(&st, res);
    }

    fn dispatch(&self, path: &str, msgs: &[Vec<u8>], st: &Stream) -> Result<Vec<Vec<u8>>, GrpcStatus> {
        let first = msgs.first().map(|v| v.as_slice()).unwrap_or(&[]);
        let bad = |e: zel_proto::DecodeError| GrpcStatus::new(3, format!("bad request: {e}"));
        let cfg = self.sh.cfg.lock().unwrap().clone();
        if path == CAPS {
            let _ = GetCapabilitiesRequest::decode(first).map_err(bad)?;
            let caps = ServerCapabilities {
                cache_capabilities: Some(CacheCapabilities {
                    digest_functions: cfg.advertise_digest_functions.clone(),
                    action_cache_update_capabilities: Some(ActionCacheUpdateCapabilities {
                        update_enabled: cfg.update_enabled,
                    }),
                    max_batch_total_size_bytes: cfg.max_batch_total_size_bytes,
                    ..Default::default()
                }),
                execution_capabilities: Some(ExecutionCapabilities {
                    digest_function: digest_function::SHA256,
                    exec_enabled: cfg.exec_enabled,
                    ..Default::default()
                }),
                low_api_version: Some(cfg.low_api_version.clone()),
                high_api_version: Some(cfg.high_api_version.clone()),
                ..Default::default()
            };
            return Ok(vec![caps.encode_to_vec()]);
        }
        match path.strip_prefix(CAS) {
            Some("FindMissingBlobs") => {
                let r = FindMissingBlobsRequest::decode(first).map_err(bad)?;
                let mut g = self.sh.inner.lock().unwrap();
                let missing: Vec<Digest> =
                    r.blob_digests.iter().filter(|d| !g.cas.contains_key(&d.hash)).cloned().collect();
                let forget: Vec<String> = g.forget_after_find_missing.clone();
                for d in &r.blob_digests {
                    if let Some(i) = forget.iter().position(|h| *h == d.hash) {
                        if g.cas.remove(&d.hash).is_some() {
                            g.forget_after_find_missing.retain(|h| *h != forget[i]);
                        }
                    }
                }
                return Ok(vec![FindMissingBlobsResponse { missing_blob_digests: missing }.encode_to_vec()]);
            }
            Some("BatchUpdateBlobs") => {
                let r = BatchUpdateBlobsRequest::decode(first).map_err(bad)?;
                let total: i64 = r.requests.iter().map(|x| x.data.len() as i64).sum();
                if cfg.max_batch_total_size_bytes > 0 && total > cfg.max_batch_total_size_bytes {
                    return Err(GrpcStatus::new(3, "batch too large"));
                }
                let mut resp = BatchUpdateBlobsResponse::default();
                let mut g = self.sh.inner.lock().unwrap();
                for item in r.requests {
                    let d = item.digest.clone().unwrap_or_default();
                    let ok = sha256::hex_digest(&item.data) == d.hash && item.data.len() as i64 == d.size_bytes;
                    let status = if ok {
                        g.cas.insert(d.hash.clone(), item.data);
                        g.cas_writes += 1;
                        RpcStatus::default()
                    } else {
                        RpcStatus { code: 3, message: "digest mismatch".into(), details: vec![] }
                    };
                    resp.responses.push(BatchUpdateBlobsResponseItem { digest: Some(d), status: Some(status) });
                }
                return Ok(vec![resp.encode_to_vec()]);
            }
            Some("BatchReadBlobs") => {
                let r = BatchReadBlobsRequest::decode(first).map_err(bad)?;
                let mut resp = BatchReadBlobsResponse::default();
                let mut g = self.sh.inner.lock().unwrap();
                for d in r.digests {
                    let evicted = g.evict_on_read.iter().position(|h| *h == d.hash).map(|i| g.evict_on_read.remove(i)).is_some();
                    let (data, status) = match g.cas.get(&d.hash) {
                        Some(b) if !evicted => (b.clone(), RpcStatus::default()),
                        _ => (vec![], RpcStatus { code: 5, message: "not found".into(), details: vec![] }),
                    };
                    resp.responses.push(BatchReadBlobsResponseItem { digest: Some(d), data, compressor: 0, status: Some(status) });
                }
                return Ok(vec![resp.encode_to_vec()]);
            }
            Some("GetTree") => {
                let r = GetTreeRequest::decode(first).map_err(bad)?;
                let root = r.root_digest.unwrap_or_default();
                let g = self.sh.inner.lock().unwrap();
                let mut dirs = Vec::new();
                let mut stack = vec![root];
                while let Some(d) = stack.pop() {
                    let blob = g.cas.get(&d.hash).ok_or_else(|| GrpcStatus::new(5, format!("missing directory {}", d.hash)))?;
                    let dir = Directory::decode(blob).map_err(bad)?;
                    for sub in &dir.directories {
                        stack.push(sub.digest.clone().unwrap_or_default());
                    }
                    dirs.push(dir);
                }
                let page = if cfg.get_tree_page_size > 0 { cfg.get_tree_page_size } else { usize::MAX };
                let start: usize = r.page_token.parse().unwrap_or(0);
                let end = start.saturating_add(page).min(dirs.len());
                let next = if end < dirs.len() { end.to_string() } else { String::new() };
                return Ok(vec![GetTreeResponse { directories: dirs[start..end].to_vec(), next_page_token: next }.encode_to_vec()]);
            }
            Some(_) => return Err(GrpcStatus::new(12, "unknown CAS method")),
            None => {}
        }
        match path.strip_prefix(AC) {
            Some("GetActionResult") => {
                let r = GetActionResultRequest::decode(first).map_err(bad)?;
                let d = r.action_digest.unwrap_or_default();
                let g = self.sh.inner.lock().unwrap();
                return match g.ac.get(&d.hash) {
                    Some(ar) => Ok(vec![ar.encode_to_vec()]),
                    None => Err(GrpcStatus::new(5, "action result not found")),
                };
            }
            Some("UpdateActionResult") => {
                if !cfg.update_enabled {
                    return Err(GrpcStatus::new(7, "action cache updates disabled"));
                }
                let r = UpdateActionResultRequest::decode(first).map_err(bad)?;
                let d = r.action_digest.unwrap_or_default();
                let ar = r.action_result.unwrap_or_default();
                self.sh.inner.lock().unwrap().ac.insert(d.hash, ar.clone());
                return Ok(vec![ar.encode_to_vec()]);
            }
            Some(_) => return Err(GrpcStatus::new(12, "unknown AC method")),
            None => {}
        }
        match path.strip_prefix(BYTESTREAM) {
            Some("Read") => {
                let r = ReadRequest::decode(first).map_err(bad)?;
                let (_inst, hash, size) = parse_blob_resource(&r.resource_name, false)?;
                let mut g = self.sh.inner.lock().unwrap();
                let evicted = g.evict_on_read.iter().position(|h| *h == hash).map(|i| g.evict_on_read.remove(i)).is_some();
                let blob = match g.cas.get(&hash) {
                    Some(b) if !evicted => b.clone(),
                    _ => return Err(GrpcStatus::new(5, "blob not found")),
                };
                if blob.len() as i64 != size {
                    return Err(GrpcStatus::new(5, "size mismatch"));
                }
                let off = r.read_offset.max(0) as usize;
                if off > blob.len() {
                    return Err(GrpcStatus::new(11, "offset out of range"));
                }
                let mut data = &blob[off..];
                if r.read_limit > 0 && (r.read_limit as usize) < data.len() {
                    data = &data[..r.read_limit as usize];
                }
                let mut out = Vec::new();
                for ch in data.chunks(cfg.read_chunk.max(1)) {
                    out.push(ReadResponse { data: ch.to_vec() }.encode_to_vec());
                }
                if data.is_empty() {
                    out.push(ReadResponse::default().encode_to_vec());
                }
                return Ok(out);
            }
            Some("Write") => {
                let mut data = Vec::new();
                let mut resource = String::new();
                let mut finished = false;
                let mut expected_off = 0i64;
                for m in msgs {
                    let w = WriteRequest::decode(m).map_err(bad)?;
                    if resource.is_empty() {
                        resource = w.resource_name.clone();
                    }
                    if w.write_offset != expected_off {
                        return Err(GrpcStatus::new(3, "unexpected write offset"));
                    }
                    expected_off += w.data.len() as i64;
                    data.extend(w.data);
                    finished = w.finish_write;
                }
                let (_inst, hash, size) = parse_blob_resource(&resource, true)?;
                if !finished {
                    return Err(GrpcStatus::new(3, "missing finish_write"));
                }
                if data.len() as i64 != size || sha256::hex_digest(&data) != hash {
                    return Err(GrpcStatus::new(3, "digest mismatch"));
                }
                let mut g = self.sh.inner.lock().unwrap();
                g.cas.insert(hash, data);
                g.cas_writes += 1;
                return Ok(vec![WriteResponse { committed_size: size }.encode_to_vec()]);
            }
            Some(_) => return Err(GrpcStatus::new(12, "unknown ByteStream method")),
            None => {}
        }
        match path.strip_prefix(EXEC) {
            Some("Execute") => {
                if !cfg.exec_enabled {
                    return Err(GrpcStatus::new(12, "execution disabled"));
                }
                let r = ExecuteRequest::decode(first).map_err(bad)?;
                self.execute(r, &cfg, st)
            }
            Some("WaitExecution") => {
                let r = WaitExecutionRequest::decode(first).map_err(bad)?;
                let g = self.sh.inner.lock().unwrap();
                match g.ops.get(&r.name) {
                    Some(ops) => Ok(vec![ops.last().unwrap().encode_to_vec()]),
                    None => Err(GrpcStatus::new(5, "unknown operation")),
                }
            }
            _ => Err(GrpcStatus::new(12, format!("unimplemented: {path}"))),
        }
    }

    fn execute(&self, r: ExecuteRequest, cfg: &FakeConfig, st: &Stream) -> Result<Vec<Vec<u8>>, GrpcStatus> {
        let action_digest = r.action_digest.clone().unwrap_or_default();
        let name = {
            let mut g = self.sh.inner.lock().unwrap();
            g.next_op += 1;
            format!("operations/{}", g.next_op)
        };
        let meta = |stage: i32| AnyMsg {
            type_url: EXECUTE_METADATA_TYPE.into(),
            value: ExecuteOperationMetadata { stage, action_digest: Some(action_digest.clone()), ..Default::default() }.encode_to_vec(),
        };
        let mut ops: Vec<Operation> = vec![
            Operation { name: name.clone(), metadata: Some(meta(execution_stage::QUEUED)), ..Default::default() },
        ];
        // Cache lookup.
        let cached = if r.skip_cache_lookup { None } else { self.sh.inner.lock().unwrap().ac.get(&action_digest.hash).cloned() };
        let done_resp = if let Some(ar) = cached {
            ExecuteResponse { result: Some(ar), cached_result: true, ..Default::default() }
        } else {
            ops.push(Operation { name: name.clone(), metadata: Some(meta(execution_stage::EXECUTING)), ..Default::default() });
            self.run_action(&action_digest)
        };
        ops.push(Operation {
            name: name.clone(),
            metadata: Some(meta(execution_stage::COMPLETED)),
            done: true,
            response: Some(AnyMsg { type_url: EXECUTE_RESPONSE_TYPE.into(), value: done_resp.encode_to_vec() }),
            ..Default::default()
        });
        self.sh.inner.lock().unwrap().ops.insert(name, ops.clone());
        if cfg.drop_execute_stream_after_first_op {
            // Stream the first op, then break the stream with UNAVAILABLE.
            let _ = st.send_headers(&[(":status".into(), "200".into()), ("content-type".into(), "application/grpc".into())], false);
            let _ = st.send_data(&frame_message(&ops[0].encode_to_vec()), false, None);
            return Err(GrpcStatus::new(14, "stream interrupted"));
        }
        Ok(ops.iter().map(|o| o.encode_to_vec()).collect())
    }

    fn run_action(&self, action_digest: &Digest) -> ExecuteResponse {
        let missing = |what: &str, d: &Digest| ExecuteResponse {
            status: Some(RpcStatus {
                code: 9,
                message: format!("missing {what}"),
                details: vec![AnyMsg {
                    type_url: PRECONDITION_FAILURE_TYPE.into(),
                    value: PreconditionFailure {
                        violations: vec![PreconditionViolation {
                            r#type: "MISSING".into(),
                            subject: format!("blobs/{}/{}", d.hash, d.size_bytes),
                            description: what.into(),
                        }],
                    }
                    .encode_to_vec(),
                }],
            }),
            ..Default::default()
        };
        let (action, command) = {
            let g = self.sh.inner.lock().unwrap();
            let Some(ab) = g.cas.get(&action_digest.hash) else { return missing("action", action_digest) };
            let Ok(action) = Action::decode(ab) else {
                return ExecuteResponse { status: Some(RpcStatus { code: 3, message: "bad action".into(), details: vec![] }), ..Default::default() };
            };
            let cd = action.command_digest.clone().unwrap_or_default();
            let Some(cb) = g.cas.get(&cd.hash) else { return missing("command", &cd) };
            let Ok(command) = Command::decode(cb) else {
                return ExecuteResponse { status: Some(RpcStatus { code: 3, message: "bad command".into(), details: vec![] }), ..Default::default() };
            };
            let rd = action.input_root_digest.clone().unwrap_or_default();
            // Like real servers, verify the whole input tree is present in the CAS.
            let mut stack = vec![rd];
            while let Some(d) = stack.pop() {
                // The empty blob is implicitly present in every CAS.
                let empty = Vec::new();
                let b = match g.cas.get(&d.hash) {
                    Some(b) => b,
                    None if d.size_bytes == 0 => &empty,
                    None => return missing("input directory", &d),
                };
                let Ok(dir) = Directory::decode(b) else {
                    return ExecuteResponse { status: Some(RpcStatus { code: 3, message: "bad directory".into(), details: vec![] }), ..Default::default() };
                };
                for f in &dir.files {
                    let fd = f.digest.clone().unwrap_or_default();
                    if fd.size_bytes > 0 && !g.cas.contains_key(&fd.hash) {
                        return missing("input file", &fd);
                    }
                }
                for sub in &dir.directories {
                    stack.push(sub.digest.clone().unwrap_or_default());
                }
            }
            (action, command)
        };
        let exec = self.sh.executor.lock().unwrap().clone();
        let Some(exec) = exec else {
            return ExecuteResponse { status: Some(RpcStatus { code: 12, message: "no executor".into(), details: vec![] }), ..Default::default() };
        };
        self.sh.inner.lock().unwrap().executions += 1;
        let ctx = ExecContext { server: self, action: &action, command: &command, action_digest };
        match exec(&ctx) {
            Ok(ar) => {
                if !action.do_not_cache && ar.exit_code == 0 {
                    self.sh.inner.lock().unwrap().ac.insert(action_digest.hash.clone(), ar.clone());
                }
                ExecuteResponse { result: Some(ar), ..Default::default() }
            }
            Err(e) => ExecuteResponse { status: Some(RpcStatus { code: e.code, message: e.message, details: vec![] }), ..Default::default() },
        }
    }
}

fn parse_blob_resource(res: &str, write: bool) -> Result<(String, String, i64), GrpcStatus> {
    let parts: Vec<&str> = res.split('/').collect();
    let marker = if write { "uploads" } else { "blobs" };
    let pos = parts
        .iter()
        .position(|p| *p == marker)
        .ok_or_else(|| GrpcStatus::new(3, format!("bad resource name {res}")))?;
    let inst = parts[..pos].join("/");
    let rest = &parts[pos + 1..];
    let rest: Vec<&str> = if write {
        // uploads/{uuid}/blobs/{hash}/{size}[/extra]
        if rest.len() < 4 || rest[1] != "blobs" {
            return Err(GrpcStatus::new(3, "bad upload resource"));
        }
        rest[2..].to_vec()
    } else {
        rest.to_vec()
    };
    if rest.len() < 2 {
        return Err(GrpcStatus::new(3, "bad blob resource"));
    }
    let size: i64 = rest[1].parse().map_err(|_| GrpcStatus::new(3, "bad size"))?;
    Ok((inst, rest[0].to_string(), size))
}

/// Writes a complete gRPC response (headers + messages + trailers) onto `st`.
fn respond(st: &Stream, res: Result<Vec<Vec<u8>>, GrpcStatus>) -> io::Result<()> {
    match res {
        Ok(msgs) => {
            st.send_headers(
                &[(":status".into(), "200".into()), ("content-type".into(), "application/grpc".into())],
                false,
            )?;
            for m in msgs {
                st.send_data(&frame_message(&m), false, None)?;
            }
            st.send_headers(&[("grpc-status".into(), "0".into())], true)
        }
        Err(e) => st.send_headers(
            &[
                (":status".into(), "200".into()),
                ("content-type".into(), "application/grpc".into()),
                ("grpc-status".into(), e.code.to_string()),
                ("grpc-message".into(), percent_encode(&e.message)),
            ],
            true,
        ),
    }
}

struct FakeConnector(FakeReapiServer);

impl Connector for FakeConnector {
    fn connect(&self) -> io::Result<Transport> {
        Ok(self.0.connect_memory())
    }
    fn authority(&self) -> String {
        "fake-reapi.local".into()
    }
}
