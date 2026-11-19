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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use zel_proto::reapi::{code, ActionResult, Digest, Message};

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
    pub cache_compression: bool,
    pub compression_threshold: usize,
}

/// HTTP/1.1 & HTTPS REST cache client (`/ac/<hash>` and `/cas/<hash>`).
pub struct HttpCacheClient {
    connector: Arc<dyn Connector>,
    pub cfg: HttpCacheConfig,
    sem: Semaphore,
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
            out.push((k.to_string(), v.to_string()));
        }
        Ok(out)
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

        let mut transport = self.connector.connect().map_err(HttpAttemptError::Io)?;
        let authority = self.connector.authority();
        let mut req_head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: bazel/zel\r\nConnection: close\r\n"
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

        let write_res = transport
            .writer
            .write_all(req_head.as_bytes())
            .and_then(|_| {
                if let Some(b) = body {
                    transport.writer.write_all(b)?;
                }
                transport.writer.flush()
            });
        if let Err(e) = write_res {
            (transport.shutdown)();
            return Err(HttpAttemptError::Io(e));
        }

        let read_res = read_http_response(&mut *transport.reader);
        (transport.shutdown)();
        read_res.map_err(HttpAttemptError::Io)
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
        let (status, _hdrs, body) = self.request("GET", &path, None, &[])?;
        match status {
            200 => {
                let ar = ActionResult::decode(&body[..])?;
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
        let extra: &[(&str, &str)] = if self.cfg.cache_compression {
            &[("Accept-Encoding", "zstd")]
        } else {
            &[]
        };
        let (status, hdrs, mut body) = self.request("GET", &path, None, extra)?;
        match status {
            200 => {
                let is_zstd = hdrs.iter().any(|(k, v)| {
                    k.eq_ignore_ascii_case("content-encoding")
                        && v.trim().eq_ignore_ascii_case("zstd")
                });
                if is_zstd {
                    body = zel_util::zstd::decompress(&body).map_err(|e| {
                        RemoteError::Other(format!(
                            "Failed to decompress zstd CAS blob {}/{}: {e}",
                            d.hash, d.size_bytes
                        ))
                    })?;
                }
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
    Fatal(RemoteError),
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
                cache_compression: true,
                compression_threshold: 0,
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
}
