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

//! `RemoteOptions` / `AuthAndTLSOptions` handling: flag extraction, endpoint parsing
//! (`grpc://`, `grpcs://`, bare `host[:port]`, `unix:`, `http://`, `https://`), validation, and
//! channel / client construction.

use crate::credentials::{
    CallCredentialsProvider, CompositeCredentialsProvider, CredentialHelperProvider,
    GoogleAuthCallCredentialsProvider, NetrcCredentialsProvider,
};
use crate::digest::DigestFn;
use crate::grpc::{Channel, ChannelConfig, RetryPolicy};
use crate::hpack::Header;
use crate::http_cache::{HttpCacheClient, HttpCacheConfig};
use crate::reapi_client::{ReapiClient, ReapiConfig};
use crate::transport::{Connector, TcpConnector, TlsConfig, TlsConnector};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Where flag values come from (implemented for `HashMap<String, Vec<String>>`, and by the
/// server for its parsed command options).
pub trait OptionSource {
    /// All values given for `flag`, in command-line order.
    fn all(&self, flag: &str) -> Vec<String>;
    fn last(&self, flag: &str) -> Option<String> {
        self.all(flag).pop()
    }
}

impl OptionSource for HashMap<String, Vec<String>> {
    fn all(&self, flag: &str) -> Vec<String> {
        self.get(flag).cloned().unwrap_or_default()
    }
}

/// Builds an option source from `--name=value` style arguments (for tests / simple callers).
pub fn options_from_args(args: &[&str]) -> HashMap<String, Vec<String>> {
    let mut m: HashMap<String, Vec<String>> = HashMap::new();
    for a in args {
        let a = a.trim_start_matches("--");
        let (k, v) = match a.split_once('=') {
            Some((k, v)) => (k, v.to_string()),
            None => match a.strip_prefix("no") {
                Some(k) => (k, "false".to_string()),
                None => (a, "true".to_string()),
            },
        };
        m.entry(k.to_string()).or_default().push(v);
    }
    m
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DownloadMode {
    /// Download all outputs of remotely executed / cached actions.
    All,
    /// Download only outputs of top-level targets (and test outputs in `test`).
    #[default]
    TopLevel,
    /// Download as few outputs as possible.
    Minimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BuildEventUploadMode {
    #[default]
    All,
    Minimal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Tcp {
        host: String,
        port: u16,
        tls: bool,
    },
    Unix(String),
    /// HTTP/1.1 or HTTPS REST cache (`--remote_cache=http(s)://host:port/prefix`).
    Http {
        host: String,
        port: u16,
        prefix: String,
        tls: bool,
    },
}

fn redact_endpoint_userinfo(raw: &str) -> String {
    if raw.starts_with("unix:") {
        return raw.to_string();
    }
    let (prefix, rest) = match raw.split_once("://") {
        Some((sc, r)) => (Some(sc), r),
        None => (None, raw),
    };
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    let tail = &rest[auth_end..];
    let redacted_auth = match authority.rsplit_once('@') {
        Some((_, hostport)) => format!("***@{hostport}"),
        None => authority.to_string(),
    };
    match prefix {
        Some(sc) => format!("{sc}://{redacted_auth}{tail}"),
        None => format!("{redacted_auth}{tail}"),
    }
}

fn split_userinfo_hostport(authority: &str) -> (Option<String>, &str) {
    match authority.rsplit_once('@') {
        Some((ui, hp)) => {
            let userinfo = (!ui.is_empty()).then(|| ui.to_string());
            (userinfo, hp)
        }
        None => (None, authority),
    }
}

/// Builds an HTTP Basic `authorization` header from raw URL `userinfo` (`<api-key>` or
/// `<user>:<password>`), percent-decoding components and ensuring RFC 7617 `userid:password`
/// formatting (`<key>` -> `<key>:` so Go/RFC-7617 Basic auth parsers accept it).
pub fn basic_auth_header_for_userinfo(userinfo: &str) -> Option<Header> {
    if userinfo.is_empty() {
        return None;
    }
    let formatted = match userinfo.split_once(':') {
        Some((user, pass)) => format!(
            "{}:{}",
            crate::grpc::percent_decode(user),
            crate::grpc::percent_decode(pass)
        ),
        None => format!("{}:", crate::grpc::percent_decode(userinfo)),
    };
    Some((
        "authorization".to_string(),
        format!("Basic {}", zel_util::base64_encode(formatted.as_bytes())),
    ))
}

fn split_host_port(hp: &str, default: u16, raw: &str) -> Result<(String, u16), String> {
    let hp = hp.trim_end_matches('/');
    if hp.is_empty() {
        return Err(format!("Invalid remote endpoint: {raw}"));
    }
    if let Some(r) = hp.strip_prefix('[') {
        let (h, p) = r.split_once(']').ok_or("bad IPv6 address")?;
        if h.is_empty() {
            return Err(format!("Invalid IPv6 host in {raw}"));
        }
        let port = match p.strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| format!("bad port in {raw}"))?,
            None if p.is_empty() => default,
            None => return Err(format!("bad IPv6 authority in {raw}")),
        };
        return Ok((h.to_string(), port));
    }
    match hp.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => {
            if h.is_empty() {
                return Err(format!("Invalid remote endpoint: {raw}"));
            }
            Ok((
                h.to_string(),
                p.parse().map_err(|_| format!("bad port in {raw}"))?,
            ))
        }
        _ => Ok((hp.to_string(), default)),
    }
}

impl Endpoint {
    /// Parses a gRPC endpoint (`--remote_executor`, `--bes_backend`, or gRPC `--remote_cache`)
    /// matching Bazel's `GoogleAuthUtils.newChannel`, returning the sanitized [`Endpoint`] and
    /// any stripped raw `userinfo` (`grpcs://<api-key>@host` or `grpcs://user:pass@host`):
    /// - `unix:...` strips `"unix:"` and `"grpc://"` (so `unix:grpc://foo` -> `foo`,
    ///   `unix:///tmp/s` -> `///tmp/s`);
    /// - `grpc://[userinfo@]host[:port]` uses plaintext HTTP/2 with default port **443**;
    /// - `grpcs://[userinfo@]host[:port]`, `https://[userinfo@]host[:port]`, and bare
    ///   `[userinfo@]host[:port]` use TLS HTTP/2 with default port **443**.
    pub fn parse_grpc_with_userinfo(s: &str) -> Result<(Endpoint, Option<String>), String> {
        if s.is_empty() {
            return Err("Empty remote endpoint".into());
        }
        if let Some(after_unix) = s.strip_prefix("unix:") {
            let sock = after_unix.replace("grpc://", "");
            if sock.is_empty() {
                return Err(format!("Invalid unix endpoint: {s}"));
            }
            return Ok((Endpoint::Unix(sock), None));
        }
        let safe_raw = redact_endpoint_userinfo(s);
        let (scheme, rest) = match s.split_once("://") {
            Some((sc, r)) => (sc.to_ascii_lowercase(), r),
            None => (String::new(), s),
        };
        match scheme.as_str() {
            "grpc" => {
                let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
                let (userinfo, hostport) = split_userinfo_hostport(authority);
                let (host, port) = split_host_port(hostport, 443, &safe_raw)?;
                Ok((
                    Endpoint::Tcp {
                        host,
                        port,
                        tls: false,
                    },
                    userinfo,
                ))
            }
            "grpcs" | "https" | "" => {
                let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
                let (userinfo, hostport) = split_userinfo_hostport(authority);
                let (host, port) = split_host_port(hostport, 443, &safe_raw)?;
                Ok((
                    Endpoint::Tcp {
                        host,
                        port,
                        tls: true,
                    },
                    userinfo,
                ))
            }
            other => Err(format!(
                "Unsupported gRPC scheme '{other}' in endpoint '{safe_raw}'"
            )),
        }
    }

    /// Parses a gRPC endpoint (`--remote_executor`, `--bes_backend`, or gRPC `--remote_cache`),
    /// stripping any embedded `userinfo@` from the returned host.
    pub fn parse_grpc(s: &str) -> Result<Endpoint, String> {
        Self::parse_grpc_with_userinfo(s).map(|(ep, _)| ep)
    }

    /// Parses a `--remote_cache` value: `http://` and `https://` select the HTTP(S) REST cache
    /// protocol; everything else follows [`Endpoint::parse_grpc_with_userinfo`].
    pub fn parse_cache_with_userinfo(s: &str) -> Result<(Endpoint, Option<String>), String> {
        if s.is_empty() {
            return Err("Empty remote cache endpoint".into());
        }
        if let Some(after_unix) = s.strip_prefix("unix:") {
            let sock = after_unix.replace("grpc://", "");
            if sock.is_empty() {
                return Err(format!("Invalid unix endpoint: {s}"));
            }
            return Ok((Endpoint::Unix(sock), None));
        }
        if let Some((sc, rest)) = s.split_once("://") {
            let sc_lc = sc.to_ascii_lowercase();
            if sc_lc == "http" || sc_lc == "https" {
                let safe_raw = redact_endpoint_userinfo(s);
                let tls = sc_lc == "https";
                let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
                let authority = &rest[..auth_end];
                let tail = &rest[auth_end..];
                let prefix = if let Some(after_slash) = tail.strip_prefix('/') {
                    let clean_p = after_slash.split(['?', '#']).next().unwrap_or(after_slash);
                    let trimmed = clean_p.trim_end_matches('/');
                    if trimmed.is_empty() {
                        String::new()
                    } else {
                        format!("/{trimmed}")
                    }
                } else {
                    String::new()
                };
                let (userinfo, hostport) = split_userinfo_hostport(authority);
                let (host, port) =
                    split_host_port(hostport, if tls { 443 } else { 80 }, &safe_raw)?;
                return Ok((
                    Endpoint::Http {
                        host,
                        port,
                        prefix,
                        tls,
                    },
                    userinfo,
                ));
            }
        }
        Self::parse_grpc_with_userinfo(s)
    }

    /// Parses a `--remote_cache` value: `http://` and `https://` select the HTTP(S) REST cache
    /// protocol; everything else follows [`Endpoint::parse_grpc`].
    pub fn parse_cache(s: &str) -> Result<Endpoint, String> {
        Self::parse_cache_with_userinfo(s).map(|(ep, _)| ep)
    }

    /// Parses an endpoint (`--remote_cache` / general helper). `tls_enabled` is retained for API
    /// compatibility; per Bazel 9.3.2 `GoogleAuthUtils`, TLS is always enabled unless the scheme
    /// is `grpc://`, `http://`, or `unix:`.
    pub fn parse(s: &str, _tls_enabled: bool) -> Result<Endpoint, String> {
        Self::parse_cache(s)
    }

    pub fn is_tls(&self) -> bool {
        matches!(
            self,
            Endpoint::Tcp { tls: true, .. } | Endpoint::Http { tls: true, .. }
        )
    }

    pub fn is_http_cache(&self) -> bool {
        matches!(self, Endpoint::Http { .. })
    }

    pub fn auth_uri(&self) -> String {
        match self {
            Endpoint::Tcp { host, port, tls } => {
                let sc = if *tls { "https" } else { "http" };
                let h = if host.contains(':') && !host.starts_with('[') {
                    format!("[{host}]")
                } else {
                    host.clone()
                };
                if (*tls && *port == 443) || (!*tls && *port == 80) {
                    format!("{sc}://{h}")
                } else {
                    format!("{sc}://{h}:{port}")
                }
            }
            Endpoint::Http {
                host,
                port,
                prefix,
                tls,
            } => {
                let sc = if *tls { "https" } else { "http" };
                let h = if host.contains(':') && !host.starts_with('[') {
                    format!("[{host}]")
                } else {
                    host.clone()
                };
                if (*tls && *port == 443) || (!*tls && *port == 80) {
                    format!("{sc}://{h}{prefix}")
                } else {
                    format!("{sc}://{h}:{port}{prefix}")
                }
            }
            Endpoint::Unix(p) => format!("unix:{p}"),
        }
    }

    pub fn connector(&self, timeout: Duration) -> Result<Arc<dyn Connector>, String> {
        self.connector_with_tls(timeout, &TlsConfig::default(), None)
    }

    pub fn connector_with_tls(
        &self,
        timeout: Duration,
        tls_cfg: &TlsConfig,
        unix_proxy: Option<&str>,
    ) -> Result<Arc<dyn Connector>, String> {
        tls_cfg.validate()?;
        match self {
            Endpoint::Tcp {
                host,
                port,
                tls: true,
            } => Ok(Arc::new(TlsConnector {
                host: host.clone(),
                port: *port,
                connect_timeout: timeout,
                alpn: Some("h2".into()),
                tls: tls_cfg.clone(),
                unix_proxy: unix_proxy.map(str::to_string),
            })),
            Endpoint::Tcp {
                host,
                port,
                tls: false,
            } => {
                #[cfg(unix)]
                if let Some(u) = unix_proxy {
                    return Ok(Arc::new(crate::transport::UnixConnector {
                        path: u.to_string(),
                        authority_override: tls_cfg
                            .authority_override
                            .clone()
                            .or_else(|| Some(format!("{host}:{port}"))),
                    }));
                }
                Ok(Arc::new(TcpConnector {
                    host: host.clone(),
                    port: *port,
                    connect_timeout: timeout,
                    authority_override: tls_cfg.authority_override.clone(),
                }))
            }
            #[cfg(unix)]
            Endpoint::Unix(p) => Ok(Arc::new(crate::transport::UnixConnector {
                path: p.clone(),
                authority_override: tls_cfg.authority_override.clone(),
            })),
            #[cfg(not(unix))]
            Endpoint::Unix(_) => Err("unix: endpoints are not supported on this platform".into()),
            Endpoint::Http {
                host,
                port,
                tls: true,
                ..
            } => Ok(Arc::new(TlsConnector {
                host: host.clone(),
                port: *port,
                connect_timeout: timeout,
                alpn: None,
                tls: tls_cfg.clone(),
                unix_proxy: unix_proxy.map(str::to_string),
            })),
            Endpoint::Http {
                host,
                port,
                tls: false,
                ..
            } => {
                #[cfg(unix)]
                if let Some(u) = unix_proxy {
                    return Ok(Arc::new(crate::transport::UnixConnector {
                        path: u.to_string(),
                        authority_override: tls_cfg
                            .authority_override
                            .clone()
                            .or_else(|| Some(format!("{host}:{port}"))),
                    }));
                }
                Ok(Arc::new(TcpConnector {
                    host: host.clone(),
                    port: *port,
                    connect_timeout: timeout,
                    authority_override: tls_cfg.authority_override.clone(),
                }))
            }
        }
    }
}

/// Normalizes a `PathFragment` option (`~/` expansion, `.` / `..` lexical collapsing, empty ->
/// `None` so e.g. `--disk_cache=` or `--disk_cache=.` disables the option just like Bazel).
pub fn normalize_path_option(raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    let expanded = if raw == "~" {
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| raw.to_string())
    } else if let Some(rest) = raw.strip_prefix("~/") {
        match std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
            Ok(h) if !h.is_empty() => format!("{}/{rest}", h.trim_end_matches('/')),
            _ => raw.to_string(),
        }
    } else {
        raw.to_string()
    };

    let absolute = expanded.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for seg in expanded.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if let Some(&last) = stack.last() {
                    if last != ".." {
                        stack.pop();
                        continue;
                    }
                }
                if !absolute {
                    stack.push("..");
                }
            }
            other => stack.push(other),
        }
    }
    if stack.is_empty() {
        if absolute {
            Some("/".to_string())
        } else {
            None
        }
    } else if absolute {
        Some(format!("/{}", stack.join("/")))
    } else {
        Some(stack.join("/"))
    }
}

/// Parsed `--remote_*` and `AuthAndTLSOptions` flags.
#[derive(Debug, Clone)]
pub struct RemoteOptions {
    pub remote_cache: Option<String>,
    pub remote_executor: Option<String>,
    pub remote_instance_name: String,
    pub disk_cache: Option<String>,
    pub upload_local_results: bool,
    pub accept_cached: bool,
    pub download_mode: DownloadMode,
    pub headers: Vec<Header>,
    pub cache_headers: Vec<Header>,
    pub exec_headers: Vec<Header>,
    pub timeout: Duration,
    pub retries: u32,
    pub retry_max_delay: Duration,
    /// `0` means unlimited connections (Bazel parity).
    pub max_connections: usize,
    /// `0` means unlimited concurrent streams per connection (Bazel default: 100).
    pub max_concurrency_per_connection: usize,
    pub local_fallback: bool,
    pub local_fallback_for_remote_cache: bool,
    pub cache_compression: bool,
    pub cache_compression_threshold: usize,
    pub google_credentials: Option<String>,
    pub google_default_credentials: bool,
    pub google_auth_scopes: Vec<String>,
    pub remote_proxy: Option<String>,
    pub digest_fn: DigestFn,
    pub default_exec_properties: BTreeMap<String, String>,
    pub tls_enabled: bool,
    pub tls_config: TlsConfig,
    pub verify_downloads: bool,
    pub download_regex: Vec<String>,
    pub credential_helper: Option<String>,
    pub credential_helpers: Vec<String>,
    pub workspace: Option<std::path::PathBuf>,
    pub credential_helper_timeout: Duration,
    pub credential_helper_cache_duration: Duration,
    pub execution_priority: i32,
    pub result_cache_priority: i32,
    pub bytestream_uri_prefix: Option<String>,
    pub build_event_upload_mode: BuildEventUploadMode,
    pub remote_cache_ttl: Duration,
    pub scrubbing_config: Option<String>,
    pub grpc_log: Option<String>,
    pub capture_corrupted_outputs: Option<String>,
    pub guard_against_concurrent_changes: bool,
    pub build_request_id: Option<String>,
    pub netrc: Option<String>,
    pub client_env: BTreeMap<String, String>,
    pub has_client_env: bool,
}

impl Default for RemoteOptions {
    fn default() -> Self {
        Self {
            remote_cache: None,
            remote_executor: None,
            remote_instance_name: String::new(),
            disk_cache: None,
            upload_local_results: true,
            accept_cached: true,
            download_mode: DownloadMode::TopLevel,
            headers: vec![],
            cache_headers: vec![],
            exec_headers: vec![],
            timeout: Duration::from_secs(60),
            retries: 5,
            retry_max_delay: Duration::from_secs(5),
            max_connections: 100,
            max_concurrency_per_connection: 100,
            local_fallback: false,
            local_fallback_for_remote_cache: false,
            cache_compression: false,
            cache_compression_threshold: 100,
            google_credentials: None,
            google_default_credentials: false,
            google_auth_scopes: vec![],
            remote_proxy: None,
            digest_fn: DigestFn::Sha256,
            default_exec_properties: BTreeMap::new(),
            tls_enabled: true,
            tls_config: TlsConfig::default(),
            verify_downloads: true,
            download_regex: vec![],
            credential_helper: None,
            credential_helpers: vec![],
            workspace: None,
            credential_helper_timeout: Duration::from_secs(5),
            credential_helper_cache_duration: Duration::from_secs(30 * 60),
            execution_priority: 0,
            result_cache_priority: 0,
            bytestream_uri_prefix: None,
            build_event_upload_mode: BuildEventUploadMode::All,
            remote_cache_ttl: Duration::from_secs(3 * 3600),
            scrubbing_config: None,
            grpc_log: None,
            capture_corrupted_outputs: None,
            guard_against_concurrent_changes: false,
            build_request_id: None,
            netrc: None,
            client_env: BTreeMap::new(),
            has_client_env: false,
        }
    }
}

/// Parses a Bazel duration flag value: `60`, `60s`, `5m`, `1h`, `500ms`, `1d` (bare numbers are seconds).
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, mult_ms): (&str, f64) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1.0)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1000.0)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60_000.0)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3_600_000.0)
    } else if let Some(n) = s.strip_suffix('d') {
        (n, 86_400_000.0)
    } else {
        (s, 1000.0)
    };
    let v: f64 = num
        .trim()
        .parse()
        .map_err(|_| format!("Invalid duration: '{s}'"))?;
    if v < 0.0 {
        return Err(format!("Invalid duration: '{s}'"));
    }
    Ok(Duration::from_millis((v * mult_ms) as u64))
}

fn parse_bool(flag: &str, v: &str) -> Result<bool, String> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(format!(
            "While parsing option --{flag}: '{v}' is not a boolean"
        )),
    }
}

fn parse_header(flag: &str, v: &str) -> Result<Header, String> {
    let (n, val) = v
        .split_once('=')
        .ok_or_else(|| format!("Invalid value for --{flag}: '{v}' (expected Name=Value)"))?;
    if n.is_empty() {
        return Err(format!(
            "Invalid value for --{flag}: '{v}' (empty header name)"
        ));
    }
    Ok((n.to_string(), val.to_string()))
}

/// Collapses multiple headers with the same case-insensitive name using comma-joining (matching
/// `RemoteOptions` documentation for `--remote_header` / `--remote_cache_header` /
/// `--remote_exec_header`).
fn collapse_headers(headers: &[Header]) -> Vec<Header> {
    let mut out: Vec<Header> = Vec::new();
    for (k, v) in headers {
        let lc = k.to_ascii_lowercase();
        if let Some((_, existing)) = out.iter_mut().find(|(ek, _)| *ek == lc) {
            existing.push(',');
            existing.push_str(v);
        } else {
            out.push((lc, v.clone()));
        }
    }
    out
}

impl RemoteOptions {
    pub fn from_source(src: &dyn OptionSource) -> Result<Self, String> {
        let mut o = Self::default();
        let nonempty = |s: Option<String>| s.filter(|v| !v.is_empty());
        o.remote_cache = nonempty(
            src.last("remote_cache")
                .or_else(|| src.last("remote_http_cache")),
        );
        o.remote_executor = nonempty(src.last("remote_executor"));
        if let Some(v) = src.last("remote_instance_name") {
            o.remote_instance_name = v;
        }
        o.disk_cache = src
            .last("disk_cache")
            .and_then(|v| normalize_path_option(&v));
        let b = |flag: &str, d: bool| -> Result<bool, String> {
            match src.last(flag) {
                Some(v) => parse_bool(flag, &v),
                None => Ok(d),
            }
        };
        o.upload_local_results = b("remote_upload_local_results", true)?;
        o.accept_cached = b("remote_accept_cached", true)?;
        o.local_fallback = b("remote_local_fallback", false)?;
        o.local_fallback_for_remote_cache =
            b("incompatible_remote_local_fallback_for_remote_cache", false)?;
        o.cache_compression = b("remote_cache_compression", false)?
            || b("experimental_remote_cache_compression", false)?;
        if let Some(v) = src.last("experimental_remote_cache_compression_threshold") {
            let n: i64 = v.parse().map_err(|_| {
                format!(
                    "While parsing option --experimental_remote_cache_compression_threshold: '{v}' is not an int"
                )
            })?;
            if n < 0 {
                return Err(
                    "--experimental_remote_cache_compression_threshold must be non-negative".into(),
                );
            }
            o.cache_compression_threshold = n as usize;
        }
        o.tls_enabled = b("tls_enabled", true)?;
        o.verify_downloads = b("remote_verify_downloads", true)?;
        o.guard_against_concurrent_changes =
            b("experimental_guard_against_concurrent_changes", false)?;

        // Download mode: legacy shorthands first, then explicit flag (case-insensitive, last wins).
        let mut mode = DownloadMode::TopLevel;
        for (flag, m) in [
            ("remote_download_all", DownloadMode::All),
            ("remote_download_toplevel", DownloadMode::TopLevel),
            ("remote_download_minimal", DownloadMode::Minimal),
            (
                "experimental_remote_download_toplevel",
                DownloadMode::TopLevel,
            ),
            (
                "experimental_remote_download_minimal",
                DownloadMode::Minimal,
            ),
        ] {
            if let Some(v) = src.last(flag) {
                if parse_bool(flag, &v)? {
                    mode = m;
                }
            }
        }
        if let Some(v) = src
            .last("remote_download_outputs")
            .or_else(|| src.last("experimental_remote_download_outputs"))
        {
            mode = match v.to_ascii_lowercase().as_str() {
                "all" => DownloadMode::All,
                "minimal" => DownloadMode::Minimal,
                "toplevel" => DownloadMode::TopLevel,
                _ => {
                    return Err(format!(
                        "While parsing option --remote_download_outputs={v}: Invalid value '{v}': must be one of all, minimal, toplevel"
                    ))
                }
            };
        }
        o.download_mode = mode;

        if let Some(v) = src
            .last("remote_build_event_upload")
            .or_else(|| src.last("experimental_remote_build_event_upload"))
        {
            o.build_event_upload_mode = match v.to_ascii_lowercase().as_str() {
                "all" => BuildEventUploadMode::All,
                "minimal" => BuildEventUploadMode::Minimal,
                _ => {
                    return Err(format!(
                        "While parsing option --remote_build_event_upload={v}: Invalid value '{v}': must be one of all, minimal"
                    ))
                }
            };
        }

        for v in src.all("remote_header") {
            o.headers.push(parse_header("remote_header", &v)?);
        }
        for v in src.all("remote_cache_header") {
            o.cache_headers
                .push(parse_header("remote_cache_header", &v)?);
        }
        for v in src.all("remote_exec_header") {
            o.exec_headers.push(parse_header("remote_exec_header", &v)?);
        }
        if let Some(v) = src.last("remote_timeout") {
            o.timeout = parse_duration(&v)?;
        }
        if let Some(v) = src
            .last("remote_retries")
            .or_else(|| src.last("experimental_remote_retry_max_attempts"))
        {
            let r: i32 = v.parse().map_err(|_| {
                format!("While parsing option --remote_retries: '{v}' is not an int")
            })?;
            if r < 0 {
                return Err("--remote_retries must be non-negative".into());
            }
            o.retries = r as u32;
        }
        if let Some(v) = src.last("remote_retry_max_delay") {
            o.retry_max_delay = parse_duration(&v)?;
        }
        if let Some(v) = src.last("remote_max_connections") {
            let n: i32 = v.parse().map_err(|_| {
                format!("While parsing option --remote_max_connections: '{v}' is not an int")
            })?;
            // In Bazel, 0 or negative means unlimited (0).
            o.max_connections = if n <= 0 { 0 } else { n as usize };
        }
        if let Some(v) = src.last("remote_max_concurrency_per_connection") {
            let n: i32 = v.parse().map_err(|_| {
                format!(
                    "While parsing option --remote_max_concurrency_per_connection: '{v}' is not an int"
                )
            })?;
            o.max_concurrency_per_connection = if n <= 0 { 0 } else { n as usize };
        }
        if let Some(v) = src.last("remote_execution_priority") {
            o.execution_priority = v.parse().map_err(|_| {
                format!("While parsing option --remote_execution_priority: '{v}' is not an int")
            })?;
        }
        if let Some(v) = src.last("remote_result_cache_priority") {
            o.result_cache_priority = v.parse().map_err(|_| {
                format!("While parsing option --remote_result_cache_priority: '{v}' is not an int")
            })?;
        }

        o.google_credentials = nonempty(src.last("google_credentials"));
        o.google_default_credentials = b("google_default_credentials", false)?;
        for s in src.all("google_auth_scopes") {
            for part in s.split(',').filter(|p| !p.is_empty()) {
                o.google_auth_scopes.push(part.to_string());
            }
        }

        o.tls_config = TlsConfig {
            ca_certificate: src
                .last("tls_certificate")
                .and_then(|v| normalize_path_option(&v))
                .map(PathBuf::from),
            client_certificate: src
                .last("tls_client_certificate")
                .and_then(|v| normalize_path_option(&v))
                .map(PathBuf::from),
            client_key: src
                .last("tls_client_key")
                .and_then(|v| normalize_path_option(&v))
                .map(PathBuf::from),
            authority_override: nonempty(src.last("tls_authority_override")),
        };

        o.remote_proxy =
            nonempty(src.last("remote_proxy")).or_else(|| nonempty(src.last("remote_cache_proxy")));

        let mut helpers = src.all("credential_helper");
        helpers.extend(src.all("experimental_credential_helper"));
        helpers.retain(|s| !s.is_empty());
        o.credential_helper = helpers.last().cloned();
        o.credential_helpers = helpers;
        if let Some(v) = src
            .last("credential_helper_timeout")
            .or_else(|| src.last("experimental_credential_helper_timeout"))
        {
            o.credential_helper_timeout = parse_duration(&v)?;
        }
        if let Some(v) = src
            .last("credential_helper_cache_duration")
            .or_else(|| src.last("experimental_credential_helper_cache_duration"))
        {
            o.credential_helper_cache_duration = parse_duration(&v)?;
        }
        o.netrc = nonempty(src.last("netrc")).and_then(|v| normalize_path_option(&v));

        if let Some(v) = src.last("digest_function") {
            o.digest_fn = DigestFn::parse(&v)?;
        }
        for v in src.all("remote_default_exec_properties") {
            let (k, val) = v.split_once('=').ok_or_else(|| {
                format!(
                    "Invalid value for --remote_default_exec_properties: '{v}' (expected name=value)"
                )
            })?;
            o.default_exec_properties
                .insert(k.to_string(), val.to_string());
        }
        let mut regexes = src.all("remote_download_regex");
        regexes.extend(src.all("experimental_remote_download_regex"));
        regexes.retain(|r| !r.is_empty());
        o.download_regex = regexes;

        o.bytestream_uri_prefix = nonempty(src.last("remote_bytestream_uri_prefix"));
        if let Some(v) = src.last("experimental_remote_cache_ttl") {
            o.remote_cache_ttl = parse_duration(&v)?;
        }
        o.scrubbing_config = src
            .last("experimental_remote_scrubbing_config")
            .and_then(|v| normalize_path_option(&v));
        o.grpc_log = src
            .last("remote_grpc_log")
            .or_else(|| src.last("experimental_remote_grpc_log"))
            .and_then(|v| normalize_path_option(&v));
        o.capture_corrupted_outputs = src
            .last("experimental_remote_capture_corrupted_outputs")
            .and_then(|v| normalize_path_option(&v));
        o.build_request_id = nonempty(src.last("build_request_id"));

        o.validate()?;
        Ok(o)
    }

    /// Cross-flag validation mirroring `RemoteModule.beforeCommand` / `RemoteOptions`.
    pub fn validate(&self) -> Result<(), String> {
        self.tls_config.validate()?;
        if let Some(c) = &self.remote_cache {
            let ep = Endpoint::parse_cache(c)?;
            if ep.is_http_cache() && self.remote_executor.is_some() {
                return Err(
                    "Cannot combine HTTP remote cache (--remote_cache=http(s)://...) with --remote_executor"
                        .into(),
                );
            }
        }
        if let Some(e) = &self.remote_executor {
            let _ = Endpoint::parse_grpc(e)?;
        }
        if let Some(p) = &self.remote_proxy {
            if !p.starts_with("unix:") {
                return Err(format!(
                    "--remote_proxy={p}: only unix: proxies are supported (as in Bazel)"
                ));
            }
        }
        for pat in &self.download_regex {
            zel_util::java_regex::Regex::new(&format!("(?s){pat}"))
                .map_err(|e| format!("While parsing option --remote_download_regex={pat}: {e}"))?;
        }
        Ok(())
    }

    pub fn has_remote_cache(&self) -> bool {
        self.remote_cache.is_some() || self.remote_executor.is_some()
    }

    pub fn is_http_cache(&self) -> bool {
        self.remote_cache
            .as_deref()
            .and_then(|s| Endpoint::parse_cache(s).ok())
            .is_some_and(|ep| ep.is_http_cache())
    }

    pub fn retry_policy(&self) -> RetryPolicy {
        RetryPolicy {
            max_retries: self.retries,
            max_delay: self.retry_max_delay,
            ..Default::default()
        }
    }

    pub fn reapi_config(&self, tool_invocation_id: &str) -> ReapiConfig {
        ReapiConfig {
            instance_name: self.remote_instance_name.clone(),
            digest_fn: self.digest_fn,
            timeout: self.timeout,
            retry: self.retry_policy(),
            tool_invocation_id: tool_invocation_id.to_string(),
            correlated_invocations_id: self
                .build_request_id
                .clone()
                .unwrap_or_else(|| tool_invocation_id.to_string()),
            cache_compression: self.cache_compression,
            compression_threshold: self.cache_compression_threshold,
            verify_downloads: self.verify_downloads,
            execution_priority: self.execution_priority,
            result_cache_priority: self.result_cache_priority,
        }
    }

    /// Supplies the invoking command's `client_env` (`NETRC`/`HOME`/`USERPROFILE`).
    pub fn set_client_env(&mut self, env: BTreeMap<String, String>) {
        self.client_env = env;
        self.has_client_env = true;
    }

    /// Builds the `CallCredentialsProvider` configured by `--credential_helper`,
    /// `--google_credentials`, `--google_default_credentials`, and `.netrc`.
    pub fn build_credentials_provider(
        &self,
        workspace: Option<&Path>,
    ) -> Result<Option<Arc<dyn CallCredentialsProvider>>, String> {
        self.build_credentials_provider_with_userinfo(workspace, None)
    }

    fn build_credentials_provider_with_userinfo(
        &self,
        workspace: Option<&Path>,
        userinfo: Option<Header>,
    ) -> Result<Option<Arc<dyn CallCredentialsProvider>>, String> {
        let workspace = workspace.or(self.workspace.as_deref());
        let client_env =
            (self.has_client_env || !self.client_env.is_empty()).then_some(&self.client_env);
        let helper = if !self.credential_helpers.is_empty() {
            Some(CredentialHelperProvider::from_specs_with_env(
                &self.credential_helpers,
                workspace,
                client_env,
                self.credential_helper_timeout,
                self.credential_helper_cache_duration,
            ))
        } else {
            self.credential_helper.as_ref().map(|single| {
                CredentialHelperProvider::from_specs_with_env(
                    std::slice::from_ref(single),
                    workspace,
                    client_env,
                    self.credential_helper_timeout,
                    self.credential_helper_cache_duration,
                )
            })
        };

        let google = if let Some(path_str) = &self.google_credentials {
            Some(GoogleAuthCallCredentialsProvider::from_file(
                PathBuf::from(path_str),
                self.google_auth_scopes.clone(),
                self.tls_config.clone(),
            ))
        } else if self.google_default_credentials {
            let provider = if let Some(env) = client_env {
                GoogleAuthCallCredentialsProvider::from_adc_with_client_env(
                    self.google_auth_scopes.clone(),
                    self.tls_config.clone(),
                    env,
                )
            } else {
                GoogleAuthCallCredentialsProvider::from_adc(
                    self.google_auth_scopes.clone(),
                    self.tls_config.clone(),
                )
            }
            .map_err(|e| e.to_string())?;
            Some(provider)
        } else {
            None
        };

        let netrc = if let Some(path) = &self.netrc {
            Some(NetrcCredentialsProvider::from_file(Path::new(path))?)
        } else if google.is_none() {
            if let Some(env) = client_env {
                NetrcCredentialsProvider::from_client_env(env)?
            } else {
                NetrcCredentialsProvider::try_from_env()?
            }
        } else {
            None
        };

        if helper.is_none() && google.is_none() && userinfo.is_none() && netrc.is_none() {
            return Ok(None);
        }
        Ok(Some(Arc::new(CompositeCredentialsProvider {
            helper,
            google,
            userinfo,
            netrc,
        })))
    }

    fn unix_proxy_path(&self) -> Result<Option<String>, String> {
        match &self.remote_proxy {
            Some(p) => {
                if !p.starts_with("unix:") {
                    return Err(format!(
                        "--remote_proxy={p}: only unix: proxies are supported"
                    ));
                }
                match Endpoint::parse_grpc(p)? {
                    Endpoint::Unix(u) => Ok(Some(u)),
                    _ => Err(format!(
                        "--remote_proxy={p}: only unix: proxies are supported"
                    )),
                }
            }
            None => Ok(None),
        }
    }

    fn channel_for(&self, endpoint: &str, extra: &[Header]) -> Result<Arc<Channel>, String> {
        let (ep, userinfo) = Endpoint::parse_grpc_with_userinfo(endpoint)?;
        let proxy = self.unix_proxy_path()?;
        let connector = ep.connector_with_tls(self.timeout, &self.tls_config, proxy.as_deref())?;
        let mut raw_headers = self.headers.clone();
        raw_headers.extend(extra.iter().cloned());
        let headers = collapse_headers(&raw_headers);
        let has_explicit_auth = headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("authorization"));
        let userinfo_hdr = if has_explicit_auth {
            None
        } else {
            userinfo.as_deref().and_then(basic_auth_header_for_userinfo)
        };
        let creds = self.build_credentials_provider_with_userinfo(None, userinfo_hdr)?;
        let auth_uri = ep.auth_uri();
        Ok(Arc::new(Channel::with_config(
            connector,
            ChannelConfig {
                tls: ep.is_tls(),
                max_connections: self.max_connections,
                max_concurrency_per_connection: self.max_concurrency_per_connection,
                default_metadata: headers,
                credentials: creds,
                auth_uri,
            },
        )))
    }

    /// Builds an HTTP(S) REST cache client when `--remote_cache=http(s)://...` is configured.
    pub fn build_http_cache(&self) -> Result<Option<Arc<HttpCacheClient>>, String> {
        let Some(cache_str) = &self.remote_cache else {
            return Ok(None);
        };
        let (ep, userinfo) = Endpoint::parse_cache_with_userinfo(cache_str)?;
        let Endpoint::Http {
            ref prefix, tls, ..
        } = ep
        else {
            return Ok(None);
        };
        let proxy = self.unix_proxy_path()?;
        let connector = ep.connector_with_tls(self.timeout, &self.tls_config, proxy.as_deref())?;
        let mut raw_headers = self.headers.clone();
        raw_headers.extend(self.cache_headers.iter().cloned());
        let headers = collapse_headers(&raw_headers);
        let has_explicit_auth = headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("authorization"));
        let userinfo_hdr = if has_explicit_auth {
            None
        } else {
            userinfo.as_deref().and_then(basic_auth_header_for_userinfo)
        };
        let creds = self.build_credentials_provider_with_userinfo(None, userinfo_hdr)?;
        Ok(Some(Arc::new(HttpCacheClient::new(
            connector,
            HttpCacheConfig {
                uri: ep.auth_uri(),
                prefix: prefix.clone(),
                tls,
                digest_fn: self.digest_fn,
                timeout: self.timeout,
                retry: self.retry_policy(),
                max_connections: self.max_connections,
                headers,
                credentials: creds,
                verify_downloads: self.verify_downloads,
            },
        ))))
    }

    /// Builds the gRPC REAPI client for `--remote_cache` / `--remote_executor` (the executor also
    /// serves as the cache if no separate cache is given; returns `Ok(None)` when no gRPC remote
    /// endpoint is configured, e.g. HTTP REST cache only or disk cache only).
    pub fn build_client(&self, tool_invocation_id: &str) -> Result<Option<ReapiClient>, String> {
        if self.is_http_cache() {
            return Ok(None);
        }
        let cache_ep = self
            .remote_cache
            .clone()
            .or_else(|| self.remote_executor.clone());
        let Some(cache_ep) = cache_ep else {
            return Ok(None);
        };
        let cache = self.channel_for(&cache_ep, &self.cache_headers)?;
        let exec = match &self.remote_executor {
            Some(e)
                if (Some(e) == self.remote_cache.as_ref() || self.remote_cache.is_none())
                    && self.cache_headers == self.exec_headers =>
            {
                // Share a single gRPC Channel when cache and executor target the same endpoint
                // with identical headers (Bazel RemoteModule parity).
                Some(cache.clone())
            }
            Some(e) => Some(self.channel_for(e, &self.exec_headers)?),
            None => None,
        };
        Ok(Some(ReapiClient::new(
            cache,
            exec,
            self.reapi_config(tool_invocation_id),
        )))
    }

    ///Canonical `bytestream://` URI prefix (`--remote_bytestream_uri_prefix`, falling back to
    /// `<authority>[/<instance_name>]`).
    pub fn effective_bytestream_uri_prefix(&self) -> Option<String> {
        if let Some(explicit) = &self.bytestream_uri_prefix {
            return Some(explicit.trim_end_matches('/').to_string());
        }
        let ep_str = self
            .remote_cache
            .as_deref()
            .or(self.remote_executor.as_deref())?;
        let ep = Endpoint::parse_cache(ep_str).ok()?;
        let host_part = match ep {
            Endpoint::Tcp { host, port, tls } => {
                if (tls && port == 443) || (!tls && port == 80) {
                    host
                } else {
                    format!("{host}:{port}")
                }
            }
            Endpoint::Http {
                host, port, tls, ..
            } => {
                if (tls && port == 443) || (!tls && port == 80) {
                    host
                } else {
                    format!("{host}:{port}")
                }
            }
            Endpoint::Unix(_) => "localhost".to_string(),
        };
        if self.remote_instance_name.is_empty() {
            Some(host_part)
        } else {
            Some(format!("{host_part}/{}", self.remote_instance_name))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_parsing() {
        assert_eq!(
            Endpoint::parse("grpc://host:8980", false).unwrap(),
            Endpoint::Tcp {
                host: "host".into(),
                port: 8980,
                tls: false
            }
        );
        // grpc:// with no port defaults to 443 in Bazel!
        assert_eq!(
            Endpoint::parse("grpc://host", false).unwrap(),
            Endpoint::Tcp {
                host: "host".into(),
                port: 443,
                tls: false
            }
        );
        // Bare host[:port] without scheme uses TLS in Bazel!
        assert_eq!(
            Endpoint::parse("host:1", false).unwrap(),
            Endpoint::Tcp {
                host: "host".into(),
                port: 1,
                tls: true
            }
        );
        assert_eq!(
            Endpoint::parse("host", true).unwrap(),
            Endpoint::Tcp {
                host: "host".into(),
                port: 443,
                tls: true
            }
        );
        assert_eq!(
            Endpoint::parse("grpcs://h", false).unwrap(),
            Endpoint::Tcp {
                host: "h".into(),
                port: 443,
                tls: true
            }
        );
        // Bazel unix: scheme stripping: `unix:grpc://foo` -> `foo`, `unix:///tmp/s` -> `///tmp/s`.
        assert_eq!(
            Endpoint::parse("unix:/tmp/s.sock", false).unwrap(),
            Endpoint::Unix("/tmp/s.sock".into())
        );
        assert_eq!(
            Endpoint::parse("unix:grpc://foo", false).unwrap(),
            Endpoint::Unix("foo".into())
        );
        assert_eq!(
            Endpoint::parse("unix:///tmp/s", false).unwrap(),
            Endpoint::Unix("///tmp/s".into())
        );
        assert_eq!(
            Endpoint::parse("http://c:9090/cache/", false).unwrap(),
            Endpoint::Http {
                host: "c".into(),
                port: 9090,
                prefix: "/cache".into(),
                tls: false,
            }
        );
        assert_eq!(
            Endpoint::parse("https://c/cache/", false).unwrap(),
            Endpoint::Http {
                host: "c".into(),
                port: 443,
                prefix: "/cache".into(),
                tls: true,
            }
        );
        // For gRPC endpoints (--remote_executor / --bes_backend), https:// is TLS gRPC on 443.
        assert_eq!(
            Endpoint::parse_grpc("https://exec.example.com").unwrap(),
            Endpoint::Tcp {
                host: "exec.example.com".into(),
                port: 443,
                tls: true,
            }
        );
        assert_eq!(
            Endpoint::parse("grpc://[::1]:50051", false).unwrap(),
            Endpoint::Tcp {
                host: "::1".into(),
                port: 50051,
                tls: false
            }
        );
        assert!(Endpoint::parse("ftp://x", false).is_err());
        assert!(Endpoint::parse("grpc://", false).is_err());
    }

    #[test]
    fn durations_and_path_normalization() {
        assert_eq!(parse_duration("60").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
        assert_eq!(parse_duration("1.5s").unwrap(), Duration::from_millis(1500));
        assert!(parse_duration("abc").is_err());

        assert_eq!(normalize_path_option(""), None);
        assert_eq!(normalize_path_option("."), None);
        assert_eq!(
            normalize_path_option("/a/./b/../c/"),
            Some("/a/c".to_string())
        );
        assert_eq!(normalize_path_option("a/b/../c"), Some("a/c".to_string()));
    }

    #[test]
    fn full_flag_parse() {
        let src = options_from_args(&[
            "--remote_cache=grpc://c:1",
            "--remote_executor=grpc://e:2",
            "--remote_instance_name=main",
            "--disk_cache=/tmp/a/../dc",
            "--noremote_upload_local_results",
            "--remote_accept_cached=false",
            "--remote_download_outputs=Minimal",
            "--remote_header=A=1",
            "--remote_header=B=x=y",
            "--remote_timeout=2m",
            "--remote_retries=3",
            "--remote_max_connections=0",
            "--remote_local_fallback",
            "--remote_default_exec_properties=OSFamily=linux",
            "--digest_function=blake3",
        ]);
        let o = RemoteOptions::from_source(&src).unwrap();
        assert_eq!(o.remote_instance_name, "main");
        assert!(!o.upload_local_results && !o.accept_cached && o.local_fallback);
        assert_eq!(o.download_mode, DownloadMode::Minimal);
        assert_eq!(
            o.headers,
            vec![
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "x=y".to_string())
            ]
        );
        assert_eq!(o.timeout, Duration::from_secs(120));
        assert_eq!((o.retries, o.max_connections), (3, 0));
        assert_eq!(o.digest_fn, DigestFn::Blake3);
        assert_eq!(o.default_exec_properties["OSFamily"], "linux");
        assert_eq!(o.disk_cache.as_deref(), Some("/tmp/dc"));
    }

    #[test]
    fn defaults_and_errors() {
        let o = RemoteOptions::from_source(&options_from_args(&[])).unwrap();
        assert!(o.upload_local_results && o.accept_cached && !o.local_fallback);
        assert_eq!(
            (o.retries, o.max_connections, o.timeout),
            (5, 100, Duration::from_secs(60))
        );
        assert_eq!(o.download_mode, DownloadMode::TopLevel);
        assert!(!o.has_remote_cache());
        // TLS, grpcs://, --google_credentials, and blake3 are all supported!
        assert!(
            RemoteOptions::from_source(&options_from_args(&["--remote_cache=grpcs://x"])).is_ok()
        );
        assert!(RemoteOptions::from_source(&options_from_args(&[
            "--google_credentials=/k.json",
            "--remote_cache=grpc://x:1",
        ]))
        .is_ok());
        // Combining HTTP cache with gRPC executor is rejected.
        assert!(RemoteOptions::from_source(&options_from_args(&[
            "--remote_cache=http://c:8080",
            "--remote_executor=grpc://e:8980",
        ]))
        .is_err());
        assert!(RemoteOptions::from_source(&options_from_args(&[
            "--remote_download_outputs=bogus"
        ]))
        .is_err());
        assert!(
            RemoteOptions::from_source(&options_from_args(&["--remote_header=novalue"])).is_err()
        );
        assert!(
            RemoteOptions::from_source(&options_from_args(&["--remote_proxy=http://p:1"])).is_err()
        );
        let o =
            RemoteOptions::from_source(&options_from_args(&["--remote_download_minimal"])).unwrap();
        assert_eq!(o.download_mode, DownloadMode::Minimal);
    }

    #[test]
    fn executor_doubles_as_cache_and_shares_channel() {
        let o = RemoteOptions::from_source(&options_from_args(&[
            "--remote_executor=grpc://127.0.0.1:1",
        ]))
        .unwrap();
        assert!(o.has_remote_cache());
        let c = o.build_client("inv").unwrap().unwrap();
        assert!(c.executor.is_some());
        assert!(Arc::ptr_eq(&c.cache, c.executor.as_ref().unwrap()));
    }
}

#[cfg(test)]
mod socket_tests {
    use super::*;
    use crate::fake_server::FakeReapiServer;
    use crate::reapi_client::ActionMeta;

    fn exercise(c: &crate::reapi_client::ReapiClient, s: &FakeReapiServer) {
        let m = ActionMeta::default();
        c.check_compatibility(false).unwrap();
        let blob = vec![7u8; 300_000];
        let d = c.cfg.digest_fn.digest(&blob);
        let mut blobs = std::collections::BTreeMap::new();
        blobs.insert(d.clone(), crate::merkle::BlobSource::Bytes(blob.clone()));
        c.ensure_blobs_present(&blobs, &m).unwrap();
        assert_eq!(c.download_blob(&d, &m).unwrap(), blob);
        assert_eq!(s.get_blob(&d).unwrap(), blob);
    }

    #[test]
    fn grpc_over_real_tcp_loopback() {
        let l = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("SKIPPED: loopback sockets are not permitted in this environment ({e})");
                return;
            }
            Err(e) => panic!("{e}"),
        };
        let port = l.local_addr().unwrap().port();
        if let Err(e) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                eprintln!("SKIPPED: loopback connect not permitted in this environment ({e})");
                return;
            }
            panic!("{e}");
        }
        let s = FakeReapiServer::new();
        s.serve_tcp(l);
        let o = RemoteOptions::from_source(&options_from_args(&[
            &format!("--remote_cache=grpc://127.0.0.1:{port}"),
            "--remote_header=x-api-key=k1",
            "--remote_max_connections=2",
        ]))
        .unwrap();
        let c = o.build_client("inv").unwrap().unwrap();
        exercise(&c, &s);
        assert!(s
            .last_headers("FindMissingBlobs")
            .unwrap()
            .contains(&("x-api-key".to_string(), "k1".to_string())));
        // Connection refused surfaces as UNAVAILABLE after retries.
        let o = RemoteOptions::from_source(&options_from_args(&[
            "--remote_cache=grpc://127.0.0.1:1",
            "--remote_retries=1",
        ]))
        .unwrap();
        let c2 = o.build_client("inv").unwrap().unwrap();
        let e = c2.capabilities().unwrap_err().to_string();
        assert!(e.contains("UNAVAILABLE"), "{e}");
    }

    #[cfg(unix)]
    #[test]
    fn grpc_over_unix_socket_proxy() {
        let path = std::env::temp_dir().join(format!("zel-reapi-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let l = match std::os::unix::net::UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("SKIPPED: unix sockets are not permitted in this environment ({e})");
                return;
            }
            Err(e) => panic!("{e}"),
        };
        if let Err(e) = std::os::unix::net::UnixStream::connect(&path) {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                eprintln!("SKIPPED: unix connect not permitted in this environment ({e})");
                let _ = std::fs::remove_file(&path);
                return;
            }
            panic!("{e}");
        }
        let s = FakeReapiServer::new();
        let s2 = s.clone();
        std::thread::spawn(move || {
            for st in l.incoming().flatten() {
                let (Ok(r), Ok(w)) = (st.try_clone(), st.try_clone()) else {
                    continue;
                };
                s2.serve_transport(crate::transport::Transport {
                    reader: Box::new(r),
                    writer: Box::new(w),
                    shutdown: Box::new(move || {
                        let _ = st.shutdown(std::net::Shutdown::Both);
                    }),
                });
            }
        });
        let o = RemoteOptions::from_source(&options_from_args(&[&format!(
            "--remote_cache=unix:{}",
            path.display()
        )]))
        .unwrap();
        let c = o.build_client("inv").unwrap().unwrap();
        exercise(&c, &s);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn remote_options_client_env_netrc_missing_succeeds_and_malformed_errors() {
        let tmp = std::env::temp_dir().join(format!("zel-remote-netrc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let mut o_missing =
            RemoteOptions::from_source(&options_from_args(&["--remote_cache=grpc://x:1"])).unwrap();
        let mut env_missing = BTreeMap::new();
        env_missing.insert(
            "HOME".into(),
            tmp.join("missing-home").to_string_lossy().into_owned(),
        );
        o_missing.set_client_env(env_missing);
        assert!(o_missing
            .build_credentials_provider(None)
            .unwrap()
            .is_none());

        let bad_netrc = tmp.join("bad.netrc");
        std::fs::write(&bad_netrc, "machine x login\n").unwrap();
        let mut o_bad =
            RemoteOptions::from_source(&options_from_args(&["--remote_cache=grpc://x:1"])).unwrap();
        let mut env_bad = BTreeMap::new();
        env_bad.insert("NETRC".into(), bad_netrc.to_string_lossy().into_owned());
        o_bad.set_client_env(env_bad);
        let err = match o_bad.build_credentials_provider(None) {
            Err(e) => e,
            Ok(_) => panic!("expected malformed client_env NETRC to error"),
        };
        assert!(err.contains("Failed to parse"), "unexpected error: {err}");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn endpoint_userinfo_parsing_basic_auth_redaction_and_normalized_auth_uri() {
        // 1. grpcs://<api-key>@remote.buildbuddy.io
        let (ep, ui) =
            Endpoint::parse_grpc_with_userinfo("grpcs://my-bb-key@remote.buildbuddy.io").unwrap();
        assert_eq!(
            ep,
            Endpoint::Tcp {
                host: "remote.buildbuddy.io".into(),
                port: 443,
                tls: true,
            }
        );
        assert_eq!(ui.as_deref(), Some("my-bb-key"));
        assert_eq!(ep.auth_uri(), "https://remote.buildbuddy.io");
        assert_eq!(
            basic_auth_header_for_userinfo("my-bb-key"),
            Some((
                "authorization".to_string(),
                format!("Basic {}", zel_util::base64_encode(b"my-bb-key:"))
            ))
        );

        // 2. grpcs://user:pass@host:443 and percent-encoded credentials
        let (ep2, ui2) =
            Endpoint::parse_grpc_with_userinfo("grpcs://alice:s3cr3t@host.example:443").unwrap();
        assert_eq!(
            ep2,
            Endpoint::Tcp {
                host: "host.example".into(),
                port: 443,
                tls: true,
            }
        );
        assert_eq!(ui2.as_deref(), Some("alice:s3cr3t"));
        assert_eq!(ep2.auth_uri(), "https://host.example");
        assert_eq!(
            basic_auth_header_for_userinfo("alice:s3cr3t"),
            Some((
                "authorization".to_string(),
                format!("Basic {}", zel_util::base64_encode(b"alice:s3cr3t"))
            ))
        );
        assert_eq!(
            basic_auth_header_for_userinfo("user%40org:p%3Ass"),
            Some((
                "authorization".to_string(),
                format!("Basic {}", zel_util::base64_encode(b"user@org:p:ss"))
            ))
        );

        // 3. https://key@host/prefix REST cache endpoint
        let (ep_http, ui_http) =
            Endpoint::parse_cache_with_userinfo("https://cache-key@cache.example/v1/").unwrap();
        assert_eq!(
            ep_http,
            Endpoint::Http {
                host: "cache.example".into(),
                port: 443,
                prefix: "/v1".into(),
                tls: true,
            }
        );
        assert_eq!(ui_http.as_deref(), Some("cache-key"));
        assert_eq!(ep_http.auth_uri(), "https://cache.example/v1");

        // 4. IPv6 with userinfo and empty userinfo
        let (ep_v6, ui_v6) =
            Endpoint::parse_grpc_with_userinfo("grpcs://v6key@[::1]:8443").unwrap();
        assert_eq!(
            ep_v6,
            Endpoint::Tcp {
                host: "::1".into(),
                port: 8443,
                tls: true,
            }
        );
        assert_eq!(ui_v6.as_deref(), Some("v6key"));
        assert_eq!(ep_v6.auth_uri(), "https://[::1]:8443");

        let (ep_empty_ui, ui_empty) =
            Endpoint::parse_grpc_with_userinfo("grpcs://@remote.buildbuddy.io").unwrap();
        assert_eq!(
            ep_empty_ui,
            Endpoint::Tcp {
                host: "remote.buildbuddy.io".into(),
                port: 443,
                tls: true,
            }
        );
        assert_eq!(ui_empty, None);

        let (ep_q, ui_q) =
            Endpoint::parse_cache_with_userinfo("https://q-key@cache.example.com?foo=bar/baz")
                .unwrap();
        assert_eq!(
            ep_q,
            Endpoint::Http {
                host: "cache.example.com".into(),
                port: 443,
                prefix: String::new(),
                tls: true,
            }
        );
        assert_eq!(ui_q.as_deref(), Some("q-key"));

        // 5. Error messages NEVER leak embedded credentials!
        for bad in [
            "grpcs://super-secret-token@remote.buildbuddy.io:notaport",
            "grpcs://super-secret-token@",
            "ftp://super-secret-token@remote.buildbuddy.io",
            "https://super-secret-token@cache.example:badport/prefix",
        ] {
            let err = Endpoint::parse_cache(bad).unwrap_err();
            assert!(
                !err.contains("super-secret-token"),
                "error leaked secret for '{bad}': {err}"
            );
        }

        // 6. Channel authority, normalized auth_uri, and effective_bytestream_uri_prefix strip userinfo.
        let mut o = RemoteOptions::from_source(&options_from_args(&[
            "--remote_cache=grpcs://my-bb-key@remote.buildbuddy.io",
            "--remote_instance_name=main",
        ]))
        .unwrap();
        o.set_client_env(BTreeMap::new());
        let client = o.build_client("inv").unwrap().unwrap();
        assert_eq!(client.cache.authority(), "remote.buildbuddy.io");
        assert_eq!(client.cache.auth_uri(), "https://remote.buildbuddy.io");
        assert!(!client.cache.has_refreshable_credentials());
        assert_eq!(
            o.effective_bytestream_uri_prefix().as_deref(),
            Some("remote.buildbuddy.io/main")
        );
        let hdrs = client
            .cache
            .credentials()
            .unwrap()
            .get_credentials(client.cache.auth_uri())
            .unwrap();
        assert_eq!(
            hdrs,
            vec![(
                "authorization".to_string(),
                format!("Basic {}", zel_util::base64_encode(b"my-bb-key:"))
            )]
        );

        // Explicit Authorization header suppresses userinfo Basic auth.
        let mut o_override = RemoteOptions::from_source(&options_from_args(&[
            "--remote_cache=grpcs://my-bb-key@remote.buildbuddy.io",
            "--remote_header=Authorization=Bearer explicit-tok",
        ]))
        .unwrap();
        o_override.set_client_env(BTreeMap::new());
        let c_override = o_override.build_client("inv").unwrap().unwrap();
        assert!(c_override.cache.credentials().is_none());

        // Plaintext grpc:// normalizes to http:// in auth_uri.
        let mut o_plain = RemoteOptions::from_source(&options_from_args(&[
            "--remote_cache=grpc://127.0.0.1:8980",
        ]))
        .unwrap();
        o_plain.set_client_env(BTreeMap::new());
        let c_plain = o_plain.build_client("inv").unwrap().unwrap();
        assert_eq!(c_plain.cache.auth_uri(), "http://127.0.0.1:8980");
    }

    #[test]
    fn remote_options_client_env_google_adc() {
        let tmp = std::env::temp_dir().join(format!(
            "zel-remote-adc-{}-{}",
            std::process::id(),
            zel_util::uuid_v4()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let adc = tmp.join("adc.json");
        std::fs::write(&adc, r#"{"access_token":"adc-from-client-env"}"#).unwrap();

        let mut o = RemoteOptions::from_source(&options_from_args(&[
            "--remote_cache=grpcs://remote.example.com",
            "--google_default_credentials",
        ]))
        .unwrap();
        let mut env = BTreeMap::new();
        env.insert(
            "GOOGLE_APPLICATION_CREDENTIALS".into(),
            adc.to_string_lossy().into_owned(),
        );
        o.set_client_env(env);
        let creds = o.build_credentials_provider(None).unwrap().unwrap();
        assert_eq!(
            creds.get_credentials("https://remote.example.com").unwrap(),
            vec![(
                "authorization".to_string(),
                "Bearer adc-from-client-env".to_string()
            )]
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn grpc_loopback_sends_userinfo_basic_auth_and_honors_explicit_header_override() {
        let l = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("SKIPPED: loopback sockets are not permitted ({e})");
                return;
            }
            Err(e) => panic!("{e}"),
        };
        let port = l.local_addr().unwrap().port();
        if let Err(e) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                eprintln!("SKIPPED: loopback connect not permitted ({e})");
                return;
            }
            panic!("{e}");
        }
        let s = FakeReapiServer::new();
        s.serve_tcp(l);

        // 1. URL userinfo sends Basic auth and strips userinfo from :authority.
        let mut o = RemoteOptions::from_source(&options_from_args(&[&format!(
            "--remote_cache=grpc://bb-api-key@127.0.0.1:{port}"
        )]))
        .unwrap();
        o.set_client_env(BTreeMap::new());
        let c = o.build_client("inv").unwrap().unwrap();
        c.check_compatibility(false).unwrap();
        let hdrs = s.last_headers("GetCapabilities").unwrap();
        assert!(
            hdrs.contains(&(
                "authorization".to_string(),
                format!("Basic {}", zel_util::base64_encode(b"bb-api-key:"))
            )),
            "missing Basic auth from userinfo: {hdrs:?}"
        );
        assert!(
            hdrs.iter()
                .all(|(k, v)| k != ":authority" || !v.contains("bb-api-key")),
            ":authority leaked userinfo: {hdrs:?}"
        );

        // 2. Explicit Authorization header overrides userinfo (no duplicate authorization headers).
        let mut o2 = RemoteOptions::from_source(&options_from_args(&[
            &format!("--remote_cache=grpc://bb-api-key@127.0.0.1:{port}"),
            "--remote_header=Authorization=Bearer override-tok",
        ]))
        .unwrap();
        o2.set_client_env(BTreeMap::new());
        let c2 = o2.build_client("inv").unwrap().unwrap();
        c2.check_compatibility(false).unwrap();
        let hdrs2 = s.last_headers("GetCapabilities").unwrap();
        let auth_vals: Vec<&str> = hdrs2
            .iter()
            .filter(|(k, _)| k == "authorization")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(auth_vals, vec!["Bearer override-tok"]);
    }
}
