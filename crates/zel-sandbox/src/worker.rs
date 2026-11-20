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

use crate::sandbox::{
    rewrite_local_env_for_current_os, SandboxContents, SandboxHelpers, SandboxInputs,
    SandboxOutputs,
};
use crate::{
    cancelled_error, delete_tree, delete_trees_below, ExecHooks, ExecutedCommand,
    RunfilesTreeUpdater, Spawn, SpawnResult, SpawnStatus,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use zel_proto::wire;
use zel_proto::worker_protocol::{Input, WorkRequest, WorkResponse};
use zel_vfs::PathFragment;

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
    /// `--experimental_worker_allowlist`: if non-empty, only mnemonics in this list may use workers.
    pub allowlist: Vec<String>,
    /// Track worker exec root contents in memory across requests (`SandboxedWorker`).
    pub use_in_memory_tracking: bool,
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
            allowlist: Vec::new(),
            use_in_memory_tracking: false,
        }
    }
}

/// `--worker_max_instances` / `--worker_max_multiplex_instances` value `[name=]value`
/// (`WorkerOptions.MultiResourceConverter`), returning `Ok(None)` when `"null"` or bare `"auto"`
/// is passed without `=` so the empty-key default is not overwritten.
pub fn parse_instances_flag_opt(
    input: &str,
    default_auto: usize,
) -> Result<Option<(String, usize)>, String> {
    if input == "null" || input == "auto" {
        return Ok(None);
    }
    let (name, value) = match input.split_once('=') {
        Some((n, v)) => (n.to_string(), v),
        None => (String::new(), input),
    };
    if value == "auto" || value == "null" {
        return Ok(Some((name, default_auto)));
    }
    let converted = zel_util::resource_converter::IntegerSpec::WORKER_VALUE
        .convert_detected(value)
        .map_err(|e| e.to_string())?;
    Ok(Some((name, usize::try_from(converted).unwrap_or(0))))
}

/// `--worker_max_instances` / `--worker_max_multiplex_instances` value `[name=]value`.
/// (`Converter`: `WorkerOptions.MultiResourceConverter`.)
pub fn parse_instances_flag(input: &str) -> Result<(String, usize), String> {
    parse_instances_flag_opt(input, 4).map(|opt| opt.unwrap_or_else(|| (String::new(), 4)))
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

#[derive(Debug, Clone)]
pub struct WorkerKey {
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub exec_root: String,
    pub mnemonic: String,
    /// Combined hash of tool inputs; kept out of pool identity so stale workers are invalidated
    /// and logged when tool files change on disk.
    pub worker_files_hash: String,
    /// Per-file digests of tool inputs (`WorkerKey.getWorkerFilesWithDigests()`), excluded from
    /// `Eq`/`Hash` and used to log exact file diffs when invalidating a pooled worker.
    pub worker_files_with_digests: BTreeMap<PathFragment, Vec<u8>>,
    pub sandboxed: bool,
    pub multiplex: bool,
    pub cancellable: bool,
    pub protocol: ProtocolFormat,
}

impl PartialEq for WorkerKey {
    fn eq(&self, other: &Self) -> bool {
        self.args == other.args
            && self.env == other.env
            && self.exec_root == other.exec_root
            && self.mnemonic == other.mnemonic
            && self.sandboxed == other.sandboxed
            && self.multiplex == other.multiplex
            && self.cancellable == other.cancellable
            && self.protocol == other.protocol
    }
}

impl Eq for WorkerKey {}

impl std::hash::Hash for WorkerKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.args.hash(state);
        self.env.hash(state);
        self.exec_root.hash(state);
        self.mnemonic.hash(state);
        self.sandboxed.hash(state);
        self.multiplex.hash(state);
        self.cancellable.hash(state);
        self.protocol.hash(state);
    }
}

const ERROR_MESSAGE_PREFIX: &str = "Worker strategy cannot execute this ";
const REASON_NO_FLAGFILE: &str =
    "because the command-line arguments do not contain exactly one @flagfile or --flagfile=";
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
pub fn split_spawn_args(
    spawn: &Spawn,
    opts: &WorkerOptions,
) -> Result<(Vec<String>, Vec<String>), String> {
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
    info.get("supports-workers")
        .map(|v| v == "1")
        .unwrap_or(false)
}
pub fn supports_multiplex_workers(info: &BTreeMap<String, String>) -> bool {
    info.get("supports-multiplex-workers")
        .map(|v| v == "1")
        .unwrap_or(false)
}
pub fn supports_multiplex_sandboxing(info: &BTreeMap<String, String>) -> bool {
    info.get("supports-multiplex-sandboxing")
        .map(|v| v == "1")
        .unwrap_or(false)
}
pub fn supports_worker_cancellation(info: &BTreeMap<String, String>) -> bool {
    info.get("supports-worker-cancellation")
        .map(|v| v == "1")
        .unwrap_or(false)
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
    let must_sandbox = dynamic || spawn.uses_path_mapping();
    let should_multiplex = opts.multiplex && supports_multiplex_workers(&spawn.execution_info);
    let can_sandbox_multiplex =
        opts.multiplex_sandboxing && supports_multiplex_sandboxing(&spawn.execution_info);
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
        worker_files_with_digests: BTreeMap::new(),
        sandboxed,
        multiplex,
        cancellable: opts.cancellation && supports_worker_cancellation(&spawn.execution_info),
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

/// Proto: `parseDelimitedFrom` (`CodedInputStream.readRawVarint32` 32-bit semantics);
/// `Ok(None)` at clean EOF, and a non-positive 32-bit length yields `Ok(Some(Vec::new()))`.
pub fn read_delimited(r: &mut dyn Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut len: u64 = 0;
    let mut shift = 0;
    let mut first = true;
    loop {
        let mut b = [0u8; 1];
        if r.read(&mut b)? == 0 {
            return if first {
                Ok(None)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "truncated varint",
                ))
            };
        }
        first = false;
        if shift < 35 {
            len |= ((b[0] & 0x7f) as u64) << shift;
        }
        if b[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift >= 70 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "varint overflow",
            ));
        }
    }
    let len32 = (len as u32) as i32;
    if len32 <= 0 {
        return Ok(Some(Vec::new()));
    }
    let mut buf = vec![0u8; len32 as usize];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

const RECORDING_MAX_BYTES: usize = 4096;

/// `RecordingInputStream`: captures up to `4096` raw bytes read while decoding a worker response
/// so parse failures include a printable UTF-8 or 64-line hex dump of the corrupt stream prefix.
struct RecordingReader<'a, R: ?Sized> {
    inner: &'a mut R,
    recorded: Vec<u8>,
}

impl<'a, R: Read + ?Sized> RecordingReader<'a, R> {
    fn new(inner: &'a mut R) -> Self {
        Self {
            inner,
            recorded: Vec::new(),
        }
    }

    fn formatted_data(&self) -> String {
        if self.recorded.is_empty() {
            return String::new();
        }
        if let Ok(utf8) = std::str::from_utf8(&self.recorded) {
            return utf8
                .chars()
                .map(|c| {
                    if (c as u32) < 0x20 && !matches!(c, '\n' | '\r' | '\t') {
                        '?'
                    } else {
                        c
                    }
                })
                .collect();
        }
        let mut out = String::new();
        for chunk in self.recorded.chunks(16).take(64) {
            for (idx, b) in chunk.iter().enumerate() {
                if idx > 0 {
                    out.push(' ');
                }
                out.push_str(&format!("{b:02x}"));
            }
            out.push('\n');
        }
        out
    }
}

impl<R: Read + ?Sized> Read for RecordingReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 && self.recorded.len() < RECORDING_MAX_BYTES {
            let take = (RECORDING_MAX_BYTES - self.recorded.len()).min(n);
            self.recorded.extend_from_slice(&buf[..take]);
        }
        Ok(n)
    }
}

impl<R: BufRead + ?Sized> BufRead for RecordingReader<'_, R> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.inner.fill_buf()
    }
    fn consume(&mut self, amt: usize) {
        if self.recorded.len() < RECORDING_MAX_BYTES {
            if let Ok(buf) = self.inner.fill_buf() {
                let take = (RECORDING_MAX_BYTES - self.recorded.len())
                    .min(amt)
                    .min(buf.len());
                self.recorded.extend_from_slice(&buf[..take]);
            }
        }
        self.inner.consume(amt);
    }
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

impl JParser<'_> {
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
                                    let h = std::str::from_utf8(
                                        self.b.get(self.i..self.i + 4).ok_or("bad \\u")?,
                                    )
                                    .map_err(|e| e.to_string())?;
                                    let mut cp =
                                        u32::from_str_radix(h, 16).map_err(|e| e.to_string())?;
                                    self.i += 4;
                                    if (0xD800..0xDC00).contains(&cp)
                                        && self.b.get(self.i) == Some(&b'\\')
                                    {
                                        let h2 = std::str::from_utf8(
                                            self.b.get(self.i + 2..self.i + 6).ok_or("bad \\u")?,
                                        )
                                        .map_err(|e| e.to_string())?;
                                        let lo = u32::from_str_radix(h2, 16)
                                            .map_err(|e| e.to_string())?;
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
                while self.i < self.b.len()
                    && matches!(
                        self.b[self.i],
                        b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                    )
                {
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
    let mut p = JParser {
        b: s.as_bytes(),
        i: 0,
    };
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
        let n = ((ch[0] as u32) << 16)
            | ((*ch.get(1).unwrap_or(&0) as u32) << 8)
            | (*ch.get(2).unwrap_or(&0) as u32);
        o.push(B64[(n >> 18) as usize & 63] as char);
        o.push(B64[(n >> 12) as usize & 63] as char);
        o.push(if ch.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        o.push(if ch.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
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
        buf = (buf << 6) | v;
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
    o.iter()
        .find(|(k, _)| k == camel || k == snake)
        .map(|(_, v)| v)
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
    let Json::Obj(o) = parse_json(s)? else {
        return Err("WorkResponse must be a JSON object".into());
    };
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
    let Json::Obj(o) = parse_json(s)? else {
        return Err("WorkRequest must be a JSON object".into());
    };
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
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "truncated JSON message",
                ))
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
static PROXY_IDS: AtomicI32 = AtomicI32::new(1);

#[cfg(unix)]
fn poll_fd_ready(fd: std::os::unix::io::RawFd) -> io::Result<bool> {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    const POLLIN: i16 = 0x0001;
    extern "C" {
        fn poll(fds: *mut PollFd, nfds: usize, timeout: i32) -> i32;
    }
    let mut pfd = PollFd {
        fd,
        events: POLLIN,
        revents: 0,
    };
    let rc = unsafe { poll(&mut pfd, 1, 0) };
    if rc < 0 {
        let e = io::Error::last_os_error();
        if e.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(e);
    }
    Ok(rc > 0 && pfd.revents != 0)
}

use std::io;

struct Io {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// `WorkerExecRoot`: creates and manages the working directory of a persistent worker.
pub struct WorkerExecRoot {
    pub work_dir: PathBuf,
    pub extra_dirs: Vec<PathFragment>,
    pub use_in_memory_tracking: bool,
    pub sandbox_contents: Option<SandboxContents>,
}

impl WorkerExecRoot {
    pub fn new(
        work_dir: PathBuf,
        extra_dirs: Vec<PathFragment>,
        use_in_memory_tracking: bool,
    ) -> Self {
        Self {
            work_dir,
            extra_dirs,
            use_in_memory_tracking,
            sandbox_contents: None,
        }
    }

    pub fn create_file_system(
        &mut self,
        worker_files: &BTreeSet<PathFragment>,
        inputs: &SandboxInputs,
        outputs: &SandboxOutputs,
        cancel: Option<&crate::process_wrapper::CancelFlag>,
    ) -> io::Result<()> {
        std::fs::create_dir_all(&self.work_dir)?;
        let mut inputs_to_create = BTreeSet::new();
        let mut dirs_to_create: BTreeSet<PathFragment> = self.extra_dirs.iter().cloned().collect();
        let all_inputs: Vec<&PathFragment> = worker_files
            .iter()
            .chain(inputs.files.keys())
            .chain(inputs.symlinks.keys())
            .collect();
        SandboxHelpers::populate_inputs_and_dirs_to_create(
            &BTreeSet::new(),
            &mut inputs_to_create,
            &mut dirs_to_create,
            all_inputs.into_iter(),
            outputs,
        );
        let mut merged_inputs = inputs.clone();
        let parent = self.work_dir.parent().unwrap_or(&self.work_dir);
        for wf in worker_files {
            if !merged_inputs.files.contains_key(wf) && !merged_inputs.symlinks.contains_key(wf) {
                let existing = self.work_dir.join(wf.path_string());
                if let Ok(target) = std::fs::read_link(&existing) {
                    merged_inputs
                        .symlinks
                        .insert(wf.clone(), PathFragment::create(&target.to_string_lossy()));
                }
            }
        }
        SandboxHelpers::clean_existing(
            parent,
            &merged_inputs,
            &mut inputs_to_create,
            &mut dirs_to_create,
            &self.work_dir,
            self.sandbox_contents.as_ref(),
            cancel,
        )?;
        SandboxHelpers::create_directories(&dirs_to_create, &self.work_dir, false, cancel)?;
        Self::create_inputs(&inputs_to_create, &merged_inputs, &self.work_dir, cancel)?;
        if self.use_in_memory_tracking {
            self.sandbox_contents = Some(SandboxHelpers::create_content_map(
                &self.work_dir,
                &merged_inputs,
                outputs,
            ));
        }
        Ok(())
    }

    pub fn create_inputs(
        inputs_to_create: &BTreeSet<PathFragment>,
        inputs: &SandboxInputs,
        dir: &Path,
        cancel: Option<&crate::process_wrapper::CancelFlag>,
    ) -> io::Result<()> {
        for fragment in inputs_to_create {
            if cancel.is_some_and(|c| c.is_cancelled()) {
                return Err(cancelled_error());
            }
            let key = dir.join(fragment.path_string());
            if let Some(par) = key.parent() {
                std::fs::create_dir_all(par)?;
            }
            if let Some(file_dest) = inputs.files.get(fragment) {
                match file_dest {
                    Some(src) => {
                        #[cfg(unix)]
                        std::os::unix::fs::symlink(src, &key)?;
                        #[cfg(not(unix))]
                        std::fs::copy(src, &key)?;
                    }
                    None => {
                        std::fs::write(&key, b"")?;
                    }
                }
            } else if let Some(symlink_dest) = inputs.symlinks.get(fragment) {
                #[cfg(unix)]
                std::os::unix::fs::symlink(symlink_dest.path_string(), &key)?;
                #[cfg(not(unix))]
                let _ = symlink_dest;
            }
        }
        Ok(())
    }

    pub fn copy_outputs(&self, exec_root: &Path, outputs: &SandboxOutputs) -> io::Result<()> {
        SandboxHelpers::move_outputs(outputs, &self.work_dir, exec_root)
    }
}

/// `WorkerFilesHash`: hashes tool files (expanding tree artifacts and runfiles trees, applying
/// `PathMapper`) for the worker key.
pub struct WorkerFilesHash;

impl WorkerFilesHash {
    pub fn get_worker_files_with_digests(
        spawn: &Spawn,
        exec_root: &Path,
        digest_fn: fn(&[u8]) -> Vec<u8>,
    ) -> Result<BTreeMap<PathFragment, Vec<u8>>, String> {
        let mut map = BTreeMap::new();
        for tool in &spawn.tool_inputs {
            if spawn.tool_tree_inputs.contains_key(tool)
                || spawn
                    .tool_runfiles_trees
                    .iter()
                    .any(|r| &r.exec_path == tool)
            {
                continue;
            }
            let p = exec_root.join(tool.path_string());
            let md = std::fs::metadata(&p)
                .map_err(|_| format!("Missing input metadata for: '{}'", tool.path_string()))?;
            let d = if md.is_dir() {
                Vec::new()
            } else {
                let bytes = std::fs::read(&p)
                    .map_err(|_| format!("Missing input metadata for: '{}'", tool.path_string()))?;
                digest_fn(&bytes)
            };
            map.insert(spawn.path_mapper.map(tool), d);
        }
        // Tree artifacts: expand children; empty tree artifacts are dropped.
        for (parent, children) in &spawn.tool_tree_inputs {
            if children.is_empty() {
                continue;
            }
            for child_rel in children {
                let exec_path = parent.get_relative(child_rel);
                let p = exec_root.join(exec_path.path_string());
                let bytes = std::fs::read(&p).map_err(|_| {
                    format!("Missing input metadata for: '{}'", exec_path.path_string())
                })?;
                let mapped = spawn.path_mapper.map(parent).get_relative(child_rel);
                map.insert(mapped, digest_fn(&bytes));
            }
        }
        // Runfiles trees: keepRunfilesTrees = true.
        for rf in &spawn.tool_runfiles_trees {
            if rf.exec_path.is_absolute() {
                return Err(format!(
                    "IllegalStateException: runfiles root must be relative: {}",
                    rf.exec_path
                ));
            }
            for (rel_key, target) in &rf.mapping {
                let Some(target) = target else { continue };
                match target {
                    crate::RunfilesTarget::File { exec_path, .. } => {
                        let p = exec_root.join(exec_path.path_string());
                        let md = std::fs::metadata(&p).map_err(|_| {
                            format!("Missing input metadata for: '{}'", exec_path.path_string())
                        })?;
                        if md.is_dir() {
                            continue;
                        }
                        let bytes = std::fs::read(&p).map_err(|_| {
                            format!("Missing input metadata for: '{}'", exec_path.path_string())
                        })?;
                        let mapped = crate::map_for_runfiles(
                            &spawn.path_mapper,
                            &rf.exec_path,
                            &rf.exec_path.get_relative(rel_key),
                        );
                        map.insert(mapped, digest_fn(&bytes));
                    }
                    crate::RunfilesTarget::TreeArtifact {
                        exec_path,
                        children,
                    } => {
                        for child_rel in children {
                            let child_exec = exec_path.get_relative(child_rel);
                            let p = exec_root.join(child_exec.path_string());
                            let bytes = std::fs::read(&p).map_err(|_| {
                                format!(
                                    "Missing input metadata for: '{}'",
                                    child_exec.path_string()
                                )
                            })?;
                            let mapped = crate::map_for_runfiles(
                                &spawn.path_mapper,
                                &rf.exec_path,
                                &rf.exec_path.get_relative(rel_key).get_relative(child_rel),
                            );
                            map.insert(mapped, digest_fn(&bytes));
                        }
                    }
                    crate::RunfilesTarget::Fileset { .. }
                    | crate::RunfilesTarget::NestedRunfilesTree => {}
                }
            }
        }
        Ok(map)
    }

    pub fn get_combined_hash(worker_files_map: &BTreeMap<PathFragment, Vec<u8>>) -> String {
        let mut buf = Vec::new();
        for (exec_path, digest) in worker_files_map {
            let path_bytes = exec_path.path_string().as_bytes();
            buf.push(0u8);
            buf.extend_from_slice(&(path_bytes.len() as i32).to_be_bytes());
            buf.extend_from_slice(path_bytes);
            buf.extend_from_slice(&(digest.len() as i32).to_be_bytes());
            buf.extend_from_slice(digest);
        }
        zel_util::sha256::hex_digest(&buf)
    }
}

/// In-process registry of cancelled persistent-worker request IDs, matching Bazel's
/// `com.google.devtools.build.buildjar.javac.WorkerCancellationRegistry`.
#[derive(Debug, Clone, Default)]
pub struct WorkerCancellationRegistry {
    cancelled_requests: Arc<Mutex<HashSet<i32>>>,
}

impl WorkerCancellationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_request(&self, request_id: i32) {
        self.cancelled_requests.lock().unwrap().insert(request_id);
    }

    pub fn check_if_request_is_cancelled(&self, request_id: i32) -> bool {
        self.cancelled_requests
            .lock()
            .unwrap()
            .contains(&request_id)
    }

    pub fn unregister_request(&self, request_id: i32) -> bool {
        self.cancelled_requests.lock().unwrap().remove(&request_id)
    }
}

/// A running worker process (`SingleplexWorker` / the process behind a `WorkerMultiplexer`).
pub struct WorkerProcess {
    pub id: u64,
    pub key: WorkerKey,
    pub log_file: PathBuf,
    pub exec_root: PathBuf,
    pub work_dir_root: Option<PathBuf>,
    child: Mutex<Child>,
    io: Mutex<Option<Io>>,
    /// Singleplex sandboxed root (`SandboxedWorker`).
    worker_exec_root: Mutex<Option<WorkerExecRoot>>,
    /// Multiplex sandboxed startup flag (`WorkerMultiplexer.createSandboxedProcess`).
    sandbox_initialized: Mutex<bool>,
    /// Multiplex: FIFO writer channel so concurrent requests and cancellations serialize in order.
    multiplex_tx: Mutex<Option<Sender<WorkRequest>>>,
    /// Multiplex: pending request id -> responder.
    #[allow(clippy::type_complexity)]
    pending: Arc<Mutex<HashMap<i32, Sender<Result<WorkResponse, String>>>>>,
    /// Multiplex: receiver stashed when `exec_with_cancel` exits early on cancellation so
    /// `drain_cancelled_response` can await the in-flight response without sending a duplicate request.
    #[allow(clippy::type_complexity)]
    cancelled_receivers: Mutex<HashMap<i32, Receiver<Result<WorkResponse, String>>>>,
    /// Marked true when the process or its reader thread has died/been destroyed.
    dead: Arc<AtomicBool>,
    /// Number of `WorkRequest`s this process has served (`WorkerMetrics.actions_executed`).
    actions_executed: AtomicU64,
}

impl WorkerProcess {
    /// Starts the worker (stderr goes to `worker-<id>-<mnemonic>.log`, like Bazel).
    pub fn start(
        key: &WorkerKey,
        workers_dir: &Path,
        workspace_dir_name: &str,
    ) -> std::io::Result<Arc<WorkerProcess>> {
        Self::start_with_tracking(key, workers_dir, workspace_dir_name, false)
    }

    pub fn start_with_tracking(
        key: &WorkerKey,
        workers_dir: &Path,
        workspace_dir_name: &str,
        use_in_memory_tracking: bool,
    ) -> std::io::Result<Arc<WorkerProcess>> {
        let id = WORKER_IDS.fetch_add(1, Ordering::SeqCst);
        std::fs::create_dir_all(workers_dir)?;
        let log_file = workers_dir.join(format!("worker-{id}-{}.log", key.mnemonic));
        let (exec_root, work_dir_root) = if key.sandboxed {
            let base = workers_dir.join(format!("worker-{id}-{}", key.mnemonic));
            let p = base.join("execroot").join(workspace_dir_name);
            std::fs::create_dir_all(&p)?;
            // Pre-stage tool files in the sandboxed root before spawning the worker binary so a
            // relative tool argv[0] or tool runfile exists at process startup.
            let real_er = Path::new(&key.exec_root);
            for wf in key.worker_files_with_digests.keys() {
                let src = real_er.join(wf.path_string());
                let dst = p.join(wf.path_string());
                if std::fs::symlink_metadata(&src).is_ok()
                    && std::fs::symlink_metadata(&dst).is_err()
                {
                    if let Some(par) = dst.parent() {
                        std::fs::create_dir_all(par)?;
                    }
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(&src, &dst)?;
                    #[cfg(not(unix))]
                    let _ = std::fs::copy(&src, &dst);
                }
            }
            (p, Some(base))
        } else {
            (PathBuf::from(&key.exec_root), None)
        };
        let log = std::fs::File::create(&log_file)?;
        let mut args = key.args.clone();
        if let Some(arg0) = args.first_mut() {
            let p = Path::new(arg0.as_str());
            if !p.is_absolute() && p.parent().is_some_and(|par| !par.as_os_str().is_empty()) {
                *arg0 = exec_root.join(p).to_string_lossy().into_owned();
            }
        }
        let mut cmd = Command::new(&args[0]);
        cmd.args(&args[1..])
            .current_dir(&exec_root)
            .env_clear()
            .envs(&key.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = {
            let mut attempt = 0u32;
            loop {
                match cmd.spawn() {
                    Ok(c) => break c,
                    Err(e) if e.raw_os_error() == Some(26) && attempt < 10 => {
                        attempt += 1;
                        std::thread::sleep(std::time::Duration::from_millis(5 * attempt as u64));
                    }
                    Err(e) => return Err(e),
                }
            }
        };
        let io = Io {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
        };
        let worker_exec_root = if key.sandboxed && !key.multiplex {
            let tmp_dir = SandboxHelpers::get_tmp_dir_path(&exec_root);
            let _ = std::fs::create_dir_all(&tmp_dir);
            Some(WorkerExecRoot::new(
                exec_root.clone(),
                vec![PathFragment::create(&tmp_dir.to_string_lossy())],
                use_in_memory_tracking,
            ))
        } else {
            None
        };
        let w = Arc::new(WorkerProcess {
            id,
            key: key.clone(),
            log_file,
            exec_root,
            work_dir_root,
            child: Mutex::new(child),
            io: Mutex::new(Some(io)),
            worker_exec_root: Mutex::new(worker_exec_root),
            sandbox_initialized: Mutex::new(false),
            multiplex_tx: Mutex::new(None),
            pending: Arc::new(Mutex::new(HashMap::new())),
            cancelled_receivers: Mutex::new(HashMap::new()),
            dead: Arc::new(AtomicBool::new(false)),
            actions_executed: AtomicU64::new(0),
        });
        if key.multiplex {
            // Dedicated FIFO writer thread + reader thread (`WorkerMultiplexer`).
            let io = w.io.lock().unwrap().take().unwrap();
            let Io {
                mut stdin,
                mut stdout,
            } = io;
            *w.io.lock().unwrap() = None;
            let (req_tx, req_rx) = channel::<WorkRequest>();
            *w.multiplex_tx.lock().unwrap() = Some(req_tx);
            let proto = key.protocol;
            let dead_writer = w.dead.clone();
            std::thread::spawn(move || {
                while let Ok(req) = req_rx.recv() {
                    if dead_writer.load(Ordering::SeqCst) {
                        break;
                    }
                    if write_request(&mut stdin, &req, proto).is_err() {
                        dead_writer.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            });
            let w_weak = Arc::downgrade(&w);
            let pending = w.pending.clone();
            let dead = w.dead.clone();
            std::thread::spawn(move || loop {
                let msg = match read_response(&mut stdout, proto) {
                    Ok(Some(m)) => m,
                    Ok(None) => {
                        dead.store(true, Ordering::SeqCst);
                        if let Some(w_arc) = w_weak.upgrade() {
                            *w_arc.multiplex_tx.lock().unwrap() = None;
                            let mut c = w_arc.child.lock().unwrap();
                            crate::process_wrapper::signal_process_group(
                                c.id(),
                                crate::process_wrapper::SIGKILL,
                            );
                            let _ = c.kill();
                            let _ = c.wait();
                        }
                        let mut p = pending.lock().unwrap();
                        for (_, tx) in p.drain() {
                            let _ =
                                tx.send(Err("Worker process terminated unexpectedly".to_string()));
                        }
                        return;
                    }
                    Err(e) => {
                        dead.store(true, Ordering::SeqCst);
                        if let Some(w_arc) = w_weak.upgrade() {
                            *w_arc.multiplex_tx.lock().unwrap() = None;
                            let mut c = w_arc.child.lock().unwrap();
                            crate::process_wrapper::signal_process_group(
                                c.id(),
                                crate::process_wrapper::SIGKILL,
                            );
                            let _ = c.kill();
                            let _ = c.wait();
                        }
                        let err_msg =
                            format!("worker process returned an unparseable WorkResponse: {e}");
                        let mut p = pending.lock().unwrap();
                        for (_, tx) in p.drain() {
                            let _ = tx.send(Err(err_msg.clone()));
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
        !self.dead.load(Ordering::SeqCst)
            && matches!(self.child.lock().unwrap().try_wait(), Ok(None))
    }

    /// Ensures a sandboxed multiplex worker initializes `work_dir` once with only `worker_files`
    /// (`WorkerMultiplexer.createSandboxedProcess`).
    pub fn ensure_sandboxed_multiplex_root(
        &self,
        worker_files: &BTreeSet<PathFragment>,
        inputs: &SandboxInputs,
    ) -> io::Result<()> {
        let mut init = self.sandbox_initialized.lock().unwrap();
        if *init {
            return Ok(());
        }
        std::fs::create_dir_all(&self.exec_root)?;
        delete_trees_below(&self.exec_root)?;
        let mut inputs_to_create = BTreeSet::new();
        let mut dirs_to_create = BTreeSet::new();
        let empty_outs = SandboxOutputs::default();
        SandboxHelpers::populate_inputs_and_dirs_to_create(
            &BTreeSet::new(),
            &mut inputs_to_create,
            &mut dirs_to_create,
            worker_files.iter(),
            &empty_outs,
        );
        let parent = self.exec_root.parent().unwrap_or(&self.exec_root);
        SandboxHelpers::clean_existing(
            parent,
            inputs,
            &mut inputs_to_create,
            &mut dirs_to_create,
            &self.exec_root,
            None,
            None,
        )?;
        SandboxHelpers::create_directories(&dirs_to_create, &self.exec_root, false, None)?;
        let limited = inputs.limited_copy(worker_files);
        WorkerExecRoot::create_inputs(&inputs_to_create, &limited, &self.exec_root, None)?;
        *init = true;
        Ok(())
    }

    /// Sends `request` and waits for the matching response.
    pub fn exec(&self, request: &WorkRequest) -> Result<WorkResponse, String> {
        self.exec_with_cancel(request, None)
    }

    /// Sends `request` and waits for the matching response, honoring cancellation.
    pub fn exec_with_cancel(
        &self,
        request: &WorkRequest,
        cancel: Option<&crate::process_wrapper::CancelFlag>,
    ) -> Result<WorkResponse, String> {
        self.actions_executed.fetch_add(1, Ordering::Relaxed);
        if self.key.multiplex {
            if !self.is_alive() {
                return Err(format!("Worker process for {} has died", self.key.mnemonic));
            }
            let (tx, rx) = channel();
            self.pending.lock().unwrap().insert(request.request_id, tx);
            let req_tx = self
                .multiplex_tx
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| "worker stdin closed".to_string())?;
            if let Err(e) = req_tx.send(request.clone()) {
                self.pending.lock().unwrap().remove(&request.request_id);
                return Err(format!("could not send request: {e}"));
            }
            loop {
                if cancel.is_some_and(|c| c.is_cancelled()) {
                    self.cancelled_receivers
                        .lock()
                        .unwrap()
                        .insert(request.request_id, rx);
                    return Err("cancelled".to_string());
                }
                match rx.recv_timeout(Duration::from_millis(10)) {
                    Ok(res) => return res,
                    Err(RecvTimeoutError::Timeout) => {
                        if !self.is_alive() {
                            self.pending.lock().unwrap().remove(&request.request_id);
                            return Err(format!(
                                "Worker process for {} has died",
                                self.key.mnemonic
                            ));
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err(format!("Worker process for {} has died", self.key.mnemonic));
                    }
                }
            }
        }
        let mut guard = self.io.lock().unwrap();
        let io = guard.as_mut().ok_or("worker not running")?;
        write_request(&mut io.stdin, request, self.key.protocol)
            .map_err(|e| format!("could not send request: {e}"))?;
        // SingleplexWorker.getResponse: poll every 10ms while stdout has no bytes; fail if child dies.
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            loop {
                if cancel.is_some_and(|c| c.is_cancelled()) {
                    return Err("cancelled".to_string());
                }
                if !io.stdout.buffer().is_empty() {
                    break;
                }
                let fd = io.stdout.get_ref().as_raw_fd();
                if poll_fd_ready(fd).unwrap_or(true) {
                    break;
                }
                if !self.is_alive() {
                    return Err(format!(
                        "Worker process for {} died while waiting for response",
                        self.key.mnemonic
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        match read_response(&mut io.stdout, self.key.protocol) {
            Ok(Some(r)) => Ok(r),
            Ok(None) => Err(format!(
                "Worker process for {} died while waiting for response",
                self.key.mnemonic
            )),
            Err(e) => Err(format!(
                "worker process returned an unparseable WorkResponse: {e}"
            )),
        }
    }

    /// Sends a cancel request (`WorkRequest { request_id, cancel: true }`).
    pub fn send_cancel(&self, request_id: i32) {
        if !self.key.cancellable {
            return;
        }
        let req = WorkRequest {
            request_id,
            cancel: true,
            ..Default::default()
        };
        if self.key.multiplex {
            if let Some(tx) = self.multiplex_tx.lock().unwrap().clone() {
                let _ = tx.send(req);
            }
        } else if let Some(io) = self.io.lock().unwrap().as_mut() {
            let _ = write_request(&mut io.stdin, &req, self.key.protocol);
        }
    }

    /// Removes any pending multiplex responder/receiver for an abandoned request that will not be
    /// drained (e.g. when the worker is about to be destroyed).
    pub fn abandon_cancelled_request(&self, request_id: i32) {
        self.pending.lock().unwrap().remove(&request_id);
        self.cancelled_receivers.lock().unwrap().remove(&request_id);
    }

    /// Waits for the worker to finish replying to an already-dispatched cancelled request without
    /// sending a duplicate `WorkRequest`.
    pub fn drain_cancelled_response(&self, request_id: i32) -> Result<WorkResponse, String> {
        if self.key.multiplex {
            let Some(rx) = self.cancelled_receivers.lock().unwrap().remove(&request_id) else {
                return Err("missing cancelled request receiver".to_string());
            };
            loop {
                match rx.recv_timeout(Duration::from_millis(10)) {
                    Ok(res) => return res,
                    Err(RecvTimeoutError::Timeout) => {
                        if !self.is_alive() {
                            self.pending.lock().unwrap().remove(&request_id);
                            return Err(format!(
                                "Worker process for {} has died",
                                self.key.mnemonic
                            ));
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err(format!("Worker process for {} has died", self.key.mnemonic));
                    }
                }
            }
        }
        let mut guard = self.io.lock().unwrap();
        let io = guard.as_mut().ok_or("worker not running")?;
        match read_response(&mut io.stdout, self.key.protocol) {
            Ok(Some(r)) => Ok(r),
            Ok(None) => Err(format!(
                "Worker process for {} died while draining cancelled response",
                self.key.mnemonic
            )),
            Err(e) => Err(format!(
                "worker process returned an unparseable WorkResponse: {e}"
            )),
        }
    }

    pub fn destroy(&self) {
        self.dead.store(true, Ordering::SeqCst);
        *self.multiplex_tx.lock().unwrap() = None;
        *self.io.lock().unwrap() = None;
        {
            let mut c = self.child.lock().unwrap();
            crate::process_wrapper::signal_process_group(c.id(), crate::process_wrapper::SIGTERM);
            let deadline = std::time::Instant::now() + Duration::from_millis(50);
            let mut exited = false;
            while std::time::Instant::now() < deadline {
                if matches!(c.try_wait(), Ok(Some(_))) {
                    exited = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            if !exited {
                crate::process_wrapper::signal_process_group(
                    c.id(),
                    crate::process_wrapper::SIGKILL,
                );
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        let mut p = self.pending.lock().unwrap();
        for (_, tx) in p.drain() {
            let _ = tx.send(Err("worker destroyed".to_string()));
        }
        if self.key.sandboxed {
            if let Some(root) = &self.work_dir_root {
                let _ = delete_tree(root);
            }
            let tmp = SandboxHelpers::get_tmp_dir_path(&self.exec_root);
            let _ = delete_tree(&tmp);
        }
    }

    /// Contents of the worker log (stderr of the worker process).
    pub fn log_contents(&self) -> String {
        match std::fs::read(&self.log_file) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => String::new(),
        }
    }

    /// Formats the Bazel `ErrorMessage` log block, truncating to the last `max_chars` characters
    /// with `"[... truncated ...]\n"` and reporting read errors instead of dropping them.
    pub fn format_log_section(&self, max_chars: usize) -> String {
        let body = match std::fs::read(&self.log_file) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                if text.is_empty() {
                    return String::new();
                }
                let chars: Vec<char> = text.chars().collect();
                if chars.len() > max_chars {
                    let tail: String = chars[chars.len() - max_chars..].iter().collect();
                    format!("[... truncated ...]\n{tail}")
                } else {
                    text
                }
            }
            Err(e) => format!("ERROR: IOException while trying to read log file:\n{e}\n"),
        };
        let sep = if body.ends_with('\n') { "" } else { "\n" };
        format!(
            "---8<---8<--- Start of log, file at {} ---8<---8<---\n{body}{sep}---8<---8<--- End of log ---8<---8<---\n",
            self.log_file.display()
        )
    }
}

fn write_request(w: &mut dyn Write, r: &WorkRequest, proto: ProtocolFormat) -> std::io::Result<()> {
    match proto {
        ProtocolFormat::Proto => write_delimited(w, &r.encode_to_vec()),
        ProtocolFormat::Json => {
            w.write_all(work_request_to_json(r).as_bytes())?;
            w.write_all(b"\n")?;
            w.flush()
        }
    }
}

fn read_response(
    r: &mut BufReader<ChildStdout>,
    proto: ProtocolFormat,
) -> std::io::Result<Option<WorkResponse>> {
    let mut rec = RecordingReader::new(r);
    let res = match proto {
        ProtocolFormat::Proto => match read_delimited(&mut rec)? {
            None => Ok(None),
            Some(b) => WorkResponse::decode(&b)
                .map(Some)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
        },
        ProtocolFormat::Json => match read_json_object(&mut rec)? {
            None => Ok(None),
            Some(s) => work_response_from_json(&s)
                .map(Some)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        },
    };
    match res {
        Err(e) => {
            let dump = rec.formatted_data();
            if dump.is_empty() {
                Err(e)
            } else {
                Err(std::io::Error::new(
                    e.kind(),
                    format!("{e}\nRaw response prefix:\n{dump}"),
                ))
            }
        }
        ok => ok,
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
    /// Currently borrowed worker handles (so `shutdown()` destroys active as well as idle workers).
    borrowed: Vec<Arc<WorkerProcess>>,
    /// Number of currently borrowed workers/proxies (`SimpleWorkerPool.acquired`; idle workers are NOT counted).
    acquired: usize,
    /// Pool shrink count from memory pressure / failed return validation (`SimpleWorkerPool.shrunk`).
    shrunk: usize,
    /// Multiplex: the single shared process.
    shared: Option<Arc<WorkerProcess>>,
}

fn format_worker_files_diff(old_key: &WorkerKey, new_key: &WorkerKey) -> String {
    let all_paths: BTreeSet<&PathFragment> = old_key
        .worker_files_with_digests
        .keys()
        .chain(new_key.worker_files_with_digests.keys())
        .collect();
    let mut lines = String::new();
    for path in all_paths {
        let old_d = old_key.worker_files_with_digests.get(path);
        let new_d = new_key.worker_files_with_digests.get(path);
        if old_d != new_d {
            let old_s = old_d
                .map(|b| zel_util::to_hex(b))
                .unwrap_or_else(|| "<none>".to_string());
            let new_s = new_d
                .map(|b| zel_util::to_hex(b))
                .unwrap_or_else(|| "<none>".to_string());
            lines.push_str(&format!("\n  {path}: {old_s} -> {new_s}"));
        }
    }
    if lines.is_empty() {
        format!(
            "\n  hash: {} -> {}",
            old_key.worker_files_hash, new_key.worker_files_hash
        )
    } else {
        lines
    }
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
        let base_limit = self.limit(key);
        let mut st = self.state.lock().unwrap();
        loop {
            let kp = st.entry(key.clone()).or_insert_with(|| KeyPool {
                idle: Vec::new(),
                borrowed: Vec::new(),
                acquired: 0,
                shrunk: 0,
                shared: None,
            });
            let limit = base_limit.saturating_sub(kp.shrunk).max(1);
            if key.multiplex {
                if kp.acquired < limit {
                    kp.acquired += 1;
                    if let Some(w) = kp.shared.clone() {
                        if w.is_alive() {
                            if w.key.worker_files_hash == key.worker_files_hash {
                                kp.borrowed.push(w.clone());
                                return Ok(Borrowed { worker: w });
                            }
                            let diff = format_worker_files_diff(&w.key, key);
                            eprintln!(
                                "WARNING: {} multiplex-worker (id {}) can no longer be used, because its files have changed on disk:{diff}",
                                w.key.mnemonic, w.id
                            );
                            w.destroy();
                            kp.shared = None;
                        } else {
                            kp.shared = None;
                        }
                    }
                    // Started under the pool lock so concurrent borrowers share one process.
                    return match WorkerProcess::start_with_tracking(
                        key,
                        &self.workers_dir,
                        &self.workspace_dir_name,
                        self.opts.use_in_memory_tracking,
                    ) {
                        Ok(w) => {
                            self.started.fetch_add(1, Ordering::SeqCst);
                            kp.shared = Some(w.clone());
                            kp.borrowed.push(w.clone());
                            Ok(Borrowed { worker: w })
                        }
                        Err(e) => {
                            kp.acquired = kp.acquired.saturating_sub(1);
                            self.cv.notify_all();
                            Err(e)
                        }
                    };
                }
            } else if kp.acquired < limit {
                while let Some(w) = kp.idle.pop() {
                    if w.is_alive() {
                        if w.key.worker_files_hash == key.worker_files_hash {
                            kp.acquired += 1;
                            kp.borrowed.push(w.clone());
                            return Ok(Borrowed { worker: w });
                        }
                        let diff = format_worker_files_diff(&w.key, key);
                        eprintln!(
                            "WARNING: {} worker (id {}) can no longer be used, because its files have changed on disk:{diff}",
                            w.key.mnemonic, w.id
                        );
                        w.destroy();
                    }
                }
                kp.acquired += 1;
                let tracking = self.opts.use_in_memory_tracking;
                drop(st);
                return match WorkerProcess::start_with_tracking(
                    key,
                    &self.workers_dir,
                    &self.workspace_dir_name,
                    tracking,
                ) {
                    Ok(w) => {
                        self.started.fetch_add(1, Ordering::SeqCst);
                        let mut st2 = self.state.lock().unwrap();
                        if let Some(kp2) = st2.get_mut(key) {
                            kp2.borrowed.push(w.clone());
                        }
                        Ok(Borrowed { worker: w })
                    }
                    Err(e) => {
                        let mut st2 = self.state.lock().unwrap();
                        if let Some(kp2) = st2.get_mut(key) {
                            kp2.acquired = kp2.acquired.saturating_sub(1);
                        }
                        self.cv.notify_all();
                        Err(e)
                    }
                };
            }
            st = self.cv.wait(st).unwrap();
        }
    }

    pub fn release(&self, b: Borrowed) {
        let mut st = self.state.lock().unwrap();
        if let Some(kp) = st.get_mut(&b.worker.key) {
            if let Some(pos) = kp.borrowed.iter().position(|w| w.id == b.worker.id) {
                kp.borrowed.swap_remove(pos);
            }
            kp.acquired = kp.acquired.saturating_sub(1);
            if !b.worker.key.multiplex {
                if b.worker.is_alive() {
                    kp.idle.push(b.worker);
                } else {
                    b.worker.destroy();
                }
            }
        }
        self.cv.notify_all();
    }

    /// `invalidateObject`: destroys a worker that failed.
    pub fn invalidate(&self, b: Borrowed) {
        self.invalidate_with_shrink(b, false);
    }

    pub fn invalidate_with_shrink(&self, b: Borrowed, should_shrink: bool) {
        let key = b.worker.key.clone();
        let wid = b.worker.id;
        b.worker.destroy();
        let mut st = self.state.lock().unwrap();
        if let Some(kp) = st.get_mut(&key) {
            if let Some(pos) = kp.borrowed.iter().position(|w| w.id == wid) {
                kp.borrowed.swap_remove(pos);
            }
            kp.acquired = kp.acquired.saturating_sub(1);
            if should_shrink {
                kp.shrunk = kp.shrunk.saturating_add(1);
            }
            if key.multiplex && kp.shared.as_ref().is_some_and(|s| s.id == wid) {
                kp.shared = None;
            }
        }
        self.cv.notify_all();
    }

    /// Clears pool shrink counters (`WorkerPoolImpl.reset`).
    pub fn reset(&self) {
        let mut st = self.state.lock().unwrap();
        for kp in st.values_mut() {
            kp.shrunk = 0;
        }
        self.cv.notify_all();
    }

    /// Number of live worker processes (for tests and `--worker_verbose`).
    pub fn live_workers(&self) -> usize {
        let st = self.state.lock().unwrap();
        let mut ids = BTreeSet::new();
        for kp in st.values() {
            for w in kp
                .idle
                .iter()
                .chain(kp.borrowed.iter())
                .chain(kp.shared.iter())
            {
                if w.is_alive() {
                    ids.insert(w.id);
                }
            }
        }
        ids.len()
    }

    /// Live worker processes as `WorkerMetrics` rows (`WorkerProcessMetricsCollector.getLiveWorkerMetrics`).
    pub fn live_worker_metrics(&self) -> Vec<WorkerMetricsRow> {
        use std::hash::{Hash, Hasher};
        let st = self.state.lock().unwrap();
        let mut rows = Vec::new();
        let mut seen = BTreeSet::new();
        for (key, kp) in st.iter() {
            let procs = kp
                .idle
                .iter()
                .chain(kp.borrowed.iter())
                .chain(kp.shared.iter());
            for w in procs {
                if !w.is_alive() || !seen.insert(w.id) {
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

    /// Stops all idle and active workers (`--worker_quit_after_build` / `clean` / shutdown).
    pub fn shutdown(&self) {
        let mut st = self.state.lock().unwrap();
        let mut destroyed = BTreeSet::new();
        for (_, kp) in st.drain() {
            for w in kp
                .idle
                .into_iter()
                .chain(kp.borrowed.into_iter())
                .chain(kp.shared.into_iter())
            {
                if destroyed.insert(w.id) {
                    w.destroy();
                }
            }
        }
        self.cv.notify_all();
    }
}

// ---------------------------------------------------------------------------------------------
// WorkerSpawnRunner
// ---------------------------------------------------------------------------------------------

static REQUEST_IDS: AtomicI32 = AtomicI32::new(1);

fn is_external_repo_arg(arg: &str) -> bool {
    let Some(rest) = arg.strip_prefix('@') else {
        return false;
    };
    if rest.starts_with('@') {
        return true;
    }
    let Some((repo_part, _)) = rest.split_once("//") else {
        return false;
    };
    !repo_part
        .chars()
        .any(|c| matches!(c, '\r' | '\n' | '\u{0085}' | '\u{2028}' | '\u{2029}'))
}

fn split_java_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\r' {
            lines.push(&text[start..i]);
            if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                i += 2;
            } else {
                i += 1;
            }
            start = i;
        } else if bytes[i] == b'\n' {
            lines.push(&text[start..i]);
            i += 1;
            start = i;
        } else {
            i += 1;
        }
    }
    if start < bytes.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// `WorkerSpawnRunner.expandArgument`: `@file` arguments are replaced by the lines of the file
/// (which must be a declared input), recursively; `@@` escapes and external repo labels
/// (`@repo//x` with no line terminators before `//`) are left alone.
pub fn expand_argument(
    exec_root: &Path,
    declared_inputs: &[String],
    arg: &str,
    out: &mut Vec<String>,
) -> std::io::Result<()> {
    expand_argument_with_cancel(exec_root, declared_inputs, arg, out, None)
}

pub fn expand_argument_with_cancel(
    exec_root: &Path,
    declared_inputs: &[String],
    arg: &str,
    out: &mut Vec<String>,
    cancel: Option<&crate::process_wrapper::CancelFlag>,
) -> std::io::Result<()> {
    if cancel.is_some_and(|c| c.is_cancelled()) {
        return Err(cancelled_error());
    }
    let is_external = is_external_repo_arg(arg);
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
                format!(
                    "Failed to read @-argument '{value}' from file '{}'.",
                    path.display()
                ),
            )
        })?;
        for line in split_java_lines(&text) {
            expand_argument_with_cancel(exec_root, declared_inputs, line, out, cancel)?;
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
    /// Digest of a file's contents as the hex bytes used in `WorkRequest.inputs[].digest`.
    pub digest: fn(&[u8]) -> String,
}

impl WorkerSpawnRunner {
    pub fn new(
        exec_root: &Path,
        pool: Arc<WorkerPool>,
        client_env: BTreeMap<String, String>,
        digest: fn(&[u8]) -> String,
    ) -> Self {
        Self {
            exec_root: exec_root.to_path_buf(),
            pool,
            client_env,
            digest,
        }
    }

    /// `canExec`: supports workers/multiplex workers, has non-empty tool files, and passes allowlist.
    pub fn can_exec(&self, spawn: &Spawn) -> bool {
        if !supports_workers(&spawn.execution_info)
            && !supports_multiplex_workers(&spawn.execution_info)
        {
            return false;
        }
        let allowlist = &self.pool.options().allowlist;
        if !allowlist.is_empty() && !allowlist.contains(&worker_key_mnemonic(spawn)) {
            return false;
        }
        spawn.has_tools()
    }

    /// Computes `WorkerFilesHash` over tool files and builds `WorkRequest.inputs` in expanded
    /// spawn-input order (virtual inputs included; empty tree artifacts & runfiles trees excluded).
    #[allow(clippy::type_complexity)]
    fn compute_files_and_request_inputs(
        &self,
        spawn: &Spawn,
        virtual_digests: &BTreeMap<PathFragment, Vec<u8>>,
    ) -> Result<
        (
            String,
            BTreeMap<PathFragment, Vec<u8>>,
            Vec<(String, Vec<u8>)>,
        ),
        String,
    > {
        let digest_fn = self.digest;
        let digest_bytes = |b: &[u8]| -> Vec<u8> {
            let hex = digest_fn(b);
            if hex.len() >= 2 && hex.len() % 2 == 0 && hex.bytes().all(|c| c.is_ascii_hexdigit()) {
                let mut raw = Vec::with_capacity(hex.len() / 2);
                let mut i = 0;
                while i + 1 < hex.len() {
                    if let Ok(v) = u8::from_str_radix(&hex[i..i + 2], 16) {
                        raw.push(v);
                    }
                    i += 2;
                }
                raw
            } else {
                hex.into_bytes()
            }
        };
        // WorkerFilesHash over tool files using raw digest bytes.
        let mut tool_map: BTreeMap<PathFragment, Vec<u8>> = BTreeMap::new();
        for tool in &spawn.tool_inputs {
            if spawn.tool_tree_inputs.contains_key(tool)
                || spawn
                    .tool_runfiles_trees
                    .iter()
                    .any(|r| &r.exec_path == tool)
            {
                continue;
            }
            let p = self.exec_root.join(tool.path_string());
            let md = std::fs::metadata(&p)
                .map_err(|_| format!("Missing input metadata for: '{}'", tool.path_string()))?;
            let d = if md.is_dir() {
                Vec::new()
            } else {
                let bytes = std::fs::read(&p)
                    .map_err(|_| format!("Missing input metadata for: '{}'", tool.path_string()))?;
                digest_bytes(&bytes)
            };
            tool_map.insert(spawn.path_mapper.map(tool), d);
        }
        for (parent, children) in &spawn.tool_tree_inputs {
            if children.is_empty() {
                continue;
            }
            for child_rel in children {
                let child_exec = parent.get_relative(child_rel);
                let p = self.exec_root.join(child_exec.path_string());
                let bytes = std::fs::read(&p).map_err(|_| {
                    format!("Missing input metadata for: '{}'", child_exec.path_string())
                })?;
                tool_map.insert(
                    spawn.path_mapper.map(parent).get_relative(child_rel),
                    digest_bytes(&bytes),
                );
            }
        }
        for rf in &spawn.tool_runfiles_trees {
            if rf.exec_path.is_absolute() {
                return Err(format!("runfiles root must be relative: {}", rf.exec_path));
            }
            for (rel_key, target) in &rf.mapping {
                let Some(target) = target else { continue };
                match target {
                    crate::RunfilesTarget::File { exec_path, .. } => {
                        let p = self.exec_root.join(exec_path.path_string());
                        let md = std::fs::metadata(&p).map_err(|_| {
                            format!("Missing input metadata for: '{}'", exec_path.path_string())
                        })?;
                        if md.is_dir() {
                            continue;
                        }
                        let bytes = std::fs::read(&p).map_err(|_| {
                            format!("Missing input metadata for: '{}'", exec_path.path_string())
                        })?;
                        let mapped = crate::map_for_runfiles(
                            &spawn.path_mapper,
                            &rf.exec_path,
                            &rf.exec_path.get_relative(rel_key),
                        );
                        tool_map.insert(mapped, digest_bytes(&bytes));
                    }
                    crate::RunfilesTarget::TreeArtifact {
                        exec_path,
                        children,
                    } => {
                        for child_rel in children {
                            let child_exec = exec_path.get_relative(child_rel);
                            let p = self.exec_root.join(child_exec.path_string());
                            let bytes = std::fs::read(&p).map_err(|_| {
                                format!(
                                    "Missing input metadata for: '{}'",
                                    child_exec.path_string()
                                )
                            })?;
                            let mapped = crate::map_for_runfiles(
                                &spawn.path_mapper,
                                &rf.exec_path,
                                &rf.exec_path.get_relative(rel_key).get_relative(child_rel),
                            );
                            tool_map.insert(mapped, digest_bytes(&bytes));
                        }
                    }
                    crate::RunfilesTarget::Fileset { .. }
                    | crate::RunfilesTarget::NestedRunfilesTree => {}
                }
            }
        }
        let hash = WorkerFilesHash::get_combined_hash(&tool_map);

        // WorkRequest.inputs in expanded spawn-input order (keepEmptyTreeArtifacts=false, keepRunfilesTrees=false).
        let mut request_inputs: Vec<(String, Vec<u8>)> = Vec::new();
        for inp in &spawn.inputs {
            if let Some(children) = spawn.tree_inputs.get(inp) {
                for child_rel in children {
                    let child_exec = inp.get_relative(child_rel);
                    let d = std::fs::read(self.exec_root.join(child_exec.path_string()))
                        .ok()
                        .map(|b| (self.digest)(&b).into_bytes())
                        .unwrap_or_default();
                    request_inputs.push((child_exec.path_string().to_string(), d));
                }
                continue;
            }
            if spawn.runfiles_trees.iter().any(|r| &r.exec_path == inp) {
                continue;
            }
            let d = if let Some(vd) = virtual_digests.get(inp) {
                zel_util::to_hex(vd).into_bytes()
            } else {
                std::fs::read(self.exec_root.join(inp.path_string()))
                    .ok()
                    .map(|b| (self.digest)(&b).into_bytes())
                    .unwrap_or_default()
            };
            request_inputs.push((inp.path_string().to_string(), d));
        }
        // Include any virtual inputs not already listed in `spawn.inputs`.
        for v in &spawn.virtual_inputs {
            if !spawn.inputs.contains(v.exec_path()) {
                let d = virtual_digests
                    .get(v.exec_path())
                    .map(|vd| zel_util::to_hex(vd).into_bytes())
                    .unwrap_or_default();
                request_inputs.push((v.exec_path().path_string().to_string(), d));
            }
        }
        // Include any tree_inputs keys not listed in `spawn.inputs`.
        for (parent, children) in &spawn.tree_inputs {
            if !spawn.inputs.contains(parent) {
                for child_rel in children {
                    let child_exec = parent.get_relative(child_rel);
                    let d = std::fs::read(self.exec_root.join(child_exec.path_string()))
                        .ok()
                        .map(|b| (self.digest)(&b).into_bytes())
                        .unwrap_or_default();
                    request_inputs.push((child_exec.path_string().to_string(), d));
                }
            }
        }
        Ok((hash, tool_map, request_inputs))
    }

    pub fn exec(&self, spawn: &Spawn) -> std::io::Result<SpawnResult> {
        self.exec_with(spawn, &ExecHooks::default())
    }

    pub fn exec_with(&self, spawn: &Spawn, hooks: &ExecHooks) -> std::io::Result<SpawnResult> {
        let started = std::time::Instant::now();
        let fail = |status, code, msg: String| SpawnResult {
            status,
            exit_code: code,
            stderr: msg.into_bytes(),
            wall_time_ms: started.elapsed().as_millis() as u64,
            runner_name: "worker".into(),
            ..Default::default()
        };
        if !spawn.has_tools() {
            return Ok(fail(
                SpawnStatus::ExecutionFailed,
                1,
                format!(
                    "{ERROR_MESSAGE_PREFIX}{} action, because the action has no tools\n",
                    spawn.mnemonic
                ),
            ));
        }
        let opts = self.pool.options();
        let (worker_args, flag_files) = match split_spawn_args(spawn, opts) {
            Ok(x) => x,
            Err(m) => return Ok(fail(SpawnStatus::ExecutionFailed, 1, m + "\n")),
        };
        let protocol = match worker_protocol_format(&spawn.execution_info) {
            Ok(p) => p,
            Err(m) => return Ok(fail(SpawnStatus::ExecutionFailed, 1, m + "\n")),
        };

        // Update runfiles trees for tool runfiles inputs (and spawn runfiles trees) before borrowing.
        let mut all_rf = spawn.tool_runfiles_trees.clone();
        all_rf.extend(spawn.runfiles_trees.iter().cloned());
        if !all_rf.is_empty() {
            if let Some(updater) = &hooks.runfiles_updater {
                updater.update_runfiles(&self.exec_root, &all_rf)?;
            } else {
                RunfilesTreeUpdater::new().update_runfiles(&self.exec_root, &all_rf)?;
            }
        }

        let input_map = spawn
            .build_input_mapping(&PathFragment::empty())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let (sandbox_inputs, _empty_dirs) = SandboxHelpers::process_input_files(
            &input_map,
            &self.exec_root,
            hooks.cancel.as_ref(),
        )?;
        let sandbox_outputs = SandboxHelpers::get_outputs(spawn);

        let missing: Vec<String> = spawn
            .inputs
            .iter()
            .filter(|i| {
                !spawn.empty_inputs.contains(i)
                    && !spawn.virtual_inputs.iter().any(|v| v.exec_path() == *i)
            })
            .filter(|i| std::fs::symlink_metadata(self.exec_root.join(i.path_string())).is_err())
            .map(|i| i.path_string().to_string())
            .collect();
        if !missing.is_empty() {
            let mut r = fail(SpawnStatus::MissingInputs, 1, String::new());
            r.missing = missing;
            return Ok(r);
        }
        let env =
            rewrite_local_env_for_current_os(&spawn.env, &self.client_env, &spawn.execution_info)?;
        let (hash, tool_map, input_digests) =
            match self.compute_files_and_request_inputs(spawn, &sandbox_inputs.virtual_inputs) {
                Ok(x) => x,
                Err(m) => return Ok(fail(SpawnStatus::ExecutionFailed, 1, m + "\n")),
            };
        let is_dynamic = hooks.lock_outputs.is_some();
        let mut key = create_worker_key(
            spawn,
            worker_args,
            env.clone(),
            &self.exec_root.to_string_lossy(),
            hash,
            opts,
            is_dynamic,
            protocol,
        );
        key.worker_files_with_digests = tool_map;
        // Build the WorkRequest (`expandArgument` checks `sandbox_inputs.files` + `spawn.inputs`).
        let mut declared: Vec<String> = spawn
            .inputs
            .iter()
            .map(|i| i.path_string().to_string())
            .collect();
        for sf in sandbox_inputs.files.keys() {
            let s = sf.path_string().to_string();
            if !declared.contains(&s) {
                declared.push(s);
            }
        }
        let mut arguments = Vec::new();
        for ff in &flag_files {
            expand_argument_with_cancel(
                &self.exec_root,
                &declared,
                ff,
                &mut arguments,
                hooks.cancel.as_ref(),
            )?;
        }
        let mut request = WorkRequest {
            arguments,
            inputs: input_digests
                .iter()
                .map(|(p, d)| Input {
                    path: p.clone(),
                    digest: d.clone(),
                })
                .collect(),
            request_id: 0,
            cancel: false,
            verbosity: if opts.verbose { 10 } else { 0 },
            sandbox_dir: String::new(),
        };
        if key.multiplex {
            request.request_id = REQUEST_IDS.fetch_add(1, Ordering::SeqCst);
        }

        if !key.sandboxed {
            // Non-sandboxed workers write directly into exec_root.
            if hooks.is_cancelled() || (!hooks.local_lockfree_output && !hooks.lock(0)) {
                return Err(cancelled_error());
            }
            for out in &spawn.outputs {
                let p = self.exec_root.join(out.path_string());
                if let Some(par) = p.parent() {
                    std::fs::create_dir_all(par)?;
                }
            }
        }
        let b = match self.pool.borrow(&key) {
            Ok(b) => b,
            Err(e) => {
                return Ok(fail(
                    SpawnStatus::ExecutionFailed,
                    1,
                    format!(
                        "Could not start worker process '{}': {e}\n",
                        key.args.join(" ")
                    ),
                ))
            }
        };

        let mut worker_files: BTreeSet<PathFragment> = BTreeSet::new();
        for t in &spawn.tool_inputs {
            worker_files.insert(spawn.path_mapper.map(t));
        }

        let mut proxy_sandbox_dir: Option<PathBuf> = None;
        if key.sandboxed {
            if key.multiplex {
                b.worker
                    .ensure_sandboxed_multiplex_root(&worker_files, &sandbox_inputs)?;
                let proxy_id = PROXY_IDS.fetch_add(1, Ordering::SeqCst);
                let sandbox_name =
                    format!("__sandbox/{}/{}", proxy_id, self.pool.workspace_dir_name);
                let sbx_dir = b.worker.exec_root.join(&sandbox_name);
                std::fs::create_dir_all(&sbx_dir)?;
                let mut inputs_to_create = BTreeSet::new();
                let mut dirs_to_create = BTreeSet::new();
                let all_in: Vec<&PathFragment> = sandbox_inputs
                    .files
                    .keys()
                    .chain(sandbox_inputs.symlinks.keys())
                    .collect();
                SandboxHelpers::populate_inputs_and_dirs_to_create(
                    &BTreeSet::new(),
                    &mut inputs_to_create,
                    &mut dirs_to_create,
                    all_in.into_iter(),
                    &sandbox_outputs,
                );
                let parent = sbx_dir.parent().unwrap_or(&sbx_dir);
                SandboxHelpers::clean_existing(
                    parent,
                    &sandbox_inputs,
                    &mut inputs_to_create,
                    &mut dirs_to_create,
                    &sbx_dir,
                    None,
                    hooks.cancel.as_ref(),
                )?;
                SandboxHelpers::create_directories(
                    &dirs_to_create,
                    &sbx_dir,
                    false,
                    hooks.cancel.as_ref(),
                )?;
                WorkerExecRoot::create_inputs(
                    &inputs_to_create,
                    &sandbox_inputs,
                    &sbx_dir,
                    hooks.cancel.as_ref(),
                )?;
                request.sandbox_dir = sandbox_name;
                proxy_sandbox_dir = Some(sbx_dir);
            } else {
                let mut wer_guard = b.worker.worker_exec_root.lock().unwrap();
                if let Some(wer) = wer_guard.as_mut() {
                    wer.create_file_system(
                        &worker_files,
                        &sandbox_inputs,
                        &sandbox_outputs,
                        hooks.cancel.as_ref(),
                    )?;
                }
            }
        }

        let resp = b.worker.exec_with_cancel(&request, hooks.cancel.as_ref());
        if hooks.is_cancelled() {
            let can_cancel = opts.cancellation && key.cancellable;
            if can_cancel || key.sandboxed {
                let pool = self.pool.clone();
                let req_id = request.request_id;
                let sbx_to_clean = proxy_sandbox_dir.take();
                std::thread::spawn(move || {
                    if can_cancel {
                        b.worker.send_cancel(req_id);
                        if b.worker.drain_cancelled_response(req_id).is_ok() {
                            pool.release(b);
                        } else {
                            pool.invalidate(b);
                        }
                    } else if b.worker.key.multiplex {
                        if b.worker.drain_cancelled_response(req_id).is_ok() {
                            pool.release(b);
                        } else {
                            pool.invalidate(b);
                        }
                    } else {
                        b.worker.abandon_cancelled_request(req_id);
                        pool.invalidate(b);
                    }
                    if let Some(d) = sbx_to_clean {
                        let _ = delete_tree(&d);
                    }
                });
            } else {
                b.worker.abandon_cancelled_request(request.request_id);
                self.pool.invalidate(b);
            }
            return Err(cancelled_error());
        }

        let mut result;
        match resp {
            Ok(r) => {
                if r.was_cancelled {
                    self.pool.invalidate(b);
                    if let Some(d) = &proxy_sandbox_dir {
                        let _ = delete_tree(d);
                    }
                    return Ok(fail(
                        SpawnStatus::ExecutionFailed,
                        1,
                        format!(
                            "Received cancel response for {} without having cancelled\n",
                            r.request_id
                        ),
                    ));
                }
                if key.sandboxed && !hooks.lock(r.exit_code) {
                    self.pool.release(b);
                    if let Some(d) = &proxy_sandbox_dir {
                        let _ = delete_tree(d);
                    }
                    return Err(cancelled_error());
                }
                if key.sandboxed {
                    if let Some(sbx_dir) = &proxy_sandbox_dir {
                        SandboxHelpers::move_outputs(&sandbox_outputs, sbx_dir, &self.exec_root)?;
                        let _ = delete_tree(sbx_dir);
                    } else {
                        let wer_guard = b.worker.worker_exec_root.lock().unwrap();
                        if let Some(wer) = wer_guard.as_ref() {
                            wer.copy_outputs(&self.exec_root, &sandbox_outputs)?;
                        }
                    }
                }
                result = crate::worker_response_to_spawn_result(&r);
                self.pool.release(b);
            }
            Err(e) => {
                let log_section = b.worker.format_log_section(8192);
                self.pool.invalidate(b);
                if let Some(d) = &proxy_sandbox_dir {
                    let _ = delete_tree(d);
                }
                let mut msg = format!("{e}\n");
                msg.push_str(&log_section);
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
                .filter(|o| {
                    std::fs::symlink_metadata(self.exec_root.join(o.path_string())).is_err()
                })
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
        let o2 = WorkerOptions {
            extra_flags: vec![
                ("Javac".into(), "--foo".into()),
                ("Other".into(), "--bar".into()),
            ],
            ..Default::default()
        };
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
        let strict = WorkerOptions {
            strict_flagfiles: true,
            ..Default::default()
        };
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
            create_worker_key(
                s,
                vec![],
                BTreeMap::new(),
                "/er",
                "h".into(),
                o,
                dynamic,
                ProtocolFormat::Proto,
            )
        };
        assert!(!k(&s, &o, false).multiplex && !k(&s, &o, false).sandboxed);
        s.execution_info
            .insert("supports-multiplex-workers".into(), "1".into());
        assert!(k(&s, &o, false).multiplex);
        assert!(!k(&s, &o, false).sandboxed);
        o.multiplex_sandboxing = true;
        assert!(
            !k(&s, &o, false).sandboxed,
            "needs supports-multiplex-sandboxing"
        );
        s.execution_info
            .insert("supports-multiplex-sandboxing".into(), "1".into());
        assert!(k(&s, &o, false).sandboxed);
        o.multiplex = false;
        o.sandboxing = true;
        let key = k(&s, &o, false);
        assert!(!key.multiplex && key.sandboxed);
        assert!(k(&s, &o, true).sandboxed);
        s.execution_info
            .insert("worker-key-mnemonic".into(), "Special".into());
        assert_eq!(k(&s, &o, false).mnemonic, "Special");
    }

    #[test]
    fn instances_flag_parsing() {
        assert_eq!(parse_instances_flag("3").unwrap(), ("".to_string(), 3));
        assert_eq!(
            parse_instances_flag("Javac=2").unwrap(),
            ("Javac".to_string(), 2)
        );
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
            inputs: vec![Input {
                path: "p".into(),
                digest: vec![0xde, 0xad, 0xbe, 0xef],
            }],
            request_id: 5,
            cancel: false,
            verbosity: 10,
            sandbox_dir: "sbx".into(),
        };
        let j = work_request_to_json(&r);
        assert!(
            j.contains("\"requestId\":5") && j.contains("\"digest\":\"3q2+7w==\""),
            "{j}"
        );
        assert_eq!(work_request_from_json(&j).unwrap(), r);
        let resp = work_response_from_json(
            "{\"exit_code\": 2, \"output\": \"x\\u00e9\", \"requestId\": 5}",
        )
        .unwrap();
        assert_eq!(
            (resp.exit_code, resp.output.as_str(), resp.request_id),
            (2, "xé", 5)
        );
        assert_eq!(
            base64_decode(&base64_encode(b"hello!?")).unwrap(),
            b"hello!?"
        );
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
        let e =
            expand_argument(&d, &["params".to_string()], "@params", &mut Vec::new()).unwrap_err();
        assert!(
            e.to_string().contains("Failed to read @-argument 'inner'")
                || e.to_string().contains("not a declared input"),
            "{e}"
        );
        let mut o = Vec::new();
        expand_argument(&d, &[], "@repo//x:y", &mut o).unwrap();
        assert_eq!(o, ["@repo//x:y"]);
        let _ = PathFragment::create("x");
    }

    #[test]
    fn json_write_request_emits_ndjson_newline() {
        let req = WorkRequest {
            arguments: vec!["--a".into()],
            request_id: 3,
            ..Default::default()
        };
        let mut buf = Vec::new();
        write_request(&mut buf, &req, ProtocolFormat::Json).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.ends_with('\n'), "expected trailing newline in {s:?}");
        assert!(!s.contains(" \n"));
    }

    #[cfg(unix)]
    #[test]
    fn singleplex_worker_death_while_grandchild_holds_stdout_reports_promptly() {
        let d = std::env::temp_dir().join(format!("zel-wkdie-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("er")).unwrap();
        let script = d.join("die.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\ndd bs=1 count=1 >/dev/null 2>&1\nsleep 5 &\nexit 1\n",
        )
        .unwrap();
        #[cfg(unix)]
        crate::set_executable_mode(&script).unwrap();
        let key = WorkerKey {
            args: vec![script.to_string_lossy().into_owned()],
            env: BTreeMap::new(),
            exec_root: d.join("er").to_string_lossy().into_owned(),
            mnemonic: "Javac".into(),
            worker_files_hash: "h".into(),
            worker_files_with_digests: BTreeMap::new(),
            sandboxed: false,
            multiplex: false,
            cancellable: false,
            protocol: ProtocolFormat::Proto,
        };
        let w = WorkerProcess::start(&key, &d.join("workers"), "ws").unwrap();
        let t = std::time::Instant::now();
        let err = w.exec(&WorkRequest::default()).unwrap_err();
        assert!(
            err.contains("Worker process for Javac died while waiting for response"),
            "unexpected error: {err}"
        );
        assert!(
            t.elapsed().as_secs() < 3,
            "should not wait for grandchild: {:?}",
            t.elapsed()
        );
        w.destroy();
    }

    #[test]
    fn worker_files_hash_expands_trees_and_runfiles_and_rejects_missing() {
        let d = std::env::temp_dir().join(format!("zel-wfh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let er = d.join("er");
        std::fs::create_dir_all(er.join("tree")).unwrap();
        std::fs::write(er.join("tree/a.jar"), b"jarA").unwrap();
        std::fs::write(er.join("rf_file"), b"rf").unwrap();

        let mut s = Spawn::default();
        s.tool_inputs.push(PathFragment::create("tree"));
        s.tool_tree_inputs.insert(
            PathFragment::create("tree"),
            vec![PathFragment::create("a.jar")],
        );
        s.tool_tree_inputs
            .insert(PathFragment::create("empty_tree"), vec![]);
        let mut rf_map = BTreeMap::new();
        rf_map.insert(
            PathFragment::create("_main/f"),
            Some(crate::RunfilesTarget::File {
                exec_path: PathFragment::create("rf_file"),
                is_symlink: false,
            }),
        );
        rf_map.insert(PathFragment::create("_main/empty"), None);
        s.tool_runfiles_trees.push(crate::RunfilesTreeData {
            exec_path: PathFragment::create("tool.runfiles"),
            workspace_name: "_main".into(),
            mapping: rf_map,
            ..Default::default()
        });
        let map = WorkerFilesHash::get_worker_files_with_digests(&s, &er, |b| {
            zel_util::sha256::digest(b).to_vec()
        })
        .unwrap();
        assert!(map.contains_key(&PathFragment::create("tree/a.jar")));
        assert!(!map.contains_key(&PathFragment::create("empty_tree")));
        assert!(map.contains_key(&PathFragment::create("tool.runfiles/_main/f")));
        assert!(!map.contains_key(&PathFragment::create("tool.runfiles/_main/empty")));
        let h = WorkerFilesHash::get_combined_hash(&map);
        assert_eq!(h.len(), 64);

        s.tool_inputs.push(PathFragment::create("missing.jar"));
        let err =
            WorkerFilesHash::get_worker_files_with_digests(&s, &er, |b| b.to_vec()).unwrap_err();
        assert!(
            err.contains("Missing input metadata for: 'missing.jar'"),
            "{err}"
        );
    }

    #[test]
    fn worker_cancellation_registry_registers_checks_and_unregisters() {
        let reg = WorkerCancellationRegistry::new();
        assert!(!reg.check_if_request_is_cancelled(42));
        reg.register_request(42);
        assert!(reg.check_if_request_is_cancelled(42));
        assert!(!reg.check_if_request_is_cancelled(7));
        assert!(reg.unregister_request(42));
        assert!(!reg.check_if_request_is_cancelled(42));
        assert!(!reg.unregister_request(42));
    }
}
