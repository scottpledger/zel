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
//! (`grpc://`, `unix:`, `http://`), validation with Bazel-style error messages, and channel /
//! client construction.

use crate::digest::DigestFn;
use crate::grpc::{Channel, RetryPolicy};
use crate::hpack::Header;
use crate::reapi_client::{ReapiClient, ReapiConfig};
use crate::transport::{Connector, TcpConnector};
use std::collections::{BTreeMap, HashMap};
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
    /// Download only outputs of top-level targets (and test outputs).
    #[default]
    TopLevel,
    /// Download as few outputs as possible.
    Minimal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Tcp { host: String, port: u16, tls: bool },
    Unix(String),
    /// Plain HTTP/1.1 cache (`--remote_cache=http://host:port/prefix`).
    Http { host: String, port: u16, prefix: String },
}

impl Endpoint {
    /// Parses a `--remote_cache` / `--remote_executor` value.
    pub fn parse(s: &str, tls_enabled: bool) -> Result<Endpoint, String> {
        if let Some(p) = s.strip_prefix("unix:") {
            return Ok(Endpoint::Unix(p.trim_start_matches("//").to_string()));
        }
        let (scheme, rest) = match s.split_once("://") {
            Some((sc, r)) => (sc.to_ascii_lowercase(), r),
            None => (String::new(), s),
        };
        let split_host_port = |hp: &str, default: u16| -> Result<(String, u16), String> {
            let hp = hp.trim_end_matches('/');
            if let Some(r) = hp.strip_prefix('[') {
                let (h, p) = r.split_once(']').ok_or("bad IPv6 address")?;
                let port = match p.strip_prefix(':') {
                    Some(p) => p.parse().map_err(|_| format!("bad port in {s}"))?,
                    None => default,
                };
                return Ok((h.to_string(), port));
            }
            match hp.rsplit_once(':') {
                Some((h, p)) => Ok((h.to_string(), p.parse().map_err(|_| format!("bad port in {s}"))?)),
                None => Ok((hp.to_string(), default)),
            }
        };
        match scheme.as_str() {
            "grpc" | "" => {
                let tls = scheme.is_empty() && tls_enabled;
                let hostport = rest.split('/').next().unwrap_or("");
                if hostport.is_empty() {
                    return Err(format!("Invalid remote endpoint: {s}"));
                }
                let (host, port) = split_host_port(hostport, if tls { 443 } else { 80 })?;
                Ok(Endpoint::Tcp { host, port, tls })
            }
            "grpcs" => {
                let hostport = rest.split('/').next().unwrap_or("");
                let (host, port) = split_host_port(hostport, 443)?;
                Ok(Endpoint::Tcp { host, port, tls: true })
            }
            "http" => {
                let (hostport, prefix) = match rest.split_once('/') {
                    Some((h, p)) => (h, format!("/{}", p.trim_end_matches('/'))),
                    None => (rest, String::new()),
                };
                let (host, port) = split_host_port(hostport, 80)?;
                Ok(Endpoint::Http { host, port, prefix: if prefix == "/" { String::new() } else { prefix } })
            }
            "https" => Ok(Endpoint::Tcp { host: rest.split('/').next().unwrap_or("").to_string(), port: 443, tls: true }),
            other => Err(format!("Unsupported scheme '{other}' in remote endpoint '{s}'")),
        }
    }

    pub fn connector(&self, timeout: Duration) -> Result<Arc<dyn Connector>, String> {
        match self {
            Endpoint::Tcp { tls: true, .. } => Err(TLS_UNSUPPORTED.to_string()),
            Endpoint::Tcp { host, port, .. } => Ok(Arc::new(TcpConnector { host: host.clone(), port: *port, connect_timeout: timeout })),
            #[cfg(unix)]
            Endpoint::Unix(p) => Ok(Arc::new(crate::transport::UnixConnector { path: p.clone() })),
            #[cfg(not(unix))]
            Endpoint::Unix(_) => Err("unix: endpoints are not supported on this platform".into()),
            Endpoint::Http { .. } => Err("--remote_cache=http://... (the HTTP/1.1 REST cache protocol) is not supported by this build; \
use a gRPC endpoint (grpc://host:port or unix:/path)"
                .into()),
        }
    }
}

pub const TLS_UNSUPPORTED: &str = "TLS (grpcs://, https://, --tls_enabled) is not supported by this build: it contains no TLS \
implementation and may not use external crates. Use grpc:// (plaintext HTTP/2) or unix: endpoints, \
or a local TLS-terminating proxy.";

/// Parsed `--remote_*` options.
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
    pub max_connections: usize,
    pub local_fallback: bool,
    pub cache_compression: bool,
    pub google_credentials: Option<String>,
    pub remote_proxy: Option<String>,
    pub digest_fn: DigestFn,
    pub default_exec_properties: BTreeMap<String, String>,
    pub tls_enabled: bool,
    pub verify_downloads: bool,
    pub download_regex: Vec<String>,
    pub credential_helper: Option<String>,
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
            local_fallback: false,
            cache_compression: false,
            google_credentials: None,
            remote_proxy: None,
            digest_fn: DigestFn::Sha256,
            default_exec_properties: BTreeMap::new(),
            tls_enabled: false,
            verify_downloads: true,
            download_regex: vec![],
            credential_helper: None,
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
    let v: f64 = num.trim().parse().map_err(|_| format!("Invalid duration: '{s}'"))?;
    if v < 0.0 {
        return Err(format!("Invalid duration: '{s}'"));
    }
    Ok(Duration::from_millis((v * mult_ms) as u64))
}

fn parse_bool(flag: &str, v: &str) -> Result<bool, String> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(format!("While parsing option --{flag}: '{v}' is not a boolean")),
    }
}

fn parse_header(flag: &str, v: &str) -> Result<Header, String> {
    let (n, val) = v
        .split_once('=')
        .ok_or_else(|| format!("Invalid value for --{flag}: '{v}' (expected Name=Value)"))?;
    if n.is_empty() {
        return Err(format!("Invalid value for --{flag}: '{v}' (empty header name)"));
    }
    Ok((n.to_string(), val.to_string()))
}

impl RemoteOptions {
    pub fn from_source(src: &dyn OptionSource) -> Result<Self, String> {
        let mut o = Self::default();
        let nonempty = |s: Option<String>| s.filter(|v| !v.is_empty());
        o.remote_cache = nonempty(src.last("remote_cache"));
        o.remote_executor = nonempty(src.last("remote_executor"));
        if let Some(v) = src.last("remote_instance_name") {
            o.remote_instance_name = v;
        }
        o.disk_cache = nonempty(src.last("disk_cache"));
        let b = |flag: &str, d: bool| -> Result<bool, String> {
            match src.last(flag) {
                Some(v) => parse_bool(flag, &v),
                None => Ok(d),
            }
        };
        o.upload_local_results = b("remote_upload_local_results", true)?;
        o.accept_cached = b("remote_accept_cached", true)?;
        o.local_fallback = b("remote_local_fallback", false)?;
        o.cache_compression = b("remote_cache_compression", false)? || b("experimental_remote_cache_compression", false)?;
        o.tls_enabled = b("tls_enabled", false)?;
        o.verify_downloads = b("remote_verify_downloads", true)?;
        // Download mode: legacy shorthands first, then the explicit flag (last one wins).
        let mut mode = DownloadMode::TopLevel;
        for (flag, m) in [
            ("remote_download_all", DownloadMode::All),
            ("remote_download_toplevel", DownloadMode::TopLevel),
            ("remote_download_minimal", DownloadMode::Minimal),
            ("experimental_remote_download_toplevel", DownloadMode::TopLevel),
            ("experimental_remote_download_minimal", DownloadMode::Minimal),
        ] {
            if let Some(v) = src.last(flag) {
                if parse_bool(flag, &v)? {
                    mode = m;
                }
            }
        }
        if let Some(v) = src.last("remote_download_outputs").or_else(|| src.last("experimental_remote_download_outputs")) {
            mode = match v.as_str() {
                "all" => DownloadMode::All,
                "minimal" => DownloadMode::Minimal,
                "toplevel" => DownloadMode::TopLevel,
                other => {
                    return Err(format!(
                        "While parsing option --remote_download_outputs={other}: Invalid value '{other}': must be one of all, minimal, toplevel"
                    ))
                }
            };
        }
        o.download_mode = mode;
        for v in src.all("remote_header") {
            o.headers.push(parse_header("remote_header", &v)?);
        }
        for v in src.all("remote_cache_header") {
            o.cache_headers.push(parse_header("remote_cache_header", &v)?);
        }
        for v in src.all("remote_exec_header") {
            o.exec_headers.push(parse_header("remote_exec_header", &v)?);
        }
        if let Some(v) = src.last("remote_timeout") {
            o.timeout = parse_duration(&v)?;
        }
        if let Some(v) = src.last("remote_retries").or_else(|| src.last("experimental_remote_retry_max_attempts")) {
            o.retries = v.parse().map_err(|_| format!("While parsing option --remote_retries: '{v}' is not an int"))?;
        }
        if let Some(v) = src.last("remote_retry_max_delay") {
            o.retry_max_delay = parse_duration(&v)?;
        }
        if let Some(v) = src.last("remote_max_connections") {
            o.max_connections = v.parse().map_err(|_| format!("While parsing option --remote_max_connections: '{v}' is not an int"))?;
        }
        o.google_credentials = nonempty(src.last("google_credentials"));
        o.remote_proxy = nonempty(src.last("remote_proxy")).or_else(|| nonempty(src.last("remote_cache_proxy")));
        o.credential_helper = nonempty(src.last("credential_helper"));
        if let Some(v) = src.last("digest_function") {
            o.digest_fn = DigestFn::parse(&v)?;
        }
        for v in src.all("remote_default_exec_properties") {
            let (k, val) = v
                .split_once('=')
                .ok_or_else(|| format!("Invalid value for --remote_default_exec_properties: '{v}' (expected name=value)"))?;
            o.default_exec_properties.insert(k.to_string(), val.to_string());
        }
        o.download_regex = src.all("remote_download_regex");
        o.validate()?;
        Ok(o)
    }

    /// Cross-flag validation mirroring `RemoteModule.beforeCommand` / `RemoteOptions`.
    pub fn validate(&self) -> Result<(), String> {
        if self.remote_executor.is_some() && self.remote_cache.is_none() && self.remote_instance_name.is_empty() {
            // allowed: the executor doubles as the cache
        }
        for ep in [&self.remote_cache, &self.remote_executor].into_iter().flatten() {
            if let Endpoint::Tcp { tls: true, .. } = Endpoint::parse(ep, self.tls_enabled)? {
                return Err(format!("Invalid remote endpoint '{ep}': {TLS_UNSUPPORTED}"));
            }
        }
        if self.max_connections == 0 {
            return Err("--remote_max_connections must be positive".into());
        }
        if let (Some(g), true) = (&self.google_credentials, self.has_remote_cache()) {
            // Using Google service-account credentials requires an OAuth2 token exchange over
            // HTTPS, which this build cannot perform.
            return Err(format!(
                "--google_credentials={g} is not supported: obtaining OAuth2 access tokens requires TLS. \
Provide a token explicitly with --remote_header=Authorization=Bearer <token> or use --credential_helper."
            ));
        }
        if let Some(p) = &self.remote_proxy {
            if !p.starts_with("unix:") {
                return Err(format!(
                    "--remote_proxy={p}: only unix: proxies are supported (as in Bazel)"
                ));
            }
        }
        Ok(())
    }

    pub fn has_remote_cache(&self) -> bool {
        self.remote_cache.is_some() || self.remote_executor.is_some()
    }

    pub fn retry_policy(&self) -> RetryPolicy {
        RetryPolicy { max_retries: self.retries, max_delay: self.retry_max_delay, ..Default::default() }
    }

    pub fn reapi_config(&self, tool_invocation_id: &str) -> ReapiConfig {
        ReapiConfig {
            instance_name: self.remote_instance_name.clone(),
            digest_fn: self.digest_fn,
            timeout: self.timeout,
            retry: self.retry_policy(),
            tool_invocation_id: tool_invocation_id.to_string(),
            correlated_invocations_id: tool_invocation_id.to_string(),
        }
    }

    fn channel_for(&self, endpoint: &str, extra: &[Header]) -> Result<Arc<Channel>, String> {
        let ep = match &self.remote_proxy {
            Some(p) => Endpoint::parse(p, false)?,
            None => Endpoint::parse(endpoint, self.tls_enabled)?,
        };
        let connector = ep.connector(self.timeout)?;
        let mut headers = self.headers.clone();
        headers.extend(extra.iter().cloned());
        if let Some(h) = &self.credential_helper {
            headers.extend(crate::credentials::run_credential_helper(h, endpoint)?);
        }
        Ok(Arc::new(Channel::new(connector, self.max_connections, headers)))
    }

    /// Builds the REAPI client for `--remote_cache` / `--remote_executor` (the executor also
    /// serves as the cache if no separate cache is given).
    pub fn build_client(&self, tool_invocation_id: &str) -> Result<Option<ReapiClient>, String> {
        let cache_ep = self.remote_cache.clone().or_else(|| self.remote_executor.clone());
        let Some(cache_ep) = cache_ep else { return Ok(None) };
        let cache = self.channel_for(&cache_ep, &self.cache_headers)?;
        let exec = match &self.remote_executor {
            Some(e) if Some(e) == self.remote_cache.as_ref() || self.remote_cache.is_none() => {
                Some(self.channel_for(e, &self.exec_headers)?)
            }
            Some(e) => Some(self.channel_for(e, &self.exec_headers)?),
            None => None,
        };
        Ok(Some(ReapiClient::new(cache, exec, self.reapi_config(tool_invocation_id))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_parsing() {
        assert_eq!(Endpoint::parse("grpc://host:8980", false).unwrap(), Endpoint::Tcp { host: "host".into(), port: 8980, tls: false });
        assert_eq!(Endpoint::parse("host:1", false).unwrap(), Endpoint::Tcp { host: "host".into(), port: 1, tls: false });
        assert_eq!(Endpoint::parse("host", true).unwrap(), Endpoint::Tcp { host: "host".into(), port: 443, tls: true });
        assert_eq!(Endpoint::parse("grpcs://h", false).unwrap(), Endpoint::Tcp { host: "h".into(), port: 443, tls: true });
        assert_eq!(Endpoint::parse("unix:/tmp/s.sock", false).unwrap(), Endpoint::Unix("/tmp/s.sock".into()));
        assert_eq!(Endpoint::parse("http://c:9090/cache/", false).unwrap(), Endpoint::Http { host: "c".into(), port: 9090, prefix: "/cache".into() });
        assert_eq!(Endpoint::parse("grpc://[::1]:50051", false).unwrap(), Endpoint::Tcp { host: "::1".into(), port: 50051, tls: false });
        assert!(Endpoint::parse("ftp://x", false).is_err());
        assert!(Endpoint::parse("grpc://", false).is_err());
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("60").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
        assert_eq!(parse_duration("1.5s").unwrap(), Duration::from_millis(1500));
        assert!(parse_duration("abc").is_err());
    }

    #[test]
    fn full_flag_parse() {
        let src = options_from_args(&[
            "--remote_cache=grpc://c:1",
            "--remote_executor=grpc://e:2",
            "--remote_instance_name=main",
            "--disk_cache=/tmp/dc",
            "--noremote_upload_local_results",
            "--remote_accept_cached=false",
            "--remote_download_outputs=minimal",
            "--remote_header=A=1",
            "--remote_header=B=x=y",
            "--remote_timeout=2m",
            "--remote_retries=3",
            "--remote_max_connections=7",
            "--remote_local_fallback",
            "--remote_default_exec_properties=OSFamily=linux",
            "--digest_function=md5",
        ]);
        let o = RemoteOptions::from_source(&src).unwrap();
        assert_eq!(o.remote_instance_name, "main");
        assert!(!o.upload_local_results && !o.accept_cached && o.local_fallback);
        assert_eq!(o.download_mode, DownloadMode::Minimal);
        assert_eq!(o.headers, vec![("A".to_string(), "1".to_string()), ("B".to_string(), "x=y".to_string())]);
        assert_eq!(o.timeout, Duration::from_secs(120));
        assert_eq!((o.retries, o.max_connections), (3, 7));
        assert_eq!(o.digest_fn, DigestFn::Md5);
        assert_eq!(o.default_exec_properties["OSFamily"], "linux");
        assert_eq!(o.disk_cache.as_deref(), Some("/tmp/dc"));
    }

    #[test]
    fn defaults_and_errors() {
        let o = RemoteOptions::from_source(&options_from_args(&[])).unwrap();
        assert!(o.upload_local_results && o.accept_cached && !o.local_fallback);
        assert_eq!((o.retries, o.max_connections, o.timeout), (5, 100, Duration::from_secs(60)));
        assert_eq!(o.download_mode, DownloadMode::TopLevel);
        assert!(!o.has_remote_cache());
        let e = RemoteOptions::from_source(&options_from_args(&["--remote_cache=grpcs://x"])).unwrap_err();
        assert!(e.contains("TLS"));
        let e = RemoteOptions::from_source(&options_from_args(&["--google_credentials=/k.json", "--remote_cache=grpc://x:1"])).unwrap_err();
        assert!(RemoteOptions::from_source(&options_from_args(&["--google_credentials=/k.json"])).is_ok());
        assert!(e.contains("google_credentials"));
        assert!(RemoteOptions::from_source(&options_from_args(&["--remote_download_outputs=bogus"])).is_err());
        assert!(RemoteOptions::from_source(&options_from_args(&["--remote_header=novalue"])).is_err());
        assert!(RemoteOptions::from_source(&options_from_args(&["--digest_function=blake3"])).is_err());
        assert!(RemoteOptions::from_source(&options_from_args(&["--remote_proxy=http://p:1"])).is_err());
        let o = RemoteOptions::from_source(&options_from_args(&["--remote_download_minimal"])).unwrap();
        assert_eq!(o.download_mode, DownloadMode::Minimal);
    }

    #[test]
    fn executor_doubles_as_cache() {
        let o = RemoteOptions::from_source(&options_from_args(&["--remote_executor=grpc://127.0.0.1:1"])).unwrap();
        assert!(o.has_remote_cache());
        let c = o.build_client("inv").unwrap().unwrap();
        assert!(c.executor.is_some());
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
        let o = RemoteOptions::from_source(&options_from_args(&[&format!("--remote_cache=grpc://127.0.0.1:{port}"), "--remote_header=x-api-key=k1", "--remote_max_connections=2"])).unwrap();
        let c = o.build_client("inv").unwrap().unwrap();
        exercise(&c, &s);
        assert!(s.last_headers("FindMissingBlobs").unwrap().contains(&("x-api-key".to_string(), "k1".to_string())));
        // Connection refused surfaces as UNAVAILABLE after retries.
        let o = RemoteOptions::from_source(&options_from_args(&["--remote_cache=grpc://127.0.0.1:1", "--remote_retries=1"])).unwrap();
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
                let (Ok(r), Ok(w)) = (st.try_clone(), st.try_clone()) else { continue };
                s2.serve_transport(crate::transport::Transport {
                    reader: Box::new(r),
                    writer: Box::new(w),
                    shutdown: Box::new(move || {
                        let _ = st.shutdown(std::net::Shutdown::Both);
                    }),
                });
            }
        });
        let o = RemoteOptions::from_source(&options_from_args(&[&format!("--remote_cache=unix:{}", path.display())])).unwrap();
        let c = o.build_client("inv").unwrap().unwrap();
        exercise(&c, &s);
        let _ = std::fs::remove_file(&path);
    }
}
