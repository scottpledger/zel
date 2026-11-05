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

//! Byte transports for HTTP/2: TCP, Unix domain sockets (`--remote_proxy=unix:/path`) and an
//! in-memory duplex pipe (used by in-process fake servers so tests need no network access).

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
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
    /// Value used for the HTTP/2 `:authority` pseudo-header.
    fn authority(&self) -> String;
}

/// Plaintext TCP (h2c prior knowledge).
pub struct TcpConnector {
    pub host: String,
    pub port: u16,
    pub connect_timeout: Duration,
}

impl Connector for TcpConnector {
    fn connect(&self) -> io::Result<Transport> {
        let mut last = io::Error::new(io::ErrorKind::NotFound, "no addresses resolved");
        for addr in (self.host.as_str(), self.port).to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, self.connect_timeout) {
                Ok(s) => {
                    let _ = s.set_nodelay(true);
                    let r = s.try_clone()?;
                    let w = s.try_clone()?;
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

    fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Unix domain socket transport.
#[cfg(unix)]
pub struct UnixConnector {
    pub path: String,
}

#[cfg(unix)]
impl Connector for UnixConnector {
    fn connect(&self) -> io::Result<Transport> {
        let s = std::os::unix::net::UnixStream::connect(&self.path)?;
        let r = s.try_clone()?;
        let w = s.try_clone()?;
        Ok(Transport {
            reader: Box::new(r),
            writer: Box::new(w),
            shutdown: Box::new(move || {
                let _ = s.shutdown(Shutdown::Both);
            }),
        })
    }

    fn authority(&self) -> String {
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
        Arc::new(Pipe { inner: Mutex::new(PipeInner::default()), cv: Condvar::new() })
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
}
