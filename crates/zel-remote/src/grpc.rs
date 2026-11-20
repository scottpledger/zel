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
use std::sync::{Arc, Condvar, Mutex};
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
    fn hex_nibble(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(hi), Some(lo)) = (hex_nibble(b[i + 1]), hex_nibble(b[i + 2])) {
                out.push((hi << 4) | lo);
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

#[derive(Default)]
struct ConnPool {
    conns: Vec<Conn>,
    /// In-flight dials belonging to the current `close_gen`.
    dialing: usize,
    close_gen: u64,
    #[cfg(test)]
    waiters: usize,
}

struct DialGuard<'a> {
    pool: &'a Mutex<ConnPool>,
    cv: &'a Condvar,
    dial_gen: u64,
    active: bool,
}

impl<'a> DialGuard<'a> {
    fn new(pool: &'a Mutex<ConnPool>, cv: &'a Condvar, dial_gen: u64) -> Self {
        Self {
            pool,
            cv,
            dial_gen,
            active: true,
        }
    }

    fn complete(
        mut self,
        conn: Conn,
        connections_opened: &AtomicUsize,
    ) -> Result<Conn, GrpcStatus> {
        self.active = false;
        let mut p = self.pool.lock().unwrap();
        if p.close_gen != self.dial_gen {
            // `Channel::close()` already advanced `close_gen` and reset `dialing` for the
            // new generation; discard the stale connection without affecting newer dials.
            drop(p);
            conn.close();
            return Err(GrpcStatus::new(
                code::UNAVAILABLE,
                "channel closed while connecting",
            ));
        }
        p.dialing -= 1;
        connections_opened.fetch_add(1, Ordering::SeqCst);
        p.conns.push(conn.clone());
        drop(p);
        self.cv.notify_all();
        Ok(conn)
    }
}

impl Drop for DialGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            let mut should_notify = false;
            if let Ok(mut p) = self.pool.lock() {
                if p.close_gen == self.dial_gen {
                    p.dialing = p.dialing.saturating_sub(1);
                    should_notify = true;
                }
            }
            if should_notify {
                self.cv.notify_all();
            }
        }
    }
}

/// A pooled gRPC channel. Cheap to clone via `Arc`.
pub struct Channel {
    connector: Arc<dyn Connector>,
    max_connections: usize,
    streams_per_conn: usize,
    pool: Mutex<ConnPool>,
    pool_cv: Condvar,
    #[cfg(test)]
    waiter_cv: Condvar,
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
            crate::credentials::normalize_credential_uri(&cfg.auth_uri)
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
            pool: Mutex::new(ConnPool::default()),
            pool_cv: Condvar::new(),
            #[cfg(test)]
            waiter_cv: Condvar::new(),
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

    pub fn auth_uri(&self) -> &str {
        &self.auth_uri
    }

    pub fn credentials(&self) -> Option<&Arc<dyn CallCredentialsProvider>> {
        self.credentials.as_ref()
    }

    pub fn has_refreshable_credentials(&self) -> bool {
        self.credentials
            .as_ref()
            .is_some_and(|c| c.supports_refresh(&self.auth_uri))
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

    fn pick_conn(&self, deadline: Option<Instant>) -> Result<Conn, GrpcStatus> {
        let dial_gen = {
            let mut p = self.pool.lock().unwrap();
            let initial_gen = p.close_gen;
            loop {
                if p.close_gen != initial_gen {
                    return Err(GrpcStatus::new(code::UNAVAILABLE, "channel closed"));
                }
                p.conns.retain(|c| !c.is_closed());
                let least = p.conns.iter().min_by_key(|c| c.active_streams()).cloned();
                if let Some(c) = least {
                    if c.active_streams() < self.streams_per_conn
                        || p.conns.len().saturating_add(p.dialing) >= self.max_connections
                    {
                        return Ok(c);
                    }
                }
                if p.conns.len().saturating_add(p.dialing) < self.max_connections {
                    if let Some(dl) = deadline {
                        if Instant::now() >= dl {
                            return Err(GrpcStatus::new(
                                code::DEADLINE_EXCEEDED,
                                "deadline exceeded",
                            ));
                        }
                    }
                    p.dialing += 1;
                    break initial_gen;
                }
                // No open connection yet and in-flight dials already occupy `max_connections`;
                // wait for an in-flight dial to publish a connection or free a slot, bounded by
                // the caller's deadline.
                if let Some(dl) = deadline {
                    let now = Instant::now();
                    if now >= dl {
                        return Err(GrpcStatus::new(
                            code::DEADLINE_EXCEEDED,
                            "deadline exceeded",
                        ));
                    }
                    #[cfg(test)]
                    {
                        p.waiters += 1;
                        self.waiter_cv.notify_all();
                    }
                    let (next_p, _) = self.pool_cv.wait_timeout(p, dl - now).unwrap();
                    p = next_p;
                    #[cfg(test)]
                    {
                        p.waiters -= 1;
                    }
                } else {
                    #[cfg(test)]
                    {
                        p.waiters += 1;
                        self.waiter_cv.notify_all();
                    }
                    p = self.pool_cv.wait(p).unwrap();
                    #[cfg(test)]
                    {
                        p.waiters -= 1;
                    }
                }
            }
        };

        let guard = DialGuard::new(&self.pool, &self.pool_cv, dial_gen);
        let t = match deadline {
            Some(dl) => {
                let remaining = dl.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(GrpcStatus::new(
                        code::DEADLINE_EXCEEDED,
                        "deadline exceeded",
                    ));
                }
                self.connector
                    .connect_with_dial_timeout(remaining)
                    .map_err(|e| {
                        if e.kind() == std::io::ErrorKind::TimedOut && Instant::now() >= dl {
                            GrpcStatus::new(code::DEADLINE_EXCEEDED, "deadline exceeded")
                        } else {
                            GrpcStatus::new(code::UNAVAILABLE, format!("connection failed: {e}"))
                        }
                    })?
            }
            None => self.connector.connect().map_err(|e| {
                GrpcStatus::new(code::UNAVAILABLE, format!("connection failed: {e}"))
            })?,
        };
        let conn = Conn::client_from(t, self.settings.clone())
            .map_err(|e| GrpcStatus::new(code::UNAVAILABLE, format!("h2 handshake failed: {e}")))?;
        guard.complete(conn, &self.connections_opened)
    }

    pub fn close(&self) {
        let to_close: Vec<Conn> = {
            let mut p = self.pool.lock().unwrap();
            p.close_gen = p.close_gen.wrapping_add(1);
            p.dialing = 0;
            p.conns.drain(..).collect()
        };
        self.pool_cv.notify_all();
        for c in to_close {
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
        let conn = self.pick_conn(deadline)?;
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
        let conn = self.pick_conn(deadline)?;
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
                    && self.has_refreshable_credentials() =>
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
                    && self.has_refreshable_credentials() =>
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

    fn spawn_in_memory_echo_conn() -> crate::transport::Transport {
        let (client_tr, server_tr) = crate::transport::memory_pair();
        let (_sconn, incoming) =
            Conn::server_from(server_tr, LocalSettings::default()).expect("server_from");
        std::thread::spawn(move || {
            while let Ok(stream) = incoming.recv() {
                std::thread::spawn(move || {
                    let mut parser = MessageParser::default();
                    let mut last_msg = Vec::new();
                    loop {
                        match stream.recv(None) {
                            Ok(Event::Headers(_)) => {}
                            Ok(Event::Data(d)) => {
                                parser.push(&d);
                                while let Ok(Some(m)) = parser.next() {
                                    last_msg = m;
                                }
                            }
                            Ok(Event::End) => {
                                let _ = stream.send_headers(
                                    &[
                                        (":status".into(), "200".into()),
                                        ("content-type".into(), "application/grpc".into()),
                                    ],
                                    false,
                                );
                                let _ = stream.send_data(&frame_message(&last_msg), false, None);
                                let _ = stream
                                    .send_headers(&[("grpc-status".into(), "0".into())], true);
                                break;
                            }
                            Ok(Event::Trailers(_)) => {}
                            Ok(Event::Reset(_)) | Err(_) => break,
                        }
                    }
                });
            }
        });
        client_tr
    }

    struct GatedConnector {
        calls: AtomicUsize,
        block_on_call: usize,
        entered_tx: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        release_rx: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl Connector for GatedConnector {
        fn connect(&self) -> std::io::Result<crate::transport::Transport> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if n == self.block_on_call {
                if let Some(tx) = self.entered_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = self.release_rx.lock().unwrap().take() {
                    let _ = rx.recv();
                }
            }
            Ok(spawn_in_memory_echo_conn())
        }

        fn connect_with_dial_timeout(
            &self,
            timeout: Duration,
        ) -> std::io::Result<crate::transport::Transport> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if n == self.block_on_call {
                if let Some(tx) = self.entered_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = self.release_rx.lock().unwrap().take() {
                    let end = Instant::now() + timeout;
                    loop {
                        let remaining = end.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "dial timed out",
                            ));
                        }
                        match rx.recv_timeout(remaining) {
                            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        }
                    }
                }
            }
            Ok(spawn_in_memory_echo_conn())
        }

        fn authority(&self) -> String {
            "localhost:443".into()
        }
    }

    #[test]
    fn pick_conn_does_not_hold_pool_lock_during_blocked_dial() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let connector = Arc::new(GatedConnector {
            calls: AtomicUsize::new(0),
            block_on_call: 2,
            entered_tx: Mutex::new(Some(entered_tx)),
            release_rx: Mutex::new(Some(release_rx)),
        });
        let ch = Arc::new(Channel::with_config(
            connector.clone(),
            ChannelConfig {
                max_connections: 2,
                max_concurrency_per_connection: 1,
                ..Default::default()
            },
        ));

        // 1. Open conn #1 and hold its single stream slot busy.
        let (bidi1, _) = ch
            .open_bidi("/test.Svc/Hold", &CallOptions::default())
            .unwrap();
        assert_eq!(ch.connections_opened.load(Ordering::SeqCst), 1);

        // 2. Start a second call whose dial of conn #2 blocks inside `connector.connect()`.
        let ch2 = ch.clone();
        let dial2_thread = std::thread::spawn(move || {
            ch2.unary(
                "/test.Svc/Second",
                b"two",
                &CallOptions {
                    timeout: Some(Duration::from_secs(5)),
                    ..Default::default()
                },
            )
        });
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("second dial should enter connect()");

        // 3. Free the stream on conn #1 while conn #2 is still blocked in `connect()`.
        //    A concurrent RPC must reuse conn #1 immediately without waiting for conn #2.
        bidi1.cancel();
        let reply = ch
            .unary(
                "/test.Svc/Reuse",
                b"fast",
                &CallOptions {
                    timeout: Some(Duration::from_secs(2)),
                    ..Default::default()
                },
            )
            .expect("RPC reusing open conn #1 must not stall on blocked dial #2");
        assert_eq!(reply, b"fast");
        assert_eq!(ch.connections_opened.load(Ordering::SeqCst), 1);

        // 4. Unblock dial #2 and verify it finishes cleanly.
        release_tx.send(()).unwrap();
        assert_eq!(dial2_thread.join().unwrap().unwrap(), b"two");
        assert_eq!(ch.connections_opened.load(Ordering::SeqCst), 2);
        ch.close();
    }

    #[test]
    fn pick_conn_enforces_max_connections_under_concurrent_dials() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let connector = Arc::new(GatedConnector {
            calls: AtomicUsize::new(0),
            block_on_call: 1,
            entered_tx: Mutex::new(Some(entered_tx)),
            release_rx: Mutex::new(Some(release_rx)),
        });
        let ch = Arc::new(Channel::with_config(
            connector.clone(),
            ChannelConfig {
                max_connections: 1,
                max_concurrency_per_connection: 100,
                ..Default::default()
            },
        ));

        let mut handles = Vec::new();
        for i in 0..4u8 {
            let ch_i = ch.clone();
            handles.push(std::thread::spawn(move || {
                ch_i.unary(
                    "/test.Svc/Echo",
                    &[i],
                    &CallOptions {
                        timeout: Some(Duration::from_secs(5)),
                        ..Default::default()
                    },
                )
            }));
        }

        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("first dial should start");
        wait_for_pool_waiters(&ch, 3);
        assert_eq!(connector.calls.load(Ordering::SeqCst), 1);

        release_tx.send(()).unwrap();
        for (i, h) in handles.into_iter().enumerate() {
            assert_eq!(h.join().unwrap().unwrap(), vec![i as u8]);
        }
        assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
        assert_eq!(ch.connections_opened.load(Ordering::SeqCst), 1);
        ch.close();
    }

    fn wait_for_pool_waiters(ch: &Channel, min_waiters: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut p = ch.pool.lock().unwrap();
        while p.waiters < min_waiters {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out waiting for {min_waiters} pool waiters (have {})",
                p.waiters
            );
            let (next_p, _) = ch.waiter_cv.wait_timeout(p, remaining).unwrap();
            p = next_p;
        }
    }

    #[test]
    fn channel_close_while_dialing_closes_new_conn_and_allows_subsequent_rpc() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let connector = Arc::new(GatedConnector {
            calls: AtomicUsize::new(0),
            block_on_call: 1,
            entered_tx: Mutex::new(Some(entered_tx)),
            release_rx: Mutex::new(Some(release_rx)),
        });
        let ch = Arc::new(Channel::with_config(
            connector.clone(),
            ChannelConfig {
                max_connections: 1,
                max_concurrency_per_connection: 100,
                ..Default::default()
            },
        ));

        let ch1 = ch.clone();
        let t1 = std::thread::spawn(move || ch1.pick_conn(None));
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("dial 1 should start");

        let ch2 = ch.clone();
        let t2 = std::thread::spawn(move || ch2.pick_conn(None));
        wait_for_pool_waiters(&ch, 1);

        // Closing the channel wakes the pre-close waiter immediately.
        ch.close();
        let err2 = match t2.join().unwrap() {
            Ok(_) => panic!("expected pre-close waiter to fail with channel closed"),
            Err(e) => e,
        };
        assert_eq!(err2.code, code::UNAVAILABLE);
        assert!(err2.message.contains("closed"), "{err2}");

        // A subsequent RPC started AFTER `close()` succeeds immediately by opening a fresh
        // connection, even while the stale pre-close dial is still blocked.
        let reply = ch
            .unary(
                "/test.Svc/AfterClose",
                b"fresh",
                &CallOptions {
                    timeout: Some(Duration::from_secs(2)),
                    ..Default::default()
                },
            )
            .expect("post-close RPC must succeed on a fresh connection");
        assert_eq!(reply, b"fresh");
        assert_eq!(ch.connections_opened.load(Ordering::SeqCst), 1);

        // Once the stale pre-close dial unblocks, it discards its connection without disrupting
        // the post-close pool or its active connection.
        release_tx.send(()).unwrap();
        let err1 = match t1.join().unwrap() {
            Ok(_) => panic!("expected stale pre-close dial to fail after close"),
            Err(e) => e,
        };
        assert_eq!(err1.code, code::UNAVAILABLE);
        assert!(err1.message.contains("closed"), "{err1}");

        // Pool still holds the healthy post-close connection and serves subsequent RPCs on it.
        let reply2 = ch
            .unary(
                "/test.Svc/ReuseAfterClose",
                b"still-good",
                &CallOptions {
                    timeout: Some(Duration::from_secs(2)),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(reply2, b"still-good");
        assert_eq!(connector.calls.load(Ordering::SeqCst), 2);
        assert_eq!(ch.connections_opened.load(Ordering::SeqCst), 1);
        {
            let p = ch.pool.lock().unwrap();
            assert_eq!(p.conns.len(), 1);
            assert_eq!(p.dialing, 0);
        }
        ch.close();
    }

    #[test]
    fn pick_conn_honors_deadline_while_waiting_and_dialing() {
        // 1. Caller waiting on `pool_cv` returns DEADLINE_EXCEEDED before blocked dial finishes.
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let connector = Arc::new(GatedConnector {
            calls: AtomicUsize::new(0),
            block_on_call: 1,
            entered_tx: Mutex::new(Some(entered_tx)),
            release_rx: Mutex::new(Some(release_rx)),
        });
        let ch = Arc::new(Channel::with_config(
            connector,
            ChannelConfig {
                max_connections: 1,
                max_concurrency_per_connection: 100,
                ..Default::default()
            },
        ));

        let ch1 = ch.clone();
        let t1 = std::thread::spawn(move || {
            ch1.unary("/test.Svc/First", b"one", &CallOptions::default())
        });
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("first dial should enter connect()");

        let started = Instant::now();
        let err = ch
            .unary(
                "/test.Svc/Waiter",
                b"wait",
                &CallOptions {
                    timeout: Some(Duration::from_millis(80)),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert_eq!(err.code, code::DEADLINE_EXCEEDED, "err={err}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "waiter took too long: {:?}",
            started.elapsed()
        );

        release_tx.send(()).unwrap();
        assert_eq!(t1.join().unwrap().unwrap(), b"one");
        ch.close();

        // 2. Direct dialer also respects caller `deadline` via `connect_with_dial_timeout`.
        let (entered_tx2, _entered_rx2) = std::sync::mpsc::channel();
        let (_release_tx2, release_rx2) = std::sync::mpsc::channel();
        let connector2 = Arc::new(GatedConnector {
            calls: AtomicUsize::new(0),
            block_on_call: 1,
            entered_tx: Mutex::new(Some(entered_tx2)),
            release_rx: Mutex::new(Some(release_rx2)),
        });
        let ch_direct = Channel::with_config(
            connector2,
            ChannelConfig {
                max_connections: 1,
                max_concurrency_per_connection: 100,
                ..Default::default()
            },
        );
        let started2 = Instant::now();
        let err2 = ch_direct
            .unary(
                "/test.Svc/SlowDial",
                b"x",
                &CallOptions {
                    timeout: Some(Duration::from_millis(80)),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert_eq!(err2.code, code::DEADLINE_EXCEEDED, "err2={err2}");
        assert!(
            started2.elapsed() < Duration::from_secs(2),
            "direct dial took too long: {:?}",
            started2.elapsed()
        );
    }

    #[test]
    fn pick_conn_preserves_connect_timeout_when_rpc_deadline_has_not_expired() {
        struct EarlyConnectTimeoutConnector;
        impl Connector for EarlyConnectTimeoutConnector {
            fn connect(&self) -> std::io::Result<crate::transport::Transport> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "TLS connection/handshake timed out after 10ms",
                ))
            }

            fn connect_with_dial_timeout(
                &self,
                _timeout: Duration,
            ) -> std::io::Result<crate::transport::Transport> {
                self.connect()
            }

            fn authority(&self) -> String {
                "remote.example.com:443".into()
            }
        }

        let ch = Channel::with_config(
            Arc::new(EarlyConnectTimeoutConnector),
            ChannelConfig {
                max_connections: 1,
                max_concurrency_per_connection: 100,
                ..Default::default()
            },
        );
        let err = ch
            .unary(
                "/test.Svc/Ping",
                b"hi",
                &CallOptions {
                    timeout: Some(Duration::from_secs(5)),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert_eq!(err.code, code::UNAVAILABLE, "err={err}");
        assert!(
            err.message
                .contains("TLS connection/handshake timed out after 10ms"),
            "expected real connect timeout message to be preserved, got: {err}"
        );
    }
}
