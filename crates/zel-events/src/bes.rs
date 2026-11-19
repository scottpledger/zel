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

//! Build Event Service upload (`--bes_backend`) and `ByteStreamBuildEventArtifactUploader`:
//! - Publishes lifecycle events (`BuildEnqueued` seq 1, `InvocationAttemptStarted` seq 1,
//!   `InvocationAttemptFinished` seq 2, `BuildFinished` seq 2) via `PublishLifecycleEvent`
//! - Streams BEP events over a full-duplex `PublishBuildToolEventStream` bidirectional gRPC call
//!   with strict acknowledgement sequence checking, `--bes_timeout` cancellation, and
//!   `--bes_upload_mode` (`wait_for_upload_complete`, `nowait_for_upload_complete`, `fully_async`)
//! - Supports TLS/mTLS (`Endpoint::parse_grpc`), `--bes_proxy`, REAPI `RequestMetadata`, and
//!   `CallCredentialsProvider`
//! - Uploads BEP-referenced local artifacts to CAS and rewrites `File.uri` to `bytestream://...`
//!   (`ByteStreamBuildEventArtifactUploader`).

use crate::bep::{BesUploadMode, BuildEventTransport};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use zel_proto::dynamic::{Msg, Value};
use zel_proto::reapi::{code, Digest, Message as ReapiMessage, RequestMetadata, ToolDetails};
use zel_remote::credentials::{
    CallCredentialsProvider, CompositeCredentialsProvider, CredentialHelperProvider,
    GoogleAuthCallCredentialsProvider, NetrcCredentialsProvider,
};
use zel_remote::digest::DigestFn;
use zel_remote::grpc::{BidiStream, CallOptions, Channel, ChannelConfig, GrpcStatus, RetryPolicy};
use zel_remote::http_cache::HttpCacheClient;
use zel_remote::merkle::BlobSource;
use zel_remote::options::{BuildEventUploadMode, Endpoint};
use zel_remote::reapi_client::{ActionMeta, ReapiClient};
use zel_remote::transport::{Connector, TlsConfig};
use zel_util::base64_encode;
use zel_util::java_regex::Regex;

pub const PUBLISH_LIFECYCLE: &str =
    "/google.devtools.build.v1.PublishBuildEvent/PublishLifecycleEvent";
pub const PUBLISH_STREAM: &str =
    "/google.devtools.build.v1.PublishBuildEvent/PublishBuildToolEventStream";
const BEP_TYPE_URL: &str = "type.googleapis.com/build_event_stream.BuildEvent";
const REQUEST_METADATA_HEADER: &str = "build.bazel.remote.execution.v2.requestmetadata-bin";
const BES_MAX_RETRIES: u32 = 4;

/// Classification of BES upload errors for Bazel exit codes (`38` transient vs `45` persistent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BesErrorKind {
    /// `ExitCode.TRANSIENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR` (`38`).
    Transient,
    /// `ExitCode.PERSISTENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR` (`45`).
    Persistent,
}

#[derive(Debug, Clone)]
pub struct BesUploadError {
    pub kind: BesErrorKind,
    pub message: String,
    pub timed_out: bool,
}

impl BesUploadError {
    fn from_grpc(st: &GrpcStatus) -> Self {
        let kind = match st.code {
            code::INVALID_ARGUMENT | code::PERMISSION_DENIED => BesErrorKind::Persistent,
            _ => BesErrorKind::Transient,
        };
        Self {
            kind,
            message: st.to_string(),
            timed_out: st.code == code::DEADLINE_EXCEEDED,
        }
    }

    fn transient(msg: impl Into<String>) -> Self {
        Self {
            kind: BesErrorKind::Transient,
            message: msg.into(),
            timed_out: false,
        }
    }

    fn timeout() -> Self {
        Self {
            kind: BesErrorKind::Transient,
            message: "Build Event Service upload timed out (--bes_timeout)".into(),
            timed_out: true,
        }
    }
}

/// Default maximum duration Bazel waits at the start of a command for a previous invocation's
/// background BES upload (`BuildEventServiceModule.getMaxWaitForPreviousInvocation` = 5s).
pub const DEFAULT_PREVIOUS_UPLOAD_MAX_WAIT: Duration = Duration::from_secs(5);

struct BackgroundUpload {
    mode: BesUploadMode,
    timeout: Option<Duration>,
    started_close: Instant,
    shared: Arc<Shared>,
    done_rx: Option<mpsc::Receiver<Result<(), BesUploadError>>>,
    worker: JoinHandle<()>,
}

/// Outcome of waiting on previous background BES uploads (`BuildEventServiceModule.waitForPreviousInvocation`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreviousUploadDrain {
    pub had_any: bool,
    pub waited: Duration,
    pub warnings: Vec<String>,
}

/// Process-wide list of background BES uploads deferred via
/// `--bes_upload_mode=nowait_for_upload_complete` or `fully_async`.
static BACKGROUND_UPLOADS: OnceLock<Mutex<Vec<BackgroundUpload>>> = OnceLock::new();

fn background_uploads() -> &'static Mutex<Vec<BackgroundUpload>> {
    BACKGROUND_UPLOADS.get_or_init(|| Mutex::new(Vec::new()))
}

fn cancel_background_shared(shared: &Arc<Shared>) {
    shared.cancelled.store(true, Ordering::SeqCst);
    if let Some(b) = shared.active_bidi.lock().unwrap().take() {
        b.cancel();
    }
    shared.cv.notify_all();
}

fn format_previous_upload_error(err: &BesUploadError) -> String {
    if err.timed_out {
        "The background upload of the Build Event Protocol for the previous invocation failed due to a network timeout. Ignoring the failure and starting a new invocation...".to_string()
    } else {
        format!(
            "The background upload of the Build Event Protocol for the previous invocation failed with the following exception: '{}'. Ignoring the failure and starting a new invocation...",
            err.message
        )
    }
}

/// Waits for background BES uploads started by previous commands, bounded by `max_wait`
/// (`BuildEventServiceModule.waitForPreviousInvocation`). Previous upload failures or timeouts are
/// reported in `warnings` rather than failing the new invocation.
pub fn drain_previous_uploads_ext(is_shutdown: bool, max_wait: Duration) -> PreviousUploadDrain {
    let uploads: Vec<BackgroundUpload> = std::mem::take(&mut *background_uploads().lock().unwrap());
    if uploads.is_empty() {
        return PreviousUploadDrain::default();
    }
    let started = Instant::now();
    let overall_deadline = started + max_wait;
    let mut warnings = Vec::new();

    for mut u in uploads {
        if u.mode == BesUploadMode::FullyAsync && !is_shutdown {
            let mut timed_out_wait = false;
            {
                let mut q = u.shared.q.lock().unwrap();
                while !q.half_closed && !q.finished {
                    let now = Instant::now();
                    if now >= overall_deadline {
                        timed_out_wait = true;
                        break;
                    }
                    let (nq, _) = u.shared.cv.wait_timeout(q, overall_deadline - now).unwrap();
                    q = nq;
                }
            }
            if timed_out_wait {
                let ms = started.elapsed().as_millis();
                warnings.push(format!(
                    "The background upload of the Build Event Protocol for the previous invocation failed to complete in {}.{:03} seconds. Cancelling and starting a new invocation...",
                    ms / 1000,
                    ms % 1000
                ));
                cancel_background_shared(&u.shared);
                let _ = u.worker.join();
            } else if let Some(rx) = u.done_rx.take() {
                match rx.try_recv() {
                    Ok(Ok(())) | Err(mpsc::TryRecvError::Disconnected) => {
                        let _ = u.worker.join();
                    }
                    Ok(Err(err)) => {
                        warnings.push(format_previous_upload_error(&err));
                        cancel_background_shared(&u.shared);
                        let _ = u.worker.join();
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        // `BuildEventServiceModule.waitForPreviousInvocation`: `fully_async`
                        // waits only on `halfCloseFuture` (unless shutting down) and is not
                        // cancelled if half-close succeeded without error.
                    }
                }
            }
            continue;
        }

        let bes_dl = u.timeout.map(|t| u.started_close + t);
        let eff_dl = match bes_dl {
            Some(bdl) if bdl < overall_deadline => bdl,
            _ => overall_deadline,
        };
        if let Some(rx) = u.done_rx.take() {
            let rem = eff_dl.saturating_duration_since(Instant::now());
            match rx.recv_timeout(rem) {
                Ok(Ok(())) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = u.worker.join();
                }
                Ok(Err(err)) => {
                    warnings.push(format_previous_upload_error(&err));
                    cancel_background_shared(&u.shared);
                    let _ = u.worker.join();
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if bes_dl.is_some_and(|bdl| bdl <= overall_deadline && Instant::now() >= bdl) {
                        warnings.push(format_previous_upload_error(&BesUploadError::timeout()));
                    } else {
                        let ms = started.elapsed().as_millis();
                        warnings.push(format!(
                            "The background upload of the Build Event Protocol for the previous invocation failed to complete in {}.{:03} seconds. Cancelling and starting a new invocation...",
                            ms / 1000,
                            ms % 1000
                        ));
                    }
                    cancel_background_shared(&u.shared);
                    let _ = u.worker.join();
                }
            }
        } else {
            let _ = u.worker.join();
        }
    }

    PreviousUploadDrain {
        had_any: true,
        waited: started.elapsed(),
        warnings,
    }
}

/// Waits for any background BES uploads started by previous commands. Returns `true` if at least
/// one background upload was waited on.
pub fn drain_previous_uploads() -> bool {
    drain_previous_uploads_ext(false, DEFAULT_PREVIOUS_UPLOAD_MAX_WAIT).had_any
}

// ------------------------------------------------------------------ ByteStream artifact uploader

/// Uploads local BEP-referenced files to CAS and rewrites `File.uri` to `bytestream://...`
/// (`remote/ByteStreamBuildEventArtifactUploader.java`).
pub struct ByteStreamBuildEventArtifactUploader {
    reapi: Option<Arc<ReapiClient>>,
    http_cache: Option<Arc<HttpCacheClient>>,
    digest_fn: DigestFn,
    upload_mode: BuildEventUploadMode,
    uri_prefix: String,
    known_remote: Mutex<HashMap<String, Digest>>,
    warnings: Mutex<Vec<String>>,
    minimal_testlog_re: Regex,
    minimal_stdio_re: Regex,
}

impl ByteStreamBuildEventArtifactUploader {
    pub fn new(
        reapi: Option<Arc<ReapiClient>>,
        http_cache: Option<Arc<HttpCacheClient>>,
        digest_fn: DigestFn,
        upload_mode: BuildEventUploadMode,
        authority: &str,
        instance_name: &str,
        bytestream_uri_prefix: Option<&str>,
    ) -> Self {
        let uri_prefix = match bytestream_uri_prefix.filter(|s| !s.is_empty()) {
            Some(p) => p.trim_end_matches('/').to_string(),
            None => {
                let auth = authority.trim_end_matches('/');
                if instance_name.is_empty() {
                    auth.to_string()
                } else {
                    format!("{auth}/{}", instance_name.trim_matches('/'))
                }
            }
        };
        Self {
            reapi,
            http_cache,
            digest_fn,
            upload_mode,
            uri_prefix,
            known_remote: Mutex::new(HashMap::new()),
            warnings: Mutex::new(Vec::new()),
            minimal_testlog_re: Regex::new(r"(?s)^.*/bazel-out/[^/]*/testlogs/.*$").unwrap(),
            minimal_stdio_re: Regex::new(r"(?s)^.*/bazel-out/_tmp/actions/std(err|out)-.*$")
                .unwrap(),
        }
    }

    /// Registers an already-remote artifact path + digest so `bytestream://` conversion skips
    /// local file reads and CAS re-uploads.
    pub fn register_remote_artifact(&self, abs_path: String, digest: Digest) {
        self.known_remote
            .lock()
            .unwrap()
            .insert(normalize_local_path_key(&abs_path), digest);
    }

    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut *self.warnings.lock().unwrap())
    }

    pub fn bytestream_uri(&self, d: &Digest) -> String {
        match self.digest_fn.resource_name() {
            Some(fn_name) => format!(
                "bytestream://{}/blobs/{fn_name}/{}/{}",
                self.uri_prefix, d.hash, d.size_bytes
            ),
            None => format!(
                "bytestream://{}/blobs/{}/{}",
                self.uri_prefix, d.hash, d.size_bytes
            ),
        }
    }

    fn should_upload_in_minimal(&self, path: &str, is_build_tool_logs: bool) -> bool {
        if is_build_tool_logs {
            return true;
        }
        let slash_path;
        let p = if path.contains('\\') {
            slash_path = path.replace('\\', "/");
            slash_path.as_str()
        } else {
            path
        };
        self.minimal_testlog_re.is_match(p) || self.minimal_stdio_re.is_match(p)
    }

    fn upload_blob(&self, d: &Digest, data: Vec<u8>) -> Result<(), String> {
        if d.size_bytes <= 0 {
            return Ok(());
        }
        if let Some(c) = &self.reapi {
            let mut map = BTreeMap::new();
            map.insert(d.clone(), BlobSource::Bytes(data));
            let meta = ActionMeta {
                action_id: "bes-upload".into(),
                ..Default::default()
            };
            c.ensure_blobs_present(&map, &meta)
                .map(|_| ())
                .map_err(|e| e.to_string())
        } else if let Some(hc) = &self.http_cache {
            hc.upload_blob(d, &data).map_err(|e| e.to_string())
        } else {
            Ok(())
        }
    }

    fn convert_file_msg(&self, f: &mut Msg, is_build_tool_logs: bool) {
        let uri = f.get_str("uri").to_string();
        let Some(local_path) = decode_file_uri(&uri) else {
            return;
        };
        let key = normalize_local_path_key(&local_path);
        if let Some(d) = self.known_remote.lock().unwrap().get(&key).cloned() {
            let bs = self.bytestream_uri(&d);
            f.clear("uri");
            f.set_str("uri", bs);
            return;
        }
        let Ok(md) = std::fs::symlink_metadata(&local_path) else {
            return;
        };
        let ft = md.file_type();
        // Directories and symlinks are never uploaded to CAS.
        if ft.is_dir() || ft.is_symlink() || !ft.is_file() {
            return;
        }
        let data = match std::fs::read(&local_path) {
            Ok(b) => b,
            Err(e) => {
                self.warnings.lock().unwrap().push(format!(
                    "Uploading BEP referenced local file {local_path}: {e}"
                ));
                return;
            }
        };
        let d = self.digest_fn.digest(&data);
        match self.upload_mode {
            BuildEventUploadMode::Minimal => {
                if self.should_upload_in_minimal(&local_path, is_build_tool_logs) {
                    if let Err(e) = self.upload_blob(&d, data) {
                        self.warnings.lock().unwrap().push(format!(
                            "Uploading BEP referenced local file {local_path}: {e}"
                        ));
                    } else {
                        self.known_remote.lock().unwrap().insert(key, d.clone());
                    }
                }
                // In MINIMAL mode, every digested regular file receives a `bytestream://` URI even
                // if the blob was not uploaded.
                let bs = self.bytestream_uri(&d);
                f.clear("uri");
                f.set_str("uri", bs);
            }
            BuildEventUploadMode::All => {
                match self.upload_blob(&d, data) {
                    Ok(()) => {
                        self.known_remote.lock().unwrap().insert(key, d.clone());
                        let bs = self.bytestream_uri(&d);
                        f.clear("uri");
                        f.set_str("uri", bs);
                    }
                    Err(e) => {
                        self.warnings.lock().unwrap().push(format!(
                            "Uploading BEP referenced local file {local_path}: {e}"
                        ));
                        // Leave local `file://` URI intact on upload failure in ALL mode.
                    }
                }
            }
        }
    }
}

fn has_windows_drive_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b.len() == 2 || b[2] == b'/' || b[2] == b'\\')
}

fn normalize_local_path_key(path: &str) -> String {
    let replaced = path.replace('\\', "/");
    let trimmed = if replaced.starts_with('/') && has_windows_drive_prefix(&replaced[1..]) {
        &replaced[1..]
    } else {
        replaced.as_str()
    };
    if has_windows_drive_prefix(trimmed) {
        let mut out = trimmed.to_string();
        out.replace_range(0..1, &out[0..1].to_ascii_uppercase());
        out
    } else {
        trimmed.to_string()
    }
}

fn decode_file_uri(uri: &str) -> Option<String> {
    decode_file_uri_for_os(uri, cfg!(windows))
}

fn decode_file_uri_for_os(uri: &str, windows: bool) -> Option<String> {
    let mut rest = uri.strip_prefix("file://")?;
    if rest.len() >= 10 && rest[..9].eq_ignore_ascii_case("localhost") && rest.as_bytes()[9] == b'/'
    {
        rest = &rest[9..];
    }
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    let decoded = String::from_utf8(out).ok()?;
    if decoded.starts_with('/') && has_windows_drive_prefix(&decoded[1..]) {
        return Some(decoded[1..].to_string());
    }
    if !decoded.starts_with('/') && !has_windows_drive_prefix(&decoded) && !decoded.is_empty() {
        if windows {
            return Some(format!("\\\\{}", decoded.replace('/', "\\")));
        }
        return Some(format!("//{decoded}"));
    }
    Some(decoded)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Rewrites `build_event_stream.File` entries inside `ev` according to `path_conversion` and an
/// optional [`ByteStreamBuildEventArtifactUploader`].
pub fn rewrite_bep_event(
    ev: &Msg,
    path_conversion: bool,
    uploader: Option<&ByteStreamBuildEventArtifactUploader>,
) -> Msg {
    let mut out = ev.clone();
    let transform_file = |f: &mut Msg, is_tool_logs: bool| {
        if !path_conversion {
            let uri = f.get_str("uri").to_string();
            if let Some(local) = decode_file_uri(&uri) {
                if let Ok(md) = std::fs::symlink_metadata(&local) {
                    if md.file_type().is_dir() || md.file_type().is_symlink() {
                        f.clear("uri");
                    }
                }
            }
        } else if let Some(u) = uploader {
            u.convert_file_msg(f, is_tool_logs);
        }
    };

    if let Some(ns) = out.get_msg("named_set_of_files").cloned() {
        let mut new_ns = ns.clone();
        let files: Vec<Msg> = ns.get_msgs("files").into_iter().cloned().collect();
        if !files.is_empty() {
            new_ns.clear("files");
            for mut f in files {
                transform_file(&mut f, false);
                new_ns.set_msg("files", f);
            }
            out.clear("named_set_of_files");
            out.set_msg("named_set_of_files", new_ns);
        }
    }

    if let Some(act) = out.get_msg("action").cloned() {
        let mut new_act = act.clone();
        let mut changed = false;
        for single in ["primary_output", "stdout", "stderr"] {
            if let Some(mut f) = act.get_msg(single).cloned() {
                transform_file(&mut f, false);
                new_act.clear(single);
                new_act.set_msg(single, f);
                changed = true;
            }
        }
        let logs: Vec<Msg> = act
            .get_msgs("action_metadata_logs")
            .into_iter()
            .cloned()
            .collect();
        if !logs.is_empty() {
            new_act.clear("action_metadata_logs");
            for mut f in logs {
                transform_file(&mut f, false);
                new_act.set_msg("action_metadata_logs", f);
            }
            changed = true;
        }
        if changed {
            out.clear("action");
            out.set_msg("action", new_act);
        }
    }

    if let Some(tr) = out.get_msg("test_result").cloned() {
        let files: Vec<Msg> = tr
            .get_msgs("test_action_output")
            .into_iter()
            .cloned()
            .collect();
        if !files.is_empty() {
            let mut new_tr = tr;
            new_tr.clear("test_action_output");
            for mut f in files {
                transform_file(&mut f, false);
                new_tr.set_msg("test_action_output", f);
            }
            out.clear("test_result");
            out.set_msg("test_result", new_tr);
        }
    }

    if let Some(ts) = out.get_msg("test_summary").cloned() {
        let mut new_ts = ts.clone();
        let mut changed = false;
        for list_field in ["passed", "failed"] {
            let files: Vec<Msg> = ts.get_msgs(list_field).into_iter().cloned().collect();
            if !files.is_empty() {
                new_ts.clear(list_field);
                for mut f in files {
                    transform_file(&mut f, false);
                    new_ts.set_msg(list_field, f);
                }
                changed = true;
            }
        }
        if changed {
            out.clear("test_summary");
            out.set_msg("test_summary", new_ts);
        }
    }

    if let Some(btl) = out.get_msg("build_tool_logs").cloned() {
        let files: Vec<Msg> = btl.get_msgs("log").into_iter().cloned().collect();
        if !files.is_empty() {
            let mut new_btl = btl;
            new_btl.clear("log");
            for mut f in files {
                transform_file(&mut f, true);
                new_btl.set_msg("log", f);
            }
            out.clear("build_tool_logs");
            out.set_msg("build_tool_logs", new_btl);
        }
    }

    out
}

// ------------------------------------------------------------------ BES config & transport

/// Parsed `--bes_*` configuration.
#[derive(Debug, Clone, Default)]
pub struct BesConfig {
    pub backend: String,
    pub proxy: Option<String>,
    pub results_url: Option<String>,
    pub timeout: Option<Duration>,
    pub upload_mode: BesUploadMode,
    pub headers: Vec<(String, String)>,
    pub instance_name: String,
    pub keywords: Vec<String>,
    pub system_keywords: Vec<String>,
    pub project_id: Option<String>,
    pub lifecycle_events: bool,
    pub check_preceding_lifecycle_events: bool,
    pub build_id: String,
    pub invocation_id: String,
    pub command_name: String,
    pub remote_bytestream_uri_prefix: Option<String>,
    pub tls_config: TlsConfig,
    pub google_default_credentials: bool,
    pub google_credentials: Option<String>,
    pub google_auth_scopes: Vec<String>,
    pub credential_helpers: Vec<String>,
    pub credential_helper_timeout: Duration,
    pub credential_helper_cache_duration: Duration,
    pub netrc: Option<String>,
    pub client_env: std::collections::BTreeMap<String, String>,
}

impl BesConfig {
    pub fn from_options(
        o: &crate::bep::BepOptions,
        build_id: &str,
        invocation_id: &str,
        command: &str,
    ) -> Option<Self> {
        Some(Self {
            backend: o.bes_backend.clone()?,
            proxy: o.bes_proxy.clone(),
            results_url: o.bes_results_url.clone(),
            timeout: o.bes_timeout.or_else(|| {
                o.bes_timeout_secs
                    .filter(|s| *s > 0)
                    .map(Duration::from_secs)
            }),
            upload_mode: o.bes_upload_mode,
            headers: o.bes_headers.clone(),
            instance_name: o.bes_instance_name.clone(),
            keywords: o.bes_keywords.clone(),
            system_keywords: o.bes_system_keywords.clone(),
            project_id: o.project_id.clone(),
            lifecycle_events: o.bes_lifecycle_events,
            check_preceding_lifecycle_events: o.bes_check_preceding_lifecycle_events,
            build_id: build_id.to_string(),
            invocation_id: invocation_id.to_string(),
            command_name: command.to_string(),
            remote_bytestream_uri_prefix: o.remote_bytestream_uri_prefix.clone(),
            tls_config: TlsConfig {
                ca_certificate: o.tls_certificate.as_ref().map(PathBuf::from),
                client_certificate: o.tls_client_certificate.as_ref().map(PathBuf::from),
                client_key: o.tls_client_key.as_ref().map(PathBuf::from),
                authority_override: o.tls_authority_override.clone(),
            },
            google_default_credentials: o.google_default_credentials,
            google_credentials: o.google_credentials.clone(),
            google_auth_scopes: o.google_auth_scopes.clone(),
            credential_helpers: o.credential_helpers.clone(),
            credential_helper_timeout: o
                .credential_helper_timeout
                .unwrap_or(Duration::from_secs(5)),
            credential_helper_cache_duration: o
                .credential_helper_cache_duration
                .unwrap_or(Duration::from_secs(1800)),
            netrc: o.netrc.clone(),
            client_env: Default::default(),
        })
    }

    /// Effective `project_id` sent on BES requests (`--bes_instance_name` or `--project_id`).
    pub fn effective_project_id(&self) -> Option<&str> {
        if !self.instance_name.is_empty() {
            Some(self.instance_name.as_str())
        } else {
            self.project_id.as_deref().filter(|s| !s.is_empty())
        }
    }

    /// `INFO: Streaming build results to: <url>/<invocation id>` line, when a non-empty results
    /// URL is set, inserting a `/` separator if `<url>` does not already end with `/`.
    pub fn streaming_message(&self) -> Option<String> {
        let u = self.results_url.as_deref()?.trim();
        if u.is_empty() {
            return None;
        }
        if u.ends_with('/') {
            Some(format!(
                "Streaming build results to: {u}{}",
                self.invocation_id
            ))
        } else {
            Some(format!(
                "Streaming build results to: {u}/{}",
                self.invocation_id
            ))
        }
    }

    /// Normalized keywords in Bazel's exact order:
    /// `protocol_name=BEP`, `command_name=<cmd>`, `<system_keywords...>`, `user_keyword=<kw>...`,
    /// deduplicated keeping the first occurrence.
    pub fn all_keywords(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut push_dedup = |s: String| {
            if !out.contains(&s) {
                out.push(s);
            }
        };
        push_dedup("protocol_name=BEP".to_string());
        push_dedup(format!("command_name={}", self.command_name));
        for sys in &self.system_keywords {
            push_dedup(sys.clone());
        }
        for u in &self.keywords {
            push_dedup(format!("user_keyword={u}"));
        }
        out
    }

    /// Encodes REAPI `RequestMetadata` header attached to BES RPCs (`BuildEventServiceGrpcClient`).
    pub fn request_metadata_headers(&self) -> Vec<(String, String)> {
        let rm = RequestMetadata {
            tool_details: Some(ToolDetails {
                tool_name: "bazel".into(),
                tool_version: crate::BUILD_TOOL_VERSION.into(),
            }),
            action_id: "bes-upload".into(),
            tool_invocation_id: self.invocation_id.clone(),
            correlated_invocations_id: self.build_id.clone(),
            ..Default::default()
        };
        vec![(
            REQUEST_METADATA_HEADER.to_string(),
            base64_encode(&rm.encode_to_vec()),
        )]
    }

    fn build_credentials(&self) -> Result<Option<Arc<dyn CallCredentialsProvider>>, String> {
        let helper = if !self.credential_helpers.is_empty() {
            Some(CredentialHelperProvider::from_specs(
                &self.credential_helpers,
                None,
                self.credential_helper_timeout,
                self.credential_helper_cache_duration,
            ))
        } else {
            None
        };
        let google = if let Some(path) = &self.google_credentials {
            Some(GoogleAuthCallCredentialsProvider::from_file(
                PathBuf::from(path),
                self.google_auth_scopes.clone(),
                self.tls_config.clone(),
            ))
        } else if self.google_default_credentials {
            Some(
                GoogleAuthCallCredentialsProvider::from_adc(
                    self.google_auth_scopes.clone(),
                    self.tls_config.clone(),
                )
                .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        let netrc = if let Some(path) = &self.netrc {
            Some(NetrcCredentialsProvider::from_file(Path::new(path))?)
        } else if self.client_env.contains_key("NETRC")
            || self.client_env.contains_key("HOME")
            || self.client_env.contains_key("USERPROFILE")
        {
            NetrcCredentialsProvider::from_client_env(&self.client_env)
                .ok()
                .flatten()
        } else {
            NetrcCredentialsProvider::from_env()
        };
        if helper.is_none() && google.is_none() && netrc.is_none() {
            return Ok(None);
        }
        Ok(Some(Arc::new(CompositeCredentialsProvider {
            helper,
            google,
            netrc,
        })))
    }

    /// Opens a gRPC channel for the BES backend (`Endpoint::parse_grpc`: bare host and
    /// `grpcs://`/`https://` use TLS on port 443; `grpc://` uses plaintext on port 443; `unix:`
    /// uses Unix domain sockets), applying `--bes_proxy`, TLS/mTLS, headers, and credentials.
    pub fn channel(&self) -> Result<Arc<Channel>, String> {
        self.tls_config.validate()?;
        let ep = Endpoint::parse_grpc(&self.backend)?;
        let proxy = match &self.proxy {
            Some(p) => {
                if !p.starts_with("unix:") {
                    return Err(format!("--bes_proxy={p}: only unix: proxies are supported"));
                }
                match Endpoint::parse_grpc(p)? {
                    Endpoint::Unix(u) => Some(u),
                    _ => return Err(format!("--bes_proxy={p}: only unix: proxies are supported")),
                }
            }
            None => None,
        };
        let conn =
            ep.connector_with_tls(Duration::from_secs(30), &self.tls_config, proxy.as_deref())?;
        // Collapse repeated header names (last-wins in first-seen key order).
        let mut collapsed: Vec<(String, String)> = Vec::new();
        for (k, v) in &self.headers {
            let lk = k.to_ascii_lowercase();
            if let Some(slot) = collapsed.iter_mut().find(|(ek, _)| ek == &lk) {
                slot.1 = v.clone();
            } else {
                collapsed.push((lk, v.clone()));
            }
        }
        let creds = self.build_credentials()?;
        let auth_uri = if self.backend.contains("://") || self.backend.starts_with("unix:") {
            self.backend.clone()
        } else {
            ep.auth_uri()
        };
        Ok(Arc::new(Channel::with_config(
            conn,
            ChannelConfig {
                tls: ep.is_tls(),
                max_connections: 1,
                max_concurrency_per_connection: 100,
                default_metadata: collapsed,
                credentials: creds,
                auth_uri,
            },
        )))
    }
}

fn timestamp_now(m: &mut Msg, field: &str) -> Msg {
    let ms = crate::now_millis();
    let mut t = m.new_child(field);
    t.set_int("seconds", ms.div_euclid(1000));
    t.set_int("nanos", ms.rem_euclid(1000) * 1_000_000);
    t
}

fn stream_id(cfg: &BesConfig, component: &str, with_invocation: bool) -> Msg {
    let mut s = Msg::new("google.devtools.build.v1.StreamId");
    s.set_str("build_id", cfg.build_id.clone());
    if with_invocation {
        s.set_str("invocation_id", cfg.invocation_id.clone());
    }
    s.set_enum("component", component);
    s
}

fn any(type_url: &str, value: Vec<u8>) -> Msg {
    let mut a = Msg::new("google.protobuf.Any");
    a.set_str("type_url", type_url);
    a.set_bytes("value", value);
    a
}

fn ordered(cfg: &BesConfig, component: &str, with_inv: bool, seq: i64, mut ev: Msg) -> Msg {
    let t = timestamp_now(&mut ev, "event_time");
    ev.set_msg("event_time", t);
    let mut o = Msg::new("google.devtools.build.v1.OrderedBuildEvent");
    o.set_msg("stream_id", stream_id(cfg, component, with_inv));
    o.set_int("sequence_number", seq);
    o.set_msg("event", ev);
    o
}

/// Builds a `PublishLifecycleEventRequest` for the given kind (`build_enqueued`,
/// `invocation_attempt_started`, `invocation_attempt_finished`, `build_finished`).
pub fn lifecycle_request(cfg: &BesConfig, kind: &str, success: bool) -> Msg {
    let mut ev = Msg::new("google.devtools.build.v1.BuildEvent");
    let seq = match kind {
        "build_enqueued" => {
            ev.set_msg(
                "build_enqueued",
                Msg::new("google.devtools.build.v1.BuildEvent.BuildEnqueued"),
            );
            1
        }
        "invocation_attempt_started" => {
            let mut s = Msg::new("google.devtools.build.v1.BuildEvent.InvocationAttemptStarted");
            s.set_int("attempt_number", 1);
            ev.set_msg("invocation_attempt_started", s);
            1
        }
        "invocation_attempt_finished" => {
            let mut f = Msg::new("google.devtools.build.v1.BuildEvent.InvocationAttemptFinished");
            let mut st = Msg::new("google.devtools.build.v1.BuildStatus");
            st.set_enum(
                "result",
                if success {
                    "COMMAND_SUCCEEDED"
                } else {
                    "COMMAND_FAILED"
                },
            );
            f.set_msg("invocation_status", st);
            ev.set_msg("invocation_attempt_finished", f);
            2
        }
        _ => {
            let mut f = Msg::new("google.devtools.build.v1.BuildEvent.BuildFinished");
            let mut st = Msg::new("google.devtools.build.v1.BuildStatus");
            st.set_enum(
                "result",
                if success {
                    "COMMAND_SUCCEEDED"
                } else {
                    "COMMAND_FAILED"
                },
            );
            f.set_msg("status", st);
            ev.set_msg("build_finished", f);
            2
        }
    };
    let with_inv = kind.starts_with("invocation");
    let o = ordered(cfg, "CONTROLLER", with_inv, seq, ev);
    let mut r = Msg::new("google.devtools.build.v1.PublishLifecycleEventRequest");
    r.set_enum("service_level", "INTERACTIVE");
    r.set_msg("build_event", o);
    // Bazel omits `notification_keywords` on `invocation_attempt_finished`.
    if kind != "invocation_attempt_finished" {
        for k in cfg.all_keywords() {
            r.set_str("notification_keywords", k);
        }
    }
    if let Some(p) = cfg.effective_project_id() {
        r.set_str("project_id", p.to_string());
    }
    // Note: Bazel never sets `check_preceding_lifecycle_events_present` on lifecycle requests.
    r
}

/// Builds one tool-stream request carrying a BEP event (`None` = `ComponentStreamFinished`).
pub fn stream_request(cfg: &BesConfig, seq: i64, bep: Option<&Msg>) -> Msg {
    let mut ev = Msg::new("google.devtools.build.v1.BuildEvent");
    match bep {
        Some(b) => {
            ev.set_msg("bazel_event", any(BEP_TYPE_URL, b.encode_to_vec()));
        }
        None => {
            let mut f = Msg::new("google.devtools.build.v1.BuildEvent.ComponentStreamFinished");
            f.set_enum("type", "FINISHED");
            ev.set_msg("component_stream_finished", f);
        }
    }
    let o = ordered(cfg, "TOOL", true, seq, ev);
    let mut r = Msg::new("google.devtools.build.v1.PublishBuildToolEventStreamRequest");
    r.set_msg("ordered_build_event", o);
    // `project_id` is copied from `instance_name` onto EVERY tool-stream request.
    if let Some(p) = cfg.effective_project_id() {
        r.set_str("project_id", p.to_string());
    }
    if seq == 1 {
        for k in cfg.all_keywords() {
            r.set_str("notification_keywords", k);
        }
        if cfg.check_preceding_lifecycle_events {
            r.set_bool("check_preceding_lifecycle_events_present", true);
        }
    }
    r
}

#[derive(Default)]
struct Queue {
    /// Serialized requests; index = sequence number − 1.
    requests: Vec<Vec<u8>>,
    closed: bool,
    half_closed: bool,
    finished: bool,
    acked: i64,
}

struct WorkerExitGuard(Arc<Shared>);

impl Drop for WorkerExitGuard {
    fn drop(&mut self) {
        let mut q = self.0.q.lock().unwrap();
        // Mirror `BuildEventServiceUploader` (`closeFuture.addListener(() ->
        // halfCloseFuture.setFuture(closeFuture))`): ensure `half_closed` and `finished` are set
        // whenever the uploader finishes, errors during lifecycle/open_bidi, or panics so
        // `fully_async` `close()` can never block indefinitely.
        q.half_closed = true;
        q.finished = true;
        self.0.cv.notify_all();
    }
}

struct Shared {
    q: Mutex<Queue>,
    cv: Condvar,
    cancelled: AtomicBool,
    active_bidi: Mutex<Option<BidiStream>>,
    success: AtomicBool,
}

/// BES transport. Events are queued and streamed by a background thread.
pub struct BesTransport {
    cfg: BesConfig,
    shared: Arc<Shared>,
    done_rx: Mutex<Option<mpsc::Receiver<Result<(), BesUploadError>>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    artifact_uploader: Mutex<Option<Arc<ByteStreamBuildEventArtifactUploader>>>,
    last_error: Mutex<Option<BesUploadError>>,
    close_called: AtomicBool,
}

impl BesTransport {
    /// Connects and starts the upload thread. Lifecycle start events are published on the upload
    /// worker before opening the tool stream.
    pub fn start(cfg: BesConfig, channel: Arc<Channel>) -> Arc<Self> {
        let shared = Arc::new(Shared {
            q: Mutex::new(Queue::default()),
            cv: Condvar::new(),
            cancelled: AtomicBool::new(false),
            active_bidi: Mutex::new(None),
            success: AtomicBool::new(true),
        });
        let (tx, rx) = mpsc::channel();
        let t = Arc::new(Self {
            cfg: cfg.clone(),
            shared: shared.clone(),
            done_rx: Mutex::new(Some(rx)),
            worker: Mutex::new(None),
            artifact_uploader: Mutex::new(None),
            last_error: Mutex::new(None),
            close_called: AtomicBool::new(false),
        });
        let worker_cfg = cfg;
        let worker_shared = shared.clone();
        let h = std::thread::Builder::new()
            .name("bes-upload".into())
            .spawn(move || {
                let _exit_guard = WorkerExitGuard(worker_shared.clone());
                let r = run_uploader(&worker_cfg, &channel, &worker_shared);
                let _ = tx.send(r);
            })
            .ok();
        if h.is_none() {
            let mut q = shared.q.lock().unwrap();
            q.half_closed = true;
            q.finished = true;
            shared.cv.notify_all();
        }
        *t.worker.lock().unwrap() = h;
        t
    }

    pub fn set_artifact_uploader(&self, u: Arc<ByteStreamBuildEventArtifactUploader>) {
        *self.artifact_uploader.lock().unwrap() = Some(u);
    }

    pub fn artifact_uploader(&self) -> Option<Arc<ByteStreamBuildEventArtifactUploader>> {
        self.artifact_uploader.lock().unwrap().clone()
    }

    pub fn last_error(&self) -> Option<BesUploadError> {
        self.last_error.lock().unwrap().clone()
    }

    /// Declares whether the invocation succeeded (used for the lifecycle finish events).
    pub fn set_success(&self, ok: bool) {
        self.shared.success.store(ok, Ordering::SeqCst);
    }
}

fn publish_lifecycle_rpc(
    cfg: &BesConfig,
    channel: &Channel,
    kind: &str,
    success: bool,
) -> Result<(), GrpcStatus> {
    let req = lifecycle_request(cfg, kind, success).encode_to_vec();
    let opts = CallOptions {
        timeout: Some(cfg.timeout.unwrap_or(Duration::from_secs(15))),
        metadata: cfg.request_metadata_headers(),
    };
    let policy = RetryPolicy {
        max_retries: BES_MAX_RETRIES,
        ..Default::default()
    };
    policy
        .run(|| channel.unary(PUBLISH_LIFECYCLE, &req, &opts))
        .map(|_| ())
}

fn run_uploader(
    cfg: &BesConfig,
    channel: &Channel,
    shared: &Arc<Shared>,
) -> Result<(), BesUploadError> {
    if cfg.lifecycle_events {
        for kind in ["build_enqueued", "invocation_attempt_started"] {
            if shared.cancelled.load(Ordering::SeqCst) {
                return Err(BesUploadError::timeout());
            }
            if let Err(st) = publish_lifecycle_rpc(cfg, channel, kind, true) {
                return Err(BesUploadError::from_grpc(&st));
            }
        }
    }

    let stream_res = run_bidi_stream(cfg, channel, shared);

    // Bazel ALWAYS publishes `invocation_attempt_finished` and `build_finished` after the tool
    // stream finishes, even if the tool stream failed (unless cancelled by close timeout).
    if cfg.lifecycle_events && !shared.cancelled.load(Ordering::SeqCst) {
        let ok = shared.success.load(Ordering::SeqCst);
        let mut lc_err: Option<BesUploadError> = None;
        for kind in ["invocation_attempt_finished", "build_finished"] {
            if let Err(st) = publish_lifecycle_rpc(cfg, channel, kind, ok) {
                if lc_err.is_none() {
                    lc_err = Some(BesUploadError::from_grpc(&st));
                }
            }
        }
        if stream_res.is_ok() {
            if let Some(e) = lc_err {
                return Err(e);
            }
        }
    }

    stream_res
}

fn run_bidi_stream(
    cfg: &BesConfig,
    channel: &Channel,
    shared: &Arc<Shared>,
) -> Result<(), BesUploadError> {
    let mut attempt = 0u32;
    let mut delay = Duration::from_millis(100);
    let req_meta = cfg.request_metadata_headers();

    loop {
        if shared.cancelled.load(Ordering::SeqCst) {
            return Err(BesUploadError::timeout());
        }
        let start_acked = shared.q.lock().unwrap().acked;
        let opts = CallOptions {
            timeout: None,
            metadata: req_meta.clone(),
        };
        let (bidi, deadline) = match channel.open_bidi(PUBLISH_STREAM, &opts) {
            Ok(b) => b,
            Err(st) => {
                if !is_bes_stream_retriable(&st) || attempt >= BES_MAX_RETRIES {
                    return Err(BesUploadError::from_grpc(&st));
                }
                attempt += 1;
                std::thread::sleep(delay);
                delay = Duration::from_secs_f64((delay.as_secs_f64() * 1.6).min(30.0));
                continue;
            }
        };
        *shared.active_bidi.lock().unwrap() = Some(bidi.clone());

        // Spawn concurrent writer for this stream attempt starting at `start_acked`.
        let attempt_done = Arc::new(AtomicBool::new(false));
        let send_done = attempt_done.clone();
        let send_bidi = bidi.clone();
        let send_shared = shared.clone();
        let writer = std::thread::spawn(move || -> Result<i64, GrpcStatus> {
            let mut idx = start_acked as usize;
            loop {
                if send_shared.cancelled.load(Ordering::SeqCst) {
                    send_bidi.cancel();
                    return Err(GrpcStatus::new(code::CANCELLED, "BES upload cancelled"));
                }
                if send_done.load(Ordering::SeqCst) {
                    return Ok(idx as i64);
                }
                let next_msg = {
                    let mut q = send_shared.q.lock().unwrap();
                    loop {
                        if send_shared.cancelled.load(Ordering::SeqCst) {
                            send_bidi.cancel();
                            return Err(GrpcStatus::new(code::CANCELLED, "BES upload cancelled"));
                        }
                        if send_done.load(Ordering::SeqCst) {
                            return Ok(idx as i64);
                        }
                        if idx < q.requests.len() {
                            let bytes = q.requests[idx].clone();
                            idx += 1;
                            break Some(bytes);
                        }
                        if q.closed {
                            break None;
                        }
                        q = send_shared.cv.wait(q).unwrap();
                    }
                };
                match next_msg {
                    Some(bytes) => {
                        send_bidi.send_message(&bytes, false, None)?;
                    }
                    None => {
                        send_bidi.close_send()?;
                        let mut q = send_shared.q.lock().unwrap();
                        q.half_closed = true;
                        send_shared.cv.notify_all();
                        return Ok(idx as i64);
                    }
                }
            }
        });

        let mut expected_ack = start_acked + 1;
        let mut made_progress = false;
        let mut protocol_fatal: Option<BesUploadError> = None;

        let recv_res = bidi.read_responses(deadline, &mut |msg| {
            let r = match Msg::decode(
                "google.devtools.build.v1.PublishBuildToolEventStreamResponse",
                &msg,
            ) {
                Ok(m) => m,
                Err(e) => {
                    bidi.cancel();
                    protocol_fatal = Some(BesUploadError::transient(format!("bad BES ack: {e}")));
                    return Err(GrpcStatus::new(code::INTERNAL, "bad BES ack"));
                }
            };
            let seq = r.get_i64("sequence_number");
            // Strict ack sequence validation: gap, duplicate, or `<= 0` aborts immediately
            // and is NEVER retried!
            if seq != expected_ack {
                bidi.cancel();
                protocol_fatal = Some(BesUploadError::transient(format!(
                    "Expected acknowledgement for sequence_number={expected_ack}, got {seq}"
                )));
                return Err(GrpcStatus::new(code::INTERNAL, "unexpected BES ack"));
            }
            expected_ack += 1;
            made_progress = true;
            {
                let mut q = shared.q.lock().unwrap();
                q.acked = seq;
            }
            Ok(())
        });

        *shared.active_bidi.lock().unwrap() = None;
        // Signal per-attempt completion under `shared.q` before `notify_all` so an idle writer
        // blocked in `cv.wait` wakes immediately without waiting for another BEP event or `close()`.
        {
            let _q = shared.q.lock().unwrap();
            attempt_done.store(true, Ordering::SeqCst);
        }
        shared.cv.notify_all();
        let writer_res = writer.join().unwrap_or(Ok(0));

        if shared.cancelled.load(Ordering::SeqCst) {
            return Err(BesUploadError::timeout());
        }
        if let Some(fatal) = protocol_fatal {
            return Err(fatal);
        }

        let (closed, total_len, acked) = {
            let q = shared.q.lock().unwrap();
            (q.closed, q.requests.len() as i64, q.acked)
        };

        match recv_res {
            Ok(()) => {
                if let Err(wst) = writer_res {
                    if !is_bes_stream_retriable(&wst) || attempt >= BES_MAX_RETRIES {
                        return Err(BesUploadError::from_grpc(&wst));
                    }
                } else if closed && acked == total_len {
                    return Ok(());
                } else {
                    // Clean OK completion with unacknowledged events fails immediately without
                    // retrying (`BuildEventServiceUploader`).
                    return Err(BesUploadError::transient(format!(
                        "BES stream ended OK with unacknowledged events (acked {acked} of {total_len})"
                    )));
                }
            }
            Err(st) => {
                // Non-OK status after all acknowledgements arrived is not retried.
                if closed && acked == total_len {
                    return Err(BesUploadError::from_grpc(&st));
                }
                if made_progress {
                    attempt = 0;
                    delay = Duration::from_millis(100);
                }
                if !is_bes_stream_retriable(&st) || attempt >= BES_MAX_RETRIES {
                    return Err(BesUploadError::from_grpc(&st));
                }
            }
        }

        attempt += 1;
        std::thread::sleep(delay);
        delay = Duration::from_secs_f64((delay.as_secs_f64() * 1.6).min(30.0));
    }
}

fn is_bes_stream_retriable(st: &GrpcStatus) -> bool {
    !matches!(
        st.code,
        code::INVALID_ARGUMENT | code::PERMISSION_DENIED | code::FAILED_PRECONDITION
    )
}

impl BuildEventTransport for BesTransport {
    fn name(&self) -> String {
        "Build Event Service".into()
    }

    fn send(&self, event: &Msg) {
        let uploader = self.artifact_uploader();
        let transformed = if uploader.is_some() {
            let normalized = crate::bep::apply_file_path_conversion(event, true, None);
            rewrite_bep_event(&normalized, true, uploader.as_deref())
        } else {
            crate::bep::apply_file_path_conversion(
                event,
                true,
                self.cfg.remote_bytestream_uri_prefix.as_deref(),
            )
        };
        let mut q = self.shared.q.lock().unwrap();
        if q.closed {
            return;
        }
        let seq = q.requests.len() as i64 + 1;
        let req = stream_request(&self.cfg, seq, Some(&transformed)).encode_to_vec();
        q.requests.push(req);
        self.shared.cv.notify_all();
    }

    fn close(&self) -> Result<(), String> {
        if self.close_called.swap(true, Ordering::SeqCst) {
            return match self.last_error() {
                Some(e) => Err(e.message),
                None => Ok(()),
            };
        }
        {
            let mut q = self.shared.q.lock().unwrap();
            if !q.closed {
                let seq = q.requests.len() as i64 + 1;
                let req = stream_request(&self.cfg, seq, None).encode_to_vec();
                q.requests.push(req);
                q.closed = true;
                self.shared.cv.notify_all();
            }
        }

        match self.cfg.upload_mode {
            BesUploadMode::NoWaitForUploadComplete => {
                let done_rx = self.done_rx.lock().unwrap().take();
                if let Some(h) = self.worker.lock().unwrap().take() {
                    background_uploads().lock().unwrap().push(BackgroundUpload {
                        mode: BesUploadMode::NoWaitForUploadComplete,
                        timeout: self.cfg.timeout,
                        started_close: Instant::now(),
                        shared: self.shared.clone(),
                        done_rx,
                        worker: h,
                    });
                }
                return Ok(());
            }
            BesUploadMode::FullyAsync => {
                // Wait only for half-close or worker termination (bounded by timeout if set),
                // then detach worker for `drain_previous_uploads_ext`.
                let close_started = Instant::now();
                let deadline = self.cfg.timeout.map(|t| close_started + t);
                let mut q = self.shared.q.lock().unwrap();
                while !q.half_closed && !q.finished {
                    if let Some(dl) = deadline {
                        let now = Instant::now();
                        if now >= dl {
                            break;
                        }
                        let (nq, _) = self.shared.cv.wait_timeout(q, dl - now).unwrap();
                        q = nq;
                    } else {
                        q = self.shared.cv.wait(q).unwrap();
                    }
                }
                drop(q);
                let done_rx = self.done_rx.lock().unwrap().take();
                if let Some(h) = self.worker.lock().unwrap().take() {
                    background_uploads().lock().unwrap().push(BackgroundUpload {
                        mode: BesUploadMode::FullyAsync,
                        timeout: self.cfg.timeout,
                        started_close: close_started,
                        shared: self.shared.clone(),
                        done_rx,
                        worker: h,
                    });
                }
                return Ok(());
            }
            BesUploadMode::WaitForUploadComplete => {}
        }

        let rx = self.done_rx.lock().unwrap().take();
        let mut result: Result<(), BesUploadError> = Ok(());
        if let Some(rx) = rx {
            result = match self.cfg.timeout {
                Some(t) => match rx.recv_timeout(t) {
                    Ok(r) => r,
                    Err(_) => {
                        self.shared.cancelled.store(true, Ordering::SeqCst);
                        if let Some(b) = self.shared.active_bidi.lock().unwrap().take() {
                            b.cancel();
                        }
                        self.shared.cv.notify_all();
                        Err(BesUploadError::timeout())
                    }
                },
                None => rx
                    .recv()
                    .unwrap_or_else(|_| Err(BesUploadError::transient("BES upload thread died"))),
            };
        }
        if let Some(h) = self.worker.lock().unwrap().take() {
            let _ = h.join();
        }
        if let Err(ref e) = result {
            *self.last_error.lock().unwrap() = Some(e.clone());
            return Err(e.message.clone());
        }
        Ok(())
    }
}

/// Convenience: build a transport from options, or `None` when `--bes_backend` is unset.
pub fn transport_from_options(
    o: &crate::bep::BepOptions,
    build_id: &str,
    invocation_id: &str,
    command: &str,
) -> Result<Option<(Arc<BesTransport>, BesConfig)>, String> {
    transport_from_options_with_env(
        o,
        build_id,
        invocation_id,
        command,
        &std::collections::BTreeMap::new(),
    )
}

/// Builds a BES transport from options and the invoking command's `client_env` (`$NETRC`/`$HOME`).
pub fn transport_from_options_with_env(
    o: &crate::bep::BepOptions,
    build_id: &str,
    invocation_id: &str,
    command: &str,
    client_env: &std::collections::BTreeMap<String, String>,
) -> Result<Option<(Arc<BesTransport>, BesConfig)>, String> {
    if let Some(err) = &o.bes_upload_mode_error {
        return Err(err.clone());
    }
    let Some(mut cfg) = BesConfig::from_options(o, build_id, invocation_id, command) else {
        return Ok(None);
    };
    cfg.client_env = client_env.clone();
    let ch = cfg.channel()?;
    let t = BesTransport::start(cfg.clone(), ch);
    Ok(Some((t, cfg)))
}

#[allow(unused)]
fn _connector_is_object_safe(_: &dyn Connector) {}
#[allow(unused)]
fn _value_used(_: &Value, _: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bep::{ids, BepBuilder, BuildEventStreamer};
    use std::io;
    use zel_remote::fake_server::FakeReapiServer;
    use zel_remote::grpc::{frame_message, MessageParser};
    use zel_remote::http2::{Conn, Event, LocalSettings, Stream};
    use zel_remote::transport::{memory_pair, Transport};

    /// Minimal in-memory BES server recording every request.
    #[derive(Clone, Default)]
    struct FakeBes {
        stream_reqs: Arc<Mutex<Vec<Vec<u8>>>>,
        lifecycle: Arc<Mutex<Vec<Vec<u8>>>>,
        fail_lifecycle_perm_denied: Arc<Mutex<bool>>,
        fail_first_stream: Arc<Mutex<bool>>,
        fail_stream_perm_denied: Arc<Mutex<bool>>,
        fail_stream_immediately: Arc<Mutex<bool>>,
        bad_ack_seq: Arc<Mutex<Option<i64>>>,
        headers: Arc<Mutex<Vec<(String, String)>>>,
    }

    struct Conn2(FakeBes);

    impl Connector for Conn2 {
        fn connect(&self) -> io::Result<Transport> {
            let (c, s) = memory_pair();
            let me = self.0.clone();
            std::thread::spawn(move || {
                let Ok((_conn, rx)) = Conn::server_from(s, LocalSettings::default()) else {
                    return;
                };
                while let Ok(stream) = rx.recv() {
                    let me = me.clone();
                    std::thread::spawn(move || me.handle(stream));
                }
            });
            Ok(c)
        }
        fn authority(&self) -> String {
            "bes.local".into()
        }
    }

    impl FakeBes {
        fn handle(&self, st: Stream) {
            let mut headers = Vec::new();
            let mut parser = MessageParser::default();
            let mut msgs = Vec::new();
            let reject_early = *self.fail_stream_immediately.lock().unwrap();
            loop {
                match st.recv(None) {
                    Ok(Event::Headers(h)) => {
                        headers = h;
                        let p = headers
                            .iter()
                            .find(|kv| kv.0 == ":path")
                            .map(|kv| kv.1.as_str())
                            .unwrap_or("");
                        if reject_early && p == PUBLISH_STREAM {
                            let _ = st.send_headers(
                                &[
                                    (":status".to_string(), "200".to_string()),
                                    ("content-type".to_string(), "application/grpc".to_string()),
                                    ("grpc-status".to_string(), "7".to_string()),
                                    ("grpc-message".to_string(), "denied early".to_string()),
                                ],
                                true,
                            );
                            return;
                        }
                    }
                    Ok(Event::Data(d)) => {
                        parser.push(&d);
                        while let Ok(Some(m)) = parser.next() {
                            msgs.push(m);
                        }
                    }
                    Ok(Event::End) => break,
                    _ => return,
                }
            }
            *self.headers.lock().unwrap() = headers.clone();
            let path = headers
                .iter()
                .find(|h| h.0 == ":path")
                .map(|h| h.1.clone())
                .unwrap_or_default();
            let ok_headers = [
                (":status".to_string(), "200".to_string()),
                ("content-type".to_string(), "application/grpc".to_string()),
            ];
            if path == PUBLISH_LIFECYCLE {
                if *self.fail_lifecycle_perm_denied.lock().unwrap() {
                    let _ = st.send_headers(
                        &[
                            (":status".to_string(), "200".to_string()),
                            ("content-type".to_string(), "application/grpc".to_string()),
                            ("grpc-status".to_string(), "7".to_string()),
                            ("grpc-message".to_string(), "lifecycle denied".to_string()),
                        ],
                        true,
                    );
                    return;
                }
                self.lifecycle.lock().unwrap().push(msgs[0].clone());
                let _ = st.send_headers(&ok_headers, false);
                let _ = st.send_data(&frame_message(&[]), false, None);
                let _ = st.send_headers(&[("grpc-status".to_string(), "0".to_string())], true);
                return;
            }
            if *self.fail_stream_perm_denied.lock().unwrap() {
                let _ = st.send_headers(
                    &[
                        (":status".to_string(), "200".to_string()),
                        ("content-type".to_string(), "application/grpc".to_string()),
                        ("grpc-status".to_string(), "7".to_string()),
                        ("grpc-message".to_string(), "denied".to_string()),
                    ],
                    true,
                );
                return;
            }
            let fail = std::mem::replace(&mut *self.fail_first_stream.lock().unwrap(), false);
            if fail {
                let _ = st.send_headers(
                    &[
                        (":status".to_string(), "200".to_string()),
                        ("content-type".to_string(), "application/grpc".to_string()),
                        ("grpc-status".to_string(), "14".to_string()),
                        ("grpc-message".to_string(), "unavailable".to_string()),
                    ],
                    true,
                );
                return;
            }
            let _ = st.send_headers(&ok_headers, false);
            let bad_seq = *self.bad_ack_seq.lock().unwrap();
            for (idx, m) in msgs.iter().enumerate() {
                let r = Msg::decode(
                    "google.devtools.build.v1.PublishBuildToolEventStreamRequest",
                    m,
                )
                .unwrap();
                let o = r.get_msg("ordered_build_event").unwrap();
                let seq = if idx == 0 {
                    bad_seq.unwrap_or_else(|| o.get_i64("sequence_number"))
                } else {
                    o.get_i64("sequence_number")
                };
                self.stream_reqs.lock().unwrap().push(m.clone());
                let mut resp =
                    Msg::new("google.devtools.build.v1.PublishBuildToolEventStreamResponse");
                resp.set_int("sequence_number", seq);
                if let Some(sid) = o.get_msg("stream_id") {
                    resp.set_msg("stream_id", sid.clone());
                }
                let _ = st.send_data(&frame_message(&resp.encode_to_vec()), false, None);
            }
            let _ = st.send_headers(&[("grpc-status".to_string(), "0".to_string())], true);
        }
    }

    fn cfg() -> BesConfig {
        BesConfig {
            backend: "grpc://x:1".into(),
            results_url: Some("https://results.example/invocations".into()),
            timeout: Some(Duration::from_secs(10)),
            headers: vec![
                ("x-token".into(), "old".into()),
                ("x-token".into(), "abc".into()),
            ],
            instance_name: "my-proj".into(),
            lifecycle_events: true,
            check_preceding_lifecycle_events: false,
            build_id: "b-1".into(),
            invocation_id: "inv-1".into(),
            command_name: "build".into(),
            system_keywords: vec!["sys=1".into()],
            keywords: vec!["ci".into()],
            ..Default::default()
        }
    }

    fn run_build(srv: &FakeBes) -> (Arc<BesTransport>, Result<(), String>) {
        let channel = Arc::new(Channel::new(
            Arc::new(Conn2(srv.clone())),
            1,
            vec![("x-token".into(), "abc".into())],
        ));
        let t = BesTransport::start(cfg(), channel);
        let streamer = BuildEventStreamer::new(vec![t.clone()], false);
        streamer.post(BepBuilder::build_started(
            "inv-1",
            0,
            "v",
            "",
            "build",
            "/w",
            "/w",
            1,
            "h",
            "u",
            vec![ids::progress(0), ids::build_finished()],
        ));
        streamer.post(BepBuilder::build_finished("SUCCESS", 0, 1, None));
        streamer.post(BepBuilder::build_metrics(Msg::new(
            "build_event_stream.BuildMetrics",
        )));
        streamer.post(BepBuilder::build_tool_logs(&[]));
        streamer.post(BepBuilder::convenience_symlinks(&[]));
        assert!(streamer.is_closed());
        let errs = streamer.close_errors();
        let res = if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join(";"))
        };
        (t, res)
    }

    #[test]
    fn uploads_ordered_events_lifecycle_seq_numbers_project_id_and_keywords() {
        let srv = FakeBes::default();
        let (_t, res) = run_build(&srv);
        res.unwrap();
        let reqs = srv.stream_reqs.lock().unwrap().clone();
        assert!(reqs.len() >= 5, "got {}", reqs.len());
        let decoded: Vec<Msg> = reqs
            .iter()
            .map(|m| {
                Msg::decode(
                    "google.devtools.build.v1.PublishBuildToolEventStreamRequest",
                    m,
                )
                .unwrap()
            })
            .collect();
        for (i, d) in decoded.iter().enumerate() {
            assert_eq!(
                d.get_msg("ordered_build_event")
                    .unwrap()
                    .get_i64("sequence_number"),
                i as i64 + 1
            );
            // project_id from instance_name is present on EVERY tool-stream request!
            assert_eq!(d.get_str("project_id"), "my-proj");
        }
        // Default check_preceding_lifecycle_events is false.
        assert!(!decoded[0].get_bool("check_preceding_lifecycle_events_present"));
        let kws: Vec<String> = decoded[0]
            .values("notification_keywords")
            .iter()
            .filter_map(|v| match v {
                Value::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            kws,
            vec![
                "protocol_name=BEP",
                "command_name=build",
                "sys=1",
                "user_keyword=ci",
            ]
        );
        assert!(decoded
            .last()
            .unwrap()
            .get_msg("ordered_build_event")
            .unwrap()
            .get_msg("event")
            .unwrap()
            .has("component_stream_finished"));

        // Verify lifecycle sequence numbers (1, 1, 2, 2), project_id, no check_preceding_lifecycle,
        // and keywords omitted on invocation_attempt_finished!
        let lc_raw = srv.lifecycle.lock().unwrap().clone();
        assert_eq!(lc_raw.len(), 4);
        let lc: Vec<Msg> = lc_raw
            .iter()
            .map(|b| {
                Msg::decode("google.devtools.build.v1.PublishLifecycleEventRequest", b).unwrap()
            })
            .collect();
        let seqs: Vec<i64> = lc
            .iter()
            .map(|m| m.get_msg("build_event").unwrap().get_i64("sequence_number"))
            .collect();
        assert_eq!(seqs, vec![1, 1, 2, 2]);
        for m in &lc {
            assert_eq!(m.get_str("project_id"), "my-proj");
            assert!(!m.get_bool("check_preceding_lifecycle_events_present"));
        }
        assert!(!lc[0].values("notification_keywords").is_empty());
        assert!(!lc[1].values("notification_keywords").is_empty());
        assert!(
            lc[2].values("notification_keywords").is_empty(),
            "invocation_attempt_finished must omit notification_keywords"
        );
        assert!(!lc[3].values("notification_keywords").is_empty());

        let hdrs = srv.headers.lock().unwrap().clone();
        assert!(hdrs.iter().any(|(k, v)| k == "x-token" && v == "abc"));
        assert!(hdrs.iter().any(|(k, _)| k == REQUEST_METADATA_HEADER));
        // Slash inserted automatically before invocation_id!
        assert_eq!(
            cfg().streaming_message().unwrap(),
            "Streaming build results to: https://results.example/invocations/inv-1"
        );
    }

    #[test]
    fn retries_after_unavailable_and_sends_finish_lifecycle_even_on_stream_failure() {
        let srv = FakeBes::default();
        *srv.fail_first_stream.lock().unwrap() = true;
        let (_t, res) = run_build(&srv);
        res.unwrap();
        assert!(srv.stream_reqs.lock().unwrap().len() >= 5);

        // Permanent stream failure (PERMISSION_DENIED -> Persistent exit 45) still sends finish
        // lifecycle events!
        let srv2 = FakeBes::default();
        *srv2.fail_stream_perm_denied.lock().unwrap() = true;
        let (t2, res2) = run_build(&srv2);
        assert!(res2.is_err());
        assert_eq!(t2.last_error().unwrap().kind, BesErrorKind::Persistent);
        assert_eq!(srv2.lifecycle.lock().unwrap().len(), 4);

        // Out-of-order / negative ack aborts immediately without retry!
        let srv3 = FakeBes::default();
        *srv3.bad_ack_seq.lock().unwrap() = Some(-1);
        let (_t3, res3) = run_build(&srv3);
        assert!(res3.is_err());
    }

    #[test]
    fn tls_channel_and_proxy_validation() {
        let mut c = cfg();
        c.backend = "grpcs://bes.example".into();
        assert!(c.channel().is_ok());

        c.proxy = Some("http://bad-proxy:8080".into());
        let e = c.channel().err().unwrap();
        assert!(e.contains("only unix: proxies"), "{e}");
    }

    #[test]
    fn bytestream_artifact_uploader_all_and_minimal_and_blake3() {
        let server = FakeReapiServer::new();
        let client = Arc::new(server.client(false));
        let tmp = std::env::temp_dir().join(format!("zel-bes-up-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("bazel-out/k8-fastbuild/testlogs/pkg")).unwrap();
        let art = tmp.join("out.bin");
        let tlog = tmp.join("bazel-out/k8-fastbuild/testlogs/pkg/test.log");
        std::fs::write(&art, b"artifact-bytes").unwrap();
        std::fs::write(&tlog, b"test-log-bytes").unwrap();

        let uploader_all = ByteStreamBuildEventArtifactUploader::new(
            Some(client.clone()),
            None,
            DigestFn::Sha256,
            BuildEventUploadMode::All,
            "cas.example:443",
            "main",
            None,
        );
        let (_, nodes) = BepBuilder::named_sets(
            "0",
            vec![BepBuilder::file(
                "out.bin",
                art.to_str().unwrap(),
                &[],
                None,
                None,
            )],
            None,
        );
        let ev = nodes[0].to_proto(false);
        let rewritten = rewrite_bep_event(&ev, true, Some(&uploader_all));
        let f_uri = rewritten
            .get_msg("named_set_of_files")
            .unwrap()
            .get_msgs("files")[0]
            .get_str("uri")
            .to_string();
        let expected_d = DigestFn::Sha256.digest(b"artifact-bytes");
        assert_eq!(
            f_uri,
            format!(
                "bytestream://cas.example:443/main/blobs/{}/{}",
                expected_d.hash, expected_d.size_bytes
            )
        );
        assert!(server.get_blob(&expected_d).is_some());

        // BLAKE3 includes `/blobs/blake3/{hash}/{size}` and MINIMAL skips regular non-testlog CAS
        // upload while still rewriting URI!
        let server2 = FakeReapiServer::new();
        server2.configure(|c| {
            c.advertise_digest_functions = vec![zel_proto::reapi::digest_function::BLAKE3]
        });
        let uploader_min = ByteStreamBuildEventArtifactUploader::new(
            Some(Arc::new(server2.client(false))),
            None,
            DigestFn::Blake3,
            BuildEventUploadMode::Minimal,
            "cas.example:443",
            "",
            Some("custom.prefix/inst"),
        );
        let (_, nodes_min) = BepBuilder::named_sets(
            "1",
            vec![
                BepBuilder::file("out.bin", art.to_str().unwrap(), &[], None, None),
                BepBuilder::file("test.log", tlog.to_str().unwrap(), &[], None, None),
            ],
            None,
        );
        let ev_min = rewrite_bep_event(&nodes_min[0].to_proto(false), true, Some(&uploader_min));
        let files = ev_min
            .get_msg("named_set_of_files")
            .unwrap()
            .get_msgs("files");
        let d_art = DigestFn::Blake3.digest(b"artifact-bytes");
        let d_tlog = DigestFn::Blake3.digest(b"test-log-bytes");
        assert_eq!(
            files[0].get_str("uri"),
            format!(
                "bytestream://custom.prefix/inst/blobs/blake3/{}/{}",
                d_art.hash, d_art.size_bytes
            )
        );
        assert!(
            server2.get_blob(&d_art).is_none(),
            "MINIMAL must not upload regular outputs"
        );
        assert!(
            server2.get_blob(&d_tlog).is_some(),
            "MINIMAL must upload testlogs"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    static ASYNC_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn lifecycle_sequence_project_id_and_bep_special_files() {
        let srv = FakeBes::default();
        let mut c = cfg();
        c.project_id = Some("proj-42".into());
        let channel = Arc::new(Channel::new(Arc::new(Conn2(srv.clone())), 1, vec![]));
        let t = BesTransport::start(c, channel);
        let streamer = BuildEventStreamer::new(vec![t], false);
        streamer.post(BepBuilder::build_started(
            "inv-1",
            0,
            "v",
            "",
            "build",
            "/w",
            "/w",
            1,
            "h",
            "u",
            vec![ids::progress(0), ids::build_finished()],
        ));
        streamer.post(BepBuilder::build_finished("SUCCESS", 0, 1, None));
        streamer.post(BepBuilder::build_metrics(Msg::new(
            "build_event_stream.BuildMetrics",
        )));
        streamer.post(BepBuilder::build_tool_logs(&[]));
        streamer.post(BepBuilder::convenience_symlinks(&[]));
        assert!(streamer.close_errors().is_empty());

        let lc: Vec<Msg> = srv
            .lifecycle
            .lock()
            .unwrap()
            .iter()
            .map(|b| {
                Msg::decode("google.devtools.build.v1.PublishLifecycleEventRequest", b).unwrap()
            })
            .collect();
        assert_eq!(lc.len(), 4);
        assert_eq!(
            lc[0]
                .get_msg("build_event")
                .unwrap()
                .get_i64("sequence_number"),
            1
        );
        assert_eq!(
            lc[2]
                .get_msg("build_event")
                .unwrap()
                .get_i64("sequence_number"),
            2
        );
        assert!(lc[2].get_str("notification_keywords").is_empty());
        assert_eq!(
            lc[3]
                .get_msg("build_event")
                .unwrap()
                .get_i64("sequence_number"),
            2
        );

        let tool_reqs: Vec<Msg> = srv
            .stream_reqs
            .lock()
            .unwrap()
            .iter()
            .map(|b| {
                Msg::decode(
                    "google.devtools.build.v1.PublishBuildToolEventStreamRequest",
                    b,
                )
                .unwrap()
            })
            .collect();
        assert!(tool_reqs
            .iter()
            .all(|m| m.get_str("project_id") == "proj-42"));

        let sym = BepBuilder::symlink_file("out.lnk", &["bin".into()], "../target");
        assert!(!sym.has("uri"));
        assert_eq!(sym.get_str("symlink_target_path"), "../target");

        let dir = BepBuilder::directory_file("tree_dir", &["bin".into()]);
        assert!(!dir.has("uri"));

        let mut exec_info = std::collections::BTreeMap::new();
        exec_info.insert("no-remote-exec".to_string(), String::new());
        assert_eq!(
            crate::exec_log::execution_flags(&exec_info),
            (false, true, true)
        );
        exec_info.insert("local".to_string(), String::new());
        assert_eq!(
            crate::exec_log::execution_flags(&exec_info),
            (false, true, false)
        );
    }

    #[test]
    fn fully_async_close_never_hangs_on_lifecycle_or_open_stream_failure() {
        let _guard = ASYNC_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let run_case = |srv: FakeBes| {
            let channel = Arc::new(Channel::new(Arc::new(Conn2(srv)), 1, vec![]));
            let mut c = cfg();
            c.upload_mode = BesUploadMode::FullyAsync;
            c.timeout = None; // unset `--bes_timeout` (default `0s`)

            let (done_tx, done_rx) = mpsc::channel();
            std::thread::spawn(move || {
                let t = BesTransport::start(c, channel);
                t.send(&BepBuilder::build_finished("SUCCESS", 0, 1, None).to_proto(false));
                let res = t.close();
                drain_previous_uploads();
                let _ = done_tx.send(res);
            });
            let res = done_rx.recv_timeout(Duration::from_secs(3)).expect(
                "fully_async close() must not block forever when worker errors before half-close",
            );
            assert!(
                res.is_ok(),
                "fully_async close() must ignore upload errors: {res:?}"
            );
        };

        // Failure during initial `PublishLifecycleEvent` before `run_bidi_stream` is ever reached.
        let srv_lc = FakeBes::default();
        *srv_lc.fail_lifecycle_perm_denied.lock().unwrap() = true;
        run_case(srv_lc);

        // Failure on `PublishBuildToolEventStream` before the writer completes client half-close.
        let srv_stream = FakeBes::default();
        *srv_stream.fail_stream_immediately.lock().unwrap() = true;
        run_case(srv_stream);
    }

    #[test]
    fn windows_and_unc_file_uris_decode_and_rewrite_to_bytestream() {
        use crate::bep::file_uri_for_os;

        // `FileUriPathConverter` Windows encoding (`C:\Temp\Foo Bar.txt` -> `file:///C:/Temp/Foo%20Bar.txt`)
        // and UNC encoding (`\\server\share\Foo Bar.txt` -> `file://server/share/Foo%20Bar.txt`).
        assert_eq!(
            file_uri_for_os(r"C:\Temp\Foo Bar.txt", true),
            "file:///C:/Temp/Foo%20Bar.txt"
        );
        assert_eq!(
            file_uri_for_os(r"\\server\share\Foo Bar.txt", true),
            "file://server/share/Foo%20Bar.txt"
        );

        // `decode_file_uri_for_os` handles percent-encoded backslashes (`%5C`), forward slashes,
        // `localhost` authority, and UNC authorities.
        let encoded_bs =
            "file:///C:%5CUsers%5CRUNNER~1%5CAppData%5CLocal%5CTemp%5Czel-bes-up-2192%5Cout.bin";
        let forward_slash = "file:///C:/Users/RUNNER~1/AppData/Local/Temp/zel-bes-up-2192/out.bin";
        assert_eq!(
            decode_file_uri_for_os(encoded_bs, true).as_deref(),
            Some(r"C:\Users\RUNNER~1\AppData\Local\Temp\zel-bes-up-2192\out.bin")
        );
        assert_eq!(
            decode_file_uri_for_os(forward_slash, true).as_deref(),
            Some("C:/Users/RUNNER~1/AppData/Local/Temp/zel-bes-up-2192/out.bin")
        );
        assert_eq!(
            decode_file_uri_for_os("file://localhost/C:/Temp/Foo%20Bar.txt", true).as_deref(),
            Some("C:/Temp/Foo Bar.txt")
        );
        assert_eq!(
            decode_file_uri_for_os("file://server/share/Foo%20Bar.txt", true).as_deref(),
            Some(r"\\server\share\Foo Bar.txt")
        );

        // Rewriting BEP `File` messages works across both `file:///C:%5C...` and `file:///C:/...`
        // (and case-insensitive drive letters) even when exercised on a non-Windows host.
        let uploader = ByteStreamBuildEventArtifactUploader::new(
            None,
            None,
            DigestFn::Sha256,
            BuildEventUploadMode::All,
            "cas.example:443",
            "main",
            None,
        );
        let expected_d = DigestFn::Sha256.digest(b"artifact-bytes");
        uploader.register_remote_artifact(
            r"c:\Users\RUNNER~1\AppData\Local\Temp\zel-bes-up-2192\out.bin".to_string(),
            expected_d.clone(),
        );
        let expected_bs = format!(
            "bytestream://cas.example:443/main/blobs/{}/{}",
            expected_d.hash, expected_d.size_bytes
        );

        for candidate_uri in [encoded_bs, forward_slash] {
            let mut f = Msg::new("build_event_stream.File");
            f.set_str("name", "out.bin");
            f.set_str("uri", candidate_uri);
            let (_, nodes) = BepBuilder::named_sets("0", vec![f], None);
            let rewritten = rewrite_bep_event(&nodes[0].to_proto(false), true, Some(&uploader));
            let got = rewritten
                .get_msg("named_set_of_files")
                .unwrap()
                .get_msgs("files")[0]
                .get_str("uri")
                .to_string();
            assert_eq!(got, expected_bs, "failed to rewrite {candidate_uri}");
        }

        // Windows backslash paths also match `MINIMAL` mode testlog/stdio upload patterns.
        assert!(uploader.should_upload_in_minimal(
            r"C:\Users\RUNNER~1\AppData\Local\Temp\zel-bes-up\bazel-out\x64_windows-fastbuild\testlogs\pkg\test.log",
            false
        ));
        assert!(uploader.should_upload_in_minimal(
            r"C:\Users\RUNNER~1\AppData\Local\Temp\zel-bes-up\bazel-out\_tmp\actions\stderr-3",
            false
        ));
        assert!(!uploader.should_upload_in_minimal(
            r"C:\Users\RUNNER~1\AppData\Local\Temp\zel-bes-up\out.bin",
            false
        ));
    }

    #[test]
    fn bes_forwards_netrc_flag_and_client_env_credentials() {
        let tmp = std::env::temp_dir().join(format!("zel-bes-netrc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let flag_netrc = tmp.join("flag.netrc");
        let env_netrc = tmp.join("env.netrc");
        std::fs::write(
            &flag_netrc,
            "machine bes.example login flaguser password flagpass\n",
        )
        .unwrap();
        std::fs::write(
            &env_netrc,
            "machine bes.example login envuser password envpass\n",
        )
        .unwrap();

        let flag_path = flag_netrc.to_string_lossy().into_owned();
        let opts = crate::bep::BepOptions::from_flags(
            &|name| match name {
                "bes_backend" => Some("grpc://bes.example:443".into()),
                "netrc" => Some(flag_path.clone()),
                _ => None,
            },
            &|_| Vec::new(),
        );
        let c = BesConfig::from_options(&opts, "b-1", "inv-1", "build").unwrap();
        assert_eq!(c.netrc.as_deref(), Some(flag_path.as_str()));
        let creds = c
            .build_credentials()
            .unwrap()
            .expect("credentials from --netrc");
        let hdrs = creds
            .get_credentials("https://bes.example:443/google.devtools.build.v1.PublishBuildEvent")
            .unwrap();
        let expected_flag = format!("Basic {}", base64_encode(b"flaguser:flagpass"));
        assert!(
            hdrs.iter()
                .any(|(k, v)| k == "authorization" && v == &expected_flag),
            "unexpected headers: {hdrs:?}"
        );

        // Without `--netrc`, `client_env` (`NETRC`) is honored (`GoogleAuthUtils.newCredentialsFromNetrc`).
        let mut c_env = BesConfig {
            backend: "grpc://bes.example:443".into(),
            ..Default::default()
        };
        c_env
            .client_env
            .insert("NETRC".into(), env_netrc.to_string_lossy().into_owned());
        let creds_env = c_env
            .build_credentials()
            .unwrap()
            .expect("credentials from client_env NETRC");
        let hdrs_env = creds_env
            .get_credentials("https://bes.example:443/google.devtools.build.v1.PublishBuildEvent")
            .unwrap();
        let expected_env = format!("Basic {}", base64_encode(b"envuser:envpass"));
        assert!(
            hdrs_env
                .iter()
                .any(|(k, v)| k == "authorization" && v == &expected_env),
            "unexpected client_env headers: {hdrs_env:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn stream_retry_wakes_idle_writer_between_events_without_waiting_for_close() {
        struct MidBuildFailConn {
            attempts: Arc<Mutex<u32>>,
            second_attempt_event_tx: Mutex<Option<mpsc::Sender<()>>>,
        }

        impl Connector for MidBuildFailConn {
            fn connect(&self) -> io::Result<Transport> {
                let (c, s) = memory_pair();
                let attempts = self.attempts.clone();
                let tx = self.second_attempt_event_tx.lock().unwrap().clone();
                std::thread::spawn(move || {
                    let Ok((_conn, rx)) = Conn::server_from(s, LocalSettings::default()) else {
                        return;
                    };
                    while let Ok(st) = rx.recv() {
                        let attempts = attempts.clone();
                        let tx = tx.clone();
                        std::thread::spawn(move || {
                            let mut parser = MessageParser::default();
                            let mut path = String::new();
                            let ok_headers = [
                                (":status".to_string(), "200".to_string()),
                                ("content-type".to_string(), "application/grpc".to_string()),
                            ];
                            let mut stream_no = 0u32;
                            let mut sent_hdrs = false;
                            loop {
                                match st.recv(None) {
                                    Ok(Event::Headers(h)) => {
                                        path = h
                                            .iter()
                                            .find(|kv| kv.0 == ":path")
                                            .map(|kv| kv.1.clone())
                                            .unwrap_or_default();
                                        if path == PUBLISH_STREAM {
                                            let mut g = attempts.lock().unwrap();
                                            *g += 1;
                                            stream_no = *g;
                                        }
                                    }
                                    Ok(Event::Data(d)) => {
                                        parser.push(&d);
                                        while let Ok(Some(m)) = parser.next() {
                                            if path == PUBLISH_STREAM {
                                                if stream_no == 1 {
                                                    // Close the stream with retriable UNAVAILABLE
                                                    // while the client has NOT half-closed yet and
                                                    // its writer thread is parked in `cv.wait`.
                                                    let _ = st.send_headers(
                                                        &[
                                                            (
                                                                ":status".to_string(),
                                                                "200".to_string(),
                                                            ),
                                                            (
                                                                "content-type".to_string(),
                                                                "application/grpc".to_string(),
                                                            ),
                                                            (
                                                                "grpc-status".to_string(),
                                                                "14".to_string(),
                                                            ),
                                                            (
                                                                "grpc-message".to_string(),
                                                                "mid-build drop".to_string(),
                                                            ),
                                                        ],
                                                        true,
                                                    );
                                                    return;
                                                }
                                                if !sent_hdrs {
                                                    let _ = st.send_headers(&ok_headers, false);
                                                    sent_hdrs = true;
                                                }
                                                if let Some(ref notify) = tx {
                                                    let _ = notify.send(());
                                                }
                                                let r = Msg::decode(
                                                    "google.devtools.build.v1.PublishBuildToolEventStreamRequest",
                                                    &m,
                                                )
                                                .unwrap();
                                                let o = r.get_msg("ordered_build_event").unwrap();
                                                let mut resp = Msg::new(
                                                    "google.devtools.build.v1.PublishBuildToolEventStreamResponse",
                                                );
                                                resp.set_int(
                                                    "sequence_number",
                                                    o.get_i64("sequence_number"),
                                                );
                                                if let Some(sid) = o.get_msg("stream_id") {
                                                    resp.set_msg("stream_id", sid.clone());
                                                }
                                                let _ = st.send_data(
                                                    &frame_message(&resp.encode_to_vec()),
                                                    false,
                                                    None,
                                                );
                                            }
                                        }
                                    }
                                    Ok(Event::End) => {
                                        if path == PUBLISH_LIFECYCLE {
                                            let _ = st.send_headers(&ok_headers, false);
                                            let _ = st.send_data(&frame_message(&[]), false, None);
                                            let _ = st.send_headers(
                                                &[("grpc-status".to_string(), "0".to_string())],
                                                true,
                                            );
                                        } else if sent_hdrs {
                                            let _ = st.send_headers(
                                                &[("grpc-status".to_string(), "0".to_string())],
                                                true,
                                            );
                                        }
                                        return;
                                    }
                                    _ => return,
                                }
                            }
                        });
                    }
                });
                Ok(c)
            }
            fn authority(&self) -> String {
                "bes.local".into()
            }
        }

        let (tx, rx) = mpsc::channel();
        let attempts = Arc::new(Mutex::new(0u32));
        let conn = Arc::new(MidBuildFailConn {
            attempts: attempts.clone(),
            second_attempt_event_tx: Mutex::new(Some(tx)),
        });
        let channel = Arc::new(Channel::new(conn, 1, vec![]));
        let mut c = cfg();
        c.lifecycle_events = false;
        let t = BesTransport::start(c, channel);

        // Send a single mid-build event and do NOT call `close()` yet.
        t.send(
            &BepBuilder::build_started(
                "inv-1",
                0,
                "9",
                "",
                "build",
                "/w",
                "/w",
                1,
                "h",
                "u",
                vec![ids::build_finished()],
            )
            .to_proto(false),
        );

        // Retry must wake the idle writer and re-send event 1 on stream attempt #2 before `close()`.
        rx.recv_timeout(Duration::from_secs(2))
            .expect("BES retry stalled waiting on idle writer between events");
        assert!(
            *attempts.lock().unwrap() >= 2,
            "expected at least 2 stream attempts before close()"
        );

        t.send(&BepBuilder::build_finished("SUCCESS", 0, 1, None).to_proto(false));
        t.close().expect("clean close after mid-build stream retry");
    }

    #[test]
    fn drain_previous_uploads_surfaces_async_upload_errors_and_timeouts_as_warnings() {
        let _guard = ASYNC_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = drain_previous_uploads_ext(true, Duration::from_millis(50));

        // 1. `nowait_for_upload_complete` failing with PERMISSION_DENIED surfaces Bazel's warning
        //    on the next command's drain without failing `close()`.
        let srv_fail = FakeBes::default();
        *srv_fail.fail_stream_perm_denied.lock().unwrap() = true;
        let ch_fail = Arc::new(Channel::new(Arc::new(Conn2(srv_fail)), 1, vec![]));
        let mut c_fail = cfg();
        c_fail.upload_mode = BesUploadMode::NoWaitForUploadComplete;
        let t_fail = BesTransport::start(c_fail, ch_fail);
        t_fail.send(&BepBuilder::build_finished("SUCCESS", 0, 1, None).to_proto(false));
        assert!(t_fail.close().is_ok());

        let drained = drain_previous_uploads_ext(false, Duration::from_secs(3));
        assert!(drained.had_any);
        assert_eq!(drained.warnings.len(), 1, "{:?}", drained.warnings);
        assert!(
            drained.warnings[0].contains(
                "The background upload of the Build Event Protocol for the previous invocation failed with the following exception:"
            ) && drained.warnings[0].contains("Ignoring the failure and starting a new invocation..."),
            "unexpected warning: {}",
            drained.warnings[0]
        );

        // 2. `fully_async` failing during initial lifecycle RPC also records the failure for the
        //    next command's drain.
        let srv_async = FakeBes::default();
        *srv_async.fail_lifecycle_perm_denied.lock().unwrap() = true;
        let ch_async = Arc::new(Channel::new(Arc::new(Conn2(srv_async)), 1, vec![]));
        let mut c_async = cfg();
        c_async.upload_mode = BesUploadMode::FullyAsync;
        c_async.timeout = None;
        let t_async = BesTransport::start(c_async, ch_async);
        t_async.send(&BepBuilder::build_finished("SUCCESS", 0, 1, None).to_proto(false));
        assert!(t_async.close().is_ok());
        // Give the detached worker a moment to send its error to `done_rx`.
        std::thread::sleep(Duration::from_millis(30));

        let drained_async = drain_previous_uploads_ext(false, Duration::from_secs(3));
        assert!(drained_async.had_any);
        assert_eq!(
            drained_async.warnings.len(),
            1,
            "{:?}",
            drained_async.warnings
        );
        assert!(
            drained_async.warnings[0].contains("lifecycle denied"),
            "unexpected warning: {}",
            drained_async.warnings[0]
        );
    }
}
