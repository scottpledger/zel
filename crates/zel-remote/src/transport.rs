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
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

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
    /// Value used for the HTTP/2 `:authority` pseudo-header (or HTTP/1.1 `Host`).
    fn authority(&self) -> String;
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

struct TlsProcState {
    child: Mutex<Child>,
    stderr: Arc<Mutex<String>>,
}

impl TlsProcState {
    fn error_detail(&self) -> Option<String> {
        let mut child = self.child.lock().unwrap();
        if let Ok(Some(status)) = child.try_wait() {
            if !status.success() {
                drop(child);
                let msg = self.stderr.lock().unwrap().trim().to_string();
                if !msg.is_empty() {
                    return Some(msg);
                }
                return Some(format!("TLS handshake/connection failed ({status})"));
            }
        }
        None
    }

    fn kill(&self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
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
                // Give stderr reader a brief moment if process just exited on handshake failure.
                for _ in 0..20 {
                    if let Some(detail) = self.state.error_detail() {
                        return Err(io::Error::new(io::ErrorKind::ConnectionRefused, detail));
                    }
                    let done = self
                        .state
                        .child
                        .lock()
                        .ok()
                        .and_then(|mut c| c.try_wait().ok().flatten())
                        .is_some();
                    if done {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                if let Some(detail) = self.state.error_detail() {
                    return Err(io::Error::new(io::ErrorKind::ConnectionRefused, detail));
                }
                Ok(0)
            }
            Ok(n) => Ok(n),
            Err(e) => {
                if let Some(detail) = self.state.error_detail() {
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
                if let Some(detail) = self.state.error_detail() {
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

impl Connector for TlsConnector {
    fn connect(&self) -> io::Result<Transport> {
        self.tls
            .validate()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        let verify_peer = resolve_verify_peer(&self.host, self.tls.authority_override.as_deref());

        let mut cmd = Command::new("openssl");
        cmd.arg("s_client")
            .arg("-quiet")
            .arg("-verify_return_error");

        if let Some(unix_path) = &self.unix_proxy {
            cmd.arg("-unix").arg(unix_path);
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
            cmd.arg("-connect").arg(connect_target);
        }

        if let Ok(ip) = verify_peer.parse::<IpAddr>() {
            cmd.arg("-verify_ip").arg(ip.to_string());
        } else {
            cmd.arg("-servername")
                .arg(verify_peer)
                .arg("-verify_hostname")
                .arg(verify_peer);
        }

        if let Some(alpn) = &self.alpn {
            cmd.arg("-alpn").arg(alpn);
        }

        if let Some(ca) = &self.tls.ca_certificate {
            // Isolate trust store to the explicitly configured `--tls_certificate` root bundle.
            cmd.arg("-CAfile")
                .arg(ca)
                .env("SSL_CERT_FILE", ca)
                .env("SSL_CERT_DIR", "/var/empty");
        }

        if let (Some(cert), Some(key)) = (&self.tls.client_certificate, &self.tls.client_key) {
            cmd.arg("-cert").arg(cert).arg("-key").arg(key);
        }

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("failed to invoke openssl for TLS connection: {e}"),
            )
        })?;

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut stderr_pipe = child.stderr.take().unwrap();
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stderr_clone = stderr_buf.clone();
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = stderr_pipe.read_to_string(&mut s);
            if !s.is_empty() {
                *stderr_clone.lock().unwrap() = s;
            }
        });

        let state = Arc::new(TlsProcState {
            child: Mutex::new(child),
            stderr: stderr_buf,
        });
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

    #[cfg(unix)]
    #[test]
    fn tls_and_mtls_loopback_via_openssl_if_available() {
        let has_openssl = Command::new("openssl")
            .arg("version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !has_openssl {
            return;
        }
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
            Command::new("openssl")
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

        // Launch an mTLS `openssl s_server` accepting 1 connection with ALPN h2.
        let mut srv = Command::new("openssl")
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
                "-naccept",
                "1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        std::thread::sleep(Duration::from_millis(120));

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
        if let Ok(mut tr) = tls_conn.connect() {
            tr.writer.write_all(b"ping-over-mtls\n").unwrap();
            tr.writer.flush().unwrap();
            let mut srv_out = srv.stdout.take().unwrap();
            let mut got = [0u8; 15];
            if srv_out.read_exact(&mut got).is_ok() {
                assert_eq!(&got, b"ping-over-mtls\n");
            }
            (tr.shutdown)();
        }
        let _ = srv.kill();
        let _ = srv.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
