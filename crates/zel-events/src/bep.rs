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

//! Build Event Protocol: event ids, the event DAG streamer
//! (`runtime/BuildEventStreamer.java`) and file transports
//! (`buildeventstream/transports`).
//!
//! All events are real `build_event_stream.BuildEvent` protos (schema-driven via
//! `zel_proto::dynamic`), so every payload type of `build_event_stream.proto` is representable.

use std::collections::HashSet;
use std::io::Write;
use std::sync::{Arc, Mutex};
use zel_proto::dynamic::{Msg, Value};

pub const BEP: &str = "build_event_stream";

fn bid(variant: &str, inner: Msg) -> Msg {
    let mut m = Msg::new("build_event_stream.BuildEventId");
    m.set_msg(variant, inner);
    m
}

fn sub(name: &str) -> Msg {
    Msg::new(&format!("build_event_stream.BuildEventId.{name}"))
}

/// Constructors for `BuildEventId` messages (`BuildEventIdUtil.java`).
pub mod ids {
    use super::*;

    pub fn started() -> Msg {
        bid("started", sub("BuildStartedId"))
    }
    pub fn progress(n: i32) -> Msg {
        let mut p = sub("ProgressId");
        p.set_int("opaque_count", n as i64);
        bid("progress", p)
    }
    pub fn unstructured_command_line() -> Msg {
        bid(
            "unstructured_command_line",
            sub("UnstructuredCommandLineId"),
        )
    }
    pub fn structured_command_line(label: &str) -> Msg {
        let mut p = sub("StructuredCommandLineId");
        p.set_str("command_line_label", label);
        bid("structured_command_line", p)
    }
    pub fn workspace_status() -> Msg {
        bid("workspace_status", sub("WorkspaceStatusId"))
    }
    pub fn options_parsed() -> Msg {
        bid("options_parsed", sub("OptionsParsedId"))
    }
    pub fn fetch(url: &str, downloader: &str) -> Msg {
        let mut p = sub("FetchId");
        p.set_str("url", url);
        p.set_enum("downloader", downloader);
        bid("fetch", p)
    }
    pub fn configuration(id: &str) -> Msg {
        bid("configuration", configuration_id(id))
    }
    pub fn configuration_id(id: &str) -> Msg {
        let mut p = sub("ConfigurationId");
        p.set_str("id", id);
        p
    }
    pub fn none_configuration() -> Msg {
        configuration_id("none")
    }
    pub fn null_configuration() -> Msg {
        bid("configuration", none_configuration())
    }
    pub fn pattern(patterns: &[String]) -> Msg {
        let mut p = sub("PatternExpandedId");
        for s in patterns {
            p.set_str("pattern", s.clone());
        }
        bid("pattern", p)
    }
    pub fn pattern_skipped(patterns: &[String]) -> Msg {
        let mut p = sub("PatternExpandedId");
        for s in patterns {
            p.set_str("pattern", s.clone());
        }
        bid("pattern_skipped", p)
    }
    pub fn target_configured(label: &str) -> Msg {
        let mut p = sub("TargetConfiguredId");
        p.set_str("label", label);
        bid("target_configured", p)
    }
    pub fn aspect_configured(label: &str, aspect: &str) -> Msg {
        let mut p = sub("TargetConfiguredId");
        p.set_str("label", label);
        if !aspect.is_empty() {
            p.set_str("aspect", aspect);
        }
        bid("target_configured", p)
    }
    pub fn named_set(id: &str) -> Msg {
        let mut p = sub("NamedSetOfFilesId");
        p.set_str("id", id);
        bid("named_set", p)
    }
    pub fn named_set_id(id: &str) -> Msg {
        let mut p = sub("NamedSetOfFilesId");
        p.set_str("id", id);
        p
    }
    pub fn target_completed(label: &str, config_id: &str) -> Msg {
        let mut p = sub("TargetCompletedId");
        p.set_str("label", label);
        p.set_msg("configuration", configuration_id(config_id));
        bid("target_completed", p)
    }
    pub fn aspect_completed(label: &str, config_id: &str, aspect: &str) -> Msg {
        let mut p = sub("TargetCompletedId");
        p.set_str("label", label);
        p.set_msg("configuration", configuration_id(config_id));
        if !aspect.is_empty() {
            p.set_str("aspect", aspect);
        }
        bid("target_completed", p)
    }
    pub fn action_completed(primary_output: &str, label: &str, config_id: &str) -> Msg {
        let mut p = sub("ActionCompletedId");
        p.set_str("primary_output", primary_output);
        p.set_str("label", label);
        p.set_msg("configuration", configuration_id(config_id));
        bid("action_completed", p)
    }
    pub fn unconfigured_label(label: &str) -> Msg {
        let mut p = sub("UnconfiguredLabelId");
        p.set_str("label", label);
        bid("unconfigured_label", p)
    }
    pub fn configured_label(label: &str, config_id: &str) -> Msg {
        let mut p = sub("ConfiguredLabelId");
        p.set_str("label", label);
        p.set_msg("configuration", configuration_id(config_id));
        bid("configured_label", p)
    }
    /// `BuildEventIdUtil.testResult`: `run` and `shard` are 0-indexed and stored 1-indexed in BEP.
    pub fn test_result(label: &str, config_id: &str, run: i32, shard: i32, attempt: i32) -> Msg {
        let mut p = sub("TestResultId");
        p.set_str("label", label);
        p.set_msg("configuration", configuration_id(config_id));
        p.set_int("run", run.wrapping_add(1) as i64);
        p.set_int("shard", shard.wrapping_add(1) as i64);
        p.set_int("attempt", attempt as i64);
        bid("test_result", p)
    }
    pub fn test_result_first_attempt(label: &str, run: i32, shard: i32, config_id: &str) -> Msg {
        test_result(label, config_id, run, shard, 1)
    }
    pub fn test_progress(label: &str, config_id: &str, run: i32, shard: i32, attempt: i32) -> Msg {
        test_progress_counted(label, config_id, run, shard, attempt, 0)
    }
    pub fn test_progress_counted(
        label: &str,
        config_id: &str,
        run: i32,
        shard: i32,
        attempt: i32,
        opaque_count: i32,
    ) -> Msg {
        let mut p = sub("TestProgressId");
        p.set_str("label", label);
        p.set_msg("configuration", configuration_id(config_id));
        p.set_int("run", run.wrapping_add(1) as i64);
        p.set_int("shard", shard.wrapping_add(1) as i64);
        p.set_int("attempt", attempt as i64);
        if opaque_count != 0 {
            p.set_int("opaque_count", opaque_count as i64);
        }
        bid("test_progress", p)
    }
    pub fn test_summary(label: &str, config_id: &str) -> Msg {
        let mut p = sub("TestSummaryId");
        p.set_str("label", label);
        p.set_msg("configuration", configuration_id(config_id));
        bid("test_summary", p)
    }
    pub fn target_summary(label: &str, config_id: &str) -> Msg {
        let mut p = sub("TargetSummaryId");
        p.set_str("label", label);
        p.set_msg("configuration", configuration_id(config_id));
        bid("target_summary", p)
    }
    pub fn build_finished() -> Msg {
        bid("build_finished", sub("BuildFinishedId"))
    }
    pub fn build_tool_logs() -> Msg {
        bid("build_tool_logs", sub("BuildToolLogsId"))
    }
    pub fn build_metrics() -> Msg {
        bid("build_metrics", sub("BuildMetricsId"))
    }
    pub fn workspace() -> Msg {
        bid("workspace", sub("WorkspaceConfigId"))
    }
    pub fn build_metadata() -> Msg {
        bid("build_metadata", sub("BuildMetadataId"))
    }
    pub fn convenience_symlinks_identified() -> Msg {
        bid(
            "convenience_symlinks_identified",
            sub("ConvenienceSymlinksIdentifiedId"),
        )
    }
    pub fn exec_request() -> Msg {
        bid("exec_request", sub("ExecRequestId"))
    }
    pub fn unknown(details: &str) -> Msg {
        let mut p = sub("UnknownBuildEventId");
        p.set_str("details", details);
        bid("unknown", p)
    }
}

/// A BEP event before it is stamped into a `BuildEvent` proto by the streamer.
#[derive(Debug, Clone)]
pub struct BuildEventNode {
    pub id: Msg,
    pub children: Vec<Msg>,
    /// A `BuildEvent` with exactly one `payload` member set (id/children are filled in later).
    pub payload: Msg,
    /// Event ids that must have been posted before this event (`BuildEventWithOrderConstraint`).
    pub posted_after: Vec<Msg>,
    /// Marks events that complete the build (`BuildCompletingEvent`): after posting, the
    /// streamer posts the final progress event and aborts anything still announced.
    pub completes_build: bool,
    /// Marks replaceable events (`ReplaceableBuildEvent`) buffered until replaced or build completion.
    pub replaceable: bool,
}

impl BuildEventNode {
    /// Creates a node with payload member `payload_field` set to `body`.
    pub fn new(id: Msg, children: Vec<Msg>, payload_field: &str, body: Msg) -> Self {
        let mut payload = Msg::new("build_event_stream.BuildEvent");
        payload.set_msg(payload_field, body);
        Self {
            id,
            children,
            payload,
            posted_after: Vec::new(),
            completes_build: false,
            replaceable: false,
        }
    }

    pub fn after(mut self, id: Msg) -> Self {
        self.posted_after.push(id);
        self
    }

    pub fn completing(mut self) -> Self {
        self.completes_build = true;
        self
    }

    pub fn as_replaceable(mut self) -> Self {
        self.replaceable = true;
        self
    }

    /// Materializes the full proto (`GenericBuildEvent.protoChaining`).
    pub fn to_proto(&self, last_message: bool) -> Msg {
        let mut p = self.payload.clone();
        p.set_msg("id", self.id.clone());
        for c in &self.children {
            p.set_msg("children", c.clone());
        }
        if last_message {
            p.set_bool("last_message", true);
        }
        p
    }
}

fn key(id: &Msg) -> Vec<u8> {
    id.encode_to_vec()
}

/// Destination for serialized events (`BuildEventTransport`).
pub trait BuildEventTransport: Send + Sync {
    fn name(&self) -> String;
    fn send(&self, event: &Msg);
    fn close(&self) -> Result<(), String>;
}

/// Output encodings for file transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BepFormat {
    /// `JsonFormatFileTransport`: one compact JSON object per line.
    Json,
    /// `BinaryFormatFileTransport`: varint-delimited protos.
    Binary,
    /// `TextFormatFileTransport`: `event {\n<text>}\n\n`.
    Text,
}

/// Serializes one event in the given file format.
pub fn serialize_event(format: BepFormat, event: &Msg) -> Vec<u8> {
    serialize_event_with_options(format, event, true, None)
}

/// Serializes one event, honoring `--build_event_*_file_path_conversion`.
pub fn serialize_event_with_conversion(
    format: BepFormat,
    event: &Msg,
    path_conversion: bool,
) -> Vec<u8> {
    serialize_event_with_options(format, event, path_conversion, None)
}

/// Serializes one event, applying Bazel's BEP `PathConverter` semantics:
/// when `path_conversion` is `true` and `bytestream_prefix` is configured, files carrying a digest
/// use `bytestream://<prefix>/blobs/<digest>/<length>` (`ByteStreamBuildEventArtifactUploader`);
/// when `path_conversion` is `false` (or no remote uploader is active), `LocalFilesArtifactUploader`
/// semantics apply (`file://` URIs for regular files and omitted `uri` on `directory_output`).
pub fn serialize_event_with_options(
    format: BepFormat,
    event: &Msg,
    path_conversion: bool,
    bytestream_prefix: Option<&str>,
) -> Vec<u8> {
    let rewritten = apply_file_path_conversion(event, path_conversion, bytestream_prefix);
    let ev = &rewritten;
    match format {
        BepFormat::Json => {
            let mut s = ev.to_json();
            s.push('\n');
            s.into_bytes()
        }
        BepFormat::Binary => {
            let body = ev.encode_to_vec();
            let mut out = Vec::with_capacity(body.len() + 5);
            zel_proto::wire::encode_varint(body.len() as u64, &mut out);
            out.extend_from_slice(&body);
            out
        }
        BepFormat::Text => format!("event {{\n{}}}\n\n", ev.to_text()).into_bytes(),
    }
}

fn has_windows_drive_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

/// Decodes a `file://` URI back into a raw filesystem path.
pub fn file_uri_to_local_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let rest = if rest.len() >= 10 && rest[..10].eq_ignore_ascii_case("localhost/") {
        &rest[9..]
    } else {
        rest
    };
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                out.push(((hi << 4) | lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    let decoded = String::from_utf8(out).ok()?;
    if decoded.starts_with('/') && has_windows_drive_prefix(&decoded[1..]) {
        Some(decoded[1..].to_string())
    } else {
        Some(decoded)
    }
}

fn convert_file_msg(f: &Msg, path_conversion: bool, bytestream_prefix: Option<&str>) -> Msg {
    let mut c = f.clone();
    let uri = c.get_str("uri").to_string();
    if uri.is_empty() {
        return c;
    }
    if path_conversion {
        if let Some(prefix) = bytestream_prefix.filter(|p| !p.is_empty()) {
            if uri.starts_with("bytestream://") {
                return c;
            }
            let p = prefix.trim_end_matches('/');
            let digest = c.get_str("digest").to_string();
            if !digest.is_empty() {
                let len = c.get_i64("length");
                c.clear("uri");
                c.set_str("uri", format!("bytestream://{p}/blobs/{digest}/{len}"));
                return c;
            }
            // For `ActionExecuted`, `TestResult`, `TestSummary`, and `BuildToolLogs` (where the
            // `File` proto does not pre-populate `digest`), `ByteStreamBuildEventArtifactUploader`
            // hashes the referenced local file on disk (`readPathMetadata` -> `digestUtil.compute`).
            // When the local file cannot be hashed (missing, directory, or unreadable),
            // `PathConverterImpl.apply` returns `null` so `File.uri` is omitted rather than
            // exposing a `file://` URI pointing at the builder's local disk.
            let local_path = if let Some(decoded) = file_uri_to_local_path(&uri) {
                Some(decoded)
            } else if !uri.contains("://") {
                Some(uri)
            } else {
                // Non-local URI scheme (e.g. pre-uploaded `https://` in `BuildToolLogs.futureUris`).
                return c;
            };
            c.clear("uri");
            if let Some(lp) = local_path {
                if let Ok((len, hex_digest)) =
                    zel_util::sha256::hash_file(std::path::Path::new(&lp))
                {
                    c.set_str("uri", format!("bytestream://{p}/blobs/{hex_digest}/{len}"));
                }
            }
            return c;
        }
        if !uri.contains("://") {
            c.clear("uri");
            c.set_str("uri", file_uri(&uri));
        }
    } else if uri.starts_with("file://") {
        // `LocalFilesArtifactUploader` (`FileLocalPathConverter`) always emits `file://` URIs.
    } else if !uri.contains("://") {
        c.clear("uri");
        c.set_str("uri", file_uri(&uri));
    } else {
        // Remote URI (e.g. `bytestream://`) with `--nobuild_event_*_file_path_conversion`:
        // `LocalFilesArtifactUploader` never emits remote URIs.
        c.clear("uri");
    }
    c
}

fn convert_singular_file(
    parent: &mut Msg,
    field: &str,
    path_conversion: bool,
    bytestream_prefix: Option<&str>,
) {
    if let Some(f) = parent.get_msg(field).cloned() {
        parent.clear(field);
        parent.set_msg(
            field,
            convert_file_msg(&f, path_conversion, bytestream_prefix),
        );
    }
}

fn convert_repeated_files(
    parent: &mut Msg,
    field: &str,
    path_conversion: bool,
    bytestream_prefix: Option<&str>,
) {
    let files: Vec<Msg> = parent
        .get_msgs(field)
        .into_iter()
        .map(|f| convert_file_msg(f, path_conversion, bytestream_prefix))
        .collect();
    if !files.is_empty() {
        parent.clear(field);
        for f in files {
            parent.set_msg(field, f);
        }
    }
}

/// Applies Bazel BEP `PathConverter` rules across every `build_event_stream.File` in `event`.
pub fn apply_file_path_conversion(
    event: &Msg,
    path_conversion: bool,
    bytestream_prefix: Option<&str>,
) -> Msg {
    let mut ev = event.clone();
    if let Some(ns) = ev.get_msg("named_set_of_files").cloned() {
        let mut ns = ns;
        convert_repeated_files(&mut ns, "files", path_conversion, bytestream_prefix);
        ev.clear("named_set_of_files");
        ev.set_msg("named_set_of_files", ns);
    }
    if let Some(ae) = ev.get_msg("action").cloned() {
        let mut ae = ae;
        convert_singular_file(&mut ae, "stdout", path_conversion, bytestream_prefix);
        convert_singular_file(&mut ae, "stderr", path_conversion, bytestream_prefix);
        convert_singular_file(
            &mut ae,
            "primary_output",
            path_conversion,
            bytestream_prefix,
        );
        ev.clear("action");
        ev.set_msg("action", ae);
    }
    if let Some(tc) = ev.get_msg("completed").cloned() {
        let mut tc = tc;
        convert_repeated_files(
            &mut tc,
            "important_output",
            path_conversion,
            bytestream_prefix,
        );
        let groups: Vec<Msg> = tc
            .get_msgs("output_group")
            .into_iter()
            .map(|g| {
                let mut g = g.clone();
                convert_repeated_files(&mut g, "inline_files", path_conversion, bytestream_prefix);
                g
            })
            .collect();
        if !groups.is_empty() {
            tc.clear("output_group");
            for g in groups {
                tc.set_msg("output_group", g);
            }
        }
        let dirs: Vec<Msg> = tc
            .get_msgs("directory_output")
            .into_iter()
            .map(|d| {
                let mut d = d.clone();
                // Per `build_event_stream.proto` and both `LocalFilesArtifactUploader` and
                // `ByteStreamBuildEventArtifactUploader`, `directory_output` entries never carry `uri`.
                d.clear("uri");
                d
            })
            .collect();
        if !dirs.is_empty() {
            tc.clear("directory_output");
            for d in dirs {
                tc.set_msg("directory_output", d);
            }
        }
        ev.clear("completed");
        ev.set_msg("completed", tc);
    }
    if let Some(btl) = ev.get_msg("build_tool_logs").cloned() {
        let mut btl = btl;
        convert_repeated_files(&mut btl, "log", path_conversion, bytestream_prefix);
        ev.clear("build_tool_logs");
        ev.set_msg("build_tool_logs", btl);
    }
    if let Some(tr) = ev.get_msg("test_result").cloned() {
        let mut tr = tr;
        convert_repeated_files(
            &mut tr,
            "test_action_output",
            path_conversion,
            bytestream_prefix,
        );
        ev.clear("test_result");
        ev.set_msg("test_result", tr);
    }
    if let Some(ts) = ev.get_msg("test_summary").cloned() {
        let mut ts = ts;
        convert_repeated_files(&mut ts, "passed", path_conversion, bytestream_prefix);
        convert_repeated_files(&mut ts, "failed", path_conversion, bytestream_prefix);
        ev.clear("test_summary");
        ev.set_msg("test_summary", ts);
    }
    ev
}

/// Streaming file transport; every event is written and flushed as it is sent.
pub struct FileTransport {
    format: BepFormat,
    label: String,
    path_conversion: bool,
    bytestream_prefix: Option<String>,
    sink: Mutex<Box<dyn Write + Send>>,
    error: Mutex<Option<String>>,
}

impl FileTransport {
    pub fn new(format: BepFormat, label: impl Into<String>, sink: Box<dyn Write + Send>) -> Self {
        Self::new_with_conversion(format, label, true, sink)
    }

    pub fn new_with_conversion(
        format: BepFormat,
        label: impl Into<String>,
        path_conversion: bool,
        sink: Box<dyn Write + Send>,
    ) -> Self {
        Self::new_with_options(format, label, path_conversion, None, sink)
    }

    pub fn new_with_options(
        format: BepFormat,
        label: impl Into<String>,
        path_conversion: bool,
        bytestream_prefix: Option<String>,
        sink: Box<dyn Write + Send>,
    ) -> Self {
        Self {
            format,
            label: label.into(),
            path_conversion,
            bytestream_prefix,
            sink: Mutex::new(sink),
            error: Mutex::new(None),
        }
    }

    /// Creates `path` (and parent directories) on the real filesystem.
    pub fn open(format: BepFormat, path: &str) -> std::io::Result<Self> {
        Self::open_with_conversion(format, path, true)
    }

    pub fn open_with_conversion(
        format: BepFormat,
        path: &str,
        path_conversion: bool,
    ) -> std::io::Result<Self> {
        Self::open_with_options(format, path, path_conversion, None)
    }

    pub fn open_with_options(
        format: BepFormat,
        path: &str,
        path_conversion: bool,
        bytestream_prefix: Option<String>,
    ) -> std::io::Result<Self> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let f = std::fs::File::create(path)?;
        Ok(Self::new_with_options(
            format,
            path,
            path_conversion,
            bytestream_prefix,
            Box::new(f),
        ))
    }
}

impl BuildEventTransport for FileTransport {
    fn name(&self) -> String {
        match self.format {
            BepFormat::Json => "JsonFormatFileTransport",
            BepFormat::Binary => "BinaryFormatFileTransport",
            BepFormat::Text => "TextFormatFileTransport",
        }
        .to_string()
    }

    fn send(&self, event: &Msg) {
        let bytes = serialize_event_with_options(
            self.format,
            event,
            self.path_conversion,
            self.bytestream_prefix.as_deref(),
        );
        let mut sink = self.sink.lock().unwrap();
        if let Err(e) = sink.write_all(&bytes).and_then(|_| sink.flush()) {
            *self.error.lock().unwrap() = Some(format!("{}: {e}", self.label));
        }
    }

    fn close(&self) -> Result<(), String> {
        let _ = self.sink.lock().unwrap().flush();
        match self.error.lock().unwrap().take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// In-memory transport (tests and history).
#[derive(Default)]
pub struct MemoryTransport {
    pub events: Mutex<Vec<Msg>>,
}

impl BuildEventTransport for MemoryTransport {
    fn name(&self) -> String {
        "MemoryTransport".into()
    }
    fn send(&self, event: &Msg) {
        self.events.lock().unwrap().push(event.clone());
    }
    fn close(&self) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Default)]
struct State {
    frontier: Option<HashSet<Vec<u8>>>,
    posted: HashSet<Vec<u8>>,
    progress_count: i32,
    buffered_pairs: Option<Vec<(String, String)>>,
    out_chunks: Vec<String>,
    err_chunks: Vec<String>,
    /// stdout/stderr alternation tracking (`ProgressBufferState`).
    saw_stdout_after_stderr: bool,
    pending: Vec<(Vec<u8>, BuildEventNode)>,
    replaceable: Vec<(Vec<u8>, BuildEventNode)>,
    final_events_to_come: Option<HashSet<Vec<u8>>>,
    closed: bool,
    abort_reason: Option<(String, String)>,
    configurations_posted: HashSet<String>,
}

/// Orders and streams BEP events (`BuildEventStreamer`): maintains the announced/posted event
/// frontier, chains progress events (carrying stdout/stderr), marks `last_message`, holds
/// order-constrained events until prerequisites arrive, and aborts unposted announced events when
/// the stream closes.
pub struct BuildEventStreamer {
    transports: Vec<Arc<dyn BuildEventTransport>>,
    state: Mutex<State>,
    publish_all_actions: bool,
    close_errors: Mutex<Vec<String>>,
}

fn pairs(out: &[String], err: &[String]) -> Vec<(String, String)> {
    // `consumeAsPairs`: stdout chunks paired with stderr, stderr first-class on the last left.
    let mut res = Vec::new();
    if out.is_empty() && err.is_empty() {
        return res;
    }
    let mut ri = 0;
    for (i, l) in out.iter().enumerate() {
        let last_left = i + 1 == out.len();
        let right = if last_left && ri < err.len() {
            ri += 1;
            err[ri - 1].clone()
        } else {
            String::new()
        };
        res.push((l.clone(), right));
    }
    while ri < err.len() {
        res.push((String::new(), err[ri].clone()));
        ri += 1;
    }
    res
}

impl BuildEventStreamer {
    pub fn new(transports: Vec<Arc<dyn BuildEventTransport>>, publish_all_actions: bool) -> Self {
        Self {
            transports,
            state: Mutex::new(State::default()),
            publish_all_actions,
            close_errors: Mutex::new(Vec::new()),
        }
    }

    pub fn publish_all_actions(&self) -> bool {
        self.publish_all_actions
    }

    pub fn is_closed(&self) -> bool {
        self.state.lock().unwrap().closed
    }

    pub fn close_errors(&self) -> Vec<String> {
        self.close_errors.lock().unwrap().clone()
    }

    /// Records build stdout to be carried by the next progress event.
    pub fn report_stdout(&self, s: &str) {
        let mut st = self.state.lock().unwrap();
        if st.final_events_to_come.is_some() || s.is_empty() {
            return;
        }
        st.out_chunks.push(s.to_string());
    }

    /// Records build stderr to be carried by the next progress event.
    pub fn report_stderr(&self, s: &str) {
        let mut st = self.state.lock().unwrap();
        if st.final_events_to_come.is_some() || s.is_empty() {
            return;
        }
        st.err_chunks.push(s.to_string());
    }

    fn send_all(&self, ev: &Msg) {
        for t in &self.transports {
            t.send(ev);
        }
    }

    fn take_out_err(st: &mut State) -> Vec<(String, String)> {
        // The UI's buffered output reaches the streamer as one chunk per stream.
        let join = |v: Vec<String>| if v.len() > 1 { vec![v.concat()] } else { v };
        let out = join(std::mem::take(&mut st.out_chunks));
        let err = join(std::mem::take(&mut st.err_chunks));
        st.saw_stdout_after_stderr = false;
        pairs(&out, &err)
    }

    fn progress_node(
        n: i32,
        out: &str,
        err: &str,
        chain_in: Option<&Msg>,
        has_next: bool,
    ) -> BuildEventNode {
        let mut children = Vec::new();
        if has_next {
            children.push(ids::progress(n + 1));
        }
        if let Some(c) = chain_in {
            children.push(c.clone());
        }
        let mut body = Msg::new("build_event_stream.Progress");
        if !out.is_empty() {
            body.set_str("stdout", out);
        }
        if !err.is_empty() {
            body.set_str("stderr", err);
        }
        BuildEventNode::new(ids::progress(n), children, "progress", body)
    }

    fn register(st: &mut State, ids: &[Msg]) {
        if st.final_events_to_come.is_some() {
            return;
        }
        let progress_count = st.progress_count;
        let posted: Vec<bool> = ids
            .iter()
            .map(|i| Self::is_posted(st, i, progress_count))
            .collect();
        if let Some(f) = st.frontier.as_mut() {
            for (i, p) in ids.iter().zip(posted) {
                if !p {
                    f.insert(key(i));
                }
            }
        }
    }

    fn is_posted(st: &State, id: &Msg, progress_count: i32) -> bool {
        if id.has("progress") {
            id.get_msg("progress")
                .map(|p| p.get_i64("opaque_count") < progress_count as i64)
                .unwrap_or(false)
        } else {
            st.posted.contains(&key(id))
        }
    }

    fn record_posted(st: &mut State, id: &Msg) {
        if id.has("progress") {
            st.progress_count += 1;
        } else {
            st.posted.insert(key(id));
        }
    }

    /// Core of `BuildEventStreamer.post`. Returns events to send (links first, then main).
    fn post_locked(&self, st: &mut State, node: &BuildEventNode) {
        let id = &node.id;
        let mut link: Vec<BuildEventNode> = Vec::new();
        let mut flush: Vec<BuildEventNode> = Vec::new();
        if st.frontier.is_none() {
            st.frontier = Some(HashSet::new());
            Self::register(st, std::slice::from_ref(id));
            let is_initial = id.has("progress")
                && id
                    .get_msg("progress")
                    .map(|p| p.get_i64("opaque_count") == 0)
                    .unwrap_or(false);
            let announces_initial = node.children.iter().any(|c| {
                c.has("progress") && c.get_msg("progress").unwrap().get_i64("opaque_count") == 0
            });
            if !is_initial && !announces_initial {
                let p = Self::progress_node(st.progress_count, "", "", Some(id), true);
                st.progress_count += 1;
                Self::register(st, &p.children);
                link.push(p);
            }
            if let Some(buf) = st.buffered_pairs.take() {
                for (o, e) in buf {
                    let n = Self::progress_node(st.progress_count, &o, &e, None, true);
                    st.progress_count += 1;
                    Self::register(st, &n.children);
                    if let Some(f) = st.frontier.as_mut() {
                        f.remove(&key(&n.id));
                    }
                    flush.push(n);
                }
            }
            st.buffered_pairs = None;
        } else {
            let k = key(id);
            let announced = st
                .frontier
                .as_ref()
                .map(|f| f.contains(&k))
                .unwrap_or(false);
            if !announced && !Self::is_posted(st, id, st.progress_count) {
                // `consumeAsPairs`: with nothing buffered the last consumer still runs once with
                // (null, null), so an unannounced event is always chained in via a progress event.
                let mut chain = Self::take_out_err(st);
                if chain.is_empty() {
                    chain.push((String::new(), String::new()));
                } else if chain.len() > 1 {
                    // The UI writes its buffered output as one chunk per stream, so everything
                    // pending is carried by a single progress event.
                    let o: String = chain.iter().map(|p| p.0.as_str()).collect();
                    let e: String = chain.iter().map(|p| p.1.as_str()).collect();
                    chain = vec![(o, e)];
                }
                for (o, e) in chain {
                    let n = Self::progress_node(st.progress_count, &o, &e, Some(id), true);
                    st.progress_count += 1;
                    Self::register(st, &n.children);
                    if let Some(f) = st.frontier.as_mut() {
                        f.remove(&key(&n.id));
                    }
                    link.push(n);
                }
            }
        }

        // BuildInfo-like and Configuration duplicates are ignored.
        if !id.has("progress")
            && st.posted.contains(&key(id))
            && (id.has("workspace_status") || id.has("configuration"))
        {
            return;
        }
        if let Some(cfg) = id.get_msg("configuration") {
            st.configurations_posted
                .insert(cfg.get_str("id").to_string());
        }

        Self::record_posted(st, id);
        if let Some(f) = st.frontier.as_mut() {
            f.remove(&key(id));
        }
        Self::register(st, &node.children);
        let last = st.frontier.as_ref().map(|f| f.is_empty()).unwrap_or(false);
        for l in &link {
            self.send_all(&l.to_proto(false));
        }
        self.send_all(&node.to_proto(last));
        for f in &flush {
            self.send_all(&f.to_proto(false));
        }
    }

    /// Posts an event, honoring ordering constraints and post-completion filtering
    /// (`BuildEventStreamer.buildEvent`).
    pub fn post(&self, node: BuildEventNode) {
        if !self.publish_all_actions {
            if let Some(act) = node.payload.get_msg("action") {
                if act.get_bool("success") && act.get_str("type") != "ExtraAction" {
                    return;
                }
            }
        }
        let mut close_now = false;
        {
            let mut st = self.state.lock().unwrap();
            if st.closed {
                return;
            }
            if let Some(final_ids) = st.final_events_to_come.as_mut() {
                if !final_ids.remove(&key(&node.id)) {
                    return;
                }
            }
            self.route_locked(&mut st, node);
            if let Some(f) = &st.final_events_to_come {
                if f.is_empty() {
                    close_now = true;
                }
            }
        }
        if close_now {
            self.close();
        }
    }

    fn extract_config_id(id: &Msg) -> Option<&str> {
        for field in [
            "target_completed",
            "action_completed",
            "configured_label",
            "test_result",
            "test_progress",
            "test_summary",
            "target_summary",
        ] {
            if let Some(sub) = id.get_msg(field) {
                if let Some(cfg) = sub.get_msg("configuration") {
                    let cid = cfg.get_str("id");
                    if !cid.is_empty() {
                        return Some(cid);
                    }
                }
            }
        }
        None
    }

    fn ensure_null_configuration(&self, st: &mut State, node: &BuildEventNode) {
        let needs_null = Self::extract_config_id(&node.id) == Some("none")
            || node
                .children
                .iter()
                .any(|c| Self::extract_config_id(c) == Some("none"));
        if needs_null && !st.configurations_posted.contains("none") {
            st.configurations_posted.insert("none".to_string());
            let null_cfg = BepBuilder::null_configuration();
            self.post_locked(st, &null_cfg);
        }
    }

    fn route_locked(&self, st: &mut State, node: BuildEventNode) {
        if node.replaceable {
            let k = key(&node.id);
            if st.posted.contains(&k) {
                return;
            }
            if let Some(existing) = st.replaceable.iter_mut().find(|(ek, _)| *ek == k) {
                existing.1 = node;
            } else {
                st.replaceable.push((k, node));
            }
            return;
        } else {
            let k = key(&node.id);
            st.replaceable.retain(|(ek, _)| *ek != k);
        }
        // Buffer until prerequisites have been posted.
        for after in &node.posted_after {
            if !Self::is_posted(st, after, st.progress_count) {
                st.pending.push((key(after), node));
                return;
            }
        }
        self.ensure_null_configuration(st, &node);
        // Ensure the artificial start event exists when the build ends early.
        if node.completes_build
            && !st.posted.contains(&key(&ids::started()))
            && st.frontier.is_none()
        {
            // Nothing posted yet: the completing event itself becomes the stream root.
        }
        let id = node.id.clone();
        let completes = node.completes_build;
        let children = node.children.clone();
        self.post_locked(st, &node);
        // Release events blocked on this one.
        let k = key(&id);
        let mut freed = Vec::new();
        let mut i = 0;
        while i < st.pending.len() {
            if st.pending[i].0 == k {
                freed.push(st.pending.remove(i).1);
            } else {
                i += 1;
            }
        }
        for f in freed {
            self.route_locked(st, f);
        }
        if completes {
            self.build_complete_locked(st, &children);
        }
    }

    fn abort_info(st: &State) -> (String, String) {
        st.abort_reason
            .clone()
            .unwrap_or(("UNKNOWN".into(), String::new()))
    }

    fn aborted_node(id: Msg, children: Vec<Msg>, reason: &(String, String)) -> BuildEventNode {
        let mut a = Msg::new("build_event_stream.Aborted");
        a.set_enum("reason", &reason.0);
        if !reason.1.is_empty() {
            a.set_str("description", reason.1.clone());
        }
        BuildEventNode::new(id, children, "aborted", a)
    }

    fn build_complete_locked(&self, st: &mut State, completing_children: &[Msg]) {
        // Flush any buffered ReplaceableBuildEvent entries that were not replaced by a final post.
        let replaceable = std::mem::take(&mut st.replaceable);
        for (rk, mut rnode) in replaceable {
            if !st.posted.contains(&rk) {
                rnode.replaceable = false;
                self.ensure_null_configuration(st, &rnode);
                self.post_locked(st, &rnode);
            }
        }
        // clearPendingEvents: abort anything blocked on never-posted prerequisites.
        let reason = Self::abort_info(st);
        while !st.pending.is_empty() {
            let (await_key, _) = st.pending[0].clone();
            let mut buffered = Vec::new();
            let mut i = 0;
            while i < st.pending.len() {
                if st.pending[i].0 == await_key {
                    buffered.push(st.pending.remove(i).1);
                } else {
                    i += 1;
                }
            }
            if !st.posted.contains(&await_key) {
                if let Ok(await_id) = Msg::decode("build_event_stream.BuildEventId", &await_key) {
                    let children: Vec<Msg> = buffered.iter().map(|n| n.id.clone()).collect();
                    let node = Self::aborted_node(await_id, children, &reason);
                    self.post_locked(st, &node);
                }
            }
        }
        // Final progress event, carrying remaining output.
        let ps = if st.buffered_pairs.is_some() {
            let mut buf = st.buffered_pairs.take().unwrap_or_default();
            buf.extend(Self::take_out_err(st));
            buf
        } else {
            Self::take_out_err(st)
        };
        if ps.is_empty() {
            let n = Self::progress_node(st.progress_count, "", "", None, false);
            self.post_locked(st, &n);
        } else {
            let len = ps.len();
            for (i, (o, e)) in ps.into_iter().enumerate() {
                let n = Self::progress_node(st.progress_count, &o, &e, None, i + 1 != len);
                self.post_locked(st, &n);
            }
        }
        // clearFrontierEvents (except the completing event's children).
        let keep: HashSet<Vec<u8>> = completing_children.iter().map(key).collect();
        let ids: Vec<Vec<u8>> = st
            .frontier
            .as_ref()
            .map(|f| f.iter().cloned().collect())
            .unwrap_or_default();
        for k in ids {
            if keep.contains(&k) {
                continue;
            }
            if let Ok(id) = Msg::decode("build_event_stream.BuildEventId", &k) {
                let node = Self::aborted_node(id, vec![], &reason);
                self.post_locked(st, &node);
            }
        }
        let remaining: HashSet<Vec<u8>> = st.frontier.clone().unwrap_or_default();
        st.final_events_to_come = Some(remaining);
        if st
            .final_events_to_come
            .as_ref()
            .map(HashSet::is_empty)
            .unwrap_or(true)
        {
            // close() is invoked by the caller path when empty.
        }
    }

    /// Sets the reason used for `aborted` events generated at close.
    pub fn add_abort_reason(&self, reason: &str, description: &str) {
        self.state.lock().unwrap().abort_reason =
            Some((reason.to_string(), description.to_string()));
    }

    /// Whether a configuration event was already posted for `id` (dedup helper).
    pub fn configuration_posted(&self, id: &str) -> bool {
        !self
            .state
            .lock()
            .unwrap()
            .configurations_posted
            .insert(id.to_string())
    }

    /// Flushes accumulated stdout/stderr into progress events immediately (`flush()`).
    pub fn flush(&self) {
        let mut st = self.state.lock().unwrap();
        if st.final_events_to_come.is_some() {
            Self::take_out_err(&mut st);
            return;
        }
        let ps = Self::take_out_err(&mut st);
        if ps.is_empty() {
            return;
        }
        if st.frontier.is_some() {
            for (o, e) in ps {
                let n = Self::progress_node(st.progress_count, &o, &e, None, true);
                st.progress_count += 1;
                Self::register(&mut st, &n.children);
                if let Some(f) = st.frontier.as_mut() {
                    f.remove(&key(&n.id));
                }
                self.send_all(&n.to_proto(false));
            }
        } else {
            st.buffered_pairs.get_or_insert_with(Vec::new).extend(ps);
        }
    }

    /// Closes the stream, aborting anything still announced (`close`).
    pub fn close(&self) {
        {
            let mut st = self.state.lock().unwrap();
            if st.closed {
                return;
            }
            st.closed = true;
            if st.final_events_to_come.is_none() {
                // Crash/early close: finish the stream as best we can.
                if st.frontier.is_some() || st.buffered_pairs.is_some() || !st.out_chunks.is_empty()
                {
                    self.build_complete_locked(&mut st, &[]);
                }
            }
        }
        for t in &self.transports {
            if let Err(e) = t.close() {
                self.close_errors
                    .lock()
                    .unwrap()
                    .push(format!("{}: {e}", t.name()));
            }
        }
    }
}

/// Convenience builder that creates correctly linked high-level BEP events.
pub struct BepBuilder;

impl BepBuilder {
    pub fn aborted(id: Msg, reason: &str, description: &str) -> BuildEventNode {
        BuildEventStreamer::aborted_node(id, vec![], &(reason.to_string(), description.to_string()))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build_started(
        uuid: &str,
        start_time_millis: i64,
        build_tool_version: &str,
        options_description: &str,
        command: &str,
        working_directory: &str,
        workspace_directory: &str,
        server_pid: i64,
        host: &str,
        user: &str,
        children: Vec<Msg>,
    ) -> BuildEventNode {
        let mut b = Msg::new("build_event_stream.BuildStarted");
        b.set_str("uuid", uuid);
        let mut ts = b.new_child("start_time");
        ts.set_int("seconds", start_time_millis.div_euclid(1000));
        ts.set_int("nanos", start_time_millis.rem_euclid(1000) * 1_000_000);
        b.set_msg("start_time", ts);
        b.set_str("build_tool_version", build_tool_version);
        b.set_str("options_description", options_description);
        b.set_str("command", command);
        b.set_str("working_directory", working_directory);
        b.set_str("workspace_directory", workspace_directory);
        b.set_int("server_pid", server_pid);
        b.set_str("host", host);
        b.set_str("user", user);
        BuildEventNode::new(ids::started(), children, "started", b)
    }

    /// Children announced by `BuildStartingEvent`: initial progress, command lines, options,
    /// workspace status, metadata, the pattern and the finish event.
    pub fn started_children(patterns: &[String]) -> Vec<Msg> {
        let mut c = vec![
            ids::progress(0),
            ids::unstructured_command_line(),
            ids::structured_command_line("original"),
            ids::structured_command_line("canonical"),
            ids::structured_command_line("tool"),
            ids::build_metadata(),
            ids::options_parsed(),
            ids::workspace_status(),
        ];
        c.push(ids::pattern(patterns));
        c.push(ids::build_finished());
        c
    }

    pub fn build_metrics(metrics: Msg) -> BuildEventNode {
        BuildEventNode::new(ids::build_metrics(), vec![], "build_metrics", metrics)
            .after(ids::build_finished())
    }

    pub fn unstructured_command_line(args: &[String]) -> BuildEventNode {
        let mut b = Msg::new("build_event_stream.UnstructuredCommandLine");
        for a in args {
            b.set_str("args", a.clone());
        }
        BuildEventNode::new(
            ids::unstructured_command_line(),
            vec![],
            "unstructured_command_line",
            b,
        )
    }

    /// `label`: "original", "canonical" or "tool"; `sections` are `(label, kind, values)` where
    /// kind is `chunks` or `options` as `(name, value)` pairs.
    pub fn structured_command_line(
        label: &str,
        executable: &str,
        command_name: &str,
        residue: &[String],
        startup_options: &[(String, String)],
        command_options: &[(String, String)],
    ) -> BuildEventNode {
        let mut cl = Msg::new("command_line.CommandLine");
        cl.set_str("command_line_label", label);
        let section = |cl: &mut Msg,
                       label: &str,
                       chunks: Option<Vec<String>>,
                       opts: Option<&[(String, String)]>| {
            let mut sec = cl.new_child("sections");
            sec.set_str("section_label", label);
            if let Some(chunks) = chunks {
                let mut cl2 = sec.new_child("chunk_list");
                for c in chunks {
                    cl2.set_str("chunk", c);
                }
                sec.set_msg("chunk_list", cl2);
            }
            if let Some(opts) = opts {
                let mut ol = sec.new_child("option_list");
                for (n, v) in opts {
                    let mut o = ol.new_child("option");
                    o.set_str(
                        "combined_form",
                        if v.is_empty() {
                            format!("--{n}")
                        } else {
                            format!("--{n}={v}")
                        },
                    );
                    o.set_str("option_name", n.clone());
                    o.set_str("option_value", v.clone());
                    ol.set_msg("option", o);
                }
                sec.set_msg("option_list", ol);
            }
            cl.set_msg("sections", sec);
        };
        section(
            &mut cl,
            "executable",
            Some(vec![executable.to_string()]),
            None,
        );
        section(&mut cl, "startup options", None, Some(startup_options));
        section(
            &mut cl,
            "command",
            Some(vec![command_name.to_string()]),
            None,
        );
        section(&mut cl, "command options", None, Some(command_options));
        section(&mut cl, "residual", Some(residue.to_vec()), None);
        BuildEventNode {
            id: ids::structured_command_line(label),
            children: vec![],
            payload: {
                let mut p = Msg::new("build_event_stream.BuildEvent");
                p.set_msg("structured_command_line", cl);
                p
            },
            posted_after: vec![],
            completes_build: false,
            replaceable: false,
        }
    }

    pub fn options_parsed(
        startup_options: &[String],
        explicit_startup_options: &[String],
        cmd_line: &[String],
        explicit_cmd_line: &[String],
        tool_tag: &str,
    ) -> BuildEventNode {
        let mut b = Msg::new("build_event_stream.OptionsParsed");
        for (f, v) in [
            ("startup_options", startup_options),
            ("explicit_startup_options", explicit_startup_options),
            ("cmd_line", cmd_line),
            ("explicit_cmd_line", explicit_cmd_line),
        ] {
            for s in v {
                b.set_str(f, s.clone());
            }
        }
        b.set_str("tool_tag", tool_tag);
        BuildEventNode::new(ids::options_parsed(), vec![], "options_parsed", b)
    }

    pub fn workspace_status(items: &[(String, String)]) -> BuildEventNode {
        let mut b = Msg::new("build_event_stream.WorkspaceStatus");
        for (k, v) in items {
            let mut i = b.new_child("item");
            i.set_str("key", k.clone());
            i.set_str("value", v.clone());
            b.set_msg("item", i);
        }
        BuildEventNode::new(ids::workspace_status(), vec![], "workspace_status", b)
    }

    pub fn build_metadata(md: &[(String, String)]) -> BuildEventNode {
        let mut b = Msg::new("build_event_stream.BuildMetadata");
        let mut dedup = std::collections::BTreeMap::new();
        for (k, v) in md {
            dedup.insert(k.clone(), v.clone());
        }
        for (k, v) in dedup {
            b.put_map("metadata", Value::Str(k), Value::Str(v));
        }
        BuildEventNode::new(ids::build_metadata(), vec![], "build_metadata", b)
            .after(ids::started())
    }

    pub fn workspace_config(local_exec_root: &str) -> BuildEventNode {
        let mut b = Msg::new("build_event_stream.WorkspaceConfig");
        b.set_str("local_exec_root", local_exec_root);
        BuildEventNode::new(ids::workspace(), vec![], "workspace_info", b)
    }

    /// `PatternExpanded` posted after `TargetParsingCompleteEvent`: children are the
    /// configured/unconfigured target ids.
    pub fn pattern_expanded(patterns: &[String], target_labels: &[String]) -> BuildEventNode {
        let b = Msg::new("build_event_stream.PatternExpanded");
        let children = target_labels
            .iter()
            .map(|l| ids::unconfigured_label(l))
            .collect();
        BuildEventNode::new(ids::pattern(patterns), children, "expanded", b)
    }

    pub fn null_configuration() -> BuildEventNode {
        let c = Msg::new("build_event_stream.Configuration");
        BuildEventNode::new(ids::null_configuration(), vec![], "configuration", c)
    }

    pub fn configuration(
        id: &str,
        mnemonic: &str,
        platform: &str,
        cpu: &str,
        make_vars: &[(String, String)],
        is_tool: bool,
    ) -> BuildEventNode {
        let mut c = Msg::new("build_event_stream.Configuration");
        c.set_str("mnemonic", mnemonic);
        c.set_str("platform_name", platform);
        c.set_str("cpu", cpu);
        for (k, v) in make_vars {
            c.put_map(
                "make_variable",
                Value::Str(k.clone()),
                Value::Str(v.clone()),
            );
        }
        c.set_bool("is_tool", is_tool);
        BuildEventNode::new(ids::configuration(id), vec![], "configuration", c)
    }

    /// An unconfigured label event that announces the configured target (children).
    pub fn unconfigured_label(label: &str, config_id: &str) -> BuildEventNode {
        let b = Msg::new("build_event_stream.Aborted");
        let _ = b;
        // `UnconfiguredLabelId` is an announcement-only id: Bazel posts it as an aborted/skipped
        // event when analysis never produced a configured target; otherwise it is chained.
        let mut a = Msg::new("build_event_stream.Aborted");
        a.set_enum("reason", "SKIPPED");
        let _ = config_id;
        BuildEventNode::new(ids::unconfigured_label(label), vec![], "aborted", a)
    }

    pub fn target_configured(
        label: &str,
        config_id: &str,
        target_kind: &str,
        tags: &[String],
        test_size: Option<&str>,
        completed_child: bool,
    ) -> BuildEventNode {
        let mut c = Msg::new("build_event_stream.TargetConfigured");
        c.set_str("target_kind", target_kind);
        if let Some(sz) = test_size {
            c.set_enum("test_size", sz);
        }
        for t in tags {
            c.set_str("tag", t.clone());
        }
        let mut children = Vec::new();
        if completed_child {
            children.push(ids::target_completed(label, config_id));
        }
        BuildEventNode::new(ids::target_configured(label), children, "configured", c)
    }

    pub fn aspect_configured(
        label: &str,
        aspect: &str,
        config_id: &str,
        completed_child: bool,
    ) -> BuildEventNode {
        let c = Msg::new("build_event_stream.TargetConfigured");
        let mut children = Vec::new();
        if completed_child {
            children.push(ids::aspect_completed(label, config_id, aspect));
        }
        BuildEventNode::new(
            ids::aspect_configured(label, aspect),
            children,
            "configured",
            c,
        )
    }

    /// `ActionExecuted`; with `publish_all_actions` false only failed actions are published.
    #[allow(clippy::too_many_arguments)]
    pub fn action_executed(
        primary_output: &str,
        label: &str,
        config_id: &str,
        mnemonic: &str,
        success: bool,
        exit_code: i32,
        stdout: Option<&[u8]>,
        stderr: Option<&[u8]>,
        command_line: &[String],
        start_millis: i64,
        end_millis: i64,
        failure_detail: Option<&zel_proto::failure_details::FailureDetail>,
    ) -> BuildEventNode {
        Self::action_executed_with_primary_output_path(
            primary_output,
            None,
            label,
            config_id,
            mnemonic,
            success,
            exit_code,
            stdout,
            stderr,
            command_line,
            start_millis,
            end_millis,
            failure_detail,
        )
    }

    /// `ActionExecuted` with optional local `primary_output_path` populated on `primary_output`
    /// when `success` is `true` (`ActionExecutedEvent.asStreamProto`).
    #[allow(clippy::too_many_arguments)]
    pub fn action_executed_with_primary_output_path(
        primary_output: &str,
        primary_output_path: Option<&str>,
        label: &str,
        config_id: &str,
        mnemonic: &str,
        success: bool,
        exit_code: i32,
        stdout: Option<&[u8]>,
        stderr: Option<&[u8]>,
        command_line: &[String],
        start_millis: i64,
        end_millis: i64,
        failure_detail: Option<&zel_proto::failure_details::FailureDetail>,
    ) -> BuildEventNode {
        let mut a = Msg::new("build_event_stream.ActionExecuted");
        a.set_bool("success", success);
        a.set_str("type", mnemonic);
        a.set_int("exit_code", exit_code as i64);
        for (f, bytes) in [("stdout", stdout), ("stderr", stderr)] {
            if let Some(b) = bytes.filter(|b| !b.is_empty()) {
                let mut file = a.new_child(f);
                file.set_str("name", f);
                file.set_bytes("contents", b.to_vec());
                a.set_msg(f, file);
            }
        }
        if success {
            if let Some(p) = primary_output_path.filter(|p| !p.is_empty()) {
                let mut po = a.new_child("primary_output");
                po.set_str("uri", file_uri(p));
                a.set_msg("primary_output", po);
            }
        }
        for c in command_line {
            a.set_str("command_line", c.clone());
        }
        let ts = |m: i64, a: &Msg, field: &str| {
            let mut t = a.new_child(field);
            t.set_int("seconds", m.div_euclid(1000));
            t.set_int("nanos", m.rem_euclid(1000) * 1_000_000);
            t
        };
        let st = ts(start_millis, &a, "start_time");
        let en = ts(end_millis, &a, "end_time");
        a.set_msg("start_time", st);
        a.set_msg("end_time", en);
        if let Some(fd) = failure_detail {
            a.set_msg("failure_detail", fd.to_msg());
        }
        BuildEventNode::new(
            ids::action_completed(primary_output, label, config_id),
            vec![],
            "action",
            a,
        )
    }

    pub fn file(
        name: &str,
        path: &str,
        prefix: &[String],
        length: Option<i64>,
        digest: Option<&str>,
    ) -> Msg {
        let mut f = Msg::new("build_event_stream.File");
        for p in prefix {
            f.set_str("path_prefix", p.clone());
        }
        f.set_str("name", name);
        f.set_str("uri", file_uri(path));
        if let Some(d) = digest {
            f.set_str("digest", d);
        }
        if let Some(l) = length {
            f.set_int("length", l);
        }
        f
    }

    /// Creates a `File` entry for a directory/tree artifact or unresolved symlink (no `uri`).
    pub fn file_without_uri(name: &str, prefix: &[String]) -> Msg {
        let mut f = Msg::new("build_event_stream.File");
        for p in prefix {
            f.set_str("path_prefix", p.clone());
        }
        f.set_str("name", name);
        f
    }

    pub fn symlink_file(name: &str, prefix: &[String], symlink_target_path: &str) -> Msg {
        let mut f = Msg::new("build_event_stream.File");
        for p in prefix {
            f.set_str("path_prefix", p.clone());
        }
        f.set_str("name", name);
        f.set_str("symlink_target_path", symlink_target_path);
        f
    }

    pub fn directory_file(name: &str, prefix: &[String]) -> Msg {
        let mut f = Msg::new("build_event_stream.File");
        for p in prefix {
            f.set_str("path_prefix", p.clone());
        }
        f.set_str("name", name);
        f
    }

    /// Splits a flattened file list into `NamedSetOfFiles` events respecting
    /// `--build_event_max_named_set_of_file_entries` (`max_entries`, only splits when `>= 2`).
    /// Recursively folds chunks so no node's total degree (`files + file_sets`) exceeds `max_entries`.
    pub fn named_sets(
        group_key: &str,
        files: Vec<Msg>,
        max_entries: Option<usize>,
    ) -> (Msg, Vec<BuildEventNode>) {
        enum Entry {
            File(Msg),
            Set(String),
        }

        let Some(limit) = max_entries.filter(|m| *m >= 2) else {
            let mut ns = Msg::new("build_event_stream.NamedSetOfFiles");
            for f in files {
                ns.set_msg("files", f);
            }
            let node =
                BuildEventNode::new(ids::named_set(group_key), vec![], "named_set_of_files", ns);
            return (ids::named_set(group_key), vec![node]);
        };

        let mut entries: Vec<Entry> = files.into_iter().map(Entry::File).collect();
        let mut nodes = Vec::new();
        let mut sub_idx = 0usize;

        while entries.len() > limit {
            let mut next_entries = Vec::with_capacity(entries.len().div_ceil(limit));
            let mut iter = entries.into_iter();
            loop {
                let mut chunk = Vec::with_capacity(limit);
                for _ in 0..limit {
                    if let Some(e) = iter.next() {
                        chunk.push(e);
                    } else {
                        break;
                    }
                }
                if chunk.is_empty() {
                    break;
                }
                if chunk.len() == 1 {
                    next_entries.push(chunk.pop().unwrap());
                } else {
                    let sub_id = format!("{group_key}-{sub_idx}");
                    sub_idx += 1;
                    let mut ns = Msg::new("build_event_stream.NamedSetOfFiles");
                    let mut child_ids = Vec::new();
                    for item in chunk {
                        match item {
                            Entry::File(f) => {
                                ns.set_msg("files", f);
                            }
                            Entry::Set(cid) => {
                                ns.set_msg("file_sets", ids::named_set_id(&cid));
                                child_ids.push(ids::named_set(&cid));
                            }
                        }
                    }
                    nodes.push(BuildEventNode::new(
                        ids::named_set(&sub_id),
                        child_ids,
                        "named_set_of_files",
                        ns,
                    ));
                    next_entries.push(Entry::Set(sub_id));
                }
            }
            entries = next_entries;
        }

        let mut root_ns = Msg::new("build_event_stream.NamedSetOfFiles");
        let mut root_children = Vec::new();
        for item in entries {
            match item {
                Entry::File(f) => {
                    root_ns.set_msg("files", f);
                }
                Entry::Set(cid) => {
                    root_ns.set_msg("file_sets", ids::named_set_id(&cid));
                    root_children.push(ids::named_set(&cid));
                }
            }
        }
        nodes.push(BuildEventNode::new(
            ids::named_set(group_key),
            root_children,
            "named_set_of_files",
            root_ns,
        ));
        (ids::named_set(group_key), nodes)
    }

    pub fn target_completed(
        label: &str,
        config_id: &str,
        success: bool,
        output_groups: &[(String, Vec<String>, bool)],
        tags: &[String],
        failure_detail: Option<&zel_proto::failure_details::FailureDetail>,
    ) -> BuildEventNode {
        let groups: Vec<BepOutputGroup> = output_groups
            .iter()
            .map(|(name, sets, incomplete)| BepOutputGroup {
                name: name.clone(),
                file_sets: sets.clone(),
                inline_files: Vec::new(),
                incomplete: *incomplete,
            })
            .collect();
        Self::target_completed_with_groups(
            label,
            config_id,
            None,
            success,
            &groups,
            &[],
            tags,
            failure_detail,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn target_completed_with_groups(
        label: &str,
        config_id: &str,
        aspect: Option<&str>,
        success: bool,
        output_groups: &[BepOutputGroup],
        directory_outputs: &[Msg],
        tags: &[String],
        failure_detail: Option<&zel_proto::failure_details::FailureDetail>,
    ) -> BuildEventNode {
        let mut c = Msg::new("build_event_stream.TargetComplete");
        c.set_bool("success", success);
        for grp in output_groups {
            let mut g = c.new_child("output_group");
            g.set_str("name", grp.name.clone());
            for s in &grp.file_sets {
                g.set_msg("file_sets", ids::named_set_id(s));
            }
            for f in &grp.inline_files {
                g.set_msg("inline_files", f.clone());
            }
            g.set_bool("incomplete", grp.incomplete);
            c.set_msg("output_group", g);
        }
        for d in directory_outputs {
            c.set_msg("directory_output", d.clone());
        }
        for t in tags {
            c.set_str("tag", t.clone());
        }
        if let Some(fd) = failure_detail {
            c.set_msg("failure_detail", fd.to_msg());
        }
        let event_id = match aspect.filter(|a| !a.is_empty()) {
            Some(asp) => ids::aspect_completed(label, config_id, asp),
            None => ids::target_completed(label, config_id),
        };
        BuildEventNode::new(event_id, vec![], "completed", c)
    }

    pub fn build_finished(
        exit_name: &str,
        exit_code: i32,
        finish_millis: i64,
        failure_detail: Option<&zel_proto::failure_details::FailureDetail>,
    ) -> BuildEventNode {
        Self::build_finished_with_children(
            exit_name,
            exit_code,
            finish_millis,
            failure_detail,
            vec![ids::build_tool_logs(), ids::build_metrics()],
        )
    }

    pub fn build_finished_with_children(
        exit_name: &str,
        exit_code: i32,
        finish_millis: i64,
        failure_detail: Option<&zel_proto::failure_details::FailureDetail>,
        children: Vec<Msg>,
    ) -> BuildEventNode {
        let mut b = Msg::new("build_event_stream.BuildFinished");
        b.set_bool("overall_success", exit_code == 0);
        let mut ec = b.new_child("exit_code");
        ec.set_str("name", exit_name);
        ec.set_int("code", exit_code as i64);
        b.set_msg("exit_code", ec);
        if finish_millis != 0 {
            b.set_int("finish_time_millis", finish_millis);
        }
        let mut ts = b.new_child("finish_time");
        ts.set_int("seconds", finish_millis.div_euclid(1000));
        ts.set_int("nanos", finish_millis.rem_euclid(1000) * 1_000_000);
        b.set_msg("finish_time", ts);
        if let Some(fd) = failure_detail {
            b.set_msg("failure_detail", fd.to_msg());
        }
        BuildEventNode::new(ids::build_finished(), children, "finished", b).completing()
    }

    pub fn build_tool_logs(logs: &[(String, String)]) -> BuildEventNode {
        let mut b = Msg::new("build_event_stream.BuildToolLogs");
        for (name, path) in logs {
            b.set_msg("log", BepBuilder::file(name, path, &[], None, None));
        }
        BuildEventNode::new(ids::build_tool_logs(), vec![], "build_tool_logs", b)
            .after(ids::build_finished())
    }

    pub fn build_tool_logs_entries(entries: &[BepLogFile]) -> BuildEventNode {
        let mut b = Msg::new("build_event_stream.BuildToolLogs");
        for entry in entries {
            let mut f = Msg::new("build_event_stream.File");
            match entry {
                BepLogFile::LocalPath { name, path } => {
                    f.set_str("name", name.clone());
                    f.set_str("uri", file_uri(path));
                }
                BepLogFile::Uri { name, uri } => {
                    f.set_str("name", name.clone());
                    if !uri.is_empty() {
                        f.set_str("uri", uri.clone());
                    }
                }
                BepLogFile::Contents { name, contents } => {
                    f.set_str("name", name.clone());
                    f.set_bytes("contents", contents.clone());
                }
            }
            b.set_msg("log", f);
        }
        BuildEventNode::new(ids::build_tool_logs(), vec![], "build_tool_logs", b)
            .after(ids::build_finished())
    }

    pub fn convenience_symlinks(links: &[(String, String, bool)]) -> BuildEventNode {
        let mut b = Msg::new("build_event_stream.ConvenienceSymlinksIdentified");
        for (path, target, create) in links {
            let mut s = b.new_child("convenience_symlinks");
            s.set_str("path", path.clone());
            s.set_enum("action", if *create { "CREATE" } else { "DELETE" });
            if *create {
                s.set_str("target", target.clone());
            }
            b.set_msg("convenience_symlinks", s);
        }
        BuildEventNode::new(
            ids::convenience_symlinks_identified(),
            vec![],
            "convenience_symlinks_identified",
            b,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn test_result(
        label: &str,
        config_id: &str,
        run: i32,
        shard: i32,
        attempt: i32,
        status: &str,
        cached_locally: bool,
        start_millis: i64,
        duration_millis: i64,
        outputs: &[(String, String)],
        strategy: &str,
        exit_code: i32,
    ) -> BuildEventNode {
        let mut r = Msg::new("build_event_stream.TestResult");
        r.set_enum("status", status);
        r.set_bool("cached_locally", cached_locally);
        let mut st = r.new_child("test_attempt_start");
        st.set_int("seconds", start_millis.div_euclid(1000));
        st.set_int("nanos", start_millis.rem_euclid(1000) * 1_000_000);
        r.set_msg("test_attempt_start", st);
        let mut d = r.new_child("test_attempt_duration");
        d.set_int("seconds", duration_millis.div_euclid(1000));
        d.set_int("nanos", duration_millis.rem_euclid(1000) * 1_000_000);
        r.set_msg("test_attempt_duration", d);
        for (name, path) in outputs {
            r.set_msg(
                "test_action_output",
                BepBuilder::file(name, path, &[], None, None),
            );
        }
        let mut ei = r.new_child("execution_info");
        ei.set_str("strategy", strategy);
        ei.set_int("exit_code", exit_code as i64);
        r.set_msg("execution_info", ei);
        BuildEventNode::new(
            ids::test_result(label, config_id, run, shard, attempt),
            vec![],
            "test_result",
            r,
        )
        .after(ids::target_completed(label, config_id))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn test_summary(
        label: &str,
        config_id: &str,
        overall: &str,
        total_run_count: i32,
        shard_count: i32,
        attempt_count: i32,
        passed: &[(String, String)],
        failed: &[(String, String)],
        total_cached: i32,
        first_start_millis: i64,
        last_stop_millis: i64,
        total_run_millis: i64,
        result_ids: Vec<Msg>,
    ) -> BuildEventNode {
        let mut s = Msg::new("build_event_stream.TestSummary");
        s.set_enum("overall_status", overall);
        s.set_int("total_run_count", total_run_count as i64);
        // `runCount` counts runs (`--runs_per_test`), not run×shard units; shards are reported
        // separately, so a sharded single run has run_count 1.
        s.set_int(
            "run_count",
            if shard_count > 1 {
                (total_run_count / shard_count).max(1) as i64
            } else {
                total_run_count as i64
            },
        );
        s.set_int("attempt_count", attempt_count as i64);
        s.set_int("shard_count", shard_count as i64);
        for (n, p) in passed {
            s.set_msg("passed", BepBuilder::file(n, p, &[], None, None));
        }
        for (n, p) in failed {
            s.set_msg("failed", BepBuilder::file(n, p, &[], None, None));
        }
        s.set_int("total_num_cached", total_cached as i64);
        let ts = |f: &str, ms: i64| {
            let mut t = s.new_child(f);
            t.set_int("seconds", ms.div_euclid(1000));
            t.set_int("nanos", ms.rem_euclid(1000) * 1_000_000);
            t
        };
        let a = ts("first_start_time", first_start_millis);
        let b = ts("last_stop_time", last_stop_millis);
        let mut c = s.new_child("total_run_duration");
        c.set_int("seconds", total_run_millis.div_euclid(1000));
        c.set_int("nanos", total_run_millis.rem_euclid(1000) * 1_000_000);
        s.set_msg("first_start_time", a);
        s.set_msg("last_stop_time", b);
        s.set_msg("total_run_duration", c);
        let mut n = BuildEventNode::new(
            ids::test_summary(label, config_id),
            vec![],
            "test_summary",
            s,
        );
        for r in result_ids {
            n = n.after(r);
        }
        n
    }

    pub fn fetch(url: &str, success: bool) -> BuildEventNode {
        let mut f = Msg::new("build_event_stream.Fetch");
        f.set_bool("success", success);
        BuildEventNode::new(ids::fetch(url, "HTTP"), vec![], "fetch", f)
    }

    pub fn exec_request(argv: &[String], cwd: &str, should_exec: bool) -> BuildEventNode {
        let mut e = Msg::new("build_event_stream.ExecRequestConstructed");
        e.set_bytes("working_directory", cwd.as_bytes().to_vec());
        for a in argv {
            e.set_bytes("argv", a.as_bytes().to_vec());
        }
        e.set_bool("should_exec", should_exec);
        BuildEventNode::new(ids::exec_request(), vec![], "exec_request", e)
    }
}

/// `PathConverter.FileUriPathConverter.pathToUriString`.
pub fn file_uri(path: &str) -> String {
    if path.contains("://") {
        return path.to_string();
    }
    let normalized;
    let path = if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        normalized = format!("//{}", unc.replace('\\', "/"));
        normalized.as_str()
    } else if let Some(verbatim) = path.strip_prefix(r"\\?\") {
        normalized = verbatim.replace('\\', "/");
        normalized.as_str()
    } else if cfg!(windows) || has_windows_drive_prefix(path) {
        normalized = path.replace('\\', "/");
        normalized.as_str()
    } else {
        path
    };
    let p = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    let mut out = String::from("file://");
    for b in p.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'/'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')'
            | b':'
            | b'@'
            | b'&'
            | b'='
            | b'+'
            | b'$'
            | b','
            | b';' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Representation mode for `OutputGroup` in BEP (`OutputGroupMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputGroupMode {
    #[default]
    NamedSetOfFilesOnly,
    InlineOnly,
    Both,
}

impl OutputGroupMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "named_set_of_files_only" => Some(Self::NamedSetOfFilesOnly),
            "inline_only" => Some(Self::InlineOnly),
            "both" => Some(Self::Both),
            _ => None,
        }
    }
}

/// Upload synchronization mode for BEP file/BES transports (`BuildEventStreamOptions.BesUploadMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BepUploadMode {
    #[default]
    WaitForUploadComplete,
    NowaitForUploadComplete,
    FullyAsync,
}

impl BepUploadMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "wait_for_upload_complete" => Some(Self::WaitForUploadComplete),
            "nowait_for_upload_complete" => Some(Self::NowaitForUploadComplete),
            "fully_async" => Some(Self::FullyAsync),
            _ => None,
        }
    }
}

/// Structured output group entry for `TargetComplete` / `AspectComplete`.
#[derive(Debug, Clone, Default)]
pub struct BepOutputGroup {
    pub name: String,
    pub file_sets: Vec<String>,
    pub inline_files: Vec<Msg>,
    pub incomplete: bool,
}

/// Log entry for `BuildToolLogs`.
#[derive(Debug, Clone)]
pub enum BepLogFile {
    LocalPath { name: String, path: String },
    Uri { name: String, uri: String },
    Contents { name: String, contents: Vec<u8> },
}

/// Deduplicates `NamedSetOfFiles` across output groups and targets (`ArtifactGroupNamer`).
#[derive(Debug, Default)]
pub struct ArtifactGroupNamer {
    next_id: u64,
    by_fingerprint: std::collections::HashMap<Vec<u8>, String>,
}

impl ArtifactGroupNamer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `(named_set_id, newly_created_nodes)`. Repeated identical file sets reuse their id
    /// and emit no duplicate `NamedSetOfFiles` events.
    pub fn maybe_deposit(
        &mut self,
        files: Vec<Msg>,
        max_entries: Option<usize>,
    ) -> (String, Vec<BuildEventNode>) {
        let mut fp = Vec::new();
        for f in &files {
            let enc = f.encode_to_vec();
            zel_proto::wire::encode_varint(enc.len() as u64, &mut fp);
            fp.extend_from_slice(&enc);
        }
        if let Some(existing) = self.by_fingerprint.get(&fp) {
            return (existing.clone(), Vec::new());
        }
        let id = self.next_id.to_string();
        self.next_id += 1;
        let (_, nodes) = BepBuilder::named_sets(&id, files, max_entries);
        self.by_fingerprint.insert(fp, id.clone());
        (id, nodes)
    }
}

/// Parsed `--build_event_*` / `--bes_*` options.
#[derive(Debug, Clone, Default)]
pub struct BepOptions {
    pub json_file: Option<String>,
    pub binary_file: Option<String>,
    pub text_file: Option<String>,
    pub json_path_conversion: bool,
    pub binary_path_conversion: bool,
    pub text_path_conversion: bool,
    pub json_upload_mode: BepUploadMode,
    pub binary_upload_mode: BepUploadMode,
    pub text_upload_mode: BepUploadMode,
    pub bes_upload_mode: BepUploadMode,
    pub output_group_modes: Vec<(String, OutputGroupMode)>,
    pub publish_all_actions: bool,
    /// `--build_event_max_named_set_of_file_entries`; `None` when negative (unlimited).
    pub max_named_set_entries: Option<usize>,
    pub bes_backend: Option<String>,
    pub bes_results_url: Option<String>,
    pub bes_timeout_secs: Option<u64>,
    pub bes_headers: Vec<(String, String)>,
    pub bes_instance_name: String,
    pub bes_keywords: Vec<String>,
    pub bes_lifecycle_events: bool,
    pub project_id: Option<String>,
    /// Remote ByteStream URI prefix (`<authority>[/<instance>]`) used when
    /// `--build_event_*_file_path_conversion` is enabled (`ByteStreamBuildEventArtifactUploader`).
    pub remote_bytestream_uri_prefix: Option<String>,
}

fn parse_bazel_bool(raw: Option<&str>, default: bool) -> bool {
    let Some(s) = raw else {
        return default;
    };
    match s.trim().to_ascii_lowercase().as_str() {
        "0" | "false" | "f" | "no" | "n" => false,
        "1" | "true" | "t" | "yes" | "y" => true,
        _ => default,
    }
}

fn extract_remote_authority(endpoint: &str) -> Option<&str> {
    let s = endpoint.trim();
    if s.is_empty() || s.starts_with("unix:") {
        return None;
    }
    let after_scheme = match s.split_once("://") {
        Some((_, rest)) => rest,
        None => s,
    };
    let authority = after_scheme.split('/').next().unwrap_or("").trim();
    (!authority.is_empty()).then_some(authority)
}

fn compute_remote_bytestream_prefix(get: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let respect_no_cache = parse_bazel_bool(
        get("incompatible_remote_build_event_upload_respect_no_cache").as_deref(),
        false,
    );
    let upload_local = parse_bazel_bool(get("remote_upload_local_results").as_deref(), true);
    if respect_no_cache && !upload_local {
        return None;
    }
    if let Some(explicit) = get("remote_bytestream_uri_prefix")
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
    {
        return Some(explicit);
    }
    let endpoint = get("remote_cache")
        .filter(|s| !s.trim().is_empty())
        .or_else(|| get("remote_executor").filter(|s| !s.trim().is_empty()))?;
    let authority = extract_remote_authority(&endpoint)?;
    let instance = get("remote_instance_name")
        .unwrap_or_default()
        .trim()
        .trim_matches('/')
        .to_string();
    if instance.is_empty() {
        Some(authority.to_string())
    } else {
        Some(format!("{authority}/{instance}"))
    }
}

impl BepOptions {
    /// Builds from a lookup of the last value for each flag (`name` without dashes).
    pub fn from_flags(
        get: &dyn Fn(&str) -> Option<String>,
        get_all: &dyn Fn(&str) -> Vec<String>,
    ) -> Self {
        let b = |n: &str, d: bool| parse_bazel_bool(get(n).as_deref(), d);
        let nonempty = |s: Option<String>| s.filter(|s| !s.is_empty());
        let max = get("build_event_max_named_set_of_file_entries")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(5000);
        let mut output_group_modes = vec![("baseline.lcov".to_string(), OutputGroupMode::Both)];
        for entry in get_all("experimental_build_event_output_group_mode") {
            if let Some((grp, mode_str)) = entry.split_once('=') {
                if let Some(mode) = OutputGroupMode::parse(mode_str) {
                    if let Some(existing) = output_group_modes
                        .iter_mut()
                        .find(|(k, _)| k.eq_ignore_ascii_case(grp))
                    {
                        existing.1 = mode;
                    } else {
                        output_group_modes.push((grp.to_string(), mode));
                    }
                }
            }
        }
        let upload_mode = |n: &str| {
            get(n)
                .and_then(|s| BepUploadMode::parse(&s))
                .unwrap_or_default()
        };
        Self {
            json_file: nonempty(get("build_event_json_file")),
            binary_file: nonempty(get("build_event_binary_file")),
            text_file: nonempty(get("build_event_text_file")),
            json_path_conversion: b("build_event_json_file_path_conversion", true),
            binary_path_conversion: b("build_event_binary_file_path_conversion", true),
            text_path_conversion: b("build_event_text_file_path_conversion", true),
            json_upload_mode: upload_mode("build_event_json_file_upload_mode"),
            binary_upload_mode: upload_mode("build_event_binary_file_upload_mode"),
            text_upload_mode: upload_mode("build_event_text_file_upload_mode"),
            bes_upload_mode: upload_mode("bes_upload_mode"),
            output_group_modes,
            publish_all_actions: b("build_event_publish_all_actions", false),
            max_named_set_entries: if max < 0 { None } else { Some(max as usize) },
            bes_backend: nonempty(get("bes_backend")),
            bes_results_url: nonempty(get("bes_results_url")),
            bes_timeout_secs: get("bes_timeout").and_then(|s| parse_duration_secs(&s)),
            bes_headers: get_all("bes_header")
                .into_iter()
                .filter_map(|h| {
                    h.split_once('=')
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                })
                .collect(),
            bes_instance_name: get("bes_instance_name").unwrap_or_default(),
            bes_keywords: get_all("bes_keywords"),
            bes_lifecycle_events: b("bes_lifecycle_events", true),
            project_id: nonempty(get("project_id")),
            remote_bytestream_uri_prefix: compute_remote_bytestream_prefix(get),
        }
    }

    pub fn output_group_mode(&self, group: &str) -> OutputGroupMode {
        self.output_group_modes
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(group))
            .map(|(_, m)| *m)
            .unwrap_or(OutputGroupMode::NamedSetOfFilesOnly)
    }

    pub fn any_enabled(&self) -> bool {
        self.json_file.is_some()
            || self.binary_file.is_some()
            || self.text_file.is_some()
            || self.bes_backend.is_some()
    }

    /// Opens the file transports (real filesystem, streaming).
    pub fn open_file_transports(&self) -> Vec<Result<Arc<dyn BuildEventTransport>, String>> {
        let mut v: Vec<Result<Arc<dyn BuildEventTransport>, String>> = Vec::new();
        for (fmt, path, conv) in [
            (BepFormat::Json, &self.json_file, self.json_path_conversion),
            (
                BepFormat::Binary,
                &self.binary_file,
                self.binary_path_conversion,
            ),
            (BepFormat::Text, &self.text_file, self.text_path_conversion),
        ] {
            if let Some(p) = path {
                v.push(
                    FileTransport::open_with_options(
                        fmt,
                        p,
                        conv,
                        self.remote_bytestream_uri_prefix.clone(),
                    )
                    .map(|t| Arc::new(t) as Arc<dyn BuildEventTransport>)
                    .map_err(|e| format!("Cannot open {p}: {e}")),
                );
            }
        }
        v
    }
}

/// Parses Bazel duration syntax (`10s`, `5m`, `1h`, `2d`, `500ms`) into whole seconds.
pub fn parse_duration_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mul) = if let Some(n) = s.strip_suffix("ms") {
        (n, 0.001)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1.0)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60.0)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600.0)
    } else if let Some(n) = s.strip_suffix('d') {
        (n, 86400.0)
    } else {
        (s, 1.0)
    };
    num.trim()
        .parse::<f64>()
        .ok()
        .map(|n| (n * mul).ceil() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn streamer() -> (BuildEventStreamer, Arc<MemoryTransport>) {
        let mem = Arc::new(MemoryTransport::default());
        (BuildEventStreamer::new(vec![mem.clone()], false), mem)
    }

    fn kinds(mem: &MemoryTransport) -> Vec<String> {
        mem.events
            .lock()
            .unwrap()
            .iter()
            .map(|e| {
                let id = e.get_msg("id").unwrap();
                id.which_oneof("id").unwrap().to_string()
            })
            .collect()
    }

    #[test]
    fn stream_dag_progress_and_last_message() {
        let (s, mem) = streamer();
        s.post(BepBuilder::build_started(
            "u",
            1_700_000_000_123,
            "release 9",
            "",
            "build",
            "/w",
            "/w",
            7,
            "h",
            "u",
            vec![ids::progress(0), ids::build_finished()],
        ));
        s.report_stderr("INFO: hello\n");
        s.post(BepBuilder::build_finished(
            "SUCCESS",
            0,
            1_700_000_001_000,
            None,
        ));
        s.post(BepBuilder::build_metrics(Msg::new(
            "build_event_stream.BuildMetrics",
        )));
        s.post(BepBuilder::build_tool_logs(&[]));
        s.post(BepBuilder::convenience_symlinks(&[]));
        let k = kinds(&mem);
        assert_eq!(k.first().map(String::as_str), Some("started"));
        assert!(k.contains(&"progress".to_string()));
        assert!(k.contains(&"build_finished".to_string()));
        assert!(s.is_closed());
        let evs = mem.events.lock().unwrap();
        // started announces progress 0 via link event
        let started = &evs[0];
        assert!(started
            .get_msgs("children")
            .iter()
            .any(|c| c.has("build_finished")));
        let last = evs.last().unwrap();
        assert!(last.get_bool("last_message"), "last: {}", last.to_json());
        // stderr text rode on a progress event
        assert!(evs.iter().any(|e| e
            .get_msg("progress")
            .map(|p| p.get_str("stderr") == "INFO: hello\n")
            .unwrap_or(false)));
        // every announced id was posted exactly once
        let mut announced: HashSet<Vec<u8>> = HashSet::new();
        let mut posted = HashSet::new();
        for e in evs.iter() {
            let id = e.get_msg("id").unwrap();
            assert!(posted.insert(id.encode_to_vec()), "dup {}", id.to_json());
            for c in e.get_msgs("children") {
                announced.insert(c.encode_to_vec());
            }
        }
        for a in &announced {
            assert!(
                posted.contains(a),
                "announced not posted: {:?}",
                Msg::decode("build_event_stream.BuildEventId", a)
                    .unwrap()
                    .to_json()
            );
        }
    }

    #[test]
    fn unposted_announced_events_are_aborted() {
        let (s, mem) = streamer();
        s.add_abort_reason("USER_INTERRUPTED", "interrupted");
        s.post(BepBuilder::build_started(
            "u",
            0,
            "v",
            "",
            "build",
            "/w",
            "/w",
            1,
            "h",
            "u",
            vec![
                ids::progress(0),
                ids::target_configured("//a:b"),
                ids::build_finished(),
            ],
        ));
        s.post(BepBuilder::build_finished("INTERRUPTED", 8, 5, None));
        s.post(BepBuilder::build_metrics(Msg::new(
            "build_event_stream.BuildMetrics",
        )));
        s.post(BepBuilder::build_tool_logs(&[]));
        s.post(BepBuilder::convenience_symlinks(&[]));
        let evs = mem.events.lock().unwrap();
        let aborted: Vec<_> = evs.iter().filter(|e| e.has("aborted")).collect();
        assert_eq!(
            aborted.len(),
            1,
            "{:?}",
            evs.iter().map(|e| e.to_json()).collect::<Vec<_>>()
        );
        assert_eq!(
            aborted[0]
                .get_msg("aborted")
                .unwrap()
                .get_enum_name("reason"),
            "USER_INTERRUPTED"
        );
        assert!(evs.last().unwrap().get_bool("last_message"));
    }

    #[test]
    fn order_constraint_buffers_until_prerequisite() {
        let (s, mem) = streamer();
        s.post(BepBuilder::build_started(
            "u",
            0,
            "v",
            "",
            "test",
            "/w",
            "/w",
            1,
            "h",
            "u",
            vec![
                ids::progress(0),
                ids::target_completed("//a:t", "c1"),
                ids::test_result("//a:t", "c1", 0, 0, 1),
                ids::build_finished(),
            ],
        ));
        s.post(BepBuilder::test_result(
            "//a:t",
            "c1",
            0,
            0,
            1,
            "PASSED",
            false,
            0,
            10,
            &[],
            "local",
            0,
        ));
        assert!(!kinds(&mem).contains(&"test_result".to_string()));
        s.post(BepBuilder::target_completed(
            "//a:t",
            "c1",
            true,
            &[],
            &[],
            None,
        ));
        let k = kinds(&mem);
        let ti = k.iter().position(|x| x == "target_completed").unwrap();
        let ri = k.iter().position(|x| x == "test_result").unwrap();
        assert!(ti < ri);
    }

    #[test]
    fn file_formats() {
        let mut ev = Msg::new("build_event_stream.BuildEvent");
        ev.set_msg("id", ids::build_finished());
        let j = serialize_event(BepFormat::Json, &ev);
        assert_eq!(
            String::from_utf8(j).unwrap(),
            "{\"id\":{\"buildFinished\":{}}}\n"
        );
        let b = serialize_event(BepFormat::Binary, &ev);
        assert_eq!(b[0] as usize, b.len() - 1);
        let t = String::from_utf8(serialize_event(BepFormat::Text, &ev)).unwrap();
        assert_eq!(t, "event {\nid {\n  build_finished {\n  }\n}\n}\n\n");
    }

    #[test]
    fn named_sets_split_and_uri() {
        let files: Vec<Msg> = (0..5)
            .map(|i| BepBuilder::file(&format!("f{i}"), &format!("/o/f {i}"), &[], None, None))
            .collect();
        let (root, nodes) = BepBuilder::named_sets("g", files.clone(), Some(2));
        assert_eq!(nodes.len(), 4);
        for n in &nodes {
            let ns = n.payload.get_msg("named_set_of_files").unwrap();
            let deg = ns.get_msgs("files").len() + ns.get_msgs("file_sets").len();
            assert!(deg <= 2, "degree {deg} exceeded limit 2");
        }
        // max_entries < 2 does not split
        let (_, unsplit) = BepBuilder::named_sets("g1", files.clone(), Some(1));
        assert_eq!(unsplit.len(), 1);
        // ArtifactGroupNamer deduplicates identical file sets
        let mut namer = ArtifactGroupNamer::new();
        let (id1, n1) = namer.maybe_deposit(files.clone(), Some(2));
        let (id2, n2) = namer.maybe_deposit(files, Some(2));
        assert_eq!(id1, "0");
        assert_eq!(id2, "0");
        assert_eq!(n1.len(), 4);
        assert!(n2.is_empty());
        assert_eq!(root.get_msg("named_set").unwrap().get_str("id"), "g");
        assert_eq!(file_uri("/a b/c"), "file:///a%20b/c");
        assert_eq!(
            file_uri_to_local_path("file:///a%20b/c").as_deref(),
            Some("/a b/c")
        );
        assert_eq!(parse_duration_secs("2m"), Some(120));
    }

    #[test]
    fn started_event_json_shape() {
        let n = BepBuilder::build_started(
            "u",
            1500,
            "v",
            "",
            "build",
            "/w",
            "/w",
            3,
            "h",
            "me",
            vec![],
        );
        let j = n.to_proto(false).to_json();
        assert!(
            j.starts_with("{\"id\":{\"started\":{}},\"started\":{\"uuid\":\"u\""),
            "{j}"
        );
        assert!(
            j.contains("\"startTime\":\"1970-01-01T00:00:01.500Z\""),
            "{j}"
        );
        assert!(j.contains("\"serverPid\":\"3\""), "{j}");
    }

    #[test]
    fn bep_parity_replaceable_null_config_and_output_group_modes() {
        // 0-indexed test_result -> 1-indexed proto fields
        let tid = ids::test_result("//p:t", "cfg1", 0, 0, 1);
        let tr = tid.get_msg("test_result").unwrap();
        assert_eq!(tr.get_i64("run"), 1);
        assert_eq!(tr.get_i64("shard"), 1);
        assert_eq!(tr.get_i64("attempt"), 1);

        // NullConfiguration automatically precedes events referencing "none",
        // and ReplaceableBuildEvent keeps only the latest version before flush.
        let (s, mem) = streamer();
        s.post(BepBuilder::build_started(
            "id-1",
            1000,
            "9.3.2",
            "",
            "build",
            "/w",
            "/w",
            1,
            "h",
            "u",
            vec![
                ids::progress(0),
                ids::target_configured("//p:a"),
                ids::build_finished(),
            ],
        ));
        s.post(
            BepBuilder::configuration(
                "cfg1",
                "k8-fastbuild",
                "k8",
                "k8",
                &[("K".into(), "V1".into())],
                false,
            )
            .as_replaceable(),
        );
        s.post(
            BepBuilder::configuration(
                "cfg1",
                "k8-fastbuild",
                "k8",
                "k8",
                &[("K".into(), "V2".into())],
                false,
            )
            .as_replaceable(),
        );
        s.post(BepBuilder::target_configured(
            "//p:a",
            "none",
            "source file",
            &[],
            None,
            true,
        ));
        s.post(BepBuilder::target_completed(
            "//p:a",
            "none",
            true,
            &[],
            &[],
            None,
        ));
        s.post(BepBuilder::build_finished_with_children(
            "SUCCESS",
            0,
            2000,
            None,
            vec![],
        ));
        let evs = mem.events.lock().unwrap();
        let null_idx = evs
            .iter()
            .position(|e| {
                e.get_msg("id")
                    .and_then(|i| i.get_msg("configuration"))
                    .map(|c| c.get_str("id") == "none")
                    .unwrap_or(false)
            })
            .expect("NullConfiguration emitted");
        let tc_idx = evs
            .iter()
            .position(|e| {
                e.get_msg("id")
                    .and_then(|i| i.get_msg("target_configured"))
                    .is_some()
            })
            .unwrap();
        assert!(null_idx < tc_idx);
        let cfg1_evs: Vec<_> = evs
            .iter()
            .filter(|e| {
                e.get_msg("id")
                    .and_then(|i| i.get_msg("configuration"))
                    .map(|c| c.get_str("id") == "cfg1")
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(cfg1_evs.len(), 1);
        assert!(cfg1_evs[0].to_json().contains("V2"));
        drop(evs);

        // Output group mode parsing & path conversion toggle
        let flags: std::collections::HashMap<&str, &str> =
            [("build_event_json_file_path_conversion", "false")]
                .into_iter()
                .collect();
        let opts = BepOptions::from_flags(&|k| flags.get(k).map(|v| v.to_string()), &|k| {
            if k == "experimental_build_event_output_group_mode" {
                vec!["default=inline_only".into(), "default=both".into()]
            } else {
                vec![]
            }
        });
        assert!(!opts.json_path_conversion);
        assert_eq!(opts.output_group_mode("default"), OutputGroupMode::Both);
        assert_eq!(
            opts.output_group_mode("baseline.lcov"),
            OutputGroupMode::Both
        );
        assert_eq!(
            opts.output_group_mode("other"),
            OutputGroupMode::NamedSetOfFilesOnly
        );
        let mut ns = Msg::new("build_event_stream.NamedSetOfFiles");
        ns.set_msg(
            "files",
            BepBuilder::file("a.txt", "/tmp/a b.txt", &[], None, None),
        );
        let f_node = BuildEventNode::new(ids::progress(9), vec![], "named_set_of_files", ns);
        let raw_proto = f_node.to_proto(false);
        let with_conv = String::from_utf8(serialize_event_with_conversion(
            BepFormat::Json,
            &raw_proto,
            true,
        ))
        .unwrap();
        let no_conv = String::from_utf8(serialize_event_with_conversion(
            BepFormat::Json,
            &raw_proto,
            false,
        ))
        .unwrap();
        assert!(with_conv.contains("file:///tmp/a%20b.txt"));
        // `--nobuild_event_*_file_path_conversion` selects `LocalFilesArtifactUploader`, which
        // always uses the `file://` URI scheme rather than remote `bytestream://` URIs.
        assert!(no_conv.contains("file:///tmp/a%20b.txt"));
    }

    #[test]
    fn bep_file_path_conversion_across_all_file_bearing_events() {
        let remote_flags: std::collections::HashMap<&str, &str> = [
            ("remote_cache", "grpc://cas.example.com:9092"),
            ("remote_instance_name", "main"),
        ]
        .into_iter()
        .collect();
        let opts =
            BepOptions::from_flags(&|k| remote_flags.get(k).map(|v| v.to_string()), &|_| vec![]);
        assert_eq!(
            opts.remote_bytestream_uri_prefix.as_deref(),
            Some("cas.example.com:9092/main")
        );
        let prefix = opts.remote_bytestream_uri_prefix.as_deref();

        // 1. ActionExecuted: singular stdout, stderr, primary_output (plus inline contents without uri)
        let mut ae = Msg::new("build_event_stream.ActionExecuted");
        ae.set_bool("success", true);
        ae.set_str("type", "Genrule");
        ae.set_msg(
            "stdout",
            BepBuilder::file("stdout", "/tmp/act out.log", &[], Some(11), Some("aaa111")),
        );
        let mut inline_stderr = Msg::new("build_event_stream.File");
        inline_stderr.set_str("name", "stderr");
        inline_stderr.set_bytes("contents", b"warn".to_vec());
        ae.set_msg("stderr", inline_stderr);
        ae.set_msg(
            "primary_output",
            BepBuilder::file(
                "out.bin",
                "/tmp/out bin",
                &["bazel-out".into()],
                Some(42),
                Some("bbb222"),
            ),
        );
        let ae_proto = BuildEventNode::new(
            ids::action_completed("bazel-out/out.bin", "//pkg:t", "cfg"),
            vec![],
            "action",
            ae,
        )
        .to_proto(false);

        let ae_remote = String::from_utf8(serialize_event_with_options(
            BepFormat::Json,
            &ae_proto,
            true,
            prefix,
        ))
        .unwrap();
        assert!(
            ae_remote.contains("bytestream://cas.example.com:9092/main/blobs/aaa111/11"),
            "{ae_remote}"
        );
        assert!(
            ae_remote.contains("bytestream://cas.example.com:9092/main/blobs/bbb222/42"),
            "{ae_remote}"
        );

        let ae_local = String::from_utf8(serialize_event_with_options(
            BepFormat::Json,
            &ae_proto,
            false,
            prefix,
        ))
        .unwrap();
        assert!(ae_local.contains("file:///tmp/act%20out.log"), "{ae_local}");
        assert!(ae_local.contains("file:///tmp/out%20bin"), "{ae_local}");
        assert!(!ae_local.contains("bytestream://"), "{ae_local}");

        // 2. TargetComplete: inline_files, important_output, and directory_output (which must never carry uri)
        let inline_file = BepBuilder::file("f.txt", "/tmp/f.txt", &[], Some(5), Some("ccc333"));
        let mut dir_with_accidental_uri =
            BepBuilder::file_without_uri("tree_dir", &["bazel-out".into()]);
        dir_with_accidental_uri.set_str("uri", "file:///tmp/tree_dir");
        let mut tc_node = BepBuilder::target_completed_with_groups(
            "//pkg:t",
            "cfg",
            None,
            true,
            &[BepOutputGroup {
                name: "default".into(),
                file_sets: vec![],
                inline_files: vec![inline_file.clone()],
                incomplete: false,
            }],
            &[dir_with_accidental_uri],
            &[],
            None,
        );
        if let Some(mut tc_body) = tc_node.payload.get_msg("completed").cloned() {
            tc_body.set_msg("important_output", inline_file);
            tc_node.payload.clear("completed");
            tc_node.payload.set_msg("completed", tc_body);
        }
        let tc_proto = tc_node.to_proto(false);

        let tc_remote = String::from_utf8(serialize_event_with_options(
            BepFormat::Json,
            &tc_proto,
            true,
            prefix,
        ))
        .unwrap();
        assert!(
            tc_remote.contains("bytestream://cas.example.com:9092/main/blobs/ccc333/5"),
            "{tc_remote}"
        );
        assert!(!tc_remote.contains("tree_dir\",\"uri\""), "{tc_remote}");
        assert!(!tc_remote.contains("file:///tmp/tree_dir"), "{tc_remote}");

        let tc_local = String::from_utf8(serialize_event_with_options(
            BepFormat::Json,
            &tc_proto,
            false,
            prefix,
        ))
        .unwrap();
        assert!(tc_local.contains("file:///tmp/f.txt"), "{tc_local}");
        assert!(!tc_local.contains("bytestream://"), "{tc_local}");
        assert!(!tc_local.contains("file:///tmp/tree_dir"), "{tc_local}");

        // 3. NamedSetOfFiles, BuildToolLogs, TestResult, and TestSummary
        let log_file = BepBuilder::file("test.log", "/tmp/test.log", &[], Some(9), Some("ddd444"));
        let mut tr = Msg::new("build_event_stream.TestResult");
        tr.set_enum("status", "PASSED");
        tr.set_msg("test_action_output", log_file.clone());
        let tr_proto = BuildEventNode::new(
            ids::test_result("//pkg:test", "cfg", 0, 0, 1),
            vec![],
            "test_result",
            tr,
        )
        .to_proto(false);
        let mut ts = Msg::new("build_event_stream.TestSummary");
        ts.set_enum("overall_status", "PASSED");
        ts.set_msg("passed", log_file.clone());
        let ts_proto = BuildEventNode::new(
            ids::test_summary("//pkg:test", "cfg"),
            vec![],
            "test_summary",
            ts,
        )
        .to_proto(false);
        let mut ns = Msg::new("build_event_stream.NamedSetOfFiles");
        ns.set_msg("files", log_file.clone());
        let ns_proto = BuildEventNode::new(ids::named_set("0"), vec![], "named_set_of_files", ns)
            .to_proto(false);
        let mut btl = Msg::new("build_event_stream.BuildToolLogs");
        btl.set_msg("log", log_file);
        let btl_proto = BuildEventNode::new(ids::build_tool_logs(), vec![], "build_tool_logs", btl)
            .to_proto(true);

        for proto in [&ns_proto, &tr_proto, &ts_proto, &btl_proto] {
            let as_remote = String::from_utf8(serialize_event_with_options(
                BepFormat::Json,
                proto,
                true,
                prefix,
            ))
            .unwrap();
            assert!(
                as_remote.contains("bytestream://cas.example.com:9092/main/blobs/ddd444/9"),
                "{as_remote}"
            );
            let as_local = String::from_utf8(serialize_event_with_options(
                BepFormat::Json,
                proto,
                false,
                prefix,
            ))
            .unwrap();
            assert!(as_local.contains("file:///tmp/test.log"), "{as_local}");
            assert!(!as_local.contains("bytestream://"), "{as_local}");
        }
    }

    #[test]
    fn bep_bytestream_hashes_local_files_and_omits_uri_without_digest() {
        let dir = std::env::temp_dir().join(format!("zel-bep-bs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let primary_path = dir.join("out bin.txt");
        let test_log_path = dir.join("test.log");
        let fail_log_path = dir.join("fail.log");
        let tool_log_path = dir.join("command.profile.gz");
        std::fs::write(&primary_path, b"primary-bytes").unwrap();
        std::fs::write(&test_log_path, b"passed-log").unwrap();
        std::fs::write(&fail_log_path, b"failed-log").unwrap();
        std::fs::write(&tool_log_path, b"profile-bytes").unwrap();

        let expected_uri = |content: &[u8]| -> String {
            format!(
                "bytestream://cas.example.com:9092/inst/blobs/{}/{}",
                zel_util::sha256::hex_digest(content),
                content.len()
            )
        };
        let prefix = Some("cas.example.com:9092/inst");

        // 1. ActionExecuted built via `action_executed_with_primary_output_path` (no pre-set digest)
        let ae_proto = BepBuilder::action_executed_with_primary_output_path(
            "bazel-out/out bin.txt",
            Some(primary_path.to_str().unwrap()),
            "//pkg:gen",
            "cfg",
            "Genrule",
            true,
            0,
            Some(b"inline-stdout"),
            None,
            &["/bin/sh".into()],
            1000,
            1010,
            None,
        )
        .to_proto(false);
        let ae_json = String::from_utf8(serialize_event_with_options(
            BepFormat::Json,
            &ae_proto,
            true,
            prefix,
        ))
        .unwrap();
        assert!(
            ae_json.contains(&expected_uri(b"primary-bytes")),
            "{ae_json}"
        );
        assert!(!ae_json.contains("file://"), "{ae_json}");

        // 2. TestResult built via `BepBuilder::test_result` (calls `BepBuilder::file` with None digest)
        let tr_proto = BepBuilder::test_result(
            "//pkg:t",
            "cfg",
            0,
            0,
            1,
            "PASSED",
            false,
            1000,
            25,
            &[(
                "test.log".to_string(),
                file_uri(test_log_path.to_str().unwrap()),
            )],
            "local",
            0,
        )
        .to_proto(false);
        let tr_json = String::from_utf8(serialize_event_with_options(
            BepFormat::Json,
            &tr_proto,
            true,
            prefix,
        ))
        .unwrap();
        assert!(tr_json.contains(&expected_uri(b"passed-log")), "{tr_json}");
        assert!(!tr_json.contains("file://"), "{tr_json}");

        // 3. TestSummary built via `BepBuilder::test_summary` (`passed` and `failed` logs)
        let ts_proto = BepBuilder::test_summary(
            "//pkg:t",
            "cfg",
            "FLAKY",
            1,
            0,
            2,
            &[(
                "test.log".to_string(),
                test_log_path.to_string_lossy().into_owned(),
            )],
            &[(
                "fail.log".to_string(),
                fail_log_path.to_string_lossy().into_owned(),
            )],
            0,
            1000,
            1050,
            50,
            vec![],
        )
        .to_proto(false);
        let ts_json = String::from_utf8(serialize_event_with_options(
            BepFormat::Json,
            &ts_proto,
            true,
            prefix,
        ))
        .unwrap();
        assert!(ts_json.contains(&expected_uri(b"passed-log")), "{ts_json}");
        assert!(ts_json.contains(&expected_uri(b"failed-log")), "{ts_json}");
        assert!(!ts_json.contains("file://"), "{ts_json}");

        // 4. BuildToolLogs built via `BepBuilder::build_tool_logs` and `build_tool_logs_entries`
        let btl_proto = BepBuilder::build_tool_logs(&[(
            "command.profile.gz".to_string(),
            tool_log_path.to_string_lossy().into_owned(),
        )])
        .to_proto(true);
        let btl_json = String::from_utf8(serialize_event_with_options(
            BepFormat::Json,
            &btl_proto,
            true,
            prefix,
        ))
        .unwrap();
        assert!(
            btl_json.contains(&expected_uri(b"profile-bytes")),
            "{btl_json}"
        );
        assert!(!btl_json.contains("file://"), "{btl_json}");

        let btl_entries_proto = BepBuilder::build_tool_logs_entries(&[
            BepLogFile::LocalPath {
                name: "command.profile.gz".to_string(),
                path: tool_log_path.to_string_lossy().into_owned(),
            },
            BepLogFile::Contents {
                name: "inline.txt".to_string(),
                contents: b"abc".to_vec(),
            },
        ])
        .to_proto(true);
        let btl_entries_json = String::from_utf8(serialize_event_with_options(
            BepFormat::Json,
            &btl_entries_proto,
            true,
            prefix,
        ))
        .unwrap();
        assert!(
            btl_entries_json.contains(&expected_uri(b"profile-bytes")),
            "{btl_entries_json}"
        );
        assert!(!btl_entries_json.contains("file://"), "{btl_entries_json}");

        // 5. Missing / unhashed local file with no digest: `ByteStreamBuildEventArtifactUploader`
        //    `PathConverter.apply` returns `null`, so `uri` is omitted instead of keeping `file://`.
        let missing_path = dir.join("does_not_exist.txt");
        let mut ns = Msg::new("build_event_stream.NamedSetOfFiles");
        ns.set_msg(
            "files",
            BepBuilder::file(
                "does_not_exist.txt",
                missing_path.to_str().unwrap(),
                &[],
                None,
                None,
            ),
        );
        let missing_proto =
            BuildEventNode::new(ids::named_set("m"), vec![], "named_set_of_files", ns)
                .to_proto(false);
        let missing_json = String::from_utf8(serialize_event_with_options(
            BepFormat::Json,
            &missing_proto,
            true,
            prefix,
        ))
        .unwrap();
        assert!(
            missing_json.contains("\"name\":\"does_not_exist.txt\""),
            "{missing_json}"
        );
        assert!(!missing_json.contains("file://"), "{missing_json}");
        assert!(!missing_json.contains("bytestream://"), "{missing_json}");
        assert!(!missing_json.contains("\"uri\""), "{missing_json}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_uri_windows_and_unix_roundtrip() {
        // `PathConverter.FileUriPathConverter`: Windows drive paths use `/` separators in `file:///C:/...`.
        assert_eq!(
            file_uri(r"C:\Users\runner\out bin.txt"),
            "file:///C:/Users/runner/out%20bin.txt"
        );
        assert_eq!(
            file_uri("C:/Users/runner/out bin.txt"),
            "file:///C:/Users/runner/out%20bin.txt"
        );
        assert_eq!(
            file_uri(r"\\?\C:\Users\runner\out bin.txt"),
            "file:///C:/Users/runner/out%20bin.txt"
        );

        // `file_uri_to_local_path` strips the leading `/` before a Windows drive prefix for both
        // forward-slash and `%5C`-encoded URIs, as well as `file://localhost/...` authority URIs.
        assert_eq!(
            file_uri_to_local_path("file:///C:/Users/runner/out%20bin.txt").as_deref(),
            Some("C:/Users/runner/out bin.txt")
        );
        assert_eq!(
            file_uri_to_local_path("file:///C:%5CUsers%5Crunner%5Cout%20bin.txt").as_deref(),
            Some(r"C:\Users\runner\out bin.txt")
        );
        assert_eq!(
            file_uri_to_local_path("file://localhost/C:/Users/runner/out%20bin.txt").as_deref(),
            Some("C:/Users/runner/out bin.txt")
        );
        assert_eq!(
            file_uri_to_local_path("file://localhost/a%20b/c").as_deref(),
            Some("/a b/c")
        );

        // On Unix, a backslash inside a `/...` path is a literal filename character (`%5C`).
        #[cfg(not(windows))]
        {
            assert_eq!(file_uri(r"/tmp/a\b"), "file:///tmp/a%5Cb");
            assert_eq!(
                file_uri_to_local_path("file:///tmp/a%5Cb").as_deref(),
                Some(r"/tmp/a\b")
            );
        }
    }
}
