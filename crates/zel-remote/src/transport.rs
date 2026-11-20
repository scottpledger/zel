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

//! Byte transports for HTTP/2 and HTTP/1.1: plaintext TCP, TLS / mTLS (`grpcs://`, `https://`,
//! `--tls_certificate`, `--tls_client_certificate`, `--tls_client_key`,
//! `--tls_authority_override`), Unix domain sockets (`unix:...`), and an in-memory duplex pipe
//! (used by in-process fake servers).

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, BufRead, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A connected byte stream split into halves plus a shutdown hook.
pub struct Transport {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    pub shutdown: Box<dyn Fn() + Send + Sync>,
}

/// Something that can open new transports to a fixed endpoint.
pub trait Connector: Send + Sync {
    fn connect(&self) -> io::Result<Transport>;
    /// Opens a transport applying `timeout` as the connect and socket read/write idle timeout
    /// when non-zero. Connectors without kernel socket timeouts fall back to [`Self::connect`]
    /// and rely on caller-level idle watchdogs.
    fn connect_with_timeout(&self, _timeout: Duration) -> io::Result<Transport> {
        self.connect()
    }
    /// Opens a transport applying `timeout` only to connection establishment (TCP connect / TLS
    /// handshake), without setting socket read/write idle timeouts. Persistent pooled HTTP/2
    /// channels use this so idle connections are not torn down by a single RPC's short timeout.
    fn connect_with_dial_timeout(&self, _timeout: Duration) -> io::Result<Transport> {
        self.connect()
    }
    /// Value used for the HTTP/2 `:authority` pseudo-header (or HTTP/1.1 `Host`).
    fn authority(&self) -> String;
}

fn effective_dial_timeout(configured: Duration, caller: Duration) -> Duration {
    match (configured.is_zero(), caller.is_zero()) {
        (true, _) => caller,
        (_, true) => configured,
        (false, false) => configured.min(caller),
    }
}

/// Plaintext TCP.
pub struct TcpConnector {
    pub host: String,
    pub port: u16,
    pub connect_timeout: Duration,
    pub authority_override: Option<String>,
}

fn connect_addrs_with<I, F>(
    addrs: I,
    connect_timeout: Duration,
    io_timeout: Option<Duration>,
    mut dial: F,
) -> io::Result<Transport>
where
    I: IntoIterator<Item = SocketAddr>,
    F: FnMut(&SocketAddr, Option<Duration>) -> io::Result<TcpStream>,
{
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no addresses resolved");
    let dial_timeout = (!connect_timeout.is_zero()).then_some(connect_timeout);
    let active_io_timeout = io_timeout.filter(|t| !t.is_zero());
    for addr in addrs {
        match dial(&addr, dial_timeout) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                if let Some(t) = active_io_timeout {
                    let _ = s.set_read_timeout(Some(t));
                    let _ = s.set_write_timeout(Some(t));
                }
                let r = s.try_clone()?;
                let w = s.try_clone()?;
                if let Some(t) = active_io_timeout {
                    let _ = r.set_read_timeout(Some(t));
                    let _ = w.set_write_timeout(Some(t));
                }
                return Ok(Transport {
                    reader: Box::new(r),
                    writer: Box::new(w),
                    shutdown: Box::new(move || {
                        let _ = s.shutdown(Shutdown::Both);
                    }),
                });
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn connect_addrs<I>(
    addrs: I,
    connect_timeout: Duration,
    io_timeout: Option<Duration>,
) -> io::Result<Transport>
where
    I: IntoIterator<Item = SocketAddr>,
{
    connect_addrs_with(
        addrs,
        connect_timeout,
        io_timeout,
        |addr, timeout| match timeout {
            Some(dur) => TcpStream::connect_timeout(addr, dur),
            None => TcpStream::connect(*addr),
        },
    )
}

fn strip_ipv6_brackets(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host)
}

impl TcpConnector {
    pub fn new(host: String, port: u16, connect_timeout: Duration) -> Self {
        Self {
            host,
            port,
            connect_timeout,
            authority_override: None,
        }
    }

    fn connect_internal(
        &self,
        connect_timeout: Duration,
        io_timeout: Option<Duration>,
    ) -> io::Result<Transport> {
        let lookup_host = strip_ipv6_brackets(&self.host);
        let addrs = (lookup_host, self.port).to_socket_addrs()?;
        connect_addrs(addrs, connect_timeout, io_timeout)
    }
}

impl Connector for TcpConnector {
    fn connect(&self) -> io::Result<Transport> {
        self.connect_internal(self.connect_timeout, None)
    }

    fn connect_with_timeout(&self, timeout: Duration) -> io::Result<Transport> {
        self.connect_internal(timeout, Some(timeout))
    }

    fn connect_with_dial_timeout(&self, timeout: Duration) -> io::Result<Transport> {
        let effective = effective_dial_timeout(self.connect_timeout, timeout);
        self.connect_internal(effective, None)
    }

    fn authority(&self) -> String {
        if let Some(ov) = &self.authority_override {
            if !ov.is_empty() {
                return ov.clone();
            }
        }
        let bare = strip_ipv6_brackets(&self.host);
        if bare.contains(':') {
            format!("[{bare}]:{}", self.port)
        } else {
            format!("{bare}:{}", self.port)
        }
    }
}

/// TLS / mTLS client configuration mirroring Bazel's `AuthAndTLSOptions` + `GoogleAuthUtils`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TlsConfig {
    pub ca_certificate: Option<PathBuf>,
    pub client_certificate: Option<PathBuf>,
    pub client_key: Option<PathBuf>,
    pub authority_override: Option<String>,
}

impl TlsConfig {
    /// Validates TLS certificate and client key paths before connecting.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(ca) = &self.ca_certificate {
            if !ca.is_file() {
                return Err(format!(
                    "TLS certificate file does not exist: {}",
                    ca.display()
                ));
            }
        }
        match (&self.client_certificate, &self.client_key) {
            (Some(cert), Some(key)) => {
                if !cert.is_file() {
                    return Err(format!(
                        "TLS client certificate file does not exist: {}",
                        cert.display()
                    ));
                }
                if !key.is_file() {
                    return Err(format!(
                        "TLS client key file does not exist: {}",
                        key.display()
                    ));
                }
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err(
                    "Both --tls_client_certificate and --tls_client_key must be specified together"
                        .to_string(),
                );
            }
            (None, None) => {}
        }
        Ok(())
    }
}

/// TLS / mTLS transport backed by system `openssl s_client` with strict certificate verification,
/// SNI, hostname/IP verification, optional custom CA bundle isolation, mTLS client cert/key, and
/// ALPN (`h2` for gRPC, HTTP/1.1 when unset).
pub struct TlsConnector {
    pub host: String,
    pub port: u16,
    pub connect_timeout: Duration,
    pub alpn: Option<String>,
    pub tls: TlsConfig,
    /// Optional Unix socket proxy (`--remote_proxy=unix:...`) when tunneling TLS over a proxy.
    pub unix_proxy: Option<String>,
}

/// Detected capabilities of an `openssl` CLI binary's `s_client` subcommand.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct OpenSslCapabilities {
    version: String,
    verify_hostname: bool,
    verify_ip: bool,
    unix_socket: bool,
    verify_return_error: bool,
    servername: bool,
    alpn: bool,
    quiet: bool,
    brief: bool,
    no_capath: bool,
    no_castore: bool,
}

impl OpenSslCapabilities {
    fn supports_strict_tls(&self) -> bool {
        self.verify_hostname && self.verify_ip && self.verify_return_error
    }

    fn missing_strict_flags(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if !self.verify_hostname {
            missing.push("-verify_hostname");
        }
        if !self.verify_ip {
            missing.push("-verify_ip");
        }
        if !self.verify_return_error {
            missing.push("-verify_return_error");
        }
        missing
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenSslBinary {
    path: PathBuf,
    caps: OpenSslCapabilities,
}

fn help_has_option(help_text: &str, opt: &str) -> bool {
    for line in help_text.lines() {
        let Some(first) = line.split_whitespace().next() else {
            continue;
        };
        let token = first
            .trim_start_matches('[')
            .trim_end_matches(&[']', ',', ';'][..]);
        if token == opt {
            return true;
        }
    }
    false
}

fn parse_s_client_capabilities(version_text: &str, help_text: &str) -> OpenSslCapabilities {
    let version = version_text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();
    OpenSslCapabilities {
        version,
        verify_hostname: help_has_option(help_text, "-verify_hostname"),
        verify_ip: help_has_option(help_text, "-verify_ip"),
        unix_socket: help_has_option(help_text, "-unix"),
        verify_return_error: help_has_option(help_text, "-verify_return_error"),
        servername: help_has_option(help_text, "-servername"),
        alpn: help_has_option(help_text, "-alpn"),
        quiet: help_has_option(help_text, "-quiet"),
        brief: help_has_option(help_text, "-brief"),
        no_capath: help_has_option(help_text, "-no-CApath"),
        no_castore: help_has_option(help_text, "-no-CAstore"),
    }
}

fn default_openssl_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from("openssl")];
    #[cfg(unix)]
    {
        for p in [
            "/opt/homebrew/opt/openssl@3/bin/openssl",
            "/opt/homebrew/opt/openssl/bin/openssl",
            "/opt/homebrew/opt/openssl@1.1/bin/openssl",
            "/opt/homebrew/bin/openssl",
            "/usr/local/opt/openssl@3/bin/openssl",
            "/usr/local/opt/openssl/bin/openssl",
            "/usr/local/opt/openssl@1.1/bin/openssl",
            "/usr/local/bin/openssl",
            "/opt/local/bin/openssl",
            "/usr/bin/openssl",
        ] {
            candidates.push(PathBuf::from(p));
        }
    }
    #[cfg(windows)]
    {
        for p in [
            r"C:\Program Files\OpenSSL-Win64\bin\openssl.exe",
            r"C:\Program Files\OpenSSL\bin\openssl.exe",
            r"C:\Program Files\Git\mingw64\bin\openssl.exe",
            r"C:\Program Files\Git\usr\bin\openssl.exe",
        ] {
            candidates.push(PathBuf::from(p));
        }
    }
    candidates
}

fn run_probe_cmd(path: &Path, args: &[&str], timeout: Duration) -> Option<(ExitStatus, String)> {
    let mut child = Command::new(path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;
    let out_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = out_t.join();
                    let _ = err_t.join();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out_t.join();
                let _ = err_t.join();
                return None;
            }
        }
    };
    let out_bytes = out_t.join().unwrap_or_default();
    let err_bytes = err_t.join().unwrap_or_default();
    let mut text = String::from_utf8_lossy(&out_bytes).into_owned();
    if !err_bytes.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&err_bytes));
    }
    Some((status, text))
}

fn probe_openssl_binary(path: &Path) -> Option<OpenSslBinary> {
    if path.is_absolute() && !path.is_file() {
        return None;
    }
    let probe_timeout = Duration::from_secs(5);
    let (version_status, version_text) = run_probe_cmd(path, &["version"], probe_timeout)?;
    if !version_status.success() && version_text.trim().is_empty() {
        return None;
    }
    // Note: LibreSSL and some OpenSSL versions exit non-zero for `s_client -help` while writing
    // usage to stderr, so capture both stdout and stderr regardless of exit status.
    let (_, help_text) = run_probe_cmd(path, &["s_client", "-help"], probe_timeout)?;
    let caps = parse_s_client_capabilities(&version_text, &help_text);
    Some(OpenSslBinary {
        path: path.to_path_buf(),
        caps,
    })
}

fn select_openssl_with<F>(candidates: &[PathBuf], mut probe: F) -> Result<OpenSslBinary, String>
where
    F: FnMut(&Path) -> Option<OpenSslBinary>,
{
    let mut best_strict_without_unix: Option<OpenSslBinary> = None;
    let mut first_unsupported: Option<OpenSslBinary> = None;

    for cand in candidates {
        let Some(bin) = probe(cand) else {
            continue;
        };
        if bin.caps.supports_strict_tls() {
            if cfg!(not(unix)) || bin.caps.unix_socket {
                return Ok(bin);
            }
            if best_strict_without_unix.is_none() {
                best_strict_without_unix = Some(bin);
            }
        } else if first_unsupported.is_none() {
            first_unsupported = Some(bin);
        }
    }

    if let Some(bin) = best_strict_without_unix {
        return Ok(bin);
    }

    if let Some(bin) = first_unsupported {
        let missing = bin.caps.missing_strict_flags().join("/");
        let detected = if bin.caps.version.is_empty() {
            bin.path.display().to_string()
        } else {
            format!("{} (at {})", bin.caps.version, bin.path.display())
        };
        return Err(format!(
            "TLS requires OpenSSL 1.1+ or 3+ with s_client -verify_hostname/-verify_ip support, \
             but only found {detected}, which does not support {missing}. \
             Install OpenSSL 3 (e.g. `brew install openssl@3` on macOS) and ensure it is installed or on PATH."
        ));
    }

    Err(
        "failed to find an openssl binary for TLS connection; install OpenSSL 1.1+ or 3+ \
         (e.g. `brew install openssl@3` on macOS) and ensure it is on PATH"
            .to_string(),
    )
}

fn resolve_default_openssl() -> Result<&'static OpenSslBinary, String> {
    static OPENSSL_BIN: OnceLock<Result<OpenSslBinary, String>> = OnceLock::new();
    match OPENSSL_BIN
        .get_or_init(|| select_openssl_with(&default_openssl_candidates(), probe_openssl_binary))
    {
        Ok(bin) => Ok(bin),
        Err(msg) => Err(msg.clone()),
    }
}

const MAX_CAPTURED_STDERR_LINES: usize = 64;

#[derive(Default)]
struct StderrState {
    established: bool,
    eof: bool,
    pre_lines: Vec<String>,
    post_error_lines: Vec<String>,
}

fn is_brief_info_line(line: &str) -> bool {
    let t = line.trim();
    t == "CONNECTION ESTABLISHED"
        || t == "Can't use SSL_get_servername"
        || t.starts_with("Protocol version:")
        || t.starts_with("Ciphersuite:")
        || t.starts_with("Peer certificate:")
        || t.starts_with("Hash used:")
        || t.starts_with("Signature type:")
        || t.starts_with("Verification: OK")
        || t.starts_with("Verified peername:")
        || t.starts_with("Server Temp Key:")
        || t.starts_with("Supported Elliptic Curve Point Formats:")
        || t.starts_with("Negotiated protocol:")
        || t.starts_with("ALPN protocol:")
        || t.starts_with("Reused,")
        || t.starts_with("depth=")
        || t.starts_with("verify return:1")
}

fn classify_tls_error_kind(detail: &str) -> io::ErrorKind {
    let lower = detail.to_ascii_lowercase();
    if lower.contains("connection refused") || lower.contains("actively refused") {
        io::ErrorKind::ConnectionRefused
    } else if lower.contains("timed out") || lower.contains("timeout") {
        io::ErrorKind::TimedOut
    } else if lower.contains("no such file or directory") {
        io::ErrorKind::NotFound
    } else if lower.contains("permission denied") {
        io::ErrorKind::PermissionDenied
    } else {
        io::ErrorKind::Other
    }
}

struct TlsProcState {
    child: Mutex<Child>,
    stderr: Arc<(Mutex<StderrState>, Condvar)>,
    killed: AtomicBool,
}

impl TlsProcState {
    fn wait_exit_bounded(&self, max_wait: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + max_wait;
        loop {
            if let Ok(mut child) = self.child.lock() {
                if let Ok(Some(status)) = child.try_wait() {
                    return Some(status);
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn format_stderr_for_status(&self, status: ExitStatus) -> String {
        let (lock, _) = &*self.stderr;
        let st = lock.lock().unwrap();
        let msg = if !st.established && !st.pre_lines.is_empty() {
            st.pre_lines.join("\n")
        } else if !st.post_error_lines.is_empty() {
            st.post_error_lines.join("\n")
        } else if !st.pre_lines.is_empty() {
            st.pre_lines.join("\n")
        } else {
            String::new()
        };
        if !msg.is_empty() {
            msg
        } else {
            format!("TLS handshake/connection failed ({status})")
        }
    }

    fn wait_error_detail(&self, max_wait: Duration) -> Option<String> {
        if self.killed.load(Ordering::SeqCst) {
            return None;
        }
        let deadline = Instant::now() + max_wait;
        {
            let (lock, cv) = &*self.stderr;
            let mut st = lock.lock().unwrap();
            while !st.eof {
                if self.killed.load(Ordering::SeqCst) {
                    return None;
                }
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let (next_st, _) = cv.wait_timeout(st, deadline - now).unwrap();
                st = next_st;
            }
        }
        if self.killed.load(Ordering::SeqCst) {
            return None;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let status = self.wait_exit_bounded(remaining)?;
        if status.success() {
            return None;
        }
        Some(self.format_stderr_for_status(status))
    }

    fn kill(&self) {
        self.killed.store(true, Ordering::SeqCst);
        {
            let (lock, cv) = &*self.stderr;
            let _g = lock.lock();
            cv.notify_all();
        }
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for TlsProcState {
    fn drop(&mut self) {
        self.kill();
    }
}

struct TlsReader {
    stdout: ChildStdout,
    state: Arc<TlsProcState>,
}

impl Read for TlsReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.stdout.read(buf) {
            Ok(0) => {
                if let Some(detail) = self.state.wait_error_detail(Duration::from_millis(200)) {
                    return Err(io::Error::new(classify_tls_error_kind(&detail), detail));
                }
                Ok(0)
            }
            Ok(n) => Ok(n),
            Err(e) => {
                if let Some(detail) = self.state.wait_error_detail(Duration::from_millis(100)) {
                    return Err(io::Error::new(e.kind(), detail));
                }
                Err(e)
            }
        }
    }
}

struct TlsWriter {
    stdin: Option<ChildStdin>,
    state: Arc<TlsProcState>,
}

impl Write for TlsWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "TLS stream closed",
            ));
        };
        match stdin.write(buf) {
            Ok(n) => Ok(n),
            Err(e) => {
                if let Some(detail) = self.state.wait_error_detail(Duration::from_millis(100)) {
                    return Err(io::Error::new(e.kind(), detail));
                }
                Err(e)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(stdin) = self.stdin.as_mut() {
            stdin.flush()
        } else {
            Ok(())
        }
    }
}

impl Drop for TlsWriter {
    fn drop(&mut self) {
        self.stdin.take();
    }
}

fn strip_port_from_authority(auth: &str) -> &str {
    if let Some(rest) = auth.strip_prefix('[') {
        if let Some((ipv6, _)) = rest.split_once(']') {
            return ipv6;
        }
    }
    match auth.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') && p.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => auth,
    }
}

fn format_openssl_connect_target(host: &str, port: u16) -> (&str, String) {
    let lookup_host = strip_ipv6_brackets(host);
    let target = if lookup_host.contains(':') {
        format!("[{lookup_host}]:{port}")
    } else {
        format!("{lookup_host}:{port}")
    };
    (lookup_host, target)
}

fn resolve_verify_peer<'a>(host: &'a str, authority_override: Option<&'a str>) -> &'a str {
    authority_override
        .filter(|s| !s.is_empty())
        .map(strip_port_from_authority)
        .map(strip_ipv6_brackets)
        .unwrap_or_else(|| strip_ipv6_brackets(host))
}

fn build_s_client_args(
    connector: &TlsConnector,
    openssl: &OpenSslBinary,
    connect_target: Option<&str>,
) -> io::Result<(Vec<OsString>, bool)> {
    let caps = &openssl.caps;
    let bin_desc = if caps.version.is_empty() {
        openssl.path.display().to_string()
    } else {
        format!("{} ({})", caps.version, openssl.path.display())
    };
    if !caps.verify_return_error {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "TLS requires OpenSSL 1.1+ or 3+ with s_client -verify_return_error support, \
                 but {bin_desc} does not support -verify_return_error"
            ),
        ));
    }

    let mut args = vec![OsString::from("s_client")];
    if caps.quiet {
        args.push(OsString::from("-quiet"));
    }
    let use_brief = caps.brief;
    if use_brief {
        args.push(OsString::from("-brief"));
    }
    args.push(OsString::from("-verify_return_error"));

    if let Some(unix_path) = &connector.unix_proxy {
        if !caps.unix_socket {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "TLS over a Unix socket proxy requires an openssl binary with s_client -unix \
                     support, but {bin_desc} does not support -unix"
                ),
            ));
        }
        args.push(OsString::from("-unix"));
        args.push(OsString::from(unix_path));
    } else if let Some(target) = connect_target {
        args.push(OsString::from("-connect"));
        args.push(OsString::from(target));
    }

    let verify_peer =
        resolve_verify_peer(&connector.host, connector.tls.authority_override.as_deref());
    if let Ok(ip) = verify_peer.parse::<IpAddr>() {
        if !caps.verify_ip {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "TLS IP verification requires OpenSSL 1.1+ or 3+ with s_client -verify_ip \
                     support, but {bin_desc} does not support -verify_ip"
                ),
            ));
        }
        args.push(OsString::from("-verify_ip"));
        args.push(OsString::from(ip.to_string()));
    } else {
        if !caps.verify_hostname {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "TLS hostname verification requires OpenSSL 1.1+ or 3+ with s_client \
                     -verify_hostname support, but {bin_desc} does not support -verify_hostname"
                ),
            ));
        }
        if caps.servername {
            args.push(OsString::from("-servername"));
            args.push(OsString::from(verify_peer));
        }
        args.push(OsString::from("-verify_hostname"));
        args.push(OsString::from(verify_peer));
    }

    if let Some(alpn) = &connector.alpn {
        if !caps.alpn {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "TLS ALPN negotiation ({alpn}) requires an openssl binary with s_client -alpn \
                     support, but {bin_desc} does not support -alpn"
                ),
            ));
        }
        args.push(OsString::from("-alpn"));
        args.push(OsString::from(alpn));
    }

    if let Some(ca) = &connector.tls.ca_certificate {
        args.push(OsString::from("-CAfile"));
        args.push(ca.as_os_str().to_os_string());
        if caps.no_capath {
            args.push(OsString::from("-no-CApath"));
        }
        if caps.no_castore {
            args.push(OsString::from("-no-CAstore"));
        }
    }

    if let (Some(cert), Some(key)) = (&connector.tls.client_certificate, &connector.tls.client_key)
    {
        args.push(OsString::from("-cert"));
        args.push(cert.as_os_str().to_os_string());
        args.push(OsString::from("-key"));
        args.push(key.as_os_str().to_os_string());
    }

    Ok((args, use_brief))
}

impl TlsConnector {
    fn connect_with_binary(
        &self,
        timeout: Duration,
        openssl: &OpenSslBinary,
    ) -> io::Result<Transport> {
        self.tls
            .validate()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        let deadline = (!timeout.is_zero()).then(|| Instant::now() + timeout);

        let connect_target = if self.unix_proxy.is_some() {
            None
        } else {
            // Ensure hostname resolves first so DNS failures surface immediately and cleanly,
            // while passing the host to `openssl s_client` so OpenSSL can fall back across all
            // resolved addresses on dual-stack hosts.
            let (lookup_host, connect_target) =
                format_openssl_connect_target(&self.host, self.port);
            let mut addrs = (lookup_host, self.port).to_socket_addrs()?;
            if addrs.next().is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "no addresses resolved",
                ));
            }
            Some(connect_target)
        };

        let (args, use_brief) = build_s_client_args(self, openssl, connect_target.as_deref())?;

        let mut cmd = Command::new(&openssl.path);
        cmd.args(&args);
        if let Some(ca) = &self.tls.ca_certificate {
            // Isolate trust store to the explicitly configured `--tls_certificate` root bundle.
            cmd.env("SSL_CERT_FILE", ca)
                .env("SSL_CERT_DIR", "/var/empty");
        }

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "failed to invoke openssl ({}) for TLS connection: {e}",
                    openssl.path.display()
                ),
            )
        })?;

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();
        let stderr_sync = Arc::new((Mutex::new(StderrState::default()), Condvar::new()));
        let stderr_bg = stderr_sync.clone();
        std::thread::spawn(move || {
            let mut reader = io::BufReader::new(stderr_pipe);
            let mut raw = Vec::new();
            loop {
                raw.clear();
                match reader.read_until(b'\n', &mut raw) {
                    Ok(0) => break,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&raw);
                        let trimmed = line.trim_end_matches(&['\r', '\n'][..]).to_string();
                        let (lock, cv) = &*stderr_bg;
                        let mut st = lock.lock().unwrap();
                        if !st.established {
                            if trimmed.contains("CONNECTION ESTABLISHED") {
                                st.established = true;
                                cv.notify_all();
                            } else if !trimmed.is_empty()
                                && st.pre_lines.len() < MAX_CAPTURED_STDERR_LINES
                            {
                                st.pre_lines.push(trimmed);
                            }
                        } else if !trimmed.is_empty()
                            && !is_brief_info_line(&trimmed)
                            && st.post_error_lines.len() < MAX_CAPTURED_STDERR_LINES
                        {
                            st.post_error_lines.push(trimmed);
                        }
                    }
                    Err(_) => break,
                }
            }
            let (lock, cv) = &*stderr_bg;
            let mut st = lock.lock().unwrap();
            st.eof = true;
            cv.notify_all();
        });

        let state = Arc::new(TlsProcState {
            child: Mutex::new(child),
            stderr: stderr_sync.clone(),
            killed: AtomicBool::new(false),
        });

        if use_brief {
            let (lock, cv) = &*stderr_sync;
            let mut st = lock.lock().unwrap();
            loop {
                if st.established || st.eof {
                    break;
                }
                if let Some(dl) = deadline {
                    let now = Instant::now();
                    if now >= dl {
                        drop(st);
                        state.kill();
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!("TLS connection/handshake timed out after {timeout:?}"),
                        ));
                    }
                    let (next_st, _) = cv.wait_timeout(st, dl - now).unwrap();
                    st = next_st;
                } else {
                    st = cv.wait(st).unwrap();
                }
            }
            let established = st.established;
            let pre_err = st.pre_lines.join("\n");
            drop(st);

            if !established {
                let status = state.wait_exit_bounded(Duration::from_millis(250));
                let detail = if !pre_err.is_empty() {
                    pre_err
                } else if let Some(s) = status {
                    format!("TLS handshake/connection failed ({s})")
                } else {
                    "TLS handshake/connection failed (openssl exited before handshake completed)"
                        .to_string()
                };
                state.kill();
                return Err(io::Error::new(classify_tls_error_kind(&detail), detail));
            }

            // Catch immediate post-handshake exit (e.g. fatal alert right after establishment).
            if let Ok(mut child) = state.child.lock() {
                if let Ok(Some(status)) = child.try_wait() {
                    drop(child);
                    if !status.success() {
                        let detail = state
                            .wait_error_detail(Duration::from_millis(100))
                            .unwrap_or_else(|| state.format_stderr_for_status(status));
                        state.kill();
                        return Err(io::Error::new(classify_tls_error_kind(&detail), detail));
                    }
                }
            }
        } else {
            // Without `-brief`, bound a short early-exit check to surface immediate startup/CLI
            // errors instead of returning silent EOF on the first read.
            let probe_wait = deadline
                .map(|dl| {
                    dl.saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(50))
                })
                .unwrap_or(Duration::from_millis(50));
            let probe_deadline = Instant::now() + probe_wait;
            let (lock, cv) = &*stderr_sync;
            let mut st = lock.lock().unwrap();
            while !st.eof {
                let now = Instant::now();
                if now >= probe_deadline {
                    break;
                }
                let (next_st, _) = cv.wait_timeout(st, probe_deadline - now).unwrap();
                st = next_st;
            }
            let eof = st.eof;
            drop(st);

            let exited = if eof {
                state.wait_exit_bounded(Duration::from_millis(250))
            } else {
                state
                    .child
                    .lock()
                    .ok()
                    .and_then(|mut c| c.try_wait().ok().flatten())
            };
            if let Some(status) = exited {
                if !status.success() {
                    let detail = state
                        .wait_error_detail(Duration::from_millis(100))
                        .unwrap_or_else(|| state.format_stderr_for_status(status));
                    state.kill();
                    return Err(io::Error::new(classify_tls_error_kind(&detail), detail));
                }
            }
        }

        let shutdown_state = state.clone();

        Ok(Transport {
            reader: Box::new(TlsReader {
                stdout,
                state: state.clone(),
            }),
            writer: Box::new(TlsWriter {
                stdin: Some(stdin),
                state,
            }),
            shutdown: Box::new(move || {
                shutdown_state.kill();
            }),
        })
    }
}

impl Connector for TlsConnector {
    fn connect(&self) -> io::Result<Transport> {
        self.connect_with_timeout(self.connect_timeout)
    }

    fn connect_with_timeout(&self, timeout: Duration) -> io::Result<Transport> {
        let openssl = resolve_default_openssl()
            .map_err(|msg| io::Error::new(io::ErrorKind::Unsupported, msg))?;
        self.connect_with_binary(timeout, openssl)
    }

    fn connect_with_dial_timeout(&self, timeout: Duration) -> io::Result<Transport> {
        let effective = effective_dial_timeout(self.connect_timeout, timeout);
        self.connect_with_timeout(effective)
    }

    fn authority(&self) -> String {
        if let Some(ov) = &self.tls.authority_override {
            if !ov.is_empty() {
                return ov.clone();
            }
        }
        let bare = strip_ipv6_brackets(&self.host);
        let host_fmt = if bare.contains(':') {
            format!("[{bare}]")
        } else {
            bare.to_string()
        };
        if self.port == 443 {
            host_fmt
        } else {
            format!("{host_fmt}:{}", self.port)
        }
    }
}

/// Unix domain socket transport.
#[cfg(unix)]
pub struct UnixConnector {
    pub path: String,
    pub authority_override: Option<String>,
}

#[cfg(unix)]
impl UnixConnector {
    pub fn new(path: String) -> Self {
        Self {
            path,
            authority_override: None,
        }
    }
}

#[cfg(unix)]
impl Connector for UnixConnector {
    fn connect(&self) -> io::Result<Transport> {
        self.connect_with_timeout(Duration::ZERO)
    }

    fn connect_with_timeout(&self, timeout: Duration) -> io::Result<Transport> {
        let s = std::os::unix::net::UnixStream::connect(&self.path)?;
        if !timeout.is_zero() {
            let _ = s.set_read_timeout(Some(timeout));
            let _ = s.set_write_timeout(Some(timeout));
        }
        let r = s.try_clone()?;
        let w = s.try_clone()?;
        if !timeout.is_zero() {
            let _ = r.set_read_timeout(Some(timeout));
            let _ = w.set_write_timeout(Some(timeout));
        }
        Ok(Transport {
            reader: Box::new(r),
            writer: Box::new(w),
            shutdown: Box::new(move || {
                let _ = s.shutdown(Shutdown::Both);
            }),
        })
    }

    fn authority(&self) -> String {
        if let Some(ov) = &self.authority_override {
            if !ov.is_empty() {
                return ov.clone();
            }
        }
        "localhost".into()
    }
}

#[derive(Default)]
struct PipeInner {
    buf: VecDeque<u8>,
    closed: bool,
}

struct Pipe {
    inner: Mutex<PipeInner>,
    cv: Condvar,
}

impl Pipe {
    fn new() -> Arc<Pipe> {
        Arc::new(Pipe {
            inner: Mutex::new(PipeInner::default()),
            cv: Condvar::new(),
        })
    }
    fn close(&self) {
        self.inner.lock().unwrap().closed = true;
        self.cv.notify_all();
    }
}

struct PipeReader(Arc<Pipe>);
struct PipeWriter(Arc<Pipe>);

impl Read for PipeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut g = self.0.inner.lock().unwrap();
        loop {
            if !g.buf.is_empty() {
                let n = out.len().min(g.buf.len());
                for slot in out.iter_mut().take(n) {
                    *slot = g.buf.pop_front().unwrap();
                }
                return Ok(n);
            }
            if g.closed {
                return Ok(0);
            }
            g = self.0.cv.wait(g).unwrap();
        }
    }
}

impl Write for PipeWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut g = self.0.inner.lock().unwrap();
        if g.closed {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed"));
        }
        g.buf.extend(data.iter().copied());
        drop(g);
        self.0.cv.notify_all();
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Creates a connected pair of in-memory transports `(a, b)`.
pub fn memory_pair() -> (Transport, Transport) {
    let a_to_b = Pipe::new();
    let b_to_a = Pipe::new();
    let mk = |rx: Arc<Pipe>, tx: Arc<Pipe>| {
        let (rx2, tx2) = (rx.clone(), tx.clone());
        Transport {
            reader: Box::new(PipeReader(rx)),
            writer: Box::new(PipeWriter(tx)),
            shutdown: Box::new(move || {
                rx2.close();
                tx2.close();
            }),
        }
    };
    (mk(b_to_a.clone(), a_to_b.clone()), mk(a_to_b, b_to_a))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_pair_moves_bytes_both_ways_and_closes() {
        let (mut a, mut b) = memory_pair();
        a.writer.write_all(b"hello").unwrap();
        let mut buf = [0u8; 5];
        b.reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello");
        b.writer.write_all(b"yo").unwrap();
        let mut buf = [0u8; 2];
        a.reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"yo");
        (a.shutdown)();
        assert_eq!(b.reader.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn tcp_connector_gives_each_resolved_address_full_connect_timeout_and_falls_back() {
        let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return,
            Err(e) => panic!("{e}"),
        };
        let live_addr = listener.local_addr().unwrap();

        let accept_handle = std::thread::spawn(move || {
            for _ in 0..3 {
                let (mut s, _) = listener.accept().unwrap();
                let mut got = [0u8; 4];
                s.read_exact(&mut got).unwrap();
                assert_eq!(&got, b"ping");
                s.write_all(b"pong").unwrap();
            }
        });

        // 1) Simulate a blackholed first address (e.g. IPv6 SYN drop) whose connect attempt
        //    consumes the entire `connect_timeout` budget: the second address (IPv4) must still
        //    receive its own full `connect_timeout` and succeed.
        let blackhole_addr: SocketAddr = "[::1]:9".parse().unwrap();
        let per_addr_timeout = Duration::from_millis(40);
        let mut dialed = Vec::new();
        let mut tr = connect_addrs_with(
            [blackhole_addr, live_addr],
            per_addr_timeout,
            Some(Duration::from_millis(500)),
            |addr, timeout| {
                dialed.push((*addr, timeout));
                if *addr == blackhole_addr {
                    std::thread::sleep(Duration::from_millis(55));
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "simulated IPv6 SYN blackhole",
                    ))
                } else {
                    TcpStream::connect_timeout(addr, timeout.unwrap())
                }
            },
        )
        .unwrap();
        assert_eq!(
            dialed,
            vec![
                (blackhole_addr, Some(per_addr_timeout)),
                (live_addr, Some(per_addr_timeout)),
            ]
        );
        tr.writer.write_all(b"ping").unwrap();
        tr.writer.flush().unwrap();
        let mut reply = [0u8; 4];
        tr.reader.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"pong");
        (tr.shutdown)();

        // 2) Real `connect_addrs` falls back from an unreachable/closed first `SocketAddr` to a
        //    live second `SocketAddr`.
        let closed_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let closed_addr = closed_listener.local_addr().unwrap();
        drop(closed_listener);

        let mut tr2 = connect_addrs(
            [closed_addr, live_addr],
            Duration::from_millis(500),
            Some(Duration::from_millis(500)),
        )
        .unwrap();
        tr2.writer.write_all(b"ping").unwrap();
        tr2.writer.flush().unwrap();
        tr2.reader.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"pong");
        (tr2.shutdown)();

        // 3) Dual-stack `localhost` resolution against an IPv4-only listener (`127.0.0.1:port`)
        //    falls back across `[::1]` -> `127.0.0.1`.
        let conn = TcpConnector::new(
            "localhost".into(),
            live_addr.port(),
            Duration::from_millis(500),
        );
        let mut tr3 = conn
            .connect_with_timeout(Duration::from_millis(500))
            .unwrap();
        tr3.writer.write_all(b"ping").unwrap();
        tr3.writer.flush().unwrap();
        tr3.reader.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"pong");
        (tr3.shutdown)();

        accept_handle.join().unwrap();
    }

    #[test]
    fn tls_config_validates_files_and_mtls_pair() {
        let cfg = TlsConfig {
            ca_certificate: Some(PathBuf::from("/nonexistent/ca.pem")),
            ..Default::default()
        };
        assert!(cfg.validate().is_err());

        let dir = std::env::temp_dir().join(format!("zel-tls-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cert = dir.join("client.crt");
        std::fs::write(&cert, "dummy").unwrap();
        let half = TlsConfig {
            client_certificate: Some(cert.clone()),
            client_key: None,
            ..Default::default()
        };
        assert!(half.validate().unwrap_err().contains("together"));
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            format_openssl_connect_target("example.com", 443),
            ("example.com", "example.com:443".into())
        );
        assert_eq!(
            format_openssl_connect_target("::1", 8443),
            ("::1", "[::1]:8443".into())
        );
        assert_eq!(
            format_openssl_connect_target("[::1]", 8443),
            ("::1", "[::1]:8443".into())
        );
        assert_eq!(resolve_verify_peer("[::1]", None), "::1");
        assert!(resolve_verify_peer("[::1]", None).parse::<IpAddr>().is_ok());
        assert_eq!(resolve_verify_peer("127.0.0.1", Some("[::1]:9443")), "::1");
        assert_eq!(
            resolve_verify_peer("127.0.0.1", Some("override.example.com:9443")),
            "override.example.com"
        );
    }

    #[test]
    fn openssl_capability_parsing_and_candidate_selection() {
        let openssl3_help = "\
Usage: s_client [options] [host:port]
 -connect val               TCP/IP where to connect
 -unix val                  Connect over the specified Unix-domain socket
 -quiet                     No s_client output
 -brief                     Restrict output to brief summary
 -servername val            Set TLS extension servername (SNI)
 -alpn val                  Enable ALPN extension
 -verify_hostname val       expected peer hostname
 -verify_ip val             expected peer IP address
 -verify_return_error       Close connection on verification error
 -no-CApath                 Do not load default CA dir
 -no-CAstore                Do not load default CA store
";
        let caps3 = parse_s_client_capabilities("OpenSSL 3.2.1 30 Jan 2024", openssl3_help);
        assert_eq!(caps3.version, "OpenSSL 3.2.1 30 Jan 2024");
        assert!(caps3.supports_strict_tls());
        assert!(caps3.unix_socket);
        assert!(caps3.brief);
        assert!(caps3.alpn);
        assert!(caps3.no_capath && caps3.no_castore);

        let libressl_help = "\
usage: s_client args
 -connect host:port - who to connect to (note: unlike -verify_hostname in OpenSSL)
 -verify depth      - turn on peer certificate verification
 -verify_return_error - return verification errors
 -servername host   - Set TLS extension servername in ClientHello
 -alpn arg          - enable the ALPN extension
 -quiet             - No s_client output
";
        let caps_libre = parse_s_client_capabilities("LibreSSL 3.3.6", libressl_help);
        assert_eq!(caps_libre.version, "LibreSSL 3.3.6");
        assert!(!caps_libre.verify_hostname);
        assert!(!caps_libre.verify_ip);
        assert!(!caps_libre.unix_socket);
        assert!(!caps_libre.brief);
        assert!(!caps_libre.supports_strict_tls());
        assert_eq!(
            caps_libre.missing_strict_flags(),
            vec!["-verify_hostname", "-verify_ip"]
        );

        let win_openssl_help = "\
Usage: s_client [options]
 -connect val
 -quiet
 -brief
 -servername val
 -alpn val
 -verify_hostname val
 -verify_ip val
 -verify_return_error
";
        let caps_win = parse_s_client_capabilities("OpenSSL 3.1.4", win_openssl_help);
        assert!(caps_win.supports_strict_tls());
        assert!(!caps_win.unix_socket);

        // Candidate selection skips LibreSSL and picks Homebrew OpenSSL 3.
        let libre_path = PathBuf::from("/usr/bin/openssl");
        let brew_path = PathBuf::from("/opt/homebrew/opt/openssl@3/bin/openssl");
        let picked = select_openssl_with(&[libre_path.clone(), brew_path.clone()], |p| {
            if p == libre_path {
                Some(OpenSslBinary {
                    path: p.to_path_buf(),
                    caps: caps_libre.clone(),
                })
            } else if p == brew_path {
                Some(OpenSslBinary {
                    path: p.to_path_buf(),
                    caps: caps3.clone(),
                })
            } else {
                None
            }
        })
        .unwrap();
        assert_eq!(picked.path, brew_path);

        // On Unix, prefer an OpenSSL binary with -unix support when a later candidate has it.
        #[cfg(unix)]
        {
            let win_path = PathBuf::from("/custom/openssl-no-unix");
            let picked_unix = select_openssl_with(&[win_path.clone(), brew_path.clone()], |p| {
                if p == win_path {
                    Some(OpenSslBinary {
                        path: p.to_path_buf(),
                        caps: caps_win.clone(),
                    })
                } else if p == brew_path {
                    Some(OpenSslBinary {
                        path: p.to_path_buf(),
                        caps: caps3.clone(),
                    })
                } else {
                    None
                }
            })
            .unwrap();
            assert_eq!(picked_unix.path, brew_path);
        }

        // Only LibreSSL found -> actionable error instead of silent downgrade.
        let err = select_openssl_with(std::slice::from_ref(&libre_path), |p| {
            Some(OpenSslBinary {
                path: p.to_path_buf(),
                caps: caps_libre.clone(),
            })
        })
        .unwrap_err();
        assert!(err.contains("LibreSSL 3.3.6"), "{err}");
        assert!(err.contains("-verify_hostname/-verify_ip"), "{err}");
        assert!(err.contains("brew install openssl@3"), "{err}");

        // No openssl found -> actionable error.
        let err_none =
            select_openssl_with(std::slice::from_ref(&libre_path), |_| None).unwrap_err();
        assert!(
            err_none.contains("failed to find an openssl binary"),
            "{err_none}"
        );
    }

    #[test]
    fn build_s_client_args_respects_capabilities_and_target_types() {
        let full_bin = OpenSslBinary {
            path: PathBuf::from("/opt/homebrew/opt/openssl@3/bin/openssl"),
            caps: OpenSslCapabilities {
                version: "OpenSSL 3.2.0".into(),
                verify_hostname: true,
                verify_ip: true,
                unix_socket: true,
                verify_return_error: true,
                servername: true,
                alpn: true,
                quiet: true,
                brief: true,
                no_capath: true,
                no_castore: true,
            },
        };

        let host_conn = TlsConnector {
            host: "remote.example.com".into(),
            port: 443,
            connect_timeout: Duration::from_secs(5),
            alpn: Some("h2".into()),
            tls: TlsConfig {
                ca_certificate: Some(PathBuf::from("/tmp/ca.pem")),
                client_certificate: Some(PathBuf::from("/tmp/cli.pem")),
                client_key: Some(PathBuf::from("/tmp/cli.key")),
                authority_override: None,
            },
            unix_proxy: None,
        };
        let (args, use_brief) =
            build_s_client_args(&host_conn, &full_bin, Some("remote.example.com:443")).unwrap();
        let has_arg = |a: &[OsString], s: &str| a.iter().any(|x| x == s);
        assert!(use_brief);
        assert!(has_arg(&args, "-quiet"));
        assert!(has_arg(&args, "-brief"));
        assert!(has_arg(&args, "-verify_return_error"));
        assert!(has_arg(&args, "-servername"));
        assert!(has_arg(&args, "-verify_hostname"));
        assert!(!has_arg(&args, "-verify_ip"));
        assert!(has_arg(&args, "-alpn"));
        assert!(has_arg(&args, "-no-CApath"));
        assert!(has_arg(&args, "-no-CAstore"));
        assert!(has_arg(&args, "-cert"));
        assert!(has_arg(&args, "-key"));

        // IPv6 peer uses -verify_ip, never -servername / -verify_hostname.
        let ip_conn = TlsConnector {
            host: "[::1]".into(),
            port: 8443,
            connect_timeout: Duration::from_secs(5),
            alpn: None,
            tls: TlsConfig::default(),
            unix_proxy: None,
        };
        let (ip_args, _) = build_s_client_args(&ip_conn, &full_bin, Some("[::1]:8443")).unwrap();
        assert!(has_arg(&ip_args, "-verify_ip"));
        assert!(has_arg(&ip_args, "::1"));
        assert!(!has_arg(&ip_args, "-servername"));
        assert!(!has_arg(&ip_args, "-verify_hostname"));

        // Unix proxy uses -unix and fails clearly when openssl lacks -unix.
        let unix_conn = TlsConnector {
            host: "remote.example.com".into(),
            port: 443,
            connect_timeout: Duration::from_secs(5),
            alpn: Some("h2".into()),
            tls: TlsConfig::default(),
            unix_proxy: Some("/tmp/proxy.sock".into()),
        };
        let (unix_args, _) = build_s_client_args(&unix_conn, &full_bin, None).unwrap();
        assert!(has_arg(&unix_args, "-unix"));
        assert!(has_arg(&unix_args, "/tmp/proxy.sock"));
        assert!(!has_arg(&unix_args, "-connect"));

        let mut no_unix_bin = full_bin.clone();
        no_unix_bin.caps.unix_socket = false;
        let unix_err = build_s_client_args(&unix_conn, &no_unix_bin, None).unwrap_err();
        assert_eq!(unix_err.kind(), io::ErrorKind::Unsupported);
        assert!(unix_err.to_string().contains("-unix"), "{unix_err}");

        // LibreSSL missing -verify_hostname returns Unsupported without weakening verification.
        let mut libre_bin = full_bin.clone();
        libre_bin.caps.version = "LibreSSL 3.3.6".into();
        libre_bin.caps.verify_hostname = false;
        libre_bin.caps.verify_ip = false;
        libre_bin.caps.brief = false;
        let host_err = build_s_client_args(&host_conn, &libre_bin, Some("remote.example.com:443"))
            .unwrap_err();
        assert_eq!(host_err.kind(), io::ErrorKind::Unsupported);
        assert!(
            host_err.to_string().contains("-verify_hostname"),
            "{host_err}"
        );
        let ip_err = build_s_client_args(&ip_conn, &libre_bin, Some("[::1]:8443")).unwrap_err();
        assert_eq!(ip_err.kind(), io::ErrorKind::Unsupported);
        assert!(ip_err.to_string().contains("-verify_ip"), "{ip_err}");
    }

    #[test]
    fn tls_connect_with_timeout_bounds_hanging_server() {
        if resolve_default_openssl().is_err() {
            return;
        }
        let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return,
            Err(e) => panic!("{e}"),
        };
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_srv = stop.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let accept_thread = std::thread::spawn(move || {
            if let Ok((_conn, _)) = listener.accept() {
                if !stop_srv.load(Ordering::SeqCst) {
                    let _ = done_rx.recv_timeout(Duration::from_secs(5));
                }
            }
        });

        struct AcceptGuard {
            port: u16,
            stop: Arc<AtomicBool>,
            done_tx: std::sync::mpsc::Sender<()>,
            handle: Option<std::thread::JoinHandle<()>>,
        }
        impl Drop for AcceptGuard {
            fn drop(&mut self) {
                self.stop.store(true, Ordering::SeqCst);
                let _ = self.done_tx.send(());
                // Wake `listener.accept()` if `openssl` timed out or failed before TCP connect
                // (e.g. slow process startup on Windows CI).
                let _ = TcpStream::connect_timeout(
                    &SocketAddr::from(([127, 0, 0, 1], self.port)),
                    Duration::from_secs(1),
                );
                if let Some(h) = self.handle.take() {
                    let _ = h.join();
                }
            }
        }
        let _guard = AcceptGuard {
            port,
            stop,
            done_tx,
            handle: Some(accept_thread),
        };

        let tls_conn = TlsConnector {
            host: "127.0.0.1".into(),
            port,
            connect_timeout: Duration::from_secs(30),
            alpn: Some("h2".into()),
            tls: TlsConfig::default(),
            unix_proxy: None,
        };
        let started = Instant::now();
        let err = match tls_conn.connect_with_timeout(Duration::from_millis(500)) {
            Ok(_) => panic!("expected TLS handshake against silent server to time out"),
            Err(e) => e,
        };
        let elapsed = started.elapsed();

        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "err={err}");
        assert!(
            err.to_string().contains("timed out"),
            "unexpected error: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "connect_with_timeout took too long: {elapsed:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn tls_early_exit_and_candidate_probing_via_scripts() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("zel-tls-fake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let make_script = |name: &str, body: &str| -> PathBuf {
            let p = dir.join(name);
            std::fs::write(&p, body).unwrap();
            let mut perms = std::fs::metadata(&p).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&p, perms).unwrap();
            p
        };

        // 1) Fake LibreSSL + fake OpenSSL 3 probing.
        let fake_libressl = make_script(
            "fake-libressl",
            "#!/bin/sh\n\
             if [ \"$1\" = \"version\" ]; then\n\
               echo \"LibreSSL 3.3.6\"\n\
               exit 0\n\
             fi\n\
             echo \"usage: s_client args\" >&2\n\
             echo \" -connect host:port - target\" >&2\n\
             echo \" -verify_return_error - fail on verify error\" >&2\n\
             echo \" -servername host - SNI\" >&2\n\
             echo \" -alpn arg - ALPN\" >&2\n\
             echo \" -quiet - quiet\" >&2\n\
             exit 1\n",
        );
        let fake_openssl3 = make_script(
            "fake-openssl3",
            "#!/bin/sh\n\
             if [ \"$1\" = \"version\" ]; then\n\
               echo \"OpenSSL 3.2.0 23 Nov 2023\"\n\
               exit 0\n\
             fi\n\
             echo \"Usage: s_client [options]\" >&2\n\
             echo \" -connect val\" >&2\n\
             echo \" -unix val\" >&2\n\
             echo \" -quiet\" >&2\n\
             echo \" -brief\" >&2\n\
             echo \" -servername val\" >&2\n\
             echo \" -alpn val\" >&2\n\
             echo \" -verify_hostname val\" >&2\n\
             echo \" -verify_ip val\" >&2\n\
             echo \" -verify_return_error\" >&2\n\
             exit 0\n",
        );

        let selected = select_openssl_with(
            &[fake_libressl.clone(), fake_openssl3.clone()],
            probe_openssl_binary,
        )
        .unwrap();
        assert_eq!(selected.path, fake_openssl3);
        assert!(selected.caps.supports_strict_tls());
        assert!(selected.caps.brief);

        let only_libre_err =
            select_openssl_with(std::slice::from_ref(&fake_libressl), probe_openssl_binary)
                .unwrap_err();
        assert!(
            only_libre_err.contains("LibreSSL 3.3.6"),
            "{only_libre_err}"
        );
        assert!(
            only_libre_err.contains("-verify_hostname/-verify_ip"),
            "{only_libre_err}"
        );

        // 2) Early failure on startup surfaces stderr synchronously for both brief and non-brief.
        let failing_bin_path = make_script(
            "fake-failing-openssl",
            "#!/bin/sh\n\
             echo \"s_client: unknown option -verify_hostname\" >&2\n\
             exit 1\n",
        );
        let conn = TlsConnector {
            host: "127.0.0.1".into(),
            port: 443,
            connect_timeout: Duration::from_secs(2),
            alpn: Some("h2".into()),
            tls: TlsConfig::default(),
            unix_proxy: None,
        };
        for brief in [true, false] {
            let fake_bin = OpenSslBinary {
                path: failing_bin_path.clone(),
                caps: OpenSslCapabilities {
                    version: "OpenSSL 3.2.0".into(),
                    verify_hostname: true,
                    verify_ip: true,
                    unix_socket: true,
                    verify_return_error: true,
                    servername: true,
                    alpn: true,
                    quiet: true,
                    brief,
                    no_capath: false,
                    no_castore: false,
                },
            };
            let err = match conn.connect_with_binary(Duration::from_secs(2), &fake_bin) {
                Ok(_) => panic!("expected early exit error (brief={brief})"),
                Err(e) => e,
            };
            assert!(
                err.to_string().contains("unknown option -verify_hostname"),
                "brief={brief}, err={err}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn tls_and_mtls_loopback_via_openssl_if_available() {
        let Ok(openssl) = resolve_default_openssl() else {
            return;
        };
        let Ok(probe) = std::net::TcpListener::bind("127.0.0.1:0") else {
            return;
        };
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let dir = std::env::temp_dir().join(format!("zel-tls-loop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ca_key = dir.join("ca.key");
        let ca_crt = dir.join("ca.crt");
        let srv_key = dir.join("srv.key");
        let srv_csr = dir.join("srv.csr");
        let srv_crt = dir.join("srv.crt");
        let srv_ext = dir.join("srv.ext");
        let cli_key = dir.join("cli.key");
        let cli_csr = dir.join("cli.csr");
        let cli_crt = dir.join("cli.crt");

        std::fs::write(&srv_ext, "subjectAltName=DNS:localhost,IP:127.0.0.1\n").unwrap();
        let run_ok = |args: &[&str]| -> bool {
            Command::new(&openssl.path)
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        if !run_ok(&[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            ca_key.to_str().unwrap(),
            "-out",
            ca_crt.to_str().unwrap(),
            "-days",
            "1",
            "-subj",
            "/CN=ZelTestCA",
        ]) {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert!(run_ok(&[
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            srv_key.to_str().unwrap(),
            "-out",
            srv_csr.to_str().unwrap(),
            "-subj",
            "/CN=localhost",
        ]));
        assert!(run_ok(&[
            "x509",
            "-req",
            "-in",
            srv_csr.to_str().unwrap(),
            "-CA",
            ca_crt.to_str().unwrap(),
            "-CAkey",
            ca_key.to_str().unwrap(),
            "-CAcreateserial",
            "-out",
            srv_crt.to_str().unwrap(),
            "-days",
            "1",
            "-extfile",
            srv_ext.to_str().unwrap(),
        ]));
        assert!(run_ok(&[
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            cli_key.to_str().unwrap(),
            "-out",
            cli_csr.to_str().unwrap(),
            "-subj",
            "/CN=zel-client",
        ]));
        assert!(run_ok(&[
            "x509",
            "-req",
            "-in",
            cli_csr.to_str().unwrap(),
            "-CA",
            ca_crt.to_str().unwrap(),
            "-CAkey",
            ca_key.to_str().unwrap(),
            "-CAcreateserial",
            "-out",
            cli_crt.to_str().unwrap(),
            "-days",
            "1",
        ]));

        // Launch an mTLS `openssl s_server` accepting connections with ALPN h2.
        let mut srv = Command::new(&openssl.path)
            .args([
                "s_server",
                "-accept",
                &format!("127.0.0.1:{port}"),
                "-cert",
                srv_crt.to_str().unwrap(),
                "-key",
                srv_key.to_str().unwrap(),
                "-CAfile",
                ca_crt.to_str().unwrap(),
                "-Verify",
                "1",
                "-verify_return_error",
                "-alpn",
                "h2",
                "-quiet",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        let tls_conn = TlsConnector {
            host: "127.0.0.1".into(),
            port,
            connect_timeout: Duration::from_secs(5),
            alpn: Some("h2".into()),
            tls: TlsConfig {
                ca_certificate: Some(ca_crt.clone()),
                client_certificate: Some(cli_crt.clone()),
                client_key: Some(cli_key.clone()),
                authority_override: Some("localhost".into()),
            },
            unix_proxy: None,
        };

        // Retry briefly while s_server finishes binding its listening socket.
        let mut connected = None;
        for _ in 0..40 {
            match tls_conn.connect() {
                Ok(tr) => {
                    connected = Some(tr);
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("unexpected mTLS connect error: {e}"),
            }
        }
        let mut tr = connected.expect("mTLS loopback connect timed out waiting for s_server");
        tr.writer.write_all(b"ping-over-mtls\n").unwrap();
        tr.writer.flush().unwrap();
        let mut srv_out = srv.stdout.take().unwrap();
        let mut got = [0u8; 15];
        srv_out.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"ping-over-mtls\n");
        (tr.shutdown)();

        // Wrong hostname must fail synchronously during connect() with the real verify error.
        let bad_host_conn = TlsConnector {
            host: "127.0.0.1".into(),
            port,
            connect_timeout: Duration::from_secs(5),
            alpn: Some("h2".into()),
            tls: TlsConfig {
                ca_certificate: Some(ca_crt.clone()),
                client_certificate: Some(cli_crt.clone()),
                client_key: Some(cli_key.clone()),
                authority_override: Some("wrong-host.example.com".into()),
            },
            unix_proxy: None,
        };
        let bad_err = match bad_host_conn.connect() {
            Ok(_) => panic!("expected hostname mismatch to fail synchronously during connect()"),
            Err(e) => e,
        };
        let bad_msg = bad_err.to_string().to_ascii_lowercase();
        assert!(
            bad_msg.contains("hostname mismatch") || bad_msg.contains("verify"),
            "expected real TLS verify error, got: {bad_err}"
        );

        let _ = srv.kill();
        let _ = srv.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
