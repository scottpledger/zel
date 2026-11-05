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

//! Spawn execution logs (`--execution_log_json_file`, `--execution_log_binary_file`,
//! `--execution_log_compact_file` and the `--experimental_execution_log_*` aliases).
//!
//! * JSON: one `SpawnExec` (spawn.proto) per line in canonical protobuf JSON.
//! * Binary: varint-delimited `SpawnExec` messages.
//! * Compact: varint-delimited `ExecLogEntry` messages (an `Invocation` entry, interned `File`
//!   / `InputSet` entries, then one `Spawn` entry per spawn) inside a zstd frame. The frame uses
//!   raw (stored) blocks: a valid zstd stream that any decoder accepts, without compression.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;
use zel_proto::dynamic::Msg;

/// Flag values for the execution log outputs.
#[derive(Debug, Clone, Default)]
pub struct ExecLogOptions {
    pub json_file: Option<String>,
    pub binary_file: Option<String>,
    pub compact_file: Option<String>,
    pub include_spawn_metrics: bool,
    pub hash_function_name: String,
}

impl ExecLogOptions {
    /// Reads `--execution_log_{json,binary,compact}_file` and the `--experimental_` spellings
    /// (`--experimental_execution_log_file` is the binary log).
    pub fn from_flags(get: &dyn Fn(&str) -> Option<String>) -> Self {
        let ne = |n: &str| get(n).filter(|s| !s.is_empty());
        Self {
            json_file: ne("execution_log_json_file").or_else(|| ne("experimental_execution_log_json_file")),
            binary_file: ne("execution_log_binary_file")
                .or_else(|| ne("experimental_execution_log_file"))
                .or_else(|| ne("experimental_execution_log_binary_file")),
            compact_file: ne("execution_log_compact_file").or_else(|| ne("experimental_execution_log_compact_file")),
            include_spawn_metrics: !matches!(
                get("experimental_execution_log_spawn_metrics").as_deref(),
                Some("false") | Some("0") | Some("no")
            ),
            hash_function_name: get("digest_function").unwrap_or_else(|| "SHA256".to_string()).to_ascii_uppercase(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.json_file.is_some() || self.binary_file.is_some() || self.compact_file.is_some()
    }
}

/// One input or output file of a spawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnFile {
    pub path: String,
    pub hash: String,
    pub size: i64,
    pub symlink_target: String,
    pub is_tool: bool,
}

/// One executed spawn.
#[derive(Debug, Clone, Default)]
pub struct SpawnRecord {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub platform: Vec<(String, String)>,
    pub inputs: Vec<SpawnFile>,
    pub listed_outputs: Vec<String>,
    pub actual_outputs: Vec<SpawnFile>,
    pub remotable: bool,
    pub cacheable: bool,
    pub remote_cacheable: bool,
    pub timeout_millis: i64,
    pub mnemonic: String,
    pub target_label: String,
    pub runner: String,
    pub cache_hit: bool,
    /// `SpawnResult.Status` name when not `SUCCESS` (e.g. `NON_ZERO_EXIT`), empty on success.
    pub status: String,
    pub exit_code: i32,
    pub action_digest: Option<(String, i64)>,
    pub total_time_ms: Option<i64>,
}

fn digest(hash: &str, size: i64, fname: &str) -> Msg {
    let mut d = Msg::new("tools.protos.Digest");
    d.set_str("hash", hash);
    d.set_int("size_bytes", size);
    if !fname.is_empty() {
        d.set_str("hash_function_name", fname);
    }
    d
}

fn file_msg(f: &SpawnFile, fname: &str) -> Msg {
    let mut m = Msg::new("tools.protos.File");
    m.set_str("path", f.path.clone());
    if !f.symlink_target.is_empty() {
        m.set_str("symlink_target_path", f.symlink_target.clone());
    } else if !f.hash.is_empty() {
        m.set_msg("digest", digest(&f.hash, f.size, fname));
    }
    if f.is_tool {
        m.set_bool("is_tool", true);
    }
    m
}

impl SpawnRecord {
    /// The `SpawnExec` proto for this spawn.
    pub fn to_spawn_exec(&self, opts: &ExecLogOptions) -> Msg {
        let mut s = Msg::new("tools.protos.SpawnExec");
        for a in &self.args {
            s.set_str("command_args", a.clone());
        }
        let mut env = self.env.clone();
        env.sort();
        for (k, v) in &env {
            let mut e = Msg::new("tools.protos.EnvironmentVariable");
            e.set_str("name", k.clone());
            e.set_str("value", v.clone());
            s.set_msg("environment_variables", e);
        }
        if !self.platform.is_empty() {
            let mut p = Msg::new("tools.protos.Platform");
            for (k, v) in &self.platform {
                let mut pr = Msg::new("tools.protos.Platform.Property");
                pr.set_str("name", k.clone());
                pr.set_str("value", v.clone());
                p.set_msg("properties", pr);
            }
            s.set_msg("platform", p);
        }
        let mut inputs = self.inputs.clone();
        inputs.sort_by(|a, b| a.path.cmp(&b.path));
        for f in &inputs {
            s.set_msg("inputs", file_msg(f, &opts.hash_function_name));
        }
        let mut listed = self.listed_outputs.clone();
        listed.sort();
        for o in &listed {
            s.set_str("listed_outputs", o.clone());
        }
        s.set_bool("remotable", self.remotable);
        s.set_bool("cacheable", self.cacheable);
        if self.timeout_millis != 0 {
            s.set_int("timeout_millis", self.timeout_millis);
        }
        s.set_str("mnemonic", self.mnemonic.clone());
        let mut outs = self.actual_outputs.clone();
        outs.sort_by(|a, b| a.path.cmp(&b.path));
        for f in &outs {
            s.set_msg("actual_outputs", file_msg(f, &opts.hash_function_name));
        }
        s.set_str("runner", self.runner.clone());
        s.set_bool("cache_hit", self.cache_hit);
        s.set_str("status", self.status.clone());
        s.set_int("exit_code", self.exit_code as i64);
        s.set_bool("remote_cacheable", self.remote_cacheable);
        s.set_str("target_label", self.target_label.clone());
        if let Some((h, sz)) = &self.action_digest {
            s.set_msg("digest", digest(h, *sz, &opts.hash_function_name));
        }
        if opts.include_spawn_metrics {
            if let Some(ms) = self.total_time_ms {
                let mut m = Msg::new("tools.protos.SpawnMetrics");
                let mut d = m.new_child("total_time");
                d.set_int("seconds", ms.div_euclid(1000));
                d.set_int("nanos", ms.rem_euclid(1000) * 1_000_000);
                m.set_msg("total_time", d);
                s.set_msg("metrics", m);
            }
        }
        s
    }
}

fn delimited(m: &Msg) -> Vec<u8> {
    let body = m.encode_to_vec();
    let mut out = Vec::with_capacity(body.len() + 5);
    zel_proto::wire::encode_varint(body.len() as u64, &mut out);
    out.extend_from_slice(&body);
    out
}

/// Wraps `data` in a zstd frame made of raw blocks (RFC 8878). Valid for every decoder.
pub fn zstd_store(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x28, 0xB5, 0x2F, 0xFD];
    // Single segment, 8-byte frame content size, no checksum, no dictionary.
    out.push(0xE0);
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    const MAX: usize = 128 * 1024;
    if data.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00]); // last, raw, size 0
    }
    let mut chunks = data.chunks(MAX).peekable();
    while let Some(c) = chunks.next() {
        let last = chunks.peek().is_none();
        let hdr = (c.len() as u32) << 3 | (last as u32); // block type 0 = raw
        out.extend_from_slice(&hdr.to_le_bytes()[..3]);
        out.extend_from_slice(c);
    }
    out
}

/// Parses a zstd frame produced by [`zstd_store`] (raw blocks only).
pub fn zstd_unstore(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 13 || data[..4] != [0x28, 0xB5, 0x2F, 0xFD] || data[4] != 0xE0 {
        return Err("unsupported zstd frame".into());
    }
    let size = u64::from_le_bytes(data[5..13].try_into().unwrap()) as usize;
    let mut p = 13;
    let mut out = Vec::with_capacity(size);
    loop {
        let h = u32::from_le_bytes([data[p], data[p + 1], data[p + 2], 0]);
        p += 3;
        if (h >> 1) & 3 != 0 {
            return Err("non-raw block".into());
        }
        let len = (h >> 3) as usize;
        out.extend_from_slice(data.get(p..p + len).ok_or("truncated block")?);
        p += len;
        if h & 1 == 1 {
            break;
        }
    }
    Ok(out)
}

struct Compact {
    entries: Vec<u8>,
    next_id: u32,
    files: HashMap<(String, String), u32>,
    started: bool,
}

/// Writes all enabled execution log formats.
pub struct ExecutionLogWriter {
    opts: ExecLogOptions,
    json: Mutex<Option<Box<dyn Write + Send>>>,
    binary: Mutex<Option<Box<dyn Write + Send>>>,
    compact: Mutex<Compact>,
    compact_path: Option<String>,
    invocation_id: String,
    errors: Mutex<Vec<String>>,
}

impl ExecutionLogWriter {
    /// Opens the configured files (creating parent directories).
    pub fn open(opts: ExecLogOptions, invocation_id: &str) -> Result<Self, String> {
        let open = |p: &Option<String>| -> Result<Option<Box<dyn Write + Send>>, String> {
            match p {
                None => Ok(None),
                Some(p) => {
                    if let Some(parent) = std::path::Path::new(p).parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    std::fs::File::create(p)
                        .map(|f| Some(Box::new(f) as Box<dyn Write + Send>))
                        .map_err(|e| format!("Cannot open execution log file {p}: {e}"))
                }
            }
        };
        Ok(Self {
            json: Mutex::new(open(&opts.json_file)?),
            binary: Mutex::new(open(&opts.binary_file)?),
            compact_path: opts.compact_file.clone(),
            compact: Mutex::new(Compact { entries: Vec::new(), next_id: 1, files: HashMap::new(), started: false }),
            opts,
            invocation_id: invocation_id.to_string(),
            errors: Mutex::new(Vec::new()),
        })
    }

    fn err(&self, e: impl std::fmt::Display) {
        self.errors.lock().unwrap().push(e.to_string());
    }

    /// Logs one spawn to every enabled format.
    pub fn log(&self, rec: &SpawnRecord) {
        let exec = rec.to_spawn_exec(&self.opts);
        if let Some(w) = self.json.lock().unwrap().as_mut() {
            let mut line = exec.to_json();
            line.push('\n');
            if let Err(e) = w.write_all(line.as_bytes()).and_then(|_| w.flush()) {
                self.err(e);
            }
        }
        if let Some(w) = self.binary.lock().unwrap().as_mut() {
            if let Err(e) = w.write_all(&delimited(&exec)).and_then(|_| w.flush()) {
                self.err(e);
            }
        }
        if self.compact_path.is_some() {
            self.log_compact(rec);
        }
    }

    fn push_entry(c: &mut Compact, mut e: Msg) -> u32 {
        let id = c.next_id;
        c.next_id += 1;
        e.set_int("id", id as i64);
        c.entries.extend(delimited(&e));
        id
    }

    fn file_entry(&self, c: &mut Compact, f: &SpawnFile) -> u32 {
        let key = (f.path.clone(), f.hash.clone());
        if let Some(id) = c.files.get(&key) {
            return *id;
        }
        let mut fe = Msg::new("tools.protos.ExecLogEntry.File");
        fe.set_str("path", f.path.clone());
        if !f.hash.is_empty() {
            fe.set_msg("digest", digest(&f.hash, f.size, &self.opts.hash_function_name));
        }
        let mut e = Msg::new("tools.protos.ExecLogEntry");
        e.set_msg("file", fe);
        let id = Self::push_entry(c, e);
        c.files.insert(key, id);
        id
    }

    fn log_compact(&self, rec: &SpawnRecord) {
        let mut c = self.compact.lock().unwrap();
        if !c.started {
            c.started = true;
            let mut inv = Msg::new("tools.protos.ExecLogEntry.Invocation");
            inv.set_str("hash_function_name", self.opts.hash_function_name.clone());
            inv.set_str("workspace_runfiles_directory", "_main");
            inv.set_str("id", self.invocation_id.clone());
            let mut e = Msg::new("tools.protos.ExecLogEntry");
            e.set_msg("invocation", inv);
            // The invocation entry carries no id of its own.
            c.entries.extend(delimited(&e));
        }
        let mut input_ids = Vec::new();
        let mut tool_ids = Vec::new();
        let mut sorted = rec.inputs.clone();
        sorted.sort_by(|a, b| a.path.cmp(&b.path));
        for f in &sorted {
            let id = self.file_entry(&mut c, f);
            if f.is_tool {
                tool_ids.push(id);
            } else {
                input_ids.push(id);
            }
        }
        let mut set = |ids: &[u32], c: &mut Compact| -> u32 {
            if ids.is_empty() {
                return 0;
            }
            let mut s = Msg::new("tools.protos.ExecLogEntry.InputSet");
            for i in ids {
                s.set_uint("input_ids", *i as u64);
            }
            let mut e = Msg::new("tools.protos.ExecLogEntry");
            e.set_msg("input_set", s);
            Self::push_entry(c, e)
        };
        let input_set = set(&input_ids, &mut c);
        let tool_set = set(&tool_ids, &mut c);
        let mut sp = Msg::new("tools.protos.ExecLogEntry.Spawn");
        for a in &rec.args {
            sp.set_str("args", a.clone());
        }
        let mut env = rec.env.clone();
        env.sort();
        for (k, v) in &env {
            let mut ev = Msg::new("tools.protos.EnvironmentVariable");
            ev.set_str("name", k.clone());
            ev.set_str("value", v.clone());
            sp.set_msg("env_vars", ev);
        }
        if input_set != 0 {
            sp.set_uint("input_set_id", input_set as u64);
        }
        if tool_set != 0 {
            sp.set_uint("tool_set_id", tool_set as u64);
        }
        let mut outs = rec.actual_outputs.clone();
        outs.sort_by(|a, b| a.path.cmp(&b.path));
        for o in &outs {
            let id = self.file_entry(&mut c, o);
            let mut om = Msg::new("tools.protos.ExecLogEntry.Output");
            om.set_uint("output_id", id as u64);
            sp.set_msg("outputs", om);
        }
        for o in &rec.listed_outputs {
            if !outs.iter().any(|f| &f.path == o) {
                let mut om = Msg::new("tools.protos.ExecLogEntry.Output");
                om.set_str("invalid_output_path", o.clone());
                sp.set_msg("outputs", om);
            }
        }
        sp.set_str("target_label", rec.target_label.clone());
        sp.set_str("mnemonic", rec.mnemonic.clone());
        sp.set_int("exit_code", rec.exit_code as i64);
        sp.set_str("status", rec.status.clone());
        sp.set_str("runner", rec.runner.clone());
        sp.set_bool("cache_hit", rec.cache_hit);
        sp.set_bool("remotable", rec.remotable);
        sp.set_bool("cacheable", rec.cacheable);
        sp.set_bool("remote_cacheable", rec.remote_cacheable);
        if let Some((h, sz)) = &rec.action_digest {
            sp.set_msg("digest", digest(h, *sz, &self.opts.hash_function_name));
        }
        if rec.timeout_millis != 0 {
            sp.set_int("timeout_millis", rec.timeout_millis);
        }
        let mut e = Msg::new("tools.protos.ExecLogEntry");
        e.set_msg("spawn", sp);
        Self::push_entry(&mut c, e);
    }

    /// Removes the files again: Bazel creates its execution log when the execution phase starts,
    /// so a build that fails before that (e.g. a missing target) leaves no file behind.
    pub fn discard(&self) {
        *self.json.lock().unwrap() = None;
        *self.binary.lock().unwrap() = None;
        for p in [&self.opts.json_file, &self.opts.binary_file, &self.opts.compact_file].into_iter().flatten() {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Flushes everything; the compact log is written (zstd framed) on close. Returns errors.
    pub fn close(&self) -> Vec<String> {
        if let Some(p) = &self.compact_path {
            let c = self.compact.lock().unwrap();
            if let Err(e) = std::fs::write(p, zstd_store(&c.entries)) {
                self.err(format!("Cannot write execution log file {p}: {e}"));
            }
        }
        self.errors.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(label: &str, input_hash: &str) -> SpawnRecord {
        SpawnRecord {
            args: vec!["/bin/bash".into(), "-c".into(), "echo hi > out".into()],
            env: vec![("PATH".into(), "/bin".into()), ("A".into(), "1".into())],
            platform: vec![("os".into(), "linux".into())],
            inputs: vec![
                SpawnFile { path: "p/in.txt".into(), hash: input_hash.into(), size: 3, ..Default::default() },
                SpawnFile { path: "tools/t".into(), hash: "ff".into(), size: 9, is_tool: true, ..Default::default() },
            ],
            listed_outputs: vec!["bazel-out/k8-fastbuild/bin/p/out".into()],
            actual_outputs: vec![SpawnFile { path: "bazel-out/k8-fastbuild/bin/p/out".into(), hash: "aa".into(), size: 3, ..Default::default() }],
            remotable: true,
            cacheable: true,
            remote_cacheable: true,
            timeout_millis: 1500,
            mnemonic: "Genrule".into(),
            target_label: label.into(),
            runner: "processwrapper-sandbox".into(),
            exit_code: 0,
            action_digest: Some(("dd".into(), 100)),
            total_time_ms: Some(1234),
            ..Default::default()
        }
    }

    fn tmp(name: &str) -> String {
        let d = std::env::temp_dir().join(format!("zel-execlog-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d.join(name).to_str().unwrap().to_string()
    }

    #[test]
    fn json_binary_and_compact_logs() {
        let opts = ExecLogOptions {
            json_file: Some(tmp("l.json")),
            binary_file: Some(tmp("l.bin")),
            compact_file: Some(tmp("l.zst")),
            include_spawn_metrics: true,
            hash_function_name: "SHA256".into(),
        };
        let w = ExecutionLogWriter::open(opts.clone(), "inv-1").unwrap();
        w.log(&rec("//p:a", "11"));
        w.log(&rec("//p:b", "11"));
        assert!(w.close().is_empty());

        // JSON: one SpawnExec per line, inputs sorted, int64 as strings.
        let json = std::fs::read_to_string(tmp("l.json")).unwrap();
        let lines: Vec<&str> = json.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("{\"commandArgs\":[\"/bin/bash\",\"-c\",\"echo hi > out\"],\"environmentVariables\":[{\"name\":\"A\",\"value\":\"1\"},{\"name\":\"PATH\",\"value\":\"/bin\"}]"), "{}", lines[0]);
        assert!(lines[0].contains("\"timeoutMillis\":\"1500\""));
        assert!(lines[0].contains("\"digest\":{\"hash\":\"dd\",\"sizeBytes\":\"100\",\"hashFunctionName\":\"SHA256\"}"));
        assert!(lines[0].contains("\"metrics\":{\"totalTime\":\"1.234s\"}"), "{}", lines[0]);
        assert!(lines[0].contains("\"isTool\":true"));

        // Binary: delimited SpawnExec that round-trips to the same JSON.
        let bin = std::fs::read(tmp("l.bin")).unwrap();
        let mut pos = 0;
        let mut n = 0;
        while pos < bin.len() {
            let len = zel_proto::wire::decode_varint(&bin, &mut pos).unwrap() as usize;
            let m = Msg::decode("tools.protos.SpawnExec", &bin[pos..pos + len]).unwrap();
            assert_eq!(m.to_json(), lines[n]);
            pos += len;
            n += 1;
        }
        assert_eq!(n, 2);

        // Compact: zstd framed ExecLogEntry stream; the shared input file is interned once.
        let z = std::fs::read(tmp("l.zst")).unwrap();
        assert_eq!(&z[..4], &[0x28, 0xB5, 0x2F, 0xFD]);
        let raw = zstd_unstore(&z).unwrap();
        let mut pos = 0;
        let mut entries = Vec::new();
        while pos < raw.len() {
            let len = zel_proto::wire::decode_varint(&raw, &mut pos).unwrap() as usize;
            entries.push(Msg::decode("tools.protos.ExecLogEntry", &raw[pos..pos + len]).unwrap());
            pos += len;
        }
        assert!(entries[0].has("invocation"));
        assert_eq!(entries[0].get_msg("invocation").unwrap().get_str("id"), "inv-1");
        let files = entries.iter().filter(|e| e.has("file")).count();
        // tools/t, p/in.txt, out (shared across the two spawns) => 3 distinct files.
        assert_eq!(files, 3);
        let spawns: Vec<_> = entries.iter().filter(|e| e.has("spawn")).collect();
        assert_eq!(spawns.len(), 2);
        let s0 = spawns[0].get_msg("spawn").unwrap();
        assert_eq!(s0.get_str("target_label"), "//p:a");
        assert!(s0.get_i64("input_set_id") > 0 && s0.get_i64("tool_set_id") > 0);
        let _ = std::fs::remove_dir_all(std::path::Path::new(&tmp("x")).parent().unwrap());
    }

    #[test]
    fn zstd_frame_is_valid_for_system_zstd() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        for data in [Vec::new(), b"hello".to_vec(), vec![7u8; 300_000]] {
            let z = zstd_store(&data);
            assert_eq!(zstd_unstore(&z).unwrap(), data);
            let Ok(mut c) = Command::new("zstd").args(["-dc"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn() else {
                return;
            };
            let mut stdin = c.stdin.take().unwrap();
            let zc = z.clone();
            let t = std::thread::spawn(move || stdin.write_all(&zc));
            let out = c.wait_with_output().unwrap();
            let _ = t.join();
            assert_eq!(out.stdout, data);
        }
    }

    #[test]
    fn option_aliases() {
        let m: std::collections::HashMap<&str, &str> = [("experimental_execution_log_file", "/a.bin"), ("execution_log_compact_file", "/c.zst")].into_iter().collect();
        let o = ExecLogOptions::from_flags(&|n| m.get(n).map(|s| s.to_string()));
        assert_eq!(o.binary_file.as_deref(), Some("/a.bin"));
        assert_eq!(o.compact_file.as_deref(), Some("/c.zst"));
        assert!(o.json_file.is_none() && o.enabled());
    }
}
