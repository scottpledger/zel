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

//! gRPC (over [`crate::http2`]) client channel with a connection pool, deadlines, metadata,
//! trailer-based status handling, unary / client-streaming / server-streaming calls and
//! Bazel-style retries (`RemoteRetrier`).

use crate::hpack::Header;
use crate::http2::{Conn, Event, LocalSettings, Stream};
use crate::transport::Connector;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zel_proto::reapi::code;

/// A gRPC status (non-OK when used as an error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrpcStatus {
    pub code: i32,
    pub message: String,
}

impl GrpcStatus {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
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
        write!(f, "{}: {}", self.code_name(), self.message)
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
        0 => code::INTERNAL, // NO_ERROR before trailers is a protocol error
        7 => code::UNAVAILABLE,   // REFUSED_STREAM
        8 => code::CANCELLED,     // CANCEL
        11 => code::RESOURCE_EXHAUSTED, // ENHANCE_YOUR_CALM
        12 => code::PERMISSION_DENIED, // INADEQUATE_SECURITY
        _ => code::INTERNAL,
    }
}

fn io_to_status(e: &std::io::Error) -> GrpcStatus {
    match e.kind() {
        std::io::ErrorKind::TimedOut => GrpcStatus::new(code::DEADLINE_EXCEEDED, "deadline exceeded"),
        _ => GrpcStatus::new(code::UNAVAILABLE, format!("transport error: {e}")),
    }
}

/// A pooled gRPC channel. Cheap to clone via `Arc`.
pub struct Channel {
    connector: Arc<dyn Connector>,
    max_connections: usize,
    conns: Mutex<Vec<Conn>>,
    default_metadata: Vec<Header>,
    scheme: &'static str,
    settings: LocalSettings,
    pub connections_opened: AtomicUsize,
}

/// Maximum concurrent streams per connection before the pool opens another one (Bazel: 100).
const STREAMS_PER_CONN: usize = 100;

impl Channel {
    pub fn new(
        connector: Arc<dyn Connector>,
        max_connections: usize,
        default_metadata: Vec<Header>,
    ) -> Self {
        Self {
            connector,
            max_connections: max_connections.max(1),
            conns: Mutex::new(Vec::new()),
            default_metadata: default_metadata
                .into_iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v))
                .collect(),
            scheme: "http",
            settings: LocalSettings::default(),
            connections_opened: AtomicUsize::new(0),
        }
    }

    fn pick_conn(&self) -> Result<Conn, GrpcStatus> {
        let mut conns = self.conns.lock().unwrap();
        conns.retain(|c| !c.is_closed());
        let least = conns.iter().min_by_key(|c| c.active_streams()).cloned();
        if let Some(c) = &least {
            if c.active_streams() < STREAMS_PER_CONN || conns.len() >= self.max_connections {
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

    /// Generic call: sends `requests` (client streaming), delivers each response message to
    /// `on_message`. Returns Ok(()) when the call finished with grpc-status OK.
    pub fn call(
        &self,
        method: &str,
        requests: &mut dyn Iterator<Item = Vec<u8>>,
        opts: &CallOptions,
        on_message: &mut dyn FnMut(Vec<u8>) -> Result<(), GrpcStatus>,
    ) -> Result<(), GrpcStatus> {
        let deadline = opts.timeout.map(|t| Instant::now() + t);
        let conn = self.pick_conn()?;
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
        headers.extend(self.default_metadata.iter().cloned());
        headers.extend(opts.metadata.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.clone())));
        let stream = conn.open_stream(&headers, false).map_err(|e| io_to_status(&e))?;

        let mut sent_end = false;
        let mut pending = requests.next();
        let mut send_failed = false;
        while let Some(msg) = pending {
            pending = requests.next();
            let last = pending.is_none();
            let remaining = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            if stream.send_data(&frame_message(&msg), last, remaining).is_err() {
                send_failed = true;
                break;
            }
            sent_end = last;
        }
        if !send_failed && !sent_end {
            if stream.send_data(&[], true, None).is_err() {
                send_failed = true;
            }
        }
        let _ = send_failed; // the response (or its absence) carries the real error

        read_response(&stream, deadline, on_message)
    }

    /// Unary call.
    pub fn unary(&self, method: &str, request: &[u8], opts: &CallOptions) -> Result<Vec<u8>, GrpcStatus> {
        let mut out: Option<Vec<u8>> = None;
        let mut reqs = std::iter::once(request.to_vec());
        self.call(method, &mut reqs, opts, &mut |m| {
            if out.is_some() {
                return Err(GrpcStatus::new(code::INTERNAL, "multiple responses to unary call"));
            }
            out = Some(m);
            Ok(())
        })?;
        out.ok_or_else(|| GrpcStatus::new(code::INTERNAL, "unary call returned no message"))
    }

    /// Server-streaming call collecting all messages.
    pub fn server_stream(
        &self,
        method: &str,
        request: &[u8],
        opts: &CallOptions,
        on_message: &mut dyn FnMut(Vec<u8>) -> Result<(), GrpcStatus>,
    ) -> Result<(), GrpcStatus> {
        let mut reqs = std::iter::once(request.to_vec());
        self.call(method, &mut reqs, opts, on_message)
    }
}

fn header<'a>(h: &'a [Header], name: &str) -> Option<&'a str> {
    h.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

fn status_from_trailers(h: &[Header]) -> Option<GrpcStatus> {
    let c = header(h, "grpc-status")?;
    let code = c.trim().parse::<i32>().unwrap_or(code::UNKNOWN);
    let msg = header(h, "grpc-message").map(percent_decode).unwrap_or_default();
    Some(GrpcStatus::new(code, msg))
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
                        // Trailers-only HTTP errors may still carry grpc-status.
                        return Err(status_from_trailers(&h).unwrap_or_else(|| {
                            GrpcStatus::new(http_status_to_grpc(st), format!("HTTP status {st}"))
                        }));
                    }
                }
                // Trailers-only response: status in the initial headers.
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
                    return Err(GrpcStatus::new(code::UNAVAILABLE, "stream closed without headers"));
                }
                if !parser.is_empty() {
                    return Err(GrpcStatus::new(code::INTERNAL, "truncated gRPC message"));
                }
                return match final_status {
                    Some(s) if s.code == code::OK => Ok(()),
                    Some(s) => Err(s),
                    None => Err(GrpcStatus::new(code::INTERNAL, "missing grpc-status trailer")),
                };
            }
            Event::Reset(c) => {
                return Err(GrpcStatus::new(h2_reset_to_grpc(c), format!("stream reset (HTTP/2 error {c})")));
            }
        }
    }
}

/// Bazel `RemoteRetrier` parameters: exponential backoff with 10% jitter.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub multiplier: f64,
    pub jitter: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(5),
            multiplier: 2.0,
            jitter: 0.1,
        }
    }
}

impl RetryPolicy {
    /// gRPC codes `RemoteRetrier.RETRIABLE_GRPC_ERRORS` retries.
    pub fn is_retriable(status: &GrpcStatus) -> bool {
        matches!(
            status.code,
            code::UNKNOWN
                | code::DEADLINE_EXCEEDED
                | code::ABORTED
                | code::INTERNAL
                | code::UNAVAILABLE
                | code::RESOURCE_EXHAUSTED
        )
    }

    fn delay(&self, attempt: u32) -> Duration {
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
            match f() {
                Ok(v) => return Ok(v),
                Err(e) => {
                    if attempt >= self.max_retries || !Self::is_retriable(&e) {
                        return Err(e);
                    }
                    std::thread::sleep(self.delay(attempt));
                    attempt += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn retry_policy_retries_only_retriable() {
        let p = RetryPolicy { max_retries: 3, initial_delay: Duration::from_millis(1), ..Default::default() };
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
            if n < 3 { Err(GrpcStatus::new(code::ABORTED, "x")) } else { Ok(n) }
        });
        assert_eq!(r.unwrap(), 3);
    }
}
