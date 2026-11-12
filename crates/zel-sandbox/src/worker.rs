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

//! Persistent workers (`src/main/java/com/google/devtools/build/lib/worker`): `WorkerParser`
//! (flag-file splitting, `--persistent_worker`, key creation), `WorkerKey`, `WorkerPool`
//! (per-key pools bounded by `--worker_max_instances`), singleplex and multiplex workers
//! (`WorkerMultiplexer`), proto (varint-delimited) and JSON protocols, worker sandboxing and
//! `WorkerSpawnRunner`.

use crate::{ExecutedCommand, Spawn, SpawnResult, SpawnStatus};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use zel_proto::worker_protocol::{Input, WorkRequest, WorkResponse};
use zel_proto::wire;

// ---------------------------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------------------------

/// `WorkerOptions` (the parts that affect execution).
#[derive(Debug, Clone)]
pub struct WorkerOptions {
    /// `--worker_max_instances` entries (`""` is the default key).
    pub max_instances: Vec<(String, usize)>,
    pub max_multiplex_instances: Vec<(String, usize)>,
    pub verbose: bool,
    pub quit_after_build: bool,
    /// `--worker_sandboxing` is a boolean; `--experimental_worker_...` per-mnemonic not modeled.
    pub sandboxing: bool,
    pub multiplex: bool,
    pub multiplex_sandboxing: bool,
    pub strict_flagfiles: bool,
    /// `--worker_extra_flag=Mnemonic=flag`.
    pub extra_flags: Vec<(String, String)>,
    pub cancellation: bool,
}

impl Default for WorkerOptions {
    fn default() -> Self {
        Self {
            max_instances: Vec::new(),
            max_multiplex_instances: Vec::new(),
            verbose: false,
            quit_after_build: false,
            sandboxing: false,
            // `--worker_multiplex` defaults to true.
            multiplex: true,
            multiplex_sandboxing: false,
            strict_flagfiles: false,
            extra_flags: Vec::new(),
            cancellation: false,
        }
    }
}

/// `--worker_max_instances` / `--worker_max_multiplex_instances` value `[name=]value`.
/// (`Converter`: `WorkerOptions.MultiResourceConverter`.)
pub fn parse_instances_flag(input: &str) -> Result<(String, usize), String> {
    let (name, value) = match input.rsplit_once('=') {
        Some((n, v)) => (n.to_string(), v),
        None => (String::new(), input),
    };
    let n: usize = match value {
        "auto" => 4,
        v => v.parse().map_err(|_| {
            format!("Illegal value for worker instances: '{input}'. Must be an integer or [name=]integer")
        })?,
    };
    Ok((name, n))
}

impl WorkerOptions {
    pub fn max_instances_for(&self, mnemonic: &str) -> usize {
        lookup(&self.max_instances, mnemonic, 4)
    }
    pub fn max_multiplex_instances_for(&self, mnemonic: &str) -> usize {
        lookup(&self.max_multiplex_instances, mnemonic, 8)
    }
}

fn lookup(v: &[(String, usize)], mnemonic: &str, default: usize) -> usize {
    // Later flags override earlier ones (last value of the multimap entry wins).
    v.iter()
        .rev()
        .find(|(n, _)| n == mnemonic)
        .or_else(|| v.iter().rev().find(|(n, _)| n.is_empty()))
        .map(|(_, c)| *c)
        .unwrap_or(default)
}

// ---------------------------------------------------------------------------------------------
// Worker key & parser
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtocolFormat {
    Proto,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkerKey {
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub exec_root: String,
    pub mnemonic: String,
    pub worker_files_hash: String,
    pub sandboxed: bool,
    pub multiplex: bool,
    pub cancellable: bool,
    pub protocol: ProtocolFormat,
}

const ERROR_MESSAGE_PREFIX: &str = "Worker strategy cannot execute this ";
const REASON_NO_FLAGFILE: &str = "because the command-line arguments do not contain exactly one @flagfile or --flagfile=";
const REASON_EXCESS_FLAGFILE: &str =
    "because the command-line arguments has a @flagfile or --flagfile= argument before the end";
const REASON_NO_FINAL_FLAGFILE: &str =
    "because the command-line arguments does not end with a @flagfile or --flagfile= argument";

/// `(?:@|--?flagfile=)(.+)` full match.
fn flag_file_arg(arg: &str) -> bool {
    for p in ["@", "-flagfile=", "--flagfile="] {
        if let Some(rest) = arg.strip_prefix(p) {
            return !rest.is_empty();
        }
    }
    false
}

fn throw_flag_file_failure(reason: &str, spawn: &Spawn) -> String {
    format!(
        "{ERROR_MESSAGE_PREFIX}{} action, {reason}\n{}",
        spawn.mnemonic,
        java_list(&spawn.argv)
    )
}

/// `List.toString()`.
fn java_list(v: &[String]) -> String {
    format!("[{}]", v.join(", "))
}

/// `WorkerParser.splitSpawnArgsIntoWorkerArgsAndFlagFiles`: returns `(worker_args, flag_files)`.
pub fn split_spawn_args(spawn: &Spawn, opts: &WorkerOptions) -> Result<(Vec<String>, Vec<String>), String> {
    let args = &spawn.argv;
    let mut worker_args = Vec::new();
    let mut flag_files = Vec::new();
    if args.is_empty() {
        return Err(throw_flag_file_failure(REASON_NO_FLAGFILE, spawn));
    }
    let is_ff = |a: &str| flag_file_arg(a) && !a.starts_with("@@");
    if opts.strict_flagfiles {
        let last = args.last().unwrap();
        if !is_ff(last) {
            return Err(throw_flag_file_failure(REASON_NO_FINAL_FLAGFILE, spawn));
        }
        flag_files.push(last.clone());
        for a in &args[..args.len() - 1] {
            if is_ff(a) {
                return Err(throw_flag_file_failure(REASON_EXCESS_FLAGFILE, spawn));
            }
            worker_args.push(a.clone());
        }
    } else {
        for a in args {
            if flag_file_arg(a) {
                flag_files.push(a.clone());
            } else {
                worker_args.push(a.clone());
            }
        }
        if flag_files.is_empty() {
            return Err(throw_flag_file_failure(REASON_NO_FLAGFILE, spawn));
        }
    }
    worker_args.push("--persistent_worker".to_string());
    let key_mnemonic = worker_key_mnemonic(spawn);
    for (m, f) in &opts.extra_flags {
        if *m == key_mnemonic {
            worker_args.push(f.clone());
        }
    }
    Ok((worker_args, flag_files))
}

/// `Spawns.getWorkerKeyMnemonic`: the `worker-key-mnemonic` requirement or the mnemonic.
pub fn worker_key_mnemonic(spawn: &Spawn) -> String {
    spawn
        .execution_info
        .get("worker-key-mnemonic")
        .cloned()
        .unwrap_or_else(|| spawn.mnemonic.clone())
}

pub fn supports_workers(info: &BTreeMap<String, String>) -> bool {
    info.get("supports-workers").map(|v| v == "1").unwrap_or(false)
}
pub fn supports_multiplex_workers(info: &BTreeMap<String, String>) -> bool {
    info.get("supports-multiplex-workers").map(|v| v == "1").unwrap_or(false)
}
pub fn supports_multiplex_sandboxing(info: &BTreeMap<String, String>) -> bool {
    info.get("supports-multiplex-sandboxing").map(|v| v == "1").unwrap_or(false)
}
pub fn supports_worker_cancellation(info: &BTreeMap<String, String>) -> bool {
    info.get("supports-worker-cancellation").map(|v| v == "1").unwrap_or(false)
}
/// `Spawns.getWorkerProtocolFormat`: `requires-worker-protocol` = `json` | `proto`.
pub fn worker_protocol_format(info: &BTreeMap<String, String>) -> Result<ProtocolFormat, String> {
    match info.get("requires-worker-protocol").map(String::as_str) {
        None | Some("proto") => Ok(ProtocolFormat::Proto),
        Some("json") => Ok(ProtocolFormat::Json),
        Some(o) => Err(format!("{o} is not a supported worker protocol")),
    }
}

/// `WorkerParser.createWorkerKey`.
#[allow(clippy::too_many_arguments)]
pub fn create_worker_key(
    spawn: &Spawn,
    worker_args: Vec<String>,
    env: BTreeMap<String, String>,
    exec_root: &str,
    files_hash: String,
    opts: &WorkerOptions,
    dynamic: bool,
    protocol: ProtocolFormat,
) -> WorkerKey {
    let mnemonic = worker_key_mnemonic(spawn);
    let must_sandbox = dynamic;
    let should_multiplex = opts.multiplex && supports_multiplex_workers(&spawn.execution_info);
    let can_sandbox_multiplex = opts.multiplex_sandboxing && supports_multiplex_sandboxing(&spawn.execution_info);
    let (sandboxed, multiplex) = if must_sandbox {
        (true, should_multiplex && can_sandbox_multiplex)
    } else if should_multiplex {
        (can_sandbox_multiplex, true)
    } else {
        (opts.sandboxing, false)
    };
    WorkerKey {
        args: worker_args,
        env,
        exec_root: exec_root.to_string(),
        mnemonic,
        worker_files_hash: files_hash,
        sandboxed,
        multiplex,
        cancellable: supports_worker_cancellation(&spawn.execution_info),
        protocol,
    }
}

// ---------------------------------------------------------------------------------------------
// Message framing
// ---------------------------------------------------------------------------------------------

/// Proto: `writeDelimitedTo` (varint length prefix).
pub fn write_delimited(w: &mut dyn Write, payload: &[u8]) -> std::io::Result<()> {
    let mut buf = Vec::new();
    wire::encode_varint(payload.len() as u64, &mut buf);
    buf.extend_from_slice(payload);
    w.write_all(&buf)?;
    w.flush()
}

/// Proto: `parseDelimitedFrom`; `Ok(None)` at clean EOF.
pub fn read_delimited(r: &mut dyn Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut len: u64 = 0;
    let mut shift = 0;
    let mut first = true;
    loop {
        let mut b = [0u8; 1];
        match r.read(&mut b)? {
            0 => {
                return if first {
                    Ok(None)
                } else {
                    Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "truncated varint"))
                }
            }
            _ => {}
        }
        first = false;
        len |= ((b[0] & 0x7f) as u64) << shift;
        if b[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift >= 64 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "varint overflow"));
        }
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

// -- minimal JSON (enough for protobuf JSON of the worker messages) --

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

pub fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

struct JParser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> JParser<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && (self.b[self.i] as char).is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        match self.b.get(self.i) {
            None => Err("unexpected end of JSON".into()),
            Some(b'{') => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    self.ws();
                    if self.b.get(self.i) == Some(&b'}') {
                        self.i += 1;
                        return Ok(Json::Obj(v));
                    }
                    if !v.is_empty() {
                        if self.b.get(self.i) != Some(&b',') {
                            return Err("expected ','".into());
                        }
                        self.i += 1;
                        self.ws();
                    }
                    let k = match self.value()? {
                        Json::Str(s) => s,
                        _ => return Err("expected string key".into()),
                    };
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return Err("expected ':'".into());
                    }
                    self.i += 1;
                    v.push((k, self.value()?));
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    self.ws();
                    if self.b.get(self.i) == Some(&b']') {
                        self.i += 1;
                        return Ok(Json::Arr(v));
                    }
                    if !v.is_empty() {
                        if self.b.get(self.i) != Some(&b',') {
                            return Err("expected ','".into());
                        }
                        self.i += 1;
                    }
                    v.push(self.value()?);
                }
            }
            Some(b'"') => {
                self.i += 1;
                let mut out: Vec<u8> = Vec::new();
                loop {
                    let c = *self.b.get(self.i).ok_or("unterminated string")?;
                    self.i += 1;
                    match c {
                        b'"' => break,
                        b'\\' => {
                            let e = *self.b.get(self.i).ok_or("bad escape")?;
                            self.i += 1;
                            match e {
                                b'n' => out.push(b'\n'),
                                b'r' => out.push(b'\r'),
                                b't' => out.push(b'\t'),
                                b'b' => out.push(8),
                                b'f' => out.push(12),
                                b'u' => {
                                    let h = std::str::from_utf8(self.b.get(self.i..self.i + 4).ok_or("bad \\u")?)
                                        .map_err(|e| e.to_string())?;
                                    let mut cp = u32::from_str_radix(h, 16).map_err(|e| e.to_string())?;
                                    self.i += 4;
                                    if (0xD800..0xDC00).contains(&cp) && self.b.get(self.i) == Some(&b'\\') {
                                        let h2 = std::str::from_utf8(self.b.get(self.i + 2..self.i + 6).ok_or("bad \\u")?)
                                            .map_err(|e| e.to_string())?;
                                        let lo = u32::from_str_radix(h2, 16).map_err(|e| e.to_string())?;
                                        cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                        self.i += 6;
                                    }
                                    let ch = char::from_u32(cp).unwrap_or('\u{fffd}');
                                    let mut tmp = [0u8; 4];
                                    out.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
                                }
                                o => out.push(o),
                            }
                        }
                        c => out.push(c),
                    }
                }
                Ok(Json::Str(String::from_utf8_lossy(&out).into_owned()))
            }
            Some(b't') if self.b[self.i..].starts_with(b"true") => {
                self.i += 4;
                Ok(Json::Bool(true))
            }
            Some(b'f') if self.b[self.i..].starts_with(b"false") => {
                self.i += 5;
                Ok(Json::Bool(false))
            }
            Some(b'n') if self.b[self.i..].starts_with(b"null") => {
                self.i += 4;
                Ok(Json::Null)
            }
            Some(_) => {
                let s = self.i;
                while self.i < self.b.len() && matches!(self.b[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    self.i += 1;
                }
                std::str::from_utf8(&self.b[s..self.i])
                    .ok()
                    .and_then(|t| t.parse().ok())
                    .map(Json::Num)
                    .ok_or_else(|| format!("invalid JSON at offset {s}"))
            }
        }
    }
}

pub fn parse_json(s: &str) -> Result<Json, String> {
    let mut p = JParser { b: s.as_bytes(), i: 0 };
    let v = p.value()?;
    p.ws();
    if p.i != s.len() {
        return Err("trailing characters after JSON value".into());
    }
    Ok(v)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut o = String::new();
    for ch in data.chunks(3) {
        let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
        o.push(B64[(n >> 18) as usize & 63] as char);
        o.push(B64[(n >> 12) as usize & 63] as char);
        o.push(if ch.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        o.push(if ch.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    o
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let v = B64.iter().position(|&x| x == c)? as u32;
        buf = buf << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// protobuf-JSON of a `WorkRequest` (`JsonFormat.printer().omittingInsignificantWhitespace()`,
/// default-valued fields omitted).
pub fn work_request_to_json(r: &WorkRequest) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !r.arguments.is_empty() {
        let a: Vec<String> = r.arguments.iter().map(|s| json_escape(s)).collect();
        parts.push(format!("\"arguments\":[{}]", a.join(",")));
    }
    if !r.inputs.is_empty() {
        let a: Vec<String> = r
            .inputs
            .iter()
            .map(|i| {
                let mut f = Vec::new();
                if !i.path.is_empty() {
                    f.push(format!("\"path\":{}", json_escape(&i.path)));
                }
                if !i.digest.is_empty() {
                    f.push(format!("\"digest\":\"{}\"", base64_encode(&i.digest)));
                }
                format!("{{{}}}", f.join(","))
            })
            .collect();
        parts.push(format!("\"inputs\":[{}]", a.join(",")));
    }
    if r.request_id != 0 {
        parts.push(format!("\"requestId\":{}", r.request_id));
    }
    if r.cancel {
        parts.push("\"cancel\":true".into());
    }
    if r.verbosity != 0 {
        parts.push(format!("\"verbosity\":{}", r.verbosity));
    }
    if !r.sandbox_dir.is_empty() {
        parts.push(format!("\"sandboxDir\":{}", json_escape(&r.sandbox_dir)));
    }
    format!("{{{}}}", parts.join(","))
}

fn field<'a>(o: &'a [(String, Json)], camel: &str, snake: &str) -> Option<&'a Json> {
    o.iter().find(|(k, _)| k == camel || k == snake).map(|(_, v)| v)
}

fn json_int(j: &Json) -> i32 {
    match j {
        Json::Num(n) => *n as i32,
        Json::Str(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

/// Parses the JSON form of a `WorkResponse` (accepts camelCase and snake_case, like protobuf).
pub fn work_response_from_json(s: &str) -> Result<WorkResponse, String> {
    let Json::Obj(o) = parse_json(s)? else { return Err("WorkResponse must be a JSON object".into()) };
    let mut r = WorkResponse::default();
    if let Some(v) = field(&o, "exitCode", "exit_code") {
        r.exit_code = json_int(v);
    }
    if let Some(Json::Str(v)) = field(&o, "output", "output") {
        r.output = v.clone();
    }
    if let Some(v) = field(&o, "requestId", "request_id") {
        r.request_id = json_int(v);
    }
    if let Some(Json::Bool(b)) = field(&o, "wasCancelled", "was_cancelled") {
        r.was_cancelled = *b;
    }
    Ok(r)
}

pub fn work_request_from_json(s: &str) -> Result<WorkRequest, String> {
    let Json::Obj(o) = parse_json(s)? else { return Err("WorkRequest must be a JSON object".into()) };
    let mut r = WorkRequest::default();
    if let Some(Json::Arr(a)) = field(&o, "arguments", "arguments") {
        for x in a {
            if let Json::Str(s) = x {
                r.arguments.push(s.clone());
            }
        }
    }
    if let Some(Json::Arr(a)) = field(&o, "inputs", "inputs") {
        for x in a {
            if let Json::Obj(io) = x {
                let mut i = Input::default();
                if let Some(Json::Str(p)) = field(io, "path", "path") {
                    i.path = p.clone();
                }
                if let Some(Json::Str(d)) = field(io, "digest", "digest") {
                    i.digest = base64_decode(d).unwrap_or_default();
                }
                r.inputs.push(i);
            }
        }
    }
    if let Some(v) = field(&o, "requestId", "request_id") {
        r.request_id = json_int(v);
    }
    if let Some(Json::Bool(b)) = field(&o, "cancel", "cancel") {
        r.cancel = *b;
    }
    if let Some(v) = field(&o, "verbosity", "verbosity") {
        r.verbosity = json_int(v);
    }
    if let Some(Json::Str(s)) = field(&o, "sandboxDir", "sandbox_dir") {
        r.sandbox_dir = s.clone();
    }
    Ok(r)
}

/// Reads one complete top-level JSON object from a byte stream (messages are not delimited).
pub fn read_json_object(r: &mut dyn BufRead) -> std::io::Result<Option<String>> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    loop {
        let mut b = [0u8; 1];
        if r.read(&mut b)? == 0 {
            return if out.is_empty() {
                Ok(None)
            } else {
                Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "truncated JSON message"))
            };
        }
        let c = b[0];
        if depth == 0 && !in_str && c != b'{' {
            continue; // whitespace between messages
        }
        out.push(c);
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(Some(String::from_utf8_lossy(&out).into_owned()));
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Worker processes
// ---------------------------------------------------------------------------------------------

static WORKER_IDS: AtomicU64 = AtomicU64::new(1);

struct Io {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// A running worker process (`SingleplexWorker` / the process behind a `WorkerMultiplexer`).
pub struct WorkerProcess {
    pub id: u64,
    pub key: WorkerKey,
    pub log_file: PathBuf,
    pub exec_root: PathBuf,
    child: Mutex<Child>,
    io: Mutex<Option<Io>>,
    /// Multiplex: pending request id -> responder.
    pending: Arc<Mutex<HashMap<i32, Sender<Result<WorkResponse, String>>>>>,
    /// Number of `WorkRequest`s this process has served (`WorkerMetrics.actions_executed`).
    actions_executed: AtomicU64,
}

impl WorkerProcess {
    /// Starts the worker (stderr goes to `worker-<id>-<mnemonic>.log`, like Bazel).
    pub fn start(key: &WorkerKey, workers_dir: &Path, workspace_dir_name: &str) -> std::io::Result<Arc<WorkerProcess>> {
        let id = WORKER_IDS.fetch_add(1, Ordering::SeqCst);
        std::fs::create_dir_all(workers_dir)?;
        let log_file = workers_dir.join(format!("worker-{id}-{}.log", key.mnemonic));
        let exec_root = if key.sandboxed {
            let p = workers_dir.join(format!("worker-{id}-{}", key.mnemonic)).join("execroot").join(workspace_dir_name);
            std::fs::create_dir_all(&p)?;
            p
        } else {
            PathBuf::from(&key.exec_root)
        };
        let log = std::fs::File::create(&log_file)?;
        let mut cmd = Command::new(&key.args[0]);
        cmd.args(&key.args[1..])
            .current_dir(&exec_root)
            .env_clear()
            .envs(&key.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log));
        if !key.env.contains_key("PATH") {
            cmd.env("PATH", "/bin:/usr/bin:/usr/local/bin");
        }
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        let mut child = cmd.spawn()?;
        let io = Io { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()) };
        let w = Arc::new(WorkerProcess {
            id,
            key: key.clone(),
            log_file,
            exec_root,
            child: Mutex::new(child),
            io: Mutex::new(Some(io)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            actions_executed: AtomicU64::new(0),
        });
        if key.multiplex {
            // Reader thread: dispatches responses by request_id (`WorkerMultiplexer`).
            let io = w.io.lock().unwrap().take().unwrap();
            let Io { stdin, mut stdout } = io;
            *w.io.lock().unwrap() = None;
            let stdin = Arc::new(Mutex::new(stdin));
            MULTIPLEX_STDIN.lock().unwrap().insert(id, stdin);
            let pending = w.pending.clone();
            let proto = key.protocol;
            std::thread::spawn(move || loop {
                let msg = match read_response(&mut stdout, proto) {
                    Ok(Some(m)) => m,
                    Ok(None) | Err(_) => {
                        // Worker died: fail all waiting requests.
                        let mut p = pending.lock().unwrap();
                        for (_, tx) in p.drain() {
                            let _ = tx.send(Err("worker process terminated unexpectedly".to_string()));
                        }
                        return;
                    }
                };
                if let Some(tx) = pending.lock().unwrap().remove(&msg.request_id) {
                    let _ = tx.send(Ok(msg));
                }
            });
        }
        Ok(w)
    }

    pub fn pid(&self) -> u32 {
        self.child.lock().unwrap().id()
    }

    pub fn is_alive(&self) -> bool {
        matches!(self.child.lock().unwrap().try_wait(), Ok(None))
    }

    /// Sends `request` and waits for the matching response.
    pub fn exec(&self, request: &WorkRequest) -> Result<WorkResponse, String> {
        self.actions_executed.fetch_add(1, Ordering::Relaxed);
        if self.key.multiplex {
            let (tx, rx) = channel();
            self.pending.lock().unwrap().insert(request.request_id, tx);
            let stdin = MULTIPLEX_STDIN.lock().unwrap().get(&self.id).cloned().ok_or("worker stdin closed")?;
            {
                let mut s = stdin.lock().unwrap();
                write_request(&mut *s, request, self.key.protocol).map_err(|e| format!("could not send request: {e}"))?;
            }
            return rx.recv().map_err(|e| e.to_string())?;
        }
        let mut guard = self.io.lock().unwrap();
        let io = guard.as_mut().ok_or("worker not running")?;
        write_request(&mut io.stdin, request, self.key.protocol).map_err(|e| format!("could not send request: {e}"))?;
        match read_response(&mut io.stdout, self.key.protocol) {
            Ok(Some(r)) => Ok(r),
            Ok(None) => Err("worker process did not return a WorkResponse (it exited)".to_string()),
            Err(e) => Err(format!("worker process returned an unparseable WorkResponse: {e}")),
        }
    }

    /// Sends a cancel request (multiplex only: needs request ids).
    pub fn send_cancel(&self, request_id: i32) {
        if !self.key.cancellable {
            return;
        }
        let req = WorkRequest { request_id, cancel: true, ..Default::default() };
        if self.key.multiplex {
            if let Some(s) = MULTIPLEX_STDIN.lock().unwrap().get(&self.id).cloned() {
                let _ = write_request(&mut *s.lock().unwrap(), &req, self.key.protocol);
            }
        } else if let Some(io) = self.io.lock().unwrap().as_mut() {
            let _ = write_request(&mut io.stdin, &req, self.key.protocol);
        }
    }

    pub fn destroy(&self) {
        MULTIPLEX_STDIN.lock().unwrap().remove(&self.id);
        *self.io.lock().unwrap() = None;
        let mut c = self.child.lock().unwrap();
        crate::process_wrapper::signal_process_group(c.id(), crate::process_wrapper::SIGKILL);
        let _ = c.kill();
        let _ = c.wait();
    }

    /// Contents of the worker log (stderr of the worker process).
    pub fn log_contents(&self) -> String {
        std::fs::read_to_string(&self.log_file).unwrap_or_default()
    }
}

static MULTIPLEX_STDIN: std::sync::LazyLock<Mutex<HashMap<u64, Arc<Mutex<ChildStdin>>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn write_request(w: &mut dyn Write, r: &WorkRequest, proto: ProtocolFormat) -> std::io::Result<()> {
    match proto {
        ProtocolFormat::Proto => write_delimited(w, &r.encode_to_vec()),
        ProtocolFormat::Json => {
            w.write_all(work_request_to_json(r).as_bytes())?;
            w.flush()
        }
    }
}

fn read_response(r: &mut BufReader<ChildStdout>, proto: ProtocolFormat) -> std::io::Result<Option<WorkResponse>> {
    match proto {
        ProtocolFormat::Proto => match read_delimited(r)? {
            None => Ok(None),
            Some(b) => WorkResponse::decode(&b)
                .map(Some)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
        },
        ProtocolFormat::Json => match read_json_object(r)? {
            None => Ok(None),
            Some(s) => work_response_from_json(&s)
                .map(Some)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Worker pool
// ---------------------------------------------------------------------------------------------

/// One live persistent worker (`WorkerMetrics` without the memory stats Zel does not sample).
#[derive(Debug, Clone)]
pub struct WorkerMetricsRow {
    pub pid: u32,
    pub mnemonic: String,
    pub key_hash: u64,
    pub worker_id: u64,
    pub actions_executed: u64,
    pub sandboxed: bool,
    pub multiplex: bool,
}

struct KeyPool {
    idle: Vec<Arc<WorkerProcess>>,
    /// Singleplex: number of live workers; multiplex: number of active proxies.
    active: usize,
    /// Multiplex: the single shared process.
    shared: Option<Arc<WorkerProcess>>,
}

/// `WorkerPoolImpl`: per-key pools bounded by `--worker_max_instances`.
pub struct WorkerPool {
    opts: WorkerOptions,
    workers_dir: PathBuf,
    workspace_dir_name: String,
    state: Mutex<HashMap<WorkerKey, KeyPool>>,
    cv: Condvar,
    pub started: AtomicU64,
}

pub struct Borrowed {
    pub worker: Arc<WorkerProcess>,
}

impl WorkerPool {
    pub fn new(opts: WorkerOptions, workers_dir: &Path, workspace_dir_name: &str) -> Self {
        Self {
            opts,
            workers_dir: workers_dir.to_path_buf(),
            workspace_dir_name: workspace_dir_name.to_string(),
            state: Mutex::new(HashMap::new()),
            cv: Condvar::new(),
            started: AtomicU64::new(0),
        }
    }

    pub fn options(&self) -> &WorkerOptions {
        &self.opts
    }

    fn limit(&self, key: &WorkerKey) -> usize {
        let n = if key.multiplex {
            self.opts.max_multiplex_instances_for(&key.mnemonic)
        } else {
            self.opts.max_instances_for(&key.mnemonic)
        };
        n.max(1)
    }

    /// Blocks until a worker for `key` is available (starting one if under the limit).
    pub fn borrow(&self, key: &WorkerKey) -> std::io::Result<Borrowed> {
        let limit = self.limit(key);
        let mut st = self.state.lock().unwrap();
        loop {
            let kp = st.entry(key.clone()).or_insert_with(|| KeyPool { idle: Vec::new(), active: 0, shared: None });
            if key.multiplex {
                if kp.active < limit {
                    kp.active += 1;
                    if let Some(w) = &kp.shared {
                        if w.is_alive() {
                            return Ok(Borrowed { worker: w.clone() });
                        }
                    }
                    // Started under the pool lock so concurrent borrowers share one process.
                    return match WorkerProcess::start(key, &self.workers_dir, &self.workspace_dir_name) {
                        Ok(w) => {
                            self.started.fetch_add(1, Ordering::SeqCst);
                            kp.shared = Some(w.clone());
                            Ok(Borrowed { worker: w })
                        }
                        Err(e) => {
                            kp.active -= 1;
                            self.cv.notify_all();
                            Err(e)
                        }
                    };
                }
            } else {
                while let Some(w) = kp.idle.pop() {
                    if w.is_alive() {
                        return Ok(Borrowed { worker: w });
                    }
                    kp.active -= 1;
                }
                if kp.active < limit {
                    kp.active += 1;
                    drop(st);
                    return match WorkerProcess::start(key, &self.workers_dir, &self.workspace_dir_name) {
                        Ok(w) => {
                            self.started.fetch_add(1, Ordering::SeqCst);
                            Ok(Borrowed { worker: w })
                        }
                        Err(e) => {
                            let mut st2 = self.state.lock().unwrap();
                            st2.get_mut(key).unwrap().active -= 1;
                            self.cv.notify_all();
                            Err(e)
                        }
                    };
                }
            }
            st = self.cv.wait(st).unwrap();
        }
    }

    pub fn release(&self, b: Borrowed) {
        let mut st = self.state.lock().unwrap();
        if let Some(kp) = st.get_mut(&b.worker.key) {
            if b.worker.key.multiplex {
                kp.active -= 1;
            } else {
                kp.idle.push(b.worker);
            }
        }
        self.cv.notify_all();
    }

    /// `invalidateObject`: destroys a worker that failed.
    pub fn invalidate(&self, b: Borrowed) {
        let key = b.worker.key.clone();
        b.worker.destroy();
        let mut st = self.state.lock().unwrap();
        if let Some(kp) = st.get_mut(&key) {
            if key.multiplex {
                kp.active -= 1;
                kp.shared = None;
            } else {
                kp.active -= 1;
            }
        }
        self.cv.notify_all();
    }

    /// Number of live worker processes (for tests and `--worker_verbose`).
    pub fn live_workers(&self) -> usize {
        let st = self.state.lock().unwrap();
        st.values()
            .map(|kp| kp.idle.len() + if kp.shared.is_some() { 1 } else { 0 } + if kp.shared.is_none() { kp.active - kp.idle.len().min(kp.active) } else { 0 })
            .sum()
    }

    /// Live worker processes as `WorkerMetrics` rows (`WorkerProcessMetricsCollector.getLiveWorkerMetrics`).
    pub fn live_worker_metrics(&self) -> Vec<WorkerMetricsRow> {
        use std::hash::{Hash, Hasher};
        let st = self.state.lock().unwrap();
        let mut rows = Vec::new();
        for (key, kp) in st.iter() {
            let procs: Vec<&Arc<WorkerProcess>> = kp.idle.iter().chain(kp.shared.iter()).collect();
            for w in procs {
                if !w.is_alive() {
                    continue;
                }
                let mut h = std::collections::hash_map::DefaultHasher::new();
                key.hash(&mut h);
                rows.push(WorkerMetricsRow {
                    pid: w.pid(),
                    mnemonic: key.mnemonic.clone(),
                    key_hash: h.finish(),
                    worker_id: w.id,
                    actions_executed: w.actions_executed.load(Ordering::Relaxed),
                    sandboxed: key.sandboxed,
                    multiplex: key.multiplex,
                });
            }
        }
        rows.sort_by_key(|r| r.worker_id);
        rows
    }

    /// Stops all idle workers (`--worker_quit_after_build` / shutdown).
    pub fn shutdown(&self) {
        let mut st = self.state.lock().unwrap();
        for (_, kp) in st.drain() {
            for w in kp.idle {
                w.destroy();
            }
            if let Some(w) = kp.shared {
                w.destroy();
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// WorkerSpawnRunner
// ---------------------------------------------------------------------------------------------

/// `WorkerSpawnRunner.expandArgument`: `@file` arguments are replaced by the lines of the file
/// (which must be a declared input), recursively; `@@` escapes and external repo labels
/// (`@repo//x`) are left alone.
pub fn expand_argument(
    exec_root: &Path,
    declared_inputs: &[String],
    arg: &str,
    out: &mut Vec<String>,
) -> std::io::Result<()> {
    let is_external = arg.starts_with('@') && (arg.contains("//") || arg.starts_with("@@"));
    if arg.starts_with('@') && !arg.starts_with("@@") && !is_external {
        let value = &arg[1..];
        if !declared_inputs.iter().any(|i| i == value) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to read @-argument '{value}': file is not a declared input"),
            ));
        }
        let path = exec_root.join(value);
        let text = std::fs::read_to_string(&path).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("Failed to read @-argument '{value}' from file '{}'.", path.display()),
            )
        })?;
        for line in text.lines() {
            expand_argument(exec_root, declared_inputs, line, out)?;
        }
    } else {
        out.push(arg.to_string());
    }
    Ok(())
}

/// Executes spawns through the worker pool (`WorkerSpawnRunner`).
pub struct WorkerSpawnRunner {
    pub exec_root: PathBuf,
    pub pool: Arc<WorkerPool>,
    pub client_env: BTreeMap<String, String>,
    request_ids: AtomicI32,
    /// Digest of a file's contents as the hex bytes used in `WorkRequest.inputs[].digest`.
    pub digest: fn(&[u8]) -> String,
}

impl WorkerSpawnRunner {
    pub fn new(exec_root: &Path, pool: Arc<WorkerPool>, client_env: BTreeMap<String, String>, digest: fn(&[u8]) -> String) -> Self {
        Self { exec_root: exec_root.to_path_buf(), pool, client_env, request_ids: AtomicI32::new(1), digest }
    }

    /// `canExec`: the spawn supports workers (and may not be forced local-only... ).
    pub fn can_exec(&self, spawn: &Spawn) -> bool {
        supports_workers(&spawn.execution_info) || supports_multiplex_workers(&spawn.execution_info)
    }

    /// Digests of all declared inputs (for `WorkRequest.inputs`) and the combined hash over the
    /// *tool* inputs only (`WorkerFilesHash.getCombinedHash`), which is part of the worker key.
    fn files_hash(&self, spawn: &Spawn) -> (String, Vec<(String, Vec<u8>)>) {
        let digest_of = |i: &zel_vfs::PathFragment| {
            std::fs::read(self.exec_root.join(i.path_string()))
                .ok()
                .map(|b| (i.path_string().to_string(), (self.digest)(&b).into_bytes()))
        };
        let mut request_inputs: Vec<(String, Vec<u8>)> = spawn.inputs.iter().filter_map(digest_of).collect();
        request_inputs.sort();
        let mut files: Vec<(String, Vec<u8>)> = spawn.tool_inputs.iter().filter_map(digest_of).collect();
        files.sort();
        let mut all = Vec::new();
        for (p, d) in &files {
            all.extend_from_slice(p.as_bytes());
            all.push(0);
            all.extend_from_slice(d);
            all.push(0);
        }
        ((self.digest)(&all), request_inputs)
    }

    pub fn exec(&self, spawn: &Spawn) -> std::io::Result<SpawnResult> {
        let started = std::time::Instant::now();
        let fail = |status, code, msg: String| SpawnResult {
            status,
            exit_code: code,
            stdout: Vec::new(),
            stderr: msg.into_bytes(),
            wall_time_ms: started.elapsed().as_millis() as u64,
            runner_name: "worker".into(),
            missing: Vec::new(),
            usage: None,
            executed_command: None,
        };
        let opts = self.pool.options();
        let (worker_args, flag_files) = match split_spawn_args(spawn, opts) {
            Ok(x) => x,
            Err(m) => return Ok(fail(SpawnStatus::ExecutionFailed, 1, m + "\n")),
        };
        let protocol = match worker_protocol_format(&spawn.execution_info) {
            Ok(p) => p,
            Err(m) => return Ok(fail(SpawnStatus::ExecutionFailed, 1, m + "\n")),
        };
        let missing: Vec<String> = spawn
            .inputs
            .iter()
            .filter(|i| std::fs::symlink_metadata(self.exec_root.join(i.path_string())).is_err())
            .map(|i| i.path_string().to_string())
            .collect();
        if !missing.is_empty() {
            let mut r = fail(SpawnStatus::MissingInputs, 1, String::new());
            r.missing = missing;
            return Ok(r);
        }
        let env = crate::sandbox::rewrite_local_env(&spawn.env, &self.client_env);
        let (hash, input_digests) = self.files_hash(spawn);
        let key = create_worker_key(
            spawn,
            worker_args,
            env.clone(),
            &self.exec_root.to_string_lossy(),
            hash,
            opts,
            false,
            protocol,
        );
        // Build the WorkRequest.
        let declared: Vec<String> = spawn.inputs.iter().map(|i| i.path_string().to_string()).collect();
        let mut arguments = Vec::new();
        for ff in &flag_files {
            expand_argument(&self.exec_root, &declared, ff, &mut arguments)?;
        }
        let mut request = WorkRequest {
            arguments,
            inputs: input_digests.iter().map(|(p, d)| Input { path: p.clone(), digest: d.clone() }).collect(),
            request_id: 0,
            cancel: false,
            verbosity: if opts.verbose { 10 } else { 0 },
            sandbox_dir: String::new(),
        };
        if key.multiplex {
            request.request_id = self.request_ids.fetch_add(1, Ordering::SeqCst);
        }
        // Stale outputs are removed and parent directories created (as for any spawn).
        for out in &spawn.outputs {
            let p = self.exec_root.join(out.path_string());
            if let Ok(md) = std::fs::symlink_metadata(&p) {
                if md.is_dir() {
                    let _ = std::fs::remove_dir_all(&p);
                } else {
                    let _ = std::fs::remove_file(&p);
                }
            }
            if let Some(par) = p.parent() {
                std::fs::create_dir_all(par)?;
            }
        }
        let b = match self.pool.borrow(&key) {
            Ok(b) => b,
            Err(e) => {
                return Ok(fail(
                    SpawnStatus::ExecutionFailed,
                    1,
                    format!("Could not start worker process '{}': {e}\n", key.args.join(" ")),
                ))
            }
        };
        // Worker sandboxing: inputs are symlinked into the worker's own execroot per request.
        let sandboxed_root = key.sandboxed.then(|| b.worker.exec_root.clone());
        if let Some(root) = &sandboxed_root {
            for i in &spawn.inputs {
                let dest = root.join(i.path_string());
                if let Some(p) = dest.parent() {
                    std::fs::create_dir_all(p)?;
                }
                if std::fs::symlink_metadata(&dest).is_err() {
                    std::os::unix::fs::symlink(self.exec_root.join(i.path_string()), &dest)?;
                }
            }
            for o in &spawn.outputs {
                if let Some(p) = root.join(o.path_string()).parent() {
                    std::fs::create_dir_all(p)?;
                }
            }
        }
        let resp = b.worker.exec(&request);
        let mut result;
        match resp {
            Ok(r) => {
                if let Some(root) = &sandboxed_root {
                    for o in &spawn.outputs {
                        let src = root.join(o.path_string());
                        let dst = self.exec_root.join(o.path_string());
                        if std::fs::symlink_metadata(&src).is_ok() {
                            let _ = std::fs::remove_file(&dst);
                            if let Some(p) = dst.parent() {
                                std::fs::create_dir_all(p)?;
                            }
                            if std::fs::rename(&src, &dst).is_err() {
                                std::fs::copy(&src, &dst)?;
                            }
                        }
                    }
                    // Clean the worker's execroot of this request's inputs.
                    for i in &spawn.inputs {
                        let _ = std::fs::remove_file(root.join(i.path_string()));
                    }
                }
                result = crate::worker_response_to_spawn_result(&r);
                self.pool.release(b);
            }
            Err(e) => {
                let log = b.worker.log_contents();
                self.pool.invalidate(b);
                let mut msg = format!("{e}\n");
                if !log.is_empty() {
                    msg.push_str("---8<---8<--- Start of log, file at ");
                    msg.push_str(&format!("{}\n{}---8<---8<--- End of log\n", key.mnemonic, log));
                }
                result = fail(SpawnStatus::ExecutionFailed, 1, msg);
            }
        }
        result.wall_time_ms = started.elapsed().as_millis() as u64;
        result.executed_command = Some(ExecutedCommand {
            argv: key.args.clone(),
            env,
            cwd: self.exec_root.to_string_lossy().into_owned(),
            debug_output: None,
        });
        if result.status == SpawnStatus::Success {
            let missing: Vec<String> = spawn
                .mandatory_outputs()
                .filter(|o| std::fs::symlink_metadata(self.exec_root.join(o.path_string())).is_err())
                .map(|o| o.path_string().to_string())
                .collect();
            if !missing.is_empty() {
                result.status = SpawnStatus::MissingOutputs;
                result.missing = missing;
            }
        }
        Ok(result)
    }
}

/// Convenience used by tests: waits at most `d` for a condition.
#[doc(hidden)]
pub fn wait_until(d: Duration, mut f: impl FnMut() -> bool) -> bool {
    let t = std::time::Instant::now();
    while t.elapsed() < d {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_vfs::PathFragment;

    fn sp(args: &[&str]) -> Spawn {
        Spawn {
            mnemonic: "Javac".into(),
            argv: args.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn splits_flagfiles_and_adds_persistent_worker() {
        let o = WorkerOptions::default();
        let (w, f) = split_spawn_args(&sp(&["java", "-jar", "x.jar", "@params"]), &o).unwrap();
        assert_eq!(w, ["java", "-jar", "x.jar", "--persistent_worker"]);
        assert_eq!(f, ["@params"]);
        let mut o2 = WorkerOptions::default();
        o2.extra_flags = vec![("Javac".into(), "--foo".into()), ("Other".into(), "--bar".into())];
        let (w, _) = split_spawn_args(&sp(&["tool", "--flagfile=p"]), &o2).unwrap();
        assert_eq!(w, ["tool", "--persistent_worker", "--foo"]);
    }

    #[test]
    fn flagfile_errors_use_bazel_messages() {
        let o = WorkerOptions::default();
        let e = split_spawn_args(&sp(&["tool", "a"]), &o).unwrap_err();
        assert_eq!(
            e,
            "Worker strategy cannot execute this Javac action, because the command-line arguments do not contain exactly one @flagfile or --flagfile=\n[tool, a]"
        );
        let mut strict = WorkerOptions::default();
        strict.strict_flagfiles = true;
        let e = split_spawn_args(&sp(&["tool", "@a", "b"]), &strict).unwrap_err();
        assert!(e.contains("does not end with a @flagfile or --flagfile= argument"));
        let e = split_spawn_args(&sp(&["tool", "@a", "@b"]), &strict).unwrap_err();
        assert!(e.contains("has a @flagfile or --flagfile= argument before the end"));
        assert!(split_spawn_args(&sp(&["tool", "@@esc", "@b"]), &strict).is_ok());
    }

    #[test]
    fn key_sandboxing_and_multiplex_decisions() {
        let mut s = sp(&["t", "@p"]);
        let mut o = WorkerOptions::default();
        let k = |s: &Spawn, o: &WorkerOptions, dynamic| {
            create_worker_key(s, vec![], BTreeMap::new(), "/er", "h".into(), o, dynamic, ProtocolFormat::Proto)
        };
        assert!(!k(&s, &o, false).multiplex && !k(&s, &o, false).sandboxed);
        s.execution_info.insert("supports-multiplex-workers".into(), "1".into());
        assert!(k(&s, &o, false).multiplex);
        assert!(!k(&s, &o, false).sandboxed);
        o.multiplex_sandboxing = true;
        assert!(!k(&s, &o, false).sandboxed, "needs supports-multiplex-sandboxing");
        s.execution_info.insert("supports-multiplex-sandboxing".into(), "1".into());
        assert!(k(&s, &o, false).sandboxed);
        o.multiplex = false;
        o.sandboxing = true;
        let key = k(&s, &o, false);
        assert!(!key.multiplex && key.sandboxed);
        assert!(k(&s, &o, true).sandboxed);
        s.execution_info.insert("worker-key-mnemonic".into(), "Special".into());
        assert_eq!(k(&s, &o, false).mnemonic, "Special");
    }

    #[test]
    fn instances_flag_parsing() {
        assert_eq!(parse_instances_flag("3").unwrap(), ("".to_string(), 3));
        assert_eq!(parse_instances_flag("Javac=2").unwrap(), ("Javac".to_string(), 2));
        assert!(parse_instances_flag("x").is_err());
        let o = WorkerOptions {
            max_instances: vec![("".into(), 6), ("Javac".into(), 2)],
            ..Default::default()
        };
        assert_eq!(o.max_instances_for("Javac"), 2);
        assert_eq!(o.max_instances_for("Other"), 6);
        assert_eq!(WorkerOptions::default().max_instances_for("X"), 4);
        assert_eq!(WorkerOptions::default().max_multiplex_instances_for("X"), 8);
    }

    #[test]
    fn json_protocol_round_trip_and_base64() {
        let r = WorkRequest {
            arguments: vec!["a\"b".into(), "c\n".into()],
            inputs: vec![Input { path: "p".into(), digest: vec![0xde, 0xad, 0xbe, 0xef] }],
            request_id: 5,
            cancel: false,
            verbosity: 10,
            sandbox_dir: "sbx".into(),
        };
        let j = work_request_to_json(&r);
        assert!(j.contains("\"requestId\":5") && j.contains("\"digest\":\"3q2+7w==\""), "{j}");
        assert_eq!(work_request_from_json(&j).unwrap(), r);
        let resp = work_response_from_json("{\"exit_code\": 2, \"output\": \"x\\u00e9\", \"requestId\": 5}").unwrap();
        assert_eq!((resp.exit_code, resp.output.as_str(), resp.request_id), (2, "xé", 5));
        assert_eq!(base64_decode(&base64_encode(b"hello!?")).unwrap(), b"hello!?");
        let mut rd = std::io::BufReader::new("  {\"a\":\"}\"}{\"b\":1}".as_bytes());
        assert_eq!(read_json_object(&mut rd).unwrap().unwrap(), "{\"a\":\"}\"}");
        assert_eq!(read_json_object(&mut rd).unwrap().unwrap(), "{\"b\":1}");
        assert!(read_json_object(&mut rd).unwrap().is_none());
    }

    #[test]
    fn delimited_framing_round_trip() {
        let mut buf = Vec::new();
        let msg = vec![7u8; 300];
        write_delimited(&mut buf, &msg).unwrap();
        write_delimited(&mut buf, b"").unwrap();
        let mut r: &[u8] = &buf;
        assert_eq!(read_delimited(&mut r).unwrap().unwrap(), msg);
        assert_eq!(read_delimited(&mut r).unwrap().unwrap(), b"");
        assert!(read_delimited(&mut r).unwrap().is_none());
    }

    #[test]
    fn expand_argument_recurses_and_requires_declared_inputs() {
        let d = std::env::temp_dir().join(format!("zel-wexp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("params"), "--a\n@inner\n@@lit\n").unwrap();
        std::fs::write(d.join("inner"), "--b\n").unwrap();
        let declared = vec!["params".to_string(), "inner".to_string()];
        let mut out = Vec::new();
        expand_argument(&d, &declared, "@params", &mut out).unwrap();
        assert_eq!(out, ["--a", "--b", "@@lit"]);
        let e = expand_argument(&d, &["params".to_string()], "@params", &mut Vec::new()).unwrap_err();
        assert!(e.to_string().contains("Failed to read @-argument 'inner'") || e.to_string().contains("not a declared input"), "{e}");
        let mut o = Vec::new();
        expand_argument(&d, &[], "@repo//x:y", &mut o).unwrap();
        assert_eq!(o, ["@repo//x:y"]);
        let _ = PathFragment::create("x");
    }
}
