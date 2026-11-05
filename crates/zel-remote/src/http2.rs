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

//! A from-scratch HTTP/2 (RFC 9113) implementation, usable as client or server over any
//! `Read`/`Write` transport: frame codec, SETTINGS negotiation, HPACK, stream multiplexing,
//! connection- and stream-level flow control (both directions), PING, GOAWAY, RST_STREAM and
//! CONTINUATION handling.
//!
//! Transport security (TLS/ALPN `h2`) is NOT implemented: without external crates there is no
//! TLS stack, so only cleartext prior-knowledge HTTP/2 (h2c) is supported. The `Conn::new`
//! constructor takes boxed reader/writer halves so a TLS stream could be slotted in later.

use crate::hpack::{Decoder, Encoder, Header};
use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

pub const T_DATA: u8 = 0;
pub const T_HEADERS: u8 = 1;
pub const T_PRIORITY: u8 = 2;
pub const T_RST_STREAM: u8 = 3;
pub const T_SETTINGS: u8 = 4;
pub const T_PUSH_PROMISE: u8 = 5;
pub const T_PING: u8 = 6;
pub const T_GOAWAY: u8 = 7;
pub const T_WINDOW_UPDATE: u8 = 8;
pub const T_CONTINUATION: u8 = 9;

pub const F_END_STREAM: u8 = 0x1;
pub const F_ACK: u8 = 0x1;
pub const F_END_HEADERS: u8 = 0x4;
pub const F_PADDED: u8 = 0x8;
pub const F_PRIORITY: u8 = 0x20;

pub const ERR_NO_ERROR: u32 = 0;
pub const ERR_PROTOCOL: u32 = 1;
pub const ERR_FLOW_CONTROL: u32 = 3;
pub const ERR_FRAME_SIZE: u32 = 6;
pub const ERR_CANCEL: u32 = 8;

const MAX_WINDOW: i64 = 0x7fff_ffff;

#[derive(Debug, Clone)]
pub struct Frame {
    pub ty: u8,
    pub flags: u8,
    pub stream: u32,
    pub payload: Vec<u8>,
}

pub fn encode_frame(ty: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(9 + payload.len());
    let len = payload.len() as u32;
    out.extend_from_slice(&[(len >> 16) as u8, (len >> 8) as u8, len as u8, ty, flags]);
    out.extend_from_slice(&(stream & 0x7fff_ffff).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn read_frame<R: Read + ?Sized>(r: &mut R, max_frame: usize) -> io::Result<Frame> {
    let mut h = [0u8; 9];
    r.read_exact(&mut h)?;
    let len = ((h[0] as usize) << 16) | ((h[1] as usize) << 8) | h[2] as usize;
    if len > max_frame {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame size {len} exceeds max {max_frame}"),
        ));
    }
    let stream = u32::from_be_bytes([h[5], h[6], h[7], h[8]]) & 0x7fff_ffff;
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok(Frame { ty: h[3], flags: h[4], stream, payload })
}

/// Local settings advertised to the peer.
#[derive(Debug, Clone)]
pub struct LocalSettings {
    pub initial_window: u32,
    pub max_frame: u32,
    pub max_concurrent_streams: u32,
    /// Extra connection-level window granted right after the preface.
    pub connection_window: u32,
}

impl Default for LocalSettings {
    fn default() -> Self {
        Self {
            initial_window: 1 << 20,
            max_frame: 16384,
            max_concurrent_streams: 1000,
            connection_window: 16 << 20,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Client,
    Server,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Headers(Vec<Header>),
    Data(Vec<u8>),
    Trailers(Vec<Header>),
    /// Remote half closed cleanly.
    End,
    /// RST_STREAM with error code.
    Reset(u32),
}

struct StreamState {
    send_window: i64,
    events: VecDeque<(Event, u32)>,
    got_headers: bool,
    remote_ended: bool,
    local_ended: bool,
    reset: bool,
}

struct State {
    conn_send_window: i64,
    peer_initial_window: i64,
    peer_max_frame: usize,
    peer_max_concurrent: u32,
    streams: HashMap<u32, StreamState>,
    next_id: u32,
    active_local: u32,
    reserved: u32,
    closed: Option<String>,
    goaway: bool,
}

struct Writer {
    out: Box<dyn Write + Send>,
    enc: Encoder,
}

struct Shared {
    role: Role,
    local: LocalSettings,
    state: Mutex<State>,
    cv: Condvar,
    writer: Mutex<Writer>,
    pending: Mutex<Vec<u8>>,
    shutdown: Box<dyn Fn() + Send + Sync>,
    incoming: Mutex<Option<Sender<Stream>>>,
    /// Connection-level receive window as seen by the peer (decremented by DATA, restored by
    /// our WINDOW_UPDATE frames).
    recv_conn_window: AtomicI64,
}

impl Shared {
    fn closed_err(st: &State) -> io::Error {
        io::Error::new(
            io::ErrorKind::ConnectionAborted,
            st.closed.clone().unwrap_or_else(|| "connection closed".into()),
        )
    }

    fn fail(&self, msg: String) {
        {
            let mut st = self.state.lock().unwrap();
            if st.closed.is_none() {
                st.closed = Some(msg);
            }
        }
        self.cv.notify_all();
        (self.shutdown)();
    }

    /// Queues a control frame and opportunistically flushes without ever blocking the caller on
    /// the writer lock (avoids reader/writer deadlocks).
    fn send_control(&self, bytes: Vec<u8>) {
        self.pending.lock().unwrap().extend_from_slice(&bytes);
        self.flush_control();
    }

    fn flush_control(&self) {
        loop {
            if self.pending.lock().unwrap().is_empty() {
                return;
            }
            match self.writer.try_lock() {
                Ok(mut w) => {
                    let data = std::mem::take(&mut *self.pending.lock().unwrap());
                    if !data.is_empty() && (w.out.write_all(&data).and_then(|_| w.out.flush())).is_err() {
                        drop(w);
                        self.fail("write failed".into());
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    }

    fn write_locked(&self, w: &mut Writer, bytes: &[u8]) -> io::Result<()> {
        let pending = std::mem::take(&mut *self.pending.lock().unwrap());
        let r = (|| {
            if !pending.is_empty() {
                w.out.write_all(&pending)?;
            }
            w.out.write_all(bytes)?;
            w.out.flush()
        })();
        if let Err(e) = &r {
            self.fail(format!("write failed: {e}"));
        }
        r
    }

    fn write_headers_block(
        &self,
        w: &mut Writer,
        id: u32,
        headers: &[Header],
        end_stream: bool,
        max_frame: usize,
    ) -> io::Result<()> {
        let block = w.enc.encode(headers);
        let mut out = Vec::new();
        let mut chunks: Vec<&[u8]> = block.chunks(max_frame.max(1)).collect();
        if chunks.is_empty() {
            chunks.push(&[]);
        }
        for (i, c) in chunks.iter().enumerate() {
            let last = i == chunks.len() - 1;
            if i == 0 {
                let mut flags = if last { F_END_HEADERS } else { 0 };
                if end_stream {
                    flags |= F_END_STREAM;
                }
                out.extend(encode_frame(T_HEADERS, flags, id, c));
            } else {
                out.extend(encode_frame(
                    T_CONTINUATION,
                    if last { F_END_HEADERS } else { 0 },
                    id,
                    c,
                ));
            }
        }
        self.write_locked(w, &out)
    }

    fn window_update(&self, stream: u32, n: u32) {
        if n > 0 {
            if stream == 0 {
                self.recv_conn_window.fetch_add(n as i64, Ordering::SeqCst);
            }
            self.send_control(encode_frame(T_WINDOW_UPDATE, 0, stream, &n.to_be_bytes()));
        }
    }
}

/// An HTTP/2 connection (client or server side).
#[derive(Clone)]
pub struct Conn {
    sh: Arc<Shared>,
}

impl Conn {
    pub fn client_from(t: crate::transport::Transport, local: LocalSettings) -> io::Result<Conn> {
        Self::client(t.reader, t.writer, t.shutdown, local)
    }

    pub fn server_from(
        t: crate::transport::Transport,
        local: LocalSettings,
    ) -> io::Result<(Conn, Receiver<Stream>)> {
        Self::server(t.reader, t.writer, t.shutdown, local)
    }

    /// Creates a client connection (sends the preface and SETTINGS).
    pub fn client(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        shutdown: Box<dyn Fn() + Send + Sync>,
        local: LocalSettings,
    ) -> io::Result<Conn> {
        Self::start(Role::Client, reader, writer, shutdown, local, None)
    }

    /// Creates a server connection; accepted streams are delivered on the returned receiver once
    /// their request HEADERS arrived.
    pub fn server(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        shutdown: Box<dyn Fn() + Send + Sync>,
        local: LocalSettings,
    ) -> io::Result<(Conn, Receiver<Stream>)> {
        let (tx, rx) = mpsc::channel();
        let c = Self::start(Role::Server, reader, writer, shutdown, local, Some(tx))?;
        Ok((c, rx))
    }

    fn start(
        role: Role,
        reader: Box<dyn Read + Send>,
        mut writer: Box<dyn Write + Send>,
        shutdown: Box<dyn Fn() + Send + Sync>,
        local: LocalSettings,
        incoming: Option<Sender<Stream>>,
    ) -> io::Result<Conn> {
        let mut init = Vec::new();
        if role == Role::Client {
            init.extend_from_slice(PREFACE);
        }
        let mut settings = Vec::new();
        for (id, v) in [
            (1u16, 4096u32),
            (2, 0),
            (3, local.max_concurrent_streams),
            (4, local.initial_window),
            (5, local.max_frame),
        ] {
            settings.extend_from_slice(&id.to_be_bytes());
            settings.extend_from_slice(&v.to_be_bytes());
        }
        init.extend(encode_frame(T_SETTINGS, 0, 0, &settings));
        if local.connection_window > 0 {
            init.extend(encode_frame(
                T_WINDOW_UPDATE,
                0,
                0,
                &local.connection_window.to_be_bytes(),
            ));
        }
        writer.write_all(&init)?;
        writer.flush()?;
        let sh = Arc::new(Shared {
            role,
            local: local.clone(),
            state: Mutex::new(State {
                conn_send_window: 65535,
                peer_initial_window: 65535,
                peer_max_frame: 16384,
                peer_max_concurrent: u32::MAX,
                streams: HashMap::new(),
                next_id: 1,
                active_local: 0,
                reserved: 0,
                closed: None,
                goaway: false,
            }),
            cv: Condvar::new(),
            writer: Mutex::new(Writer { out: writer, enc: Encoder::new() }),
            pending: Mutex::new(Vec::new()),
            shutdown,
            incoming: Mutex::new(incoming),
            recv_conn_window: AtomicI64::new(65535 + local.connection_window as i64),
        });
        let sh2 = sh.clone();
        std::thread::Builder::new()
            .name("zel-h2-reader".into())
            .spawn(move || reader_loop(sh2, reader))?;
        Ok(Conn { sh })
    }

    pub fn is_closed(&self) -> bool {
        let st = self.sh.state.lock().unwrap();
        st.closed.is_some() || st.goaway
    }

    /// Number of locally initiated streams currently open.
    pub fn active_streams(&self) -> usize {
        self.sh.state.lock().unwrap().active_local as usize
    }

    pub fn close(&self) {
        self.sh.send_control(encode_frame(T_GOAWAY, 0, 0, &[0, 0, 0, 0, 0, 0, 0, 0]));
        self.sh.fail("connection closed locally".into());
    }

    /// Opens a new client stream by sending request HEADERS.
    pub fn open_stream(&self, headers: &[Header], end_stream: bool) -> io::Result<Stream> {
        let sh = &self.sh;
        // Reserve a concurrency slot before taking the writer lock.
        {
            let mut st = sh.state.lock().unwrap();
            loop {
                if st.closed.is_some() || st.goaway {
                    return Err(Shared::closed_err(&st));
                }
                if st.active_local + st.reserved < st.peer_max_concurrent {
                    st.reserved += 1;
                    break;
                }
                st = sh.cv.wait(st).unwrap();
            }
        }
        let mut w = sh.writer.lock().unwrap();
        let (id, max_frame) = {
            let mut st = sh.state.lock().unwrap();
            st.reserved -= 1;
            if st.closed.is_some() || st.goaway {
                return Err(Shared::closed_err(&st));
            }
            let id = st.next_id;
            st.next_id += 2;
            st.active_local += 1;
            let win = st.peer_initial_window;
            st.streams.insert(
                id,
                StreamState {
                    send_window: win,
                    events: VecDeque::new(),
                    got_headers: false,
                    remote_ended: false,
                    local_ended: end_stream,
                    reset: false,
                },
            );
            (id, st.peer_max_frame)
        };
        let r = sh.write_headers_block(&mut w, id, headers, end_stream, max_frame);
        drop(w);
        sh.flush_control();
        if let Err(e) = r {
            let mut st = sh.state.lock().unwrap();
            st.streams.remove(&id);
            st.active_local -= 1;
            return Err(e);
        }
        Ok(Stream { sh: sh.clone(), id })
    }
}

/// One HTTP/2 stream.
pub struct Stream {
    sh: Arc<Shared>,
    id: u32,
}

impl Stream {
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Sends (response or trailing) HEADERS on this stream.
    pub fn send_headers(&self, headers: &[Header], end_stream: bool) -> io::Result<()> {
        let sh = &self.sh;
        let mut w = sh.writer.lock().unwrap();
        let max_frame = {
            let mut st = sh.state.lock().unwrap();
            if st.closed.is_some() {
                return Err(Shared::closed_err(&st));
            }
            if let Some(s) = st.streams.get_mut(&self.id) {
                if s.reset {
                    return Err(io::Error::new(io::ErrorKind::BrokenPipe, "stream reset"));
                }
                if end_stream {
                    s.local_ended = true;
                }
            }
            st.peer_max_frame
        };
        let r = sh.write_headers_block(&mut w, self.id, headers, end_stream, max_frame);
        drop(w);
        sh.flush_control();
        r?;
        self.maybe_cleanup();
        Ok(())
    }

    /// Sends DATA respecting stream and connection flow control; blocks while the window is 0.
    pub fn send_data(&self, data: &[u8], end_stream: bool, timeout: Option<Duration>) -> io::Result<()> {
        let sh = &self.sh;
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut off = 0usize;
        loop {
            let (n, max_frame_unused) = {
                let mut st = sh.state.lock().unwrap();
                loop {
                    if st.closed.is_some() {
                        return Err(Shared::closed_err(&st));
                    }
                    let (win, reset) = match st.streams.get(&self.id) {
                        Some(s) => (s.send_window, s.reset || s.remote_ended && s.local_ended),
                        None => return Err(io::Error::new(io::ErrorKind::BrokenPipe, "stream gone")),
                    };
                    if reset {
                        return Err(io::Error::new(io::ErrorKind::BrokenPipe, "stream reset by peer"));
                    }
                    let remaining = data.len() - off;
                    if remaining == 0 {
                        break (0usize, 0usize);
                    }
                    let avail = win.min(st.conn_send_window).min(st.peer_max_frame as i64);
                    if avail > 0 {
                        let n = (avail as usize).min(remaining);
                        st.conn_send_window -= n as i64;
                        st.streams.get_mut(&self.id).unwrap().send_window -= n as i64;
                        break (n, st.peer_max_frame);
                    }
                    st = match deadline {
                        None => sh.cv.wait(st).unwrap(),
                        Some(d) => {
                            let now = Instant::now();
                            if now >= d {
                                return Err(io::Error::new(io::ErrorKind::TimedOut, "flow control wait timed out"));
                            }
                            sh.cv.wait_timeout(st, d - now).unwrap().0
                        }
                    };
                }
            };
            let _ = max_frame_unused;
            let last = off + n == data.len();
            if n == 0 && !(end_stream && last) {
                return Ok(());
            }
            let flags = if end_stream && last { F_END_STREAM } else { 0 };
            let frame = encode_frame(T_DATA, flags, self.id, &data[off..off + n]);
            {
                let mut w = sh.writer.lock().unwrap();
                sh.write_locked(&mut w, &frame)?;
            }
            sh.flush_control();
            off += n;
            if last {
                if end_stream {
                    let mut st = sh.state.lock().unwrap();
                    if let Some(s) = st.streams.get_mut(&self.id) {
                        s.local_ended = true;
                    }
                    drop(st);
                    self.maybe_cleanup();
                }
                return Ok(());
            }
        }
    }

    /// Receives the next event (blocking until `deadline`).
    pub fn recv(&self, deadline: Option<Instant>) -> io::Result<Event> {
        let sh = &self.sh;
        let mut st = sh.state.lock().unwrap();
        loop {
            if let Some(s) = st.streams.get_mut(&self.id) {
                if let Some((ev, credit)) = s.events.pop_front() {
                    drop(st);
                    if credit > 0 {
                        sh.window_update(self.id, credit);
                        sh.window_update(0, credit);
                    }
                    if matches!(ev, Event::End | Event::Reset(_)) {
                        self.maybe_cleanup();
                    }
                    return Ok(ev);
                }
            } else {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "stream gone"));
            }
            if st.closed.is_some() {
                return Err(Shared::closed_err(&st));
            }
            st = match deadline {
                None => sh.cv.wait(st).unwrap(),
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "deadline exceeded"));
                    }
                    sh.cv.wait_timeout(st, d - now).unwrap().0
                }
            };
        }
    }

    pub fn reset(&self, code: u32) {
        self.sh.send_control(encode_frame(T_RST_STREAM, 0, self.id, &code.to_be_bytes()));
        self.remove();
    }

    fn remove(&self) {
        let mut st = self.sh.state.lock().unwrap();
        if st.streams.remove(&self.id).is_some() && self.sh.role == Role::Client {
            st.active_local -= 1;
        }
        drop(st);
        self.sh.cv.notify_all();
    }

    /// Removes the stream once both directions finished (or after reset).
    fn maybe_cleanup(&self) {
        let done = {
            let st = self.sh.state.lock().unwrap();
            match st.streams.get(&self.id) {
                Some(s) => (s.remote_ended && s.events.is_empty() && s.local_ended) || (s.reset && s.events.is_empty()),
                None => false,
            }
        };
        if done {
            self.remove();
        }
    }

    /// Stops sending (if the response already ended) so the stream can be reclaimed.
    pub fn abandon_if_remote_done(&self) {
        let rem = {
            let st = self.sh.state.lock().unwrap();
            st.streams.get(&self.id).map(|s| s.remote_ended && !s.local_ended)
        };
        if rem == Some(true) {
            self.reset(ERR_NO_ERROR);
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let exists = self.sh.state.lock().unwrap().streams.contains_key(&self.id);
        if exists && self.sh.state.lock().unwrap().closed.is_none() {
            self.sh
                .send_control(encode_frame(T_RST_STREAM, 0, self.id, &ERR_CANCEL.to_be_bytes()));
        }
        if exists {
            self.remove();
        }
    }
}

fn conn_error(sh: &Shared, code: u32, msg: &str) -> io::Error {
    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&code.to_be_bytes());
    payload.extend_from_slice(msg.as_bytes());
    sh.send_control(encode_frame(T_GOAWAY, 0, 0, &payload));
    io::Error::new(io::ErrorKind::InvalidData, format!("h2 connection error: {msg}"))
}

fn reader_loop(sh: Arc<Shared>, mut r: Box<dyn Read + Send>) {
    let res = (|| -> io::Result<()> {
        if sh.role == Role::Server {
            let mut p = [0u8; 24];
            r.read_exact(&mut p)?;
            if p != PREFACE {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "bad client preface"));
            }
        }
        let max_frame = sh.local.max_frame as usize;
        let mut dec = Decoder::new(4096);
        // (stream, block, end_stream) while awaiting CONTINUATION.
        let mut hb: Option<(u32, Vec<u8>, bool)> = None;
        loop {
            let f = read_frame(&mut r, max_frame)?;
            if let Some((sid, _, _)) = &hb {
                if f.ty != T_CONTINUATION || f.stream != *sid {
                    return Err(conn_error(&sh, ERR_PROTOCOL, "expected CONTINUATION"));
                }
            }
            match f.ty {
                T_DATA => {
                    if f.stream == 0 {
                        return Err(conn_error(&sh, ERR_PROTOCOL, "DATA on stream 0"));
                    }
                    let total = f.payload.len() as u32;
                    if sh.recv_conn_window.fetch_sub(total as i64, Ordering::SeqCst) - (total as i64) < 0 {
                        return Err(conn_error(&sh, ERR_FLOW_CONTROL, "connection window exceeded"));
                    }
                    let mut data = &f.payload[..];
                    if f.flags & F_PADDED != 0 {
                        let pad = *data.first().ok_or_else(|| conn_error(&sh, ERR_FRAME_SIZE, "bad pad"))? as usize;
                        if pad + 1 > data.len() {
                            return Err(conn_error(&sh, ERR_PROTOCOL, "padding too long"));
                        }
                        data = &data[1..data.len() - pad];
                    }
                    let data = data.to_vec();
                    let mut st = sh.state.lock().unwrap();
                    let mut credit_now = total;
                    if let Some(s) = st.streams.get_mut(&f.stream) {
                        if !data.is_empty() {
                            credit_now = total - data.len() as u32;
                            let credit = data.len() as u32;
                            s.events.push_back((Event::Data(data), credit));
                        }
                        if f.flags & F_END_STREAM != 0 {
                            s.remote_ended = true;
                            s.events.push_back((Event::End, 0));
                        }
                    }
                    drop(st);
                    sh.cv.notify_all();
                    // Padding / unknown-stream bytes are credited immediately; payload bytes are
                    // credited when the consumer reads them (backpressure).
                    sh.window_update(0, credit_now);
                }
                T_HEADERS => {
                    if f.stream == 0 {
                        return Err(conn_error(&sh, ERR_PROTOCOL, "HEADERS on stream 0"));
                    }
                    let mut p = &f.payload[..];
                    if f.flags & F_PADDED != 0 {
                        let pad = *p.first().ok_or_else(|| conn_error(&sh, ERR_FRAME_SIZE, "bad pad"))? as usize;
                        if pad + 1 > p.len() {
                            return Err(conn_error(&sh, ERR_PROTOCOL, "padding too long"));
                        }
                        p = &p[1..p.len() - pad];
                    }
                    if f.flags & F_PRIORITY != 0 {
                        if p.len() < 5 {
                            return Err(conn_error(&sh, ERR_FRAME_SIZE, "short priority"));
                        }
                        p = &p[5..];
                    }
                    let end_stream = f.flags & F_END_STREAM != 0;
                    if f.flags & F_END_HEADERS != 0 {
                        handle_headers(&sh, &mut dec, f.stream, p, end_stream)?;
                    } else {
                        hb = Some((f.stream, p.to_vec(), end_stream));
                    }
                }
                T_CONTINUATION => {
                    let (sid, mut block, end_stream) = hb.take().ok_or_else(|| conn_error(&sh, ERR_PROTOCOL, "unexpected CONTINUATION"))?;
                    block.extend_from_slice(&f.payload);
                    if f.flags & F_END_HEADERS != 0 {
                        handle_headers(&sh, &mut dec, sid, &block, end_stream)?;
                    } else {
                        hb = Some((sid, block, end_stream));
                    }
                }
                T_PRIORITY => {}
                T_RST_STREAM => {
                    if f.payload.len() != 4 {
                        return Err(conn_error(&sh, ERR_FRAME_SIZE, "bad RST_STREAM"));
                    }
                    let code = u32::from_be_bytes([f.payload[0], f.payload[1], f.payload[2], f.payload[3]]);
                    let mut st = sh.state.lock().unwrap();
                    if let Some(s) = st.streams.get_mut(&f.stream) {
                        s.reset = true;
                        s.events.push_back((Event::Reset(code), 0));
                    }
                    drop(st);
                    sh.cv.notify_all();
                }
                T_SETTINGS => {
                    if f.flags & F_ACK != 0 {
                        continue;
                    }
                    if f.payload.len() % 6 != 0 {
                        return Err(conn_error(&sh, ERR_FRAME_SIZE, "bad SETTINGS"));
                    }
                    let mut st = sh.state.lock().unwrap();
                    let mut table_size = None;
                    for c in f.payload.chunks(6) {
                        let id = u16::from_be_bytes([c[0], c[1]]);
                        let v = u32::from_be_bytes([c[2], c[3], c[4], c[5]]);
                        match id {
                            1 => table_size = Some(v as usize),
                            3 => st.peer_max_concurrent = v,
                            4 => {
                                if v as i64 > MAX_WINDOW {
                                    drop(st);
                                    return Err(conn_error(&sh, ERR_FLOW_CONTROL, "bad initial window"));
                                }
                                let delta = v as i64 - st.peer_initial_window;
                                st.peer_initial_window = v as i64;
                                for s in st.streams.values_mut() {
                                    s.send_window += delta;
                                }
                            }
                            5 => {
                                if !(16384..=16_777_215).contains(&v) {
                                    drop(st);
                                    return Err(conn_error(&sh, ERR_PROTOCOL, "bad max frame size"));
                                }
                                st.peer_max_frame = v as usize;
                            }
                            _ => {}
                        }
                    }
                    drop(st);
                    if let Some(n) = table_size {
                        sh.writer.lock().unwrap().enc.set_peer_max_table_size(n);
                    }
                    sh.cv.notify_all();
                    sh.send_control(encode_frame(T_SETTINGS, F_ACK, 0, &[]));
                }
                T_PUSH_PROMISE => return Err(conn_error(&sh, ERR_PROTOCOL, "PUSH_PROMISE not enabled")),
                T_PING => {
                    if f.flags & F_ACK == 0 {
                        sh.send_control(encode_frame(T_PING, F_ACK, 0, &f.payload));
                    }
                }
                T_GOAWAY => {
                    let last = if f.payload.len() >= 4 {
                        u32::from_be_bytes([f.payload[0], f.payload[1], f.payload[2], f.payload[3]]) & 0x7fff_ffff
                    } else {
                        0
                    };
                    let mut st = sh.state.lock().unwrap();
                    st.goaway = true;
                    // Streams the server never processed fail; they are safe to retry.
                    for (id, s) in st.streams.iter_mut() {
                        if *id % 2 == 1 && *id > last && !s.remote_ended {
                            s.reset = true;
                            s.events.push_back((Event::Reset(ERR_CANCEL), 0));
                        }
                    }
                    drop(st);
                    sh.cv.notify_all();
                }
                T_WINDOW_UPDATE => {
                    if f.payload.len() != 4 {
                        return Err(conn_error(&sh, ERR_FRAME_SIZE, "bad WINDOW_UPDATE"));
                    }
                    let inc = (u32::from_be_bytes([f.payload[0], f.payload[1], f.payload[2], f.payload[3]]) & 0x7fff_ffff) as i64;
                    if inc == 0 {
                        return Err(conn_error(&sh, ERR_PROTOCOL, "zero window increment"));
                    }
                    let mut st = sh.state.lock().unwrap();
                    if f.stream == 0 {
                        st.conn_send_window += inc;
                        if st.conn_send_window > MAX_WINDOW {
                            drop(st);
                            return Err(conn_error(&sh, ERR_FLOW_CONTROL, "connection window overflow"));
                        }
                    } else if let Some(s) = st.streams.get_mut(&f.stream) {
                        s.send_window += inc;
                    }
                    drop(st);
                    sh.cv.notify_all();
                }
                _ => {}
            }
        }
    })();
    let msg = match res {
        Ok(()) => "connection closed".to_string(),
        Err(e) => e.to_string(),
    };
    sh.fail(msg);
}

fn handle_headers(
    sh: &Arc<Shared>,
    dec: &mut Decoder,
    stream: u32,
    block: &[u8],
    end_stream: bool,
) -> io::Result<()> {
    let headers = dec
        .decode(block)
        .map_err(|e| conn_error(sh, 9, &format!("HPACK: {e}")))?;
    let mut st = sh.state.lock().unwrap();
    let mut new_stream = false;
    if !st.streams.contains_key(&stream) {
        if sh.role == Role::Server && stream % 2 == 1 {
            let win = st.peer_initial_window;
            st.streams.insert(
                stream,
                StreamState {
                    send_window: win,
                    events: VecDeque::new(),
                    got_headers: false,
                    remote_ended: false,
                    local_ended: false,
                    reset: false,
                },
            );
            new_stream = true;
        } else {
            return Ok(()); // frames for streams we already closed
        }
    }
    let s = st.streams.get_mut(&stream).unwrap();
    if s.got_headers {
        s.events.push_back((Event::Trailers(headers), 0));
    } else {
        s.got_headers = true;
        s.events.push_back((Event::Headers(headers), 0));
    }
    if end_stream {
        s.remote_ended = true;
        s.events.push_back((Event::End, 0));
    }
    drop(st);
    sh.cv.notify_all();
    if new_stream {
        if let Some(tx) = sh.incoming.lock().unwrap().as_ref() {
            let _ = tx.send(Stream { sh: sh.clone(), id: stream });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    
    fn pair(
        client_settings: LocalSettings,
        server_settings: LocalSettings,
    ) -> (Conn, Conn, Receiver<Stream>) {
        let (c, s) = crate::transport::memory_pair();
        let (sconn, rx) = Conn::server_from(s, server_settings).unwrap();
        let cconn = Conn::client_from(c, client_settings).unwrap();
        (cconn, sconn, rx)
    }

    fn hdrs(v: &[(&str, &str)]) -> Vec<Header> {
        v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    fn collect_body(s: &Stream) -> Vec<u8> {
        let mut body = Vec::new();
        loop {
            match s.recv(Some(Instant::now() + Duration::from_secs(20))).unwrap() {
                Event::Data(d) => body.extend(d),
                Event::End => return body,
                Event::Reset(c) => panic!("reset {c}"),
                _ => {}
            }
        }
    }

    #[test]
    fn echo_with_small_windows_exercises_flow_control() {
        let small = LocalSettings { initial_window: 1000, max_frame: 16384, max_concurrent_streams: 100, connection_window: 0 };
        let (client, _server, rx) = pair(small.clone(), small);
        std::thread::spawn(move || {
            while let Ok(st) = rx.recv() {
                std::thread::spawn(move || {
                    let body = collect_body(&st);
                    st.send_headers(&hdrs(&[(":status", "200")]), false).unwrap();
                    st.send_data(&body, true, Some(Duration::from_secs(20))).unwrap();
                });
            }
        });
        let payload: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let s = client
            .open_stream(&hdrs(&[(":method", "POST"), (":path", "/echo"), (":scheme", "http"), (":authority", "x")]), false)
            .unwrap();
        s.send_data(&payload, true, Some(Duration::from_secs(20))).unwrap();
        match s.recv(Some(Instant::now() + Duration::from_secs(20))).unwrap() {
            Event::Headers(h) => assert_eq!(h[0], (":status".to_string(), "200".to_string())),
            e => panic!("{e:?}"),
        }
        assert_eq!(collect_body(&s), payload);
    }

    #[test]
    fn many_concurrent_streams_and_max_concurrency_limit() {
        let server = LocalSettings { max_concurrent_streams: 2, ..LocalSettings::default() };
        let (client, _server, rx) = pair(LocalSettings::default(), server);
        std::thread::spawn(move || {
            while let Ok(st) = rx.recv() {
                std::thread::spawn(move || {
                    let body = collect_body(&st);
                    st.send_headers(&hdrs(&[(":status", "200")]), false).unwrap();
                    st.send_data(&body, true, None).unwrap();
                });
            }
        });
        let mut threads = Vec::new();
        for i in 0..16u8 {
            let c = client.clone();
            threads.push(std::thread::spawn(move || {
                let s = c.open_stream(&hdrs(&[(":method", "POST"), (":path", "/e"), (":scheme", "http"), (":authority", "x")]), false).unwrap();
                s.send_data(&vec![i; 5000], true, None).unwrap();
                loop {
                    if let Event::Headers(_) = s.recv(Some(Instant::now() + Duration::from_secs(20))).unwrap() {
                        break;
                    }
                }
                assert_eq!(collect_body(&s), vec![i; 5000]);
            }));
        }
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(client.active_streams(), 0);
    }

    #[test]
    fn continuation_for_large_headers_and_trailers() {
        let (client, _server, rx) = pair(LocalSettings::default(), LocalSettings::default());
        std::thread::spawn(move || {
            let st = rx.recv().unwrap();
            let mut got = Vec::new();
            loop {
                match st.recv(None).unwrap() {
                    Event::Headers(h) => got = h,
                    Event::End => break,
                    _ => {}
                }
            }
            let big = got.iter().find(|h| h.0 == "x-big").unwrap().1.clone();
            st.send_headers(&hdrs(&[(":status", "200")]), false).unwrap();
            st.send_data(b"ok", false, None).unwrap();
            st.send_headers(&[("x-len".to_string(), big.len().to_string())], true).unwrap();
        });
        let big = "z".repeat(40_000);
        let s = client
            .open_stream(&[(":method".into(), "GET".into()), (":path".into(), "/".into()), (":scheme".into(), "http".into()), (":authority".into(), "x".into()), ("x-big".into(), big)], true)
            .unwrap();
        let mut trailers = None;
        let mut data = Vec::new();
        loop {
            match s.recv(Some(Instant::now() + Duration::from_secs(20))).unwrap() {
                Event::Data(d) => data.extend(d),
                Event::Trailers(t) => trailers = Some(t),
                Event::End => break,
                _ => {}
            }
        }
        assert_eq!(data, b"ok");
        assert_eq!(trailers.unwrap()[0], ("x-len".to_string(), "40000".to_string()));
    }

    #[test]
    fn rst_stream_and_connection_failure_surface_as_errors() {
        let (client, server, rx) = pair(LocalSettings::default(), LocalSettings::default());
        let s = client
            .open_stream(&hdrs(&[(":method", "POST"), (":path", "/"), (":scheme", "http"), (":authority", "x")]), false)
            .unwrap();
        let ss = rx.recv().unwrap();
        ss.reset(ERR_CANCEL);
        match s.recv(Some(Instant::now() + Duration::from_secs(10))).unwrap() {
            Event::Reset(c) => assert_eq!(c, ERR_CANCEL),
            e => panic!("{e:?}"),
        }
        server.close();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !client.is_closed() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(client.is_closed());
        assert!(client.open_stream(&hdrs(&[(":method", "GET")]), true).is_err());
    }

    #[test]
    fn frame_codec_roundtrip() {
        let b = encode_frame(T_PING, F_ACK, 0, &[1, 2, 3, 4, 5, 6, 7, 8]);
        let f = read_frame(&mut &b[..], 16384).unwrap();
        assert_eq!((f.ty, f.flags, f.stream, f.payload.len()), (T_PING, F_ACK, 0, 8));
        assert!(read_frame(&mut &encode_frame(T_DATA, 0, 1, &[0; 100])[..], 50).is_err());
    }
}
