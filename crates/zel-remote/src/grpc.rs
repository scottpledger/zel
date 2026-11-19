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

//! gRPC (over [`crate::http2`]) client channel with a connection pool, deadlines, dynamic call
//! credentials, `grpc-status-details-bin` decoding, unary / client-streaming / server-streaming /
//! full-duplex bidirectional calls, and Bazel-style retries (`RemoteRetrier`).

use crate::credentials::CallCredentialsProvider;
use crate::hpack::Header;
use crate::http2::{Conn, Event, LocalSettings, Stream};
use crate::transport::Connector;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zel_proto::reapi::{code, AnyMsg, Message, PreconditionFailure, RetryInfo, RpcStatus};
use zel_util::base64_decode;

/// A gRPC status (non-OK when used as an error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrpcStatus {
    pub code: i32,
    pub message: String,
    pub details: Vec<AnyMsg>,
    pub credential_helper_error: bool,
}

impl GrpcStatus {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: Vec::new(),
            credential_helper_error: false,
        }
    }

    pub fn with_details(code: i32, message: impl Into<String>, details: Vec<AnyMsg>) -> Self {
        Self {
            code,
            message: message.into(),
            details,
            credential_helper_error: false,
        }
    }

    pub fn credential_helper(message: impl Into<String>) -> Self {
        Self {
            code: code::UNAUTHENTICATED,
            message: message.into(),
            details: Vec::new(),
            credential_helper_error: true,
        }
    }

    pub fn from_rpc_status(st: RpcStatus) -> Self {
        Self {
            code: st.code,
            message: st.message,
            details: st.details,
            credential_helper_error: false,
        }
    }

    pub fn retry_info_delay(&self) -> Option<Duration> {
        for any in &self.details {
            if any.type_url.ends_with("/google.rpc.RetryInfo")
                || any.type_url == "google.rpc.RetryInfo"
            {
                if let Ok(ri) = RetryInfo::decode(&any.value[..]) {
                    if let Some(d) = ri.retry_delay {
                        let secs = d.seconds.max(0) as u64;
                        let nanos = d.nanos.clamp(0, 999_999_999) as u32;
                        return Some(Duration::new(secs, nanos));
                    }
                    return Some(Duration::ZERO);
                }
            }
        }
        None
    }

    pub fn precondition_failure(&self) -> Option<PreconditionFailure> {
        for any in &self.details {
            if any.type_url.ends_with("/google.rpc.PreconditionFailure")
                || any.type_url == "google.rpc.PreconditionFailure"
            {
                if let Ok(pf) = PreconditionFailure::decode(&any.value[..]) {
                    return Some(pf);
                }
            }
        }
        None
    }

    /// Mirrors Bazel's `GrpcRemoteExecutor.isMissingBlobs` / `RemoteRetrier.PASS_THROUGH`:
    /// `FAILED_PRECONDITION` whose details include at least one `PreconditionFailure` with
    /// non-empty `violations` that are all `MISSING`, and no other non-informational details.
    pub fn is_missing_blobs_error(&self) -> bool {
        if self.code != code::FAILED_PRECONDITION || self.details.is_empty() {
            return false;
        }
        let mut saw_pf = false;
        for any in &self.details {
            let ty = any
                .type_url
                .rsplit('/')
                .next()
                .unwrap_or(any.type_url.as_str());
            match ty {
                "google.rpc.PreconditionFailure" => {
                    let Ok(pf) = PreconditionFailure::decode(&any.value[..]) else {
                        return false;
                    };
                    if pf.violations.is_empty()
                        || pf.violations.iter().any(|v| v.r#type != "MISSING")
                    {
                        return false;
                    }
                    saw_pf = true;
                }
                "google.rpc.RequestInfo"
                | "google.rpc.DebugInfo"
                | "google.rpc.LocalizedMessage"
                | "google.rpc.Help" => {}
                _ => return false,
            }
        }
        saw_pf
    }

    pub fn code_name(&self) -> &'static str {
        match self.code {
            code::OK => "OK",
            code::CANCELLED => "CANCELLED",
            code::UNKNOWN => "UNKNOWN",
            code::INVALID_ARGUMENT => "INVALID_ARGUMENT",
            code::DEADLINE_EXCEEDED => "DEADLINE_EXCEEDED",
            code::NOT_FOUND => "NOT_FOUND",
            code::ALREADY_EXISTS => "ALREADY_EXISTS",
            code::PERMISSION_DENIED => "PERMISSION_DENIED",
            code::RESOURCE_EXHAUSTED => "RESOURCE_EXHAUSTED",
            code::FAILED_PRECONDITION => "FAILED_PRECONDITION",
            code::ABORTED => "ABORTED",
            code::OUT_OF_RANGE => "OUT_OF_RANGE",
            code::UNIMPLEMENTED => "UNIMPLEMENTED",
            code::INTERNAL => "INTERNAL",
            code::UNAVAILABLE => "UNAVAILABLE",
            code::DATA_LOSS => "DATA_LOSS",
            code::UNAUTHENTICATED => "UNAUTHENTICATED",
            _ => "UNKNOWN_CODE",
        }
    }
}

impl fmt::Display for GrpcStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty() {
            write!(f, "{}", self.code_name())
        } else {
            write!(f, "{}: {}", self.code_name(), self.message)
        }
    }
}

impl std::error::Error for GrpcStatus {}

/// Per-call options.
#[derive(Debug, Clone, Default)]
pub struct CallOptions {
    pub timeout: Option<Duration>,
    pub metadata: Vec<Header>,
}

pub fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        if (0x20..=0x7e).contains(&b) && b != b'%' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Length-prefixed gRPC message framing (uncompressed).
pub fn frame_message(msg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + msg.len());
    out.push(0);
    out.extend_from_slice(&(msg.len() as u32).to_be_bytes());
    out.extend_from_slice(msg);
    out
}

/// Incremental parser for gRPC message framing.
#[derive(Default)]
pub struct MessageParser {
    buf: Vec<u8>,
}

impl MessageParser {
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Pops the next complete message; `Err` for compressed messages (unsupported).
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Vec<u8>>, GrpcStatus> {
        if self.buf.len() < 5 {
            return Ok(None);
        }
        let compressed = self.buf[0];
        let len = u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
        if self.buf.len() < 5 + len {
            return Ok(None);
        }
        if compressed != 0 {
            return Err(GrpcStatus::new(
                code::INTERNAL,
                "received compressed gRPC message but only the identity encoding is supported",
            ));
        }
        let msg = self.buf[5..5 + len].to_vec();
        self.buf.drain(..5 + len);
        Ok(Some(msg))
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

fn http_status_to_grpc(status: u32) -> i32 {
    match status {
        400 => code::INTERNAL,
        401 => code::UNAUTHENTICATED,
        403 => code::PERMISSION_DENIED,
        404 => code::UNIMPLEMENTED,
        429 | 502 | 503 | 504 => code::UNAVAILABLE,
        _ => code::UNKNOWN,
    }
}

fn h2_reset_to_grpc(c: u32) -> i32 {
    match c {
        0 => code::INTERNAL,            // NO_ERROR before trailers is a protocol error
        7 => code::UNAVAILABLE,         // REFUSED_STREAM
        8 => code::CANCELLED,           // CANCEL
        11 => code::RESOURCE_EXHAUSTED, // ENHANCE_YOUR_CALM
        12 => code::PERMISSION_DENIED,  // INADEQUATE_SECURITY
        _ => code::INTERNAL,
    }
}

fn io_to_status(e: &std::io::Error) -> GrpcStatus {
    match e.kind() {
        std::io::ErrorKind::TimedOut => {
            GrpcStatus::new(code::DEADLINE_EXCEEDED, "deadline exceeded")
        }
        _ => GrpcStatus::new(code::UNAVAILABLE, format!("transport error: {e}")),
    }
}

/// Configuration for [`Channel::with_config`].
pub struct ChannelConfig {
    pub tls: bool,
    /// `0` means unlimited connections (matching Bazel `--remote_max_connections=0`).
    pub max_connections: usize,
    /// `0` means unlimited concurrent streams per connection (`--remote_max_concurrency_per_connection`).
    pub max_concurrency_per_connection: usize,
    pub default_metadata: Vec<Header>,
    pub credentials: Option<Arc<dyn CallCredentialsProvider>>,
    pub auth_uri: String,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            tls: false,
            max_connections: 100,
            max_concurrency_per_connection: 100,
            default_metadata: Vec::new(),
            credentials: None,
            auth_uri: String::new(),
        }
    }
}

/// Full-duplex gRPC stream handle for concurrent client-send + server-receive RPCs (such as BES
/// `PublishBuildToolEventStream`).
#[derive(Clone)]
pub struct BidiStream {
    stream: Arc<Stream>,
}

impl BidiStream {
    pub fn send_message(
        &self,
        msg: &[u8],
        end_stream: bool,
        timeout: Option<Duration>,
    ) -> Result<(), GrpcStatus> {
        self.stream
            .send_data(&frame_message(msg), end_stream, timeout)
            .map_err(|e| io_to_status(&e))
    }

    pub fn close_send(&self) -> Result<(), GrpcStatus> {
        self.stream
            .send_data(&[], true, None)
            .map_err(|e| io_to_status(&e))
    }

    pub fn cancel(&self) {
        self.stream.reset(crate::http2::ERR_CANCEL);
    }

    pub fn read_responses(
        &self,
        deadline: Option<Instant>,
        on_message: &mut dyn FnMut(Vec<u8>) -> Result<(), GrpcStatus>,
    ) -> Result<(), GrpcStatus> {
        read_response(&self.stream, deadline, on_message)
    }
}

/// A pooled gRPC channel. Cheap to clone via `Arc`.
pub struct Channel {
    connector: Arc<dyn Connector>,
    max_connections: usize,
    streams_per_conn: usize,
    conns: Mutex<Vec<Conn>>,
    default_metadata: Vec<Header>,
    scheme: &'static str,
    settings: LocalSettings,
    credentials: Option<Arc<dyn CallCredentialsProvider>>,
    auth_uri: String,
    pub connections_opened: AtomicUsize,
}

/// Default concurrent streams per connection before opening another one (Bazel default: 100).
const DEFAULT_STREAMS_PER_CONN: usize = 100;

impl Channel {
    pub fn new(
        connector: Arc<dyn Connector>,
        max_connections: usize,
        default_metadata: Vec<Header>,
    ) -> Self {
        Self::with_config(
            connector,
            ChannelConfig {
                tls: false,
                max_connections,
                max_concurrency_per_connection: DEFAULT_STREAMS_PER_CONN,
                default_metadata,
                credentials: None,
                auth_uri: String::new(),
            },
        )
    }

    pub fn with_config(connector: Arc<dyn Connector>, cfg: ChannelConfig) -> Self {
        let auth_uri = if cfg.auth_uri.is_empty() {
            format!(
                "{}://{}",
                if cfg.tls { "https" } else { "http" },
                connector.authority()
            )
        } else {
            cfg.auth_uri
        };
        Self {
            connector,
            max_connections: if cfg.max_connections == 0 {
                usize::MAX
            } else {
                cfg.max_connections
            },
            streams_per_conn: if cfg.max_concurrency_per_connection == 0 {
                usize::MAX
            } else {
                cfg.max_concurrency_per_connection
            },
            conns: Mutex::new(Vec::new()),
            default_metadata: cfg
                .default_metadata
                .into_iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v))
                .collect(),
            scheme: if cfg.tls { "https" } else { "http" },
            settings: LocalSettings::default(),
            credentials: cfg.credentials,
            auth_uri,
            connections_opened: AtomicUsize::new(0),
        }
    }

    pub fn authority(&self) -> String {
        self.connector.authority()
    }

    pub fn credentials(&self) -> Option<&Arc<dyn CallCredentialsProvider>> {
        self.credentials.as_ref()
    }

    pub fn refresh_credentials(&self) -> Result<(), GrpcStatus> {
        if let Some(creds) = &self.credentials {
            creds.refresh().map_err(|e| {
                if e.is_helper_error() {
                    GrpcStatus::credential_helper(e.to_string())
                } else {
                    GrpcStatus::new(code::UNAUTHENTICATED, e.to_string())
                }
            })?;
        }
        Ok(())
    }

    fn pick_conn(&self) -> Result<Conn, GrpcStatus> {
        let mut conns = self.conns.lock().unwrap();
        conns.retain(|c| !c.is_closed());
        let least = conns.iter().min_by_key(|c| c.active_streams()).cloned();
        if let Some(c) = &least {
            if c.active_streams() < self.streams_per_conn || conns.len() >= self.max_connections {
                return Ok(c.clone());
            }
        }
        let t = self
            .connector
            .connect()
            .map_err(|e| GrpcStatus::new(code::UNAVAILABLE, format!("connection failed: {e}")))?;
        let conn = Conn::client_from(t, self.settings.clone())
            .map_err(|e| GrpcStatus::new(code::UNAVAILABLE, format!("h2 handshake failed: {e}")))?;
        self.connections_opened.fetch_add(1, Ordering::SeqCst);
        conns.push(conn.clone());
        Ok(conn)
    }

    pub fn close(&self) {
        for c in self.conns.lock().unwrap().drain(..) {
            c.close();
        }
    }

    fn build_request_headers(
        &self,
        method: &str,
        opts: &CallOptions,
    ) -> Result<Vec<Header>, GrpcStatus> {
        let mut headers: Vec<Header> = vec![
            (":method".into(), "POST".into()),
            (":scheme".into(), self.scheme.into()),
            (":path".into(), method.into()),
            (":authority".into(), self.connector.authority()),
            ("content-type".into(), "application/grpc".into()),
            ("te".into(), "trailers".into()),
            ("user-agent".into(), "zel-grpc/0.0.1".into()),
            ("grpc-accept-encoding".into(), "identity".into()),
        ];
        if let Some(t) = opts.timeout {
            headers.push(("grpc-timeout".into(), format!("{}m", t.as_millis().max(1))));
        }
        if let Some(creds) = &self.credentials {
            let dyn_hdrs = creds.get_credentials(&self.auth_uri).map_err(|e| {
                if e.is_helper_error() {
                    GrpcStatus::credential_helper(e.to_string())
                } else {
                    GrpcStatus::new(code::UNAUTHENTICATED, e.to_string())
                }
            })?;
            headers.extend(dyn_hdrs);
        }
        headers.extend(self.default_metadata.iter().cloned());
        headers.extend(
            opts.metadata
                .iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v.clone())),
        );
        Ok(headers)
    }

    /// Opens a full-duplex bidirectional stream (request frames sent via [`BidiStream`]).
    pub fn open_bidi(
        &self,
        method: &str,
        opts: &CallOptions,
    ) -> Result<(BidiStream, Option<Instant>), GrpcStatus> {
        let deadline = opts.timeout.map(|t| Instant::now() + t);
        let headers = self.build_request_headers(method, opts)?;
        let conn = self.pick_conn()?;
        let stream = conn
            .open_stream(&headers, false)
            .map_err(|e| io_to_status(&e))?;
        Ok((
            BidiStream {
                stream: Arc::new(stream),
            },
            deadline,
        ))
    }

    /// Generic call: sends `requests` (client streaming), delivers each response message to
    /// `on_message`. Returns Ok(()) when the call finished with grpc-status OK.
    pub fn call(
        &self,
        method: &str,
        requests: &mut dyn Iterator<Item = Vec<u8>>,
        opts: &CallOptions,
        on_message: &mut dyn FnMut(Vec<u8>) -> Result<(), GrpcStatus>,
    ) -> Result<(), GrpcStatus> {
        self.call_with_started(method, requests, opts, &mut || {}, on_message)
    }

    pub fn call_with_started(
        &self,
        method: &str,
        requests: &mut dyn Iterator<Item = Vec<u8>>,
        opts: &CallOptions,
        on_started: &mut dyn FnMut(),
        on_message: &mut dyn FnMut(Vec<u8>) -> Result<(), GrpcStatus>,
    ) -> Result<(), GrpcStatus> {
        let deadline = opts.timeout.map(|t| Instant::now() + t);
        let headers = self.build_request_headers(method, opts)?;
        let conn = self.pick_conn()?;
        let stream = conn
            .open_stream(&headers, false)
            .map_err(|e| io_to_status(&e))?;

        let mut sent_end = false;
        let mut pending = requests.next();
        let mut send_failed = false;
        while let Some(msg) = pending {
            pending = requests.next();
            let last = pending.is_none();
            let remaining = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            if stream
                .send_data(&frame_message(&msg), last, remaining)
                .is_err()
            {
                send_failed = true;
                break;
            }
            sent_end = last;
        }
        if !send_failed && !sent_end && stream.send_data(&[], true, None).is_err() {
            send_failed = true;
        }
        let _ = send_failed;
        on_started();

        read_response(&stream, deadline, on_message)
    }

    /// Unary call (refreshes dynamic credentials once on `UNAUTHENTICATED`).
    pub fn unary(
        &self,
        method: &str,
        request: &[u8],
        opts: &CallOptions,
    ) -> Result<Vec<u8>, GrpcStatus> {
        let run_once = || -> Result<Vec<u8>, GrpcStatus> {
            let mut out: Option<Vec<u8>> = None;
            let mut reqs = std::iter::once(request.to_vec());
            self.call(method, &mut reqs, opts, &mut |m| {
                if out.is_some() {
                    return Err(GrpcStatus::new(
                        code::INTERNAL,
                        "multiple responses to unary call",
                    ));
                }
                out = Some(m);
                Ok(())
            })?;
            out.ok_or_else(|| GrpcStatus::new(code::INTERNAL, "unary call returned no message"))
        };
        match run_once() {
            Err(e)
                if e.code == code::UNAUTHENTICATED
                    && !e.credential_helper_error
                    && self.credentials.is_some() =>
            {
                self.refresh_credentials()?;
                run_once()
            }
            other => other,
        }
    }

    /// Server-streaming call collecting all messages (refreshes dynamic credentials once on
    /// `UNAUTHENTICATED` before any messages are yielded).
    pub fn server_stream(
        &self,
        method: &str,
        request: &[u8],
        opts: &CallOptions,
        on_message: &mut dyn FnMut(Vec<u8>) -> Result<(), GrpcStatus>,
    ) -> Result<(), GrpcStatus> {
        let mut yielded_any = false;
        let mut reqs = std::iter::once(request.to_vec());
        let res = self.call(method, &mut reqs, opts, &mut |m| {
            yielded_any = true;
            on_message(m)
        });
        match res {
            Err(e)
                if !yielded_any
                    && e.code == code::UNAUTHENTICATED
                    && !e.credential_helper_error
                    && self.credentials.is_some() =>
            {
                self.refresh_credentials()?;
                let mut reqs2 = std::iter::once(request.to_vec());
                self.call(method, &mut reqs2, opts, on_message)
            }
            other => other,
        }
    }
}

fn header<'a>(h: &'a [Header], name: &str) -> Option<&'a str> {
    h.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

fn decode_status_details(h: &[Header]) -> Option<RpcStatus> {
    let raw_b64 = header(h, "grpc-status-details-bin")?;
    // gRPC binary metadata allows unpadded or padded standard base64.
    let mut padded = raw_b64.trim().to_string();
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    let bytes = base64_decode(&padded).ok()?;
    RpcStatus::decode(&bytes[..]).ok()
}

fn status_from_trailers(h: &[Header]) -> Option<GrpcStatus> {
    let c = header(h, "grpc-status")?;
    let code_val = c.trim().parse::<i32>().unwrap_or(code::UNKNOWN);
    let mut msg = header(h, "grpc-message")
        .map(percent_decode)
        .unwrap_or_default();
    let mut details = Vec::new();
    if let Some(rich) = decode_status_details(h) {
        if msg.is_empty() && !rich.message.is_empty() {
            msg = rich.message;
        }
        details = rich.details;
    }
    Some(GrpcStatus::with_details(code_val, msg, details))
}

fn read_response(
    stream: &Stream,
    deadline: Option<Instant>,
    on_message: &mut dyn FnMut(Vec<u8>) -> Result<(), GrpcStatus>,
) -> Result<(), GrpcStatus> {
    let mut parser = MessageParser::default();
    let mut got_headers = false;
    let mut final_status: Option<GrpcStatus> = None;
    loop {
        let ev = match stream.recv(deadline) {
            Ok(ev) => ev,
            Err(e) => {
                stream.reset(crate::http2::ERR_CANCEL);
                return Err(io_to_status(&e));
            }
        };
        match ev {
            Event::Headers(h) => {
                got_headers = true;
                if let Some(st) = header(&h, ":status") {
                    let st: u32 = st.parse().unwrap_or(0);
                    if st != 200 {
                        stream.abandon_if_remote_done();
                        return Err(status_from_trailers(&h).unwrap_or_else(|| {
                            GrpcStatus::new(http_status_to_grpc(st), format!("HTTP status {st}"))
                        }));
                    }
                }
                if let Some(s) = status_from_trailers(&h) {
                    final_status = Some(s);
                }
            }
            Event::Data(d) => {
                parser.push(&d);
                loop {
                    match parser.next() {
                        Ok(Some(m)) => {
                            if let Err(e) = on_message(m) {
                                stream.reset(crate::http2::ERR_CANCEL);
                                return Err(e);
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            stream.reset(crate::http2::ERR_CANCEL);
                            return Err(e);
                        }
                    }
                }
            }
            Event::Trailers(t) => {
                if let Some(s) = status_from_trailers(&t) {
                    final_status = Some(s);
                }
            }
            Event::End => {
                stream.abandon_if_remote_done();
                if !got_headers {
                    return Err(GrpcStatus::new(
                        code::UNAVAILABLE,
                        "stream closed without headers",
                    ));
                }
                if !parser.is_empty() {
                    return Err(GrpcStatus::new(code::INTERNAL, "truncated gRPC message"));
                }
                return match final_status {
                    Some(s) if s.code == code::OK => Ok(()),
                    Some(s) => Err(s),
                    None => Err(GrpcStatus::new(
                        code::INTERNAL,
                        "missing grpc-status trailer",
                    )),
                };
            }
            Event::Reset(c) => {
                return Err(GrpcStatus::new(
                    h2_reset_to_grpc(c),
                    format!("stream reset (HTTP/2 error {c})"),
                ));
            }
        }
    }
}

/// Bazel `RemoteRetrier` parameters: exponential backoff with jitter, `RetryInfo` support, and
/// cancellation awareness (`CANCELLED` is retried when caller is not cancelled).
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub multiplier: f64,
    pub jitter: f64,
    pub cancelled: Option<Arc<AtomicBool>>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(5),
            multiplier: 2.0,
            jitter: 0.1,
            cancelled: None,
        }
    }
}

impl RetryPolicy {
    pub fn is_caller_cancelled(&self) -> bool {
        self.cancelled
            .as_ref()
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    /// gRPC codes `RemoteRetrier.RETRIABLE_GRPC_ERRORS` retries (plus `RetryInfo` details).
    pub fn is_retriable(status: &GrpcStatus) -> bool {
        Self::is_retriable_with_cancel(status, false)
    }

    pub fn is_retriable_with_cancel(status: &GrpcStatus, caller_cancelled: bool) -> bool {
        if status.credential_helper_error || caller_cancelled {
            return false;
        }
        if status.retry_info_delay().is_some() {
            return true;
        }
        matches!(
            status.code,
            code::CANCELLED
                | code::UNKNOWN
                | code::DEADLINE_EXCEEDED
                | code::ABORTED
                | code::INTERNAL
                | code::UNAVAILABLE
                | code::RESOURCE_EXHAUSTED
        )
    }

    pub fn delay(&self, attempt: u32) -> Duration {
        let base = (self.initial_delay.as_secs_f64() * self.multiplier.powi(attempt as i32))
            .min(self.max_delay.as_secs_f64());
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let r = (nanos % 2001) as f64 / 1000.0 - 1.0; // [-1, 1]
        Duration::from_secs_f64((base * (1.0 + self.jitter * r)).max(0.0))
    }

    /// Runs `f`, retrying retriable failures with backoff up to `max_retries` times.
    pub fn run<T>(&self, mut f: impl FnMut() -> Result<T, GrpcStatus>) -> Result<T, GrpcStatus> {
        let mut attempt = 0;
        loop {
            if self.is_caller_cancelled() {
                return Err(GrpcStatus::new(code::CANCELLED, "Interrupted"));
            }
            match f() {
                Ok(v) => return Ok(v),
                Err(e) => {
                    if attempt >= self.max_retries
                        || !Self::is_retriable_with_cancel(&e, self.is_caller_cancelled())
                    {
                        return Err(e);
                    }
                    let wait = e
                        .retry_info_delay()
                        .map(|d| d.min(self.max_delay))
                        .unwrap_or_else(|| self.delay(attempt));
                    std::thread::sleep(wait);
                    attempt += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_proto::reapi::{Duration as ProtoDuration, PreconditionViolation};
    use zel_util::base64_encode;

    #[test]
    fn framing_and_parser_handle_split_chunks() {
        let a = frame_message(b"hello");
        let b = frame_message(b"");
        let mut all = a.clone();
        all.extend(&b);
        let mut p = MessageParser::default();
        for ch in all.chunks(3) {
            p.push(ch);
        }
        assert_eq!(p.next().unwrap().unwrap(), b"hello");
        assert_eq!(p.next().unwrap().unwrap(), b"");
        assert!(p.next().unwrap().is_none());
        let mut c = MessageParser::default();
        c.push(&[1, 0, 0, 0, 0]);
        assert!(c.next().is_err());
    }

    #[test]
    fn percent_coding() {
        let s = "bad \u{e9}rror 100%";
        assert_eq!(percent_decode(&percent_encode(s)), s);
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn status_details_missing_blobs_and_retry_info() {
        let pf = PreconditionFailure {
            violations: vec![PreconditionViolation {
                r#type: "MISSING".into(),
                subject: "blobs/abc/12".into(),
                description: "missing".into(),
            }],
        };
        let rpc_st = RpcStatus {
            code: code::FAILED_PRECONDITION,
            message: "missing input".into(),
            details: vec![AnyMsg {
                type_url: "type.googleapis.com/google.rpc.PreconditionFailure".into(),
                value: pf.encode_to_vec(),
            }],
        };
        let b64 = base64_encode(&rpc_st.encode_to_vec());
        let trailers = vec![
            (
                "grpc-status".to_string(),
                code::FAILED_PRECONDITION.to_string(),
            ),
            (
                "grpc-status-details-bin".to_string(),
                b64.trim_end_matches('=').to_string(),
            ),
        ];
        let parsed = status_from_trailers(&trailers).unwrap();
        assert!(parsed.is_missing_blobs_error());
        assert_eq!(parsed.message, "missing input");

        // Adding an unexpected sibling detail disqualifies is_missing_blobs_error.
        let mut mixed = parsed.clone();
        mixed.details.push(AnyMsg {
            type_url: "type.googleapis.com/google.rpc.QuotaFailure".into(),
            value: vec![],
        });
        assert!(!mixed.is_missing_blobs_error());

        // RetryInfo makes a non-default code retriable.
        let ri = RetryInfo {
            retry_delay: Some(ProtoDuration {
                seconds: 0,
                nanos: 1_000_000,
            }),
        };
        let st = GrpcStatus::with_details(
            code::FAILED_PRECONDITION,
            "busy",
            vec![AnyMsg {
                type_url: "type.googleapis.com/google.rpc.RetryInfo".into(),
                value: ri.encode_to_vec(),
            }],
        );
        assert!(RetryPolicy::is_retriable(&st));
    }

    #[test]
    fn retry_policy_retries_only_retriable() {
        let p = RetryPolicy {
            max_retries: 3,
            initial_delay: Duration::from_millis(1),
            ..Default::default()
        };
        let mut n = 0;
        let r: Result<(), _> = p.run(|| {
            n += 1;
            Err(GrpcStatus::new(code::UNAVAILABLE, "x"))
        });
        assert!(r.is_err());
        assert_eq!(n, 4);
        let mut n = 0;
        let r: Result<(), _> = p.run(|| {
            n += 1;
            Err(GrpcStatus::new(code::INVALID_ARGUMENT, "x"))
        });
        assert!(r.is_err());
        assert_eq!(n, 1);
        let mut n = 0;
        let r = p.run(|| {
            n += 1;
            if n < 3 {
                Err(GrpcStatus::new(code::ABORTED, "x"))
            } else {
                Ok(n)
            }
        });
        assert_eq!(r.unwrap(), 3);
    }
}
