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

//! HTTP/1.1 & HTTPS REST remote cache client (`--remote_cache=http://...` and
//! `--remote_cache=https://...`), mirroring Bazel's `HttpCacheClient`.

use crate::credentials::CallCredentialsProvider;
use crate::digest::DigestFn;
use crate::grpc::{GrpcStatus, RetryPolicy};
use crate::hpack::Header;
use crate::merkle::BlobSource;
use crate::reapi_client::{RemoteError, Result};
use crate::transport::Connector;
use std::collections::{BTreeMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use zel_proto::reapi::{code, ActionResult, Digest, Message};

const ACCEPT_ENCODING: &str = "gzip,deflate,zstd";

struct Semaphore {
    max: usize,
    active: Mutex<usize>,
    cv: Condvar,
}

struct SemPermit<'a>(&'a Semaphore);

impl Drop for SemPermit<'_> {
    fn drop(&mut self) {
        if self.0.max > 0 {
            let mut g = self.0.active.lock().unwrap();
            *g = g.saturating_sub(1);
            self.0.cv.notify_one();
        }
    }
}

impl Semaphore {
    fn new(max: usize) -> Self {
        Self {
            max,
            active: Mutex::new(0),
            cv: Condvar::new(),
        }
    }

    fn acquire(&self) -> SemPermit<'_> {
        if self.max > 0 {
            let mut g = self.active.lock().unwrap();
            while *g >= self.max {
                g = self.cv.wait(g).unwrap();
            }
            *g += 1;
        }
        SemPermit(self)
    }
}

/// Configuration for [`HttpCacheClient`], mirroring Bazel's `CombinedCacheClientFactory.createHttp`
/// and `HttpCacheClient`.
///
/// Per Bazel's `HttpCacheClient`, `HttpUploadHandler`, and `HttpDownloadHandler`:
/// - `timeout` (`--remote_timeout`) bounds connection establishment as well as upload/download
///   channel idle time (`CONNECT_TIMEOUT_MILLIS` and `IdleTimeoutHandler`).
/// - HTTP `PUT` uploads (`/cas/<hash>` and `/ac/<hash>`) always transmit raw uncompressed bytes
///   without `Content-Encoding` so standard HTTP / WebDAV caches store blobs under their canonical
///   digest (`--remote_cache_compression` and `--experimental_remote_cache_compression_threshold`
///   apply only to gRPC ByteStream `compressed-blobs/zstd/...`).
/// - HTTP `GET` downloads advertise `Accept-Encoding: gzip,deflate,zstd` and transparently
///   decompress `Content-Encoding` responses.
pub struct HttpCacheConfig {
    pub uri: String,
    pub prefix: String,
    pub tls: bool,
    pub digest_fn: DigestFn,
    pub timeout: Duration,
    pub retry: RetryPolicy,
    pub max_connections: usize,
    pub headers: Vec<Header>,
    pub credentials: Option<Arc<dyn CallCredentialsProvider>>,
    pub verify_downloads: bool,
}

struct IdleTracker {
    epoch: Instant,
    last_activity_ns: AtomicU64,
    timeout: Duration,
    timed_out: AtomicBool,
    done: AtomicBool,
    shutdown: Arc<dyn Fn() + Send + Sync>,
}

impl IdleTracker {
    fn new(timeout: Duration, shutdown: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            epoch: Instant::now(),
            last_activity_ns: AtomicU64::new(0),
            timeout,
            timed_out: AtomicBool::new(false),
            done: AtomicBool::new(false),
            shutdown,
        }
    }

    fn touch(&self) {
        let elapsed_ns = self.epoch.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        self.last_activity_ns.store(elapsed_ns, Ordering::Release);
    }

    fn is_timed_out(&self) -> bool {
        self.timed_out.load(Ordering::SeqCst)
    }

    fn deadline(&self) -> Instant {
        let last_ns = self.last_activity_ns.load(Ordering::Acquire);
        self.epoch + Duration::from_nanos(last_ns) + self.timeout
    }
}

struct WatchdogInner {
    entries: Vec<Arc<IdleTracker>>,
    stopped: bool,
}

struct IdleWatchdog {
    state: Arc<(Mutex<WatchdogInner>, Condvar)>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

struct WatchGuard {
    state: Arc<(Mutex<WatchdogInner>, Condvar)>,
    tracker: Arc<IdleTracker>,
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        self.tracker.done.store(true, Ordering::SeqCst);
        let mut inner = self.state.0.lock().unwrap();
        inner.entries.retain(|e| !Arc::ptr_eq(e, &self.tracker));
    }
}

impl IdleWatchdog {
    fn new() -> Self {
        Self {
            state: Arc::new((
                Mutex::new(WatchdogInner {
                    entries: Vec::new(),
                    stopped: false,
                }),
                Condvar::new(),
            )),
            worker: Mutex::new(None),
        }
    }

    fn register(&self, timeout: Duration, shutdown: Arc<dyn Fn() + Send + Sync>) -> WatchGuard {
        let tracker = Arc::new(IdleTracker::new(timeout, shutdown));
        {
            let mut worker = self.worker.lock().unwrap();
            if worker.is_none() {
                let st = self.state.clone();
                *worker = Some(std::thread::spawn(move || Self::run_loop(st)));
            }
        }
        {
            let mut inner = self.state.0.lock().unwrap();
            inner.entries.push(tracker.clone());
        }
        self.state.1.notify_one();
        WatchGuard {
            state: self.state.clone(),
            tracker,
        }
    }

    fn run_loop(state: Arc<(Mutex<WatchdogInner>, Condvar)>) {
        let (lock, cv) = &*state;
        let mut guard = lock.lock().unwrap();
        loop {
            if guard.stopped {
                return;
            }
            let now = Instant::now();
            let mut expired: Vec<Arc<IdleTracker>> = Vec::new();
            let mut next_deadline: Option<Instant> = None;
            guard.entries.retain(|entry| {
                if entry.done.load(Ordering::SeqCst) {
                    return false;
                }
                let dl = entry.deadline();
                if now >= dl {
                    expired.push(entry.clone());
                    false
                } else {
                    next_deadline = Some(match next_deadline {
                        Some(cur) => cur.min(dl),
                        None => dl,
                    });
                    true
                }
            });
            if !expired.is_empty() {
                drop(guard);
                for entry in expired {
                    if !entry.done.load(Ordering::SeqCst) {
                        entry.timed_out.store(true, Ordering::SeqCst);
                        (entry.shutdown)();
                    }
                }
                guard = lock.lock().unwrap();
                continue;
            }
            match next_deadline {
                Some(dl) => {
                    let wait = dl.saturating_duration_since(Instant::now());
                    let (g, _) = cv.wait_timeout(guard, wait).unwrap();
                    guard = g;
                }
                None => {
                    guard = cv.wait(guard).unwrap();
                }
            }
        }
    }
}

impl Drop for IdleWatchdog {
    fn drop(&mut self) {
        {
            let mut inner = self.state.0.lock().unwrap();
            inner.stopped = true;
            inner.entries.clear();
        }
        self.state.1.notify_all();
        if let Some(h) = self.worker.lock().unwrap().take() {
            let _ = h.join();
        }
    }
}

fn normalize_timeout_io_error(err: io::Error, timed_out: bool) -> io::Error {
    if timed_out
        || matches!(
            err.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        )
    {
        io::Error::new(io::ErrorKind::TimedOut, err.to_string())
    } else {
        err
    }
}

struct TimedReader<'a> {
    inner: &'a mut dyn Read,
    tracker: Option<&'a IdleTracker>,
}

impl Read for TimedReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.inner.read(buf) {
            Ok(n) => {
                if let Some(t) = self.tracker {
                    if t.is_timed_out() {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "HTTP cache read timed out",
                        ));
                    }
                    if n > 0 {
                        t.touch();
                    }
                }
                Ok(n)
            }
            Err(e) => {
                let timed_out = self.tracker.is_some_and(IdleTracker::is_timed_out);
                Err(normalize_timeout_io_error(e, timed_out))
            }
        }
    }
}

struct TimedWriter<'a> {
    inner: &'a mut dyn Write,
    tracker: Option<&'a IdleTracker>,
}

impl Write for TimedWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.inner.write(buf) {
            Ok(n) => {
                if let Some(t) = self.tracker {
                    if t.is_timed_out() {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "HTTP cache write timed out",
                        ));
                    }
                    if n > 0 {
                        t.touch();
                    }
                }
                Ok(n)
            }
            Err(e) => {
                let timed_out = self.tracker.is_some_and(IdleTracker::is_timed_out);
                Err(normalize_timeout_io_error(e, timed_out))
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.inner.flush() {
            Ok(()) => {
                if let Some(t) = self.tracker {
                    if t.is_timed_out() {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "HTTP cache flush timed out",
                        ));
                    }
                    t.touch();
                }
                Ok(())
            }
            Err(e) => {
                let timed_out = self.tracker.is_some_and(IdleTracker::is_timed_out);
                Err(normalize_timeout_io_error(e, timed_out))
            }
        }
    }
}

/// HTTP/1.1 & HTTPS REST cache client (`/ac/<hash>` and `/cas/<hash>`).
pub struct HttpCacheClient {
    connector: Arc<dyn Connector>,
    pub cfg: HttpCacheConfig,
    sem: Semaphore,
    watchdog: IdleWatchdog,
    warned_plaintext_creds: AtomicBool,
    warnings: Mutex<Vec<String>>,
    present_digests: Mutex<HashSet<Digest>>,
}

impl HttpCacheClient {
    pub fn new(connector: Arc<dyn Connector>, cfg: HttpCacheConfig) -> Self {
        let max = cfg.max_connections;
        Self {
            connector,
            cfg,
            sem: Semaphore::new(max),
            watchdog: IdleWatchdog::new(),
            warned_plaintext_creds: AtomicBool::new(false),
            warnings: Mutex::new(Vec::new()),
            present_digests: Mutex::new(HashSet::new()),
        }
    }

    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut *self.warnings.lock().unwrap())
    }

    fn path_for(&self, kind: &str, hash: &str) -> String {
        if self.cfg.prefix.is_empty() {
            format!("/{kind}/{hash}")
        } else {
            format!("{}/{kind}/{hash}", self.cfg.prefix.trim_end_matches('/'))
        }
    }

    fn is_retriable_http_status(status: u16) -> bool {
        matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
    }

    fn is_retriable_io_error(e: &io::Error) -> bool {
        if matches!(
            e.kind(),
            io::ErrorKind::TimedOut
                | io::ErrorKind::UnexpectedEof
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::Interrupted
        ) {
            return true;
        }
        let lc = e.to_string().to_ascii_lowercase();
        lc.contains("connection reset") || lc.contains("operation timed out")
    }

    fn collect_headers(&self, extra: &[(&str, &str)]) -> Result<Vec<(String, String)>> {
        let mut out: Vec<(String, String)> = Vec::new();
        if let Some(creds) = &self.cfg.credentials {
            let dyn_hdrs = creds.get_credentials(&self.cfg.uri).map_err(|e| {
                if e.is_helper_error() {
                    RemoteError::Grpc(GrpcStatus::credential_helper(e.to_string()))
                } else {
                    RemoteError::Grpc(GrpcStatus::new(code::UNAUTHENTICATED, e.to_string()))
                }
            })?;
            if !dyn_hdrs.is_empty()
                && !self.cfg.tls
                && !self.warned_plaintext_creds.swap(true, Ordering::SeqCst)
            {
                self.warnings.lock().unwrap().push(format!(
                    "Credentials are transmitted in plaintext to {}",
                    self.cfg.uri
                ));
            }
            out.extend(dyn_hdrs);
        }
        for (k, v) in &self.cfg.headers {
            out.push((k.clone(), v.clone()));
        }
        for &(k, v) in extra {
            if out.iter().any(|(ek, _)| ek.eq_ignore_ascii_case(k)) {
                continue;
            }
            out.push((k.to_string(), v.to_string()));
        }
        Ok(out)
    }

    fn timeout_error(&self, is_upload: bool, path: &str) -> HttpAttemptError {
        let msg = if is_upload {
            format!("Upload of '{path}' timed out after {:?}", self.cfg.timeout)
        } else {
            format!(
                "Download of '{path}' timed out after {:?}",
                self.cfg.timeout
            )
        };
        let err = io::Error::new(io::ErrorKind::TimedOut, msg);
        if is_upload {
            HttpAttemptError::UploadTimeout(err)
        } else {
            HttpAttemptError::Io(err)
        }
    }

    fn send_once(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        extra_headers: &[(&str, &str)],
    ) -> std::result::Result<HttpRawResponse, HttpAttemptError> {
        let _permit = self.sem.acquire();
        let hdrs = self
            .collect_headers(extra_headers)
            .map_err(HttpAttemptError::Fatal)?;
        let is_upload = method.eq_ignore_ascii_case("PUT");

        let mut transport = self
            .connector
            .connect_with_timeout(self.cfg.timeout)
            .map_err(|e| HttpAttemptError::Io(normalize_timeout_io_error(e, false)))?;
        let shutdown: Arc<dyn Fn() + Send + Sync> = Arc::from(transport.shutdown);
        let watch_guard = if self.cfg.timeout.is_zero() {
            None
        } else {
            Some(self.watchdog.register(self.cfg.timeout, shutdown.clone()))
        };
        let tracker = watch_guard.as_ref().map(|g| &*g.tracker);

        let authority = self.connector.authority();
        let mut req_head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: */*\r\nUser-Agent: bazel/zel\r\nConnection: close\r\n"
        );
        if let Some(b) = body {
            req_head.push_str(&format!(
                "Content-Type: application/octet-stream\r\nContent-Length: {}\r\n",
                b.len()
            ));
        }
        for (k, v) in &hdrs {
            req_head.push_str(&format!("{k}: {v}\r\n"));
        }
        req_head.push_str("\r\n");

        let write_res = {
            let mut writer = TimedWriter {
                inner: &mut *transport.writer,
                tracker,
            };
            writer.write_all(req_head.as_bytes()).and_then(|_| {
                if let Some(b) = body {
                    writer.write_all(b)?;
                }
                writer.flush()
            })
        };
        if let Err(e) = write_res {
            let timed_out = tracker.is_some_and(IdleTracker::is_timed_out)
                || e.kind() == io::ErrorKind::TimedOut;
            drop(watch_guard);
            shutdown();
            if timed_out {
                return Err(self.timeout_error(is_upload, path));
            }
            return Err(HttpAttemptError::Io(e));
        }

        let read_res = {
            let mut reader = TimedReader {
                inner: &mut *transport.reader,
                tracker,
            };
            read_http_response(&mut reader)
        };
        let watchdog_fired = tracker.is_some_and(IdleTracker::is_timed_out);
        drop(watch_guard);
        shutdown();
        match read_res {
            Ok(resp) if !watchdog_fired => Ok(resp),
            Ok(_) => Err(self.timeout_error(is_upload, path)),
            Err(e) if watchdog_fired || e.kind() == io::ErrorKind::TimedOut => {
                Err(self.timeout_error(is_upload, path))
            }
            Err(e) => Err(HttpAttemptError::Io(e)),
        }
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        extra_headers: &[(&str, &str)],
    ) -> Result<HttpRawResponse> {
        let mut attempt = 0u32;
        let mut refreshed_auth = false;
        loop {
            if self.cfg.retry.is_caller_cancelled() {
                return Err(RemoteError::Grpc(GrpcStatus::new(
                    code::CANCELLED,
                    "Interrupted",
                )));
            }
            match self.send_once(method, path, body, extra_headers) {
                Ok((status, hdrs, resp_body)) => {
                    if status == 401 && !refreshed_auth {
                        if let Some(creds) = &self.cfg.credentials {
                            refreshed_auth = true;
                            creds.refresh().map_err(|e| {
                                if e.is_helper_error() {
                                    RemoteError::Grpc(GrpcStatus::credential_helper(e.to_string()))
                                } else {
                                    RemoteError::Grpc(GrpcStatus::new(
                                        code::UNAUTHENTICATED,
                                        e.to_string(),
                                    ))
                                }
                            })?;
                            continue;
                        }
                    }
                    if Self::is_retriable_http_status(status)
                        && attempt < self.cfg.retry.max_retries
                    {
                        std::thread::sleep(self.cfg.retry.delay(attempt));
                        attempt += 1;
                        continue;
                    }
                    return Ok((status, hdrs, resp_body));
                }
                Err(HttpAttemptError::Fatal(e)) => return Err(e),
                Err(HttpAttemptError::UploadTimeout(e)) => return Err(RemoteError::Io(e)),
                Err(HttpAttemptError::Io(e)) => {
                    if Self::is_retriable_io_error(&e) && attempt < self.cfg.retry.max_retries {
                        std::thread::sleep(self.cfg.retry.delay(attempt));
                        attempt += 1;
                        continue;
                    }
                    return Err(RemoteError::Io(e));
                }
            }
        }
    }

    pub fn get_action_result(&self, action: &Digest) -> Result<Option<ActionResult>> {
        let path = self.path_for("ac", &action.hash);
        let (status, hdrs, body) =
            self.request("GET", &path, None, &[("Accept-Encoding", ACCEPT_ENCODING)])?;
        match status {
            200 => {
                let decoded = decode_response_body(&hdrs, body, &path)?;
                let ar = ActionResult::decode(&decoded[..])?;
                Ok(Some(ar))
            }
            404 | 204 => Ok(None),
            other => Err(RemoteError::Other(format!(
                "HTTP cache GET {path} failed with status {other}"
            ))),
        }
    }

    pub fn update_action_result(&self, action: &Digest, result: &ActionResult) -> Result<()> {
        let path = self.path_for("ac", &action.hash);
        let bytes = result.encode_to_vec();
        let (status, _hdrs, _body) = self.request("PUT", &path, Some(&bytes), &[])?;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(RemoteError::Other(format!(
                "HTTP cache PUT {path} failed with status {status}"
            )))
        }
    }

    pub fn download_blob(&self, d: &Digest) -> Result<Vec<u8>> {
        if d.size_bytes == 0 {
            return Ok(Vec::new());
        }
        let path = self.path_for("cas", &d.hash);
        let (status, hdrs, body) =
            self.request("GET", &path, None, &[("Accept-Encoding", ACCEPT_ENCODING)])?;
        match status {
            200 => {
                let body = decode_response_body(&hdrs, body, &path)?;
                if self.cfg.verify_downloads {
                    let actual = self.cfg.digest_fn.digest(&body);
                    if actual.hash != d.hash || actual.size_bytes != d.size_bytes {
                        return Err(RemoteError::Other(format!(
                            "Download of {}/{} failed: digest mismatch (got {}/{})",
                            d.hash, d.size_bytes, actual.hash, actual.size_bytes
                        )));
                    }
                }
                Ok(body)
            }
            404 => Err(RemoteError::NotFound),
            other => Err(RemoteError::Other(format!(
                "HTTP cache GET {path} failed with status {other}"
            ))),
        }
    }

    pub fn upload_blob(&self, d: &Digest, data: &[u8]) -> Result<()> {
        if d.size_bytes == 0 {
            return Ok(());
        }
        let path = self.path_for("cas", &d.hash);
        // Mirror Bazel's `HttpUploadHandler`: PUT requests always transmit raw uncompressed bytes
        // without `Content-Encoding` so HTTP/WebDAV caches store the canonical blob payload.
        let (status, _hdrs, _body) = self.request("PUT", &path, Some(data), &[])?;
        if (200..300).contains(&status) {
            self.present_digests.lock().unwrap().insert(d.clone());
            Ok(())
        } else {
            self.present_digests.lock().unwrap().remove(d);
            Err(RemoteError::Other(format!(
                "HTTP cache PUT {path} failed with status {status}"
            )))
        }
    }

    pub fn ensure_blobs_present(&self, blobs: &BTreeMap<Digest, BlobSource>) -> Result<usize> {
        let mut uploaded = 0usize;
        for (d, src) in blobs {
            if d.size_bytes <= 0 {
                continue;
            }
            if self.present_digests.lock().unwrap().contains(d) {
                continue;
            }
            let data = src.read()?;
            self.upload_blob(d, &data)?;
            uploaded += 1;
        }
        Ok(uploaded)
    }
}

enum HttpAttemptError {
    Io(io::Error),
    UploadTimeout(io::Error),
    Fatal(RemoteError),
}

fn adler32(data: &[u8]) -> u32 {
    const MOD_ADLER: u32 = 65_521;
    let mut s1 = 1u32;
    let mut s2 = 0u32;
    for &b in data {
        s1 = (s1 + u32::from(b)) % MOD_ADLER;
        s2 = (s2 + s1) % MOD_ADLER;
    }
    (s2 << 16) | s1
}

fn decode_deflate(data: &[u8]) -> std::result::Result<Vec<u8>, String> {
    if data.len() >= 6 {
        let cmf = data[0];
        let flg = data[1];
        let header_word = (u16::from(cmf) << 8) | u16::from(flg);
        if (cmf & 0x0f) == 8 && (cmf >> 4) <= 7 && header_word % 31 == 0 && (flg & 0x20) == 0 {
            if let Ok((out, used)) = zel_util::deflate::inflate(&data[2..]) {
                if 2 + used + 4 <= data.len() {
                    let expected =
                        u32::from_be_bytes(data[2 + used..2 + used + 4].try_into().unwrap());
                    if adler32(&out) == expected {
                        return Ok(out);
                    }
                }
            }
        }
    }
    zel_util::deflate::inflate(data).map(|(out, _)| out)
}

fn decode_response_body(
    hdrs: &[(String, String)],
    mut body: Vec<u8>,
    path: &str,
) -> Result<Vec<u8>> {
    let encodings: Vec<&str> = hdrs
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("content-encoding"))
        .flat_map(|(_, v)| v.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("identity"))
        .collect();
    for enc in encodings.into_iter().rev() {
        if enc.eq_ignore_ascii_case("zstd") {
            body = zel_util::zstd::decompress(&body).map_err(|e| {
                RemoteError::Other(format!(
                    "Failed to decompress zstd HTTP cache response for {path}: {e}"
                ))
            })?;
        } else if enc.eq_ignore_ascii_case("gzip") || enc.eq_ignore_ascii_case("x-gzip") {
            body = zel_util::deflate::gunzip(&body).map_err(|e| {
                RemoteError::Other(format!(
                    "Failed to decompress gzip HTTP cache response for {path}: {e}"
                ))
            })?;
        } else if enc.eq_ignore_ascii_case("deflate") {
            body = decode_deflate(&body).map_err(|e| {
                RemoteError::Other(format!(
                    "Failed to decompress deflate HTTP cache response for {path}: {e}"
                ))
            })?;
        }
    }
    Ok(body)
}

type HttpRawResponse = (u16, Vec<(String, String)>, Vec<u8>);

fn read_http_response(reader: &mut dyn Read) -> io::Result<HttpRawResponse> {
    let mut br = BufReader::new(reader);
    let mut status_line = String::new();
    let n = br.read_line(&mut status_line)?;
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "HTTP server closed connection before status line",
        ));
    }
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Malformed HTTP status line: {}", status_line.trim()),
            )
        })?;

    let mut headers = Vec::new();
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        let r = br.read_line(&mut line)?;
        if r == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Unexpected EOF reading HTTP headers",
            ));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            let key = k.trim().to_string();
            let val = v.trim().to_string();
            if key.eq_ignore_ascii_case("content-length") {
                content_length = val.parse().ok();
            } else if key.eq_ignore_ascii_case("transfer-encoding")
                && val.to_ascii_lowercase().contains("chunked")
            {
                chunked = true;
            }
            headers.push((key, val));
        }
    }

    if status == 204 || status == 304 {
        return Ok((status, headers, Vec::new()));
    }

    let body = if chunked {
        let mut out = Vec::new();
        loop {
            let mut sz_line = String::new();
            if br.read_line(&mut sz_line)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Unexpected EOF in chunked body",
                ));
            }
            let hex_part = sz_line.trim().split(';').next().unwrap_or("").trim();
            let chunk_len = usize::from_str_radix(hex_part, 16).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "Invalid HTTP chunk size")
            })?;
            if chunk_len == 0 {
                let mut trl = String::new();
                let _ = br.read_line(&mut trl);
                break;
            }
            let start = out.len();
            out.resize(start + chunk_len, 0);
            br.read_exact(&mut out[start..])?;
            let mut crlf = [0u8; 2];
            br.read_exact(&mut crlf)?;
        }
        out
    } else if let Some(len) = content_length {
        let mut out = vec![0u8; len];
        br.read_exact(&mut out)?;
        out
    } else {
        let mut out = Vec::new();
        br.read_to_end(&mut out)?;
        out
    };

    Ok((status, headers, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::CredentialError;
    use crate::transport::TcpConnector;
    use std::net::TcpListener;

    struct StaticCreds;
    impl CallCredentialsProvider for StaticCreds {
        fn get_credentials(&self, _uri: &str) -> std::result::Result<Vec<Header>, CredentialError> {
            Ok(vec![("authorization".into(), "Bearer secret".into())])
        }
        fn refresh(&self) -> std::result::Result<(), CredentialError> {
            Ok(())
        }
    }

    #[test]
    fn http_cache_ac_cas_zstd_retries_and_plaintext_warning() {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return,
            Err(e) => panic!("{e}"),
        };
        let port = listener.local_addr().unwrap().port();
        let store: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>> =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let attempts = Arc::new(Mutex::new(0u32));

        let store_srv = store.clone();
        let attempts_srv = attempts.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let store = store_srv.clone();
                let attempts = attempts_srv.clone();
                std::thread::spawn(move || {
                    let mut br = BufReader::new(&stream);
                    let mut req_line = String::new();
                    if br.read_line(&mut req_line).unwrap_or(0) == 0 {
                        return;
                    }
                    let parts: Vec<&str> = req_line.split_whitespace().collect();
                    if parts.len() < 2 {
                        return;
                    }
                    let (method, path) = (parts[0], parts[1]);
                    let mut clen = 0usize;
                    let mut accept_zstd = false;
                    loop {
                        let mut h = String::new();
                        if br.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                            break;
                        }
                        if let Some((k, v)) = h.split_once(':') {
                            if k.trim().eq_ignore_ascii_case("content-length") {
                                clen = v.trim().parse().unwrap_or(0);
                            }
                            if k.trim().eq_ignore_ascii_case("accept-encoding")
                                && v.contains("zstd")
                            {
                                accept_zstd = true;
                            }
                        }
                    }
                    let mut body = vec![0u8; clen];
                    if clen > 0 {
                        let _ = br.read_exact(&mut body);
                    }
                    let mut w = &stream;
                    let n = {
                        let mut a = attempts.lock().unwrap();
                        *a += 1;
                        *a
                    };
                    if n == 1 {
                        let _ = w.write_all(
                            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n",
                        );
                        return;
                    }
                    match method {
                        "PUT" => {
                            store.lock().unwrap().insert(path.to_string(), body);
                            let _ = w.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
                        }
                        "GET" => match store.lock().unwrap().get(path).cloned() {
                            Some(data) => {
                                if accept_zstd && path.contains("/cas/") {
                                    let z = zel_util::zstd::compress(&data);
                                    let _ = write!(
                                        w,
                                        "HTTP/1.1 200 OK\r\nContent-Encoding: zstd\r\nContent-Length: {}\r\n\r\n",
                                        z.len()
                                    );
                                    let _ = w.write_all(&z);
                                } else {
                                    let _ = write!(
                                        w,
                                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                                        data.len()
                                    );
                                    let _ = w.write_all(&data);
                                }
                            }
                            None => {
                                let _ = w.write_all(
                                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
                                );
                            }
                        },
                        _ => {}
                    }
                });
            }
        });

        let conn = Arc::new(TcpConnector::new(
            "127.0.0.1".into(),
            port,
            Duration::from_secs(5),
        ));
        let client = HttpCacheClient::new(
            conn,
            HttpCacheConfig {
                uri: format!("http://127.0.0.1:{port}/cache"),
                prefix: "/cache".into(),
                tls: false,
                digest_fn: DigestFn::Sha256,
                timeout: Duration::from_secs(5),
                retry: RetryPolicy {
                    initial_delay: Duration::from_millis(2),
                    max_retries: 3,
                    ..Default::default()
                },
                max_connections: 4,
                headers: vec![],
                credentials: Some(Arc::new(StaticCreds)),
                verify_downloads: true,
            },
        );

        let payload = b"repeated http cache payload ".repeat(64);
        let d = DigestFn::Sha256.digest(&payload);
        // First request hits 503 and retries transparently; also records plaintext credential warning!
        client.upload_blob(&d, &payload).unwrap();
        let warnings = client.take_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("plaintext"));

        let got = client.download_blob(&d).unwrap();
        assert_eq!(got, payload);

        let act = DigestFn::Sha256.digest(b"act");
        assert!(client.get_action_result(&act).unwrap().is_none());
        let ar = ActionResult {
            exit_code: 0,
            stdout_raw: b"ok".to_vec(),
            ..Default::default()
        };
        client.update_action_result(&act, &ar).unwrap();
        assert_eq!(client.get_action_result(&act).unwrap().unwrap(), ar);
    }

    #[test]
    fn http_cache_stalled_upload_and_download_honor_timeout() {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return,
            Err(e) => panic!("{e}"),
        };
        let port = listener.local_addr().unwrap().port();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let put_attempts = Arc::new(AtomicU64::new(0));
        let get_attempts = Arc::new(AtomicU64::new(0));

        let release_srv = release.clone();
        let put_srv = put_attempts.clone();
        let get_srv = get_attempts.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let release_conn = release_srv.clone();
                let put_conn = put_srv.clone();
                let get_conn = get_srv.clone();
                std::thread::spawn(move || {
                    let mut br = BufReader::new(&stream);
                    let mut req_line = String::new();
                    if br.read_line(&mut req_line).unwrap_or(0) == 0 {
                        return;
                    }
                    let method = req_line.split_whitespace().next().unwrap_or("");
                    let mut clen = 0usize;
                    loop {
                        let mut h = String::new();
                        if br.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                            break;
                        }
                        if let Some((k, v)) = h.split_once(':') {
                            if k.trim().eq_ignore_ascii_case("content-length") {
                                clen = v.trim().parse().unwrap_or(0);
                            }
                        }
                    }
                    if clen > 0 {
                        let mut body = vec![0u8; clen];
                        let _ = br.read_exact(&mut body);
                    }
                    if method == "PUT" {
                        put_conn.fetch_add(1, Ordering::SeqCst);
                    } else if method == "GET" {
                        get_conn.fetch_add(1, Ordering::SeqCst);
                    }
                    // Hold the socket open without responding until the test finishes.
                    let (lock, cv) = &*release_conn;
                    let mut done = lock.lock().unwrap();
                    while !*done {
                        let (g, _) = cv.wait_timeout(done, Duration::from_secs(5)).unwrap();
                        done = g;
                    }
                });
            }
        });

        // Deliberately configure a long connector default (30s) while setting `HttpCacheConfig.timeout`
        // to 80ms so the test proves `HttpCacheConfig.timeout` overrides the transport deadline.
        let conn = Arc::new(TcpConnector::new(
            "127.0.0.1".into(),
            port,
            Duration::from_secs(30),
        ));
        let client = HttpCacheClient::new(
            conn,
            HttpCacheConfig {
                uri: format!("http://127.0.0.1:{port}"),
                prefix: String::new(),
                tls: false,
                digest_fn: DigestFn::Sha256,
                timeout: Duration::from_millis(80),
                retry: RetryPolicy {
                    initial_delay: Duration::from_millis(5),
                    max_retries: 1,
                    ..Default::default()
                },
                max_connections: 4,
                headers: vec![],
                credentials: None,
                verify_downloads: true,
            },
        );

        let data = b"File Contents";
        let d = DigestFn::Sha256.digest(data);

        // Upload timeout (`UploadTimeoutException` parity): fails promptly on the first attempt.
        let t0 = Instant::now();
        let up_err = client.upload_blob(&d, data).unwrap_err();
        let up_elapsed = t0.elapsed();
        assert!(
            up_elapsed < Duration::from_secs(3),
            "upload took too long: {up_elapsed:?}"
        );
        match up_err {
            RemoteError::Io(e) => assert_eq!(e.kind(), io::ErrorKind::TimedOut, "{e}"),
            other => panic!("expected TimedOut Io error, got {other:?}"),
        }
        assert_eq!(put_attempts.load(Ordering::SeqCst), 1);

        // Download timeout (`DownloadTimeoutException` parity): retries `max_retries` times then fails.
        let t1 = Instant::now();
        let dl_err = client.download_blob(&d).unwrap_err();
        let dl_elapsed = t1.elapsed();
        assert!(
            dl_elapsed < Duration::from_secs(3),
            "download took too long: {dl_elapsed:?}"
        );
        match dl_err {
            RemoteError::Io(e) => assert_eq!(e.kind(), io::ErrorKind::TimedOut, "{e}"),
            other => panic!("expected TimedOut Io error, got {other:?}"),
        }
        assert_eq!(get_attempts.load(Ordering::SeqCst), 2);

        *release.0.lock().unwrap() = true;
        release.1.notify_all();
    }

    #[test]
    fn http_cache_stalled_pipe_and_partial_stream_honor_timeout() {
        use crate::transport::{memory_pair, Transport};

        struct StalledPipeConnector {
            peers: Mutex<Vec<Transport>>,
        }

        impl Connector for StalledPipeConnector {
            fn connect(&self) -> io::Result<Transport> {
                let (client_end, mut server_end) = memory_pair();
                std::thread::spawn(move || {
                    let mut br = BufReader::new(&mut *server_end.reader);
                    loop {
                        let mut line = String::new();
                        if br.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                    }
                    // Send headers with no `Content-Length` plus partial bytes, then stall so
                    // `read_to_end` blocks on a non-socket pipe without `SO_RCVTIMEO`.
                    let _ = server_end
                        .writer
                        .write_all(b"HTTP/1.1 200 OK\r\n\r\npartial");
                    let _ = server_end.writer.flush();
                    // Keep `server_end` alive long enough for the client's idle watchdog to fire.
                    std::thread::sleep(Duration::from_millis(300));
                });
                self.peers.lock().unwrap().push(Transport {
                    reader: Box::new(io::empty()),
                    writer: Box::new(io::sink()),
                    shutdown: Box::new(|| {}),
                });
                Ok(client_end)
            }

            fn authority(&self) -> String {
                "pipe.local".into()
            }
        }

        let client = HttpCacheClient::new(
            Arc::new(StalledPipeConnector {
                peers: Mutex::new(Vec::new()),
            }),
            HttpCacheConfig {
                uri: "https://pipe.local".into(),
                prefix: String::new(),
                tls: true,
                digest_fn: DigestFn::Sha256,
                timeout: Duration::from_millis(60),
                retry: RetryPolicy {
                    max_retries: 0,
                    ..Default::default()
                },
                max_connections: 2,
                headers: vec![],
                credentials: None,
                verify_downloads: false,
            },
        );

        let d = DigestFn::Sha256.digest(b"partial");
        let start = Instant::now();
        let err = client.download_blob(&d).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(2));
        match err {
            RemoteError::Io(e) => assert_eq!(e.kind(), io::ErrorKind::TimedOut, "{e}"),
            other => panic!("expected TimedOut on stalled partial stream, got {other:?}"),
        }
    }

    #[test]
    fn http_cache_uploads_raw_bytes_and_decodes_gzip_deflate_zstd_downloads() {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return,
            Err(e) => panic!("{e}"),
        };
        let port = listener.local_addr().unwrap().port();
        let put_content_encodings: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let get_accept_encodings: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let store: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>> =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let get_counter = Arc::new(AtomicU64::new(0));

        let put_ce_srv = put_content_encodings.clone();
        let get_ae_srv = get_accept_encodings.clone();
        let store_srv = store.clone();
        let cnt_srv = get_counter.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let put_ce = put_ce_srv.clone();
                let get_ae = get_ae_srv.clone();
                let store = store_srv.clone();
                let cnt = cnt_srv.clone();
                std::thread::spawn(move || {
                    let mut br = BufReader::new(&stream);
                    let mut req_line = String::new();
                    if br.read_line(&mut req_line).unwrap_or(0) == 0 {
                        return;
                    }
                    let parts: Vec<&str> = req_line.split_whitespace().collect();
                    if parts.len() < 2 {
                        return;
                    }
                    let (method, path) = (parts[0], parts[1]);
                    let mut clen = 0usize;
                    let mut ce = None;
                    let mut ae = None;
                    loop {
                        let mut h = String::new();
                        if br.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                            break;
                        }
                        if let Some((k, v)) = h.split_once(':') {
                            let k = k.trim();
                            let v = v.trim();
                            if k.eq_ignore_ascii_case("content-length") {
                                clen = v.parse().unwrap_or(0);
                            } else if k.eq_ignore_ascii_case("content-encoding") {
                                ce = Some(v.to_string());
                            } else if k.eq_ignore_ascii_case("accept-encoding") {
                                ae = Some(v.to_string());
                            }
                        }
                    }
                    let mut body = vec![0u8; clen];
                    if clen > 0 {
                        let _ = br.read_exact(&mut body);
                    }
                    let mut w = &stream;
                    match method {
                        "PUT" => {
                            if let Some(enc) = ce {
                                put_ce.lock().unwrap().push(enc);
                            }
                            store.lock().unwrap().insert(path.to_string(), body);
                            let _ =
                                w.write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n");
                        }
                        "GET" => {
                            if let Some(enc) = ae {
                                get_ae.lock().unwrap().push(enc);
                            }
                            let Some(raw) = store.lock().unwrap().get(path).cloned() else {
                                let _ = w.write_all(
                                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
                                );
                                return;
                            };
                            let idx = cnt.fetch_add(1, Ordering::SeqCst);
                            let (enc_name, encoded) = match idx % 3 {
                                0 => ("gzip", zel_util::deflate::gzip(&raw)),
                                1 => {
                                    let gz = zel_util::deflate::gzip(&raw);
                                    let deflate_stream = &gz[10..gz.len() - 8];
                                    let mut zlib = Vec::with_capacity(deflate_stream.len() + 6);
                                    zlib.extend_from_slice(&[0x78, 0x9c]);
                                    zlib.extend_from_slice(deflate_stream);
                                    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());
                                    ("deflate", zlib)
                                }
                                _ => ("zstd", zel_util::zstd::compress(&raw)),
                            };
                            let _ = write!(
                                w,
                                "HTTP/1.1 200 OK\r\nContent-Encoding: {enc_name}\r\nContent-Length: {}\r\n\r\n",
                                encoded.len()
                            );
                            let _ = w.write_all(&encoded);
                        }
                        _ => {}
                    }
                });
            }
        });

        let conn = Arc::new(TcpConnector::new(
            "127.0.0.1".into(),
            port,
            Duration::from_secs(5),
        ));
        let client = HttpCacheClient::new(
            conn,
            HttpCacheConfig {
                uri: format!("http://127.0.0.1:{port}"),
                prefix: String::new(),
                tls: false,
                digest_fn: DigestFn::Sha256,
                timeout: Duration::from_secs(5),
                retry: RetryPolicy::default(),
                max_connections: 4,
                headers: vec![],
                credentials: None,
                verify_downloads: true,
            },
        );

        let payload = b"webdav compatible uncompressed http put payload ".repeat(32);
        let d = DigestFn::Sha256.digest(&payload);
        client.upload_blob(&d, &payload).unwrap();

        // PUT must store raw uncompressed bytes with no Content-Encoding header.
        assert!(put_content_encodings.lock().unwrap().is_empty());
        assert_eq!(
            store
                .lock()
                .unwrap()
                .get(&format!("/cas/{}", d.hash))
                .unwrap(),
            &payload
        );

        // GET must advertise Accept-Encoding: gzip,deflate,zstd and decode all three encodings.
        for _ in 0..3 {
            assert_eq!(client.download_blob(&d).unwrap(), payload);
        }
        let seen_ae = get_accept_encodings.lock().unwrap().clone();
        assert_eq!(seen_ae.len(), 3);
        for ae in seen_ae {
            assert_eq!(ae, "gzip,deflate,zstd");
        }
    }
}
