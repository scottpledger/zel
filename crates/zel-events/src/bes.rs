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

//! Build Event Service upload (`--bes_backend`): a [`BuildEventTransport`] publishing BEP events
//! over `google.devtools.build.v1.PublishBuildEvent/PublishBuildToolEventStream` using the
//! HTTP/2 + gRPC channel from `zel-remote`.
//!
//! Lifecycle events (`BuildEnqueued`, `InvocationAttemptStarted/Finished`, `BuildFinished`) are
//! sent with `PublishLifecycleEvent`. The tool stream is retried from the first unacknowledged
//! event on retriable failures. Only plaintext (`grpc://`, `unix:`) backends are supported: the
//! workspace has no TLS implementation and may not use external crates.

use crate::bep::BuildEventTransport;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use zel_proto::dynamic::Msg;
use zel_remote::grpc::{CallOptions, Channel, GrpcStatus, RetryPolicy};
use zel_remote::options::Endpoint;
use zel_remote::transport::Connector;

pub const PUBLISH_LIFECYCLE: &str = "/google.devtools.build.v1.PublishBuildEvent/PublishLifecycleEvent";
pub const PUBLISH_STREAM: &str = "/google.devtools.build.v1.PublishBuildEvent/PublishBuildToolEventStream";
const BEP_TYPE_URL: &str = "type.googleapis.com/build_event_stream.BuildEvent";
const BES_MAX_RETRIES: u32 = 4;

/// Parsed `--bes_*` configuration.
#[derive(Debug, Clone, Default)]
pub struct BesConfig {
    pub backend: String,
    pub results_url: Option<String>,
    pub timeout: Option<Duration>,
    pub headers: Vec<(String, String)>,
    pub instance_name: String,
    pub keywords: Vec<String>,
    pub project_id: Option<String>,
    pub lifecycle_events: bool,
    pub build_id: String,
    pub invocation_id: String,
    pub command_name: String,
}

impl BesConfig {
    pub fn from_options(o: &crate::bep::BepOptions, build_id: &str, invocation_id: &str, command: &str) -> Option<Self> {
        Some(Self {
            backend: o.bes_backend.clone()?,
            results_url: o.bes_results_url.clone(),
            timeout: o.bes_timeout_secs.filter(|s| *s > 0).map(Duration::from_secs),
            headers: o.bes_headers.clone(),
            instance_name: o.bes_instance_name.clone(),
            keywords: o.bes_keywords.clone(),
            project_id: o.project_id.clone(),
            lifecycle_events: o.bes_lifecycle_events,
            build_id: build_id.to_string(),
            invocation_id: invocation_id.to_string(),
            command_name: command.to_string(),
        })
    }

    /// `INFO: Streaming build results to: <url>/<invocation id>` line, when a results URL is set.
    pub fn streaming_message(&self) -> Option<String> {
        self.results_url
            .as_ref()
            .map(|u| format!("Streaming build results to: {}{}", u, self.invocation_id))
    }

    /// Normalized keywords: `command_name=<cmd>`, `protocol_name=BEP`, plus user keywords
    /// (`--bes_keywords`) and the project id (as `project_id`).
    pub fn all_keywords(&self) -> Vec<String> {
        let mut k = vec![format!("command_name={}", self.command_name), "protocol_name=BEP".to_string()];
        for u in &self.keywords {
            let u = format!("user_keyword={u}");
            if !k.contains(&u) {
                k.push(u);
            }
        }
        k
    }

    /// Opens a channel for the backend. TLS endpoints return an explanatory error.
    pub fn channel(&self) -> Result<Arc<Channel>, String> {
        let ep = Endpoint::parse(&self.backend, false)?;
        let conn = ep.connector(self.timeout.unwrap_or(Duration::from_secs(30)))?;
        Ok(Arc::new(Channel::new(conn, 1, self.headers.clone())))
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
    match kind {
        "build_enqueued" => {
            ev.set_msg("build_enqueued", Msg::new("google.devtools.build.v1.BuildEvent.BuildEnqueued"));
        }
        "invocation_attempt_started" => {
            let mut s = Msg::new("google.devtools.build.v1.BuildEvent.InvocationAttemptStarted");
            s.set_int("attempt_number", 1);
            ev.set_msg("invocation_attempt_started", s);
        }
        "invocation_attempt_finished" => {
            let mut f = Msg::new("google.devtools.build.v1.BuildEvent.InvocationAttemptFinished");
            let mut st = Msg::new("google.devtools.build.v1.BuildStatus");
            st.set_enum("result", if success { "COMMAND_SUCCEEDED" } else { "COMMAND_FAILED" });
            f.set_msg("invocation_status", st);
            ev.set_msg("invocation_attempt_finished", f);
        }
        _ => {
            let mut f = Msg::new("google.devtools.build.v1.BuildEvent.BuildFinished");
            let mut st = Msg::new("google.devtools.build.v1.BuildStatus");
            st.set_enum("result", if success { "COMMAND_SUCCEEDED" } else { "COMMAND_FAILED" });
            f.set_msg("status", st);
            ev.set_msg("build_finished", f);
        }
    }
    let with_inv = kind.starts_with("invocation");
    let o = ordered(cfg, "CONTROLLER", with_inv, 1, ev);
    let mut r = Msg::new("google.devtools.build.v1.PublishLifecycleEventRequest");
    r.set_enum("service_level", "INTERACTIVE");
    r.set_msg("build_event", o);
    for k in cfg.all_keywords() {
        r.set_str("notification_keywords", k);
    }
    if let Some(p) = &cfg.project_id {
        r.set_str("project_id", p.clone());
    }
    r.set_bool("check_preceding_lifecycle_events_present", true);
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
    if seq == 1 {
        for k in cfg.all_keywords() {
            r.set_str("notification_keywords", k);
        }
        if let Some(p) = &cfg.project_id {
            r.set_str("project_id", p.clone());
        }
        r.set_bool("check_preceding_lifecycle_events_present", true);
    }
    r
}

#[derive(Default)]
struct Queue {
    /// Serialized requests; index = sequence number − 1.
    requests: Vec<Vec<u8>>,
    closed: bool,
    acked: i64,
}

struct Shared {
    q: Mutex<Queue>,
    cv: Condvar,
}

/// BES transport. Events are queued and streamed by a background thread.
pub struct BesTransport {
    cfg: BesConfig,
    shared: Arc<Shared>,
    done_rx: Mutex<Option<mpsc::Receiver<Result<(), String>>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    channel: Arc<Channel>,
    started: Mutex<bool>,
    success: Mutex<bool>,
}

impl BesTransport {
    /// Connects and starts the upload thread. Lifecycle events are published immediately.
    pub fn start(cfg: BesConfig, channel: Arc<Channel>) -> Arc<Self> {
        let shared = Arc::new(Shared { q: Mutex::new(Queue::default()), cv: Condvar::new() });
        let (tx, rx) = mpsc::channel();
        let t = Arc::new(Self {
            cfg: cfg.clone(),
            shared: shared.clone(),
            done_rx: Mutex::new(Some(rx)),
            worker: Mutex::new(None),
            channel: channel.clone(),
            started: Mutex::new(false),
            success: Mutex::new(true),
        });
        if cfg.lifecycle_events {
            for kind in ["build_enqueued", "invocation_attempt_started"] {
                let _ = t.publish_lifecycle(kind, true);
            }
        }
        let worker_cfg = cfg;
        let h = std::thread::Builder::new()
            .name("bes-upload".into())
            .spawn(move || {
                let r = run_stream(&worker_cfg, &channel, &shared);
                let _ = tx.send(r);
            })
            .ok();
        *t.worker.lock().unwrap() = h;
        t
    }

    fn publish_lifecycle(&self, kind: &str, success: bool) -> Result<(), GrpcStatus> {
        let req = lifecycle_request(&self.cfg, kind, success).encode_to_vec();
        let opts = CallOptions { timeout: self.cfg.timeout, metadata: vec![] };
        let policy = RetryPolicy { max_retries: BES_MAX_RETRIES, ..Default::default() };
        policy.run(|| self.channel.unary(PUBLISH_LIFECYCLE, &req, &opts)).map(|_| ())
    }

    /// Declares whether the invocation succeeded (used for the lifecycle finish events).
    pub fn set_success(&self, ok: bool) {
        *self.success.lock().unwrap() = ok;
    }
}

fn run_stream(cfg: &BesConfig, channel: &Channel, shared: &Shared) -> Result<(), String> {
    let mut attempt = 0u32;
    let mut delay = Duration::from_millis(100);
    loop {
        let start_idx = {
            let q = shared.q.lock().unwrap();
            q.acked as usize
        };
        let mut next = start_idx;
        let mut iter = std::iter::from_fn(|| {
            let mut q = shared.q.lock().unwrap();
            loop {
                if next < q.requests.len() {
                    let r = q.requests[next].clone();
                    next += 1;
                    return Some(r);
                }
                if q.closed {
                    return None;
                }
                q = shared.cv.wait(q).unwrap();
            }
        });
        let mut got_final = false;
        let opts = CallOptions { timeout: None, metadata: vec![] };
        let res = channel.call(PUBLISH_STREAM, &mut iter, &opts, &mut |msg| {
            let r = Msg::decode("google.devtools.build.v1.PublishBuildToolEventStreamResponse", &msg)
                .map_err(|e| GrpcStatus::new(13, format!("bad BES ack: {e}")))?;
            let seq = r.get_i64("sequence_number");
            let mut q = shared.q.lock().unwrap();
            if seq > q.acked {
                q.acked = seq;
            }
            if q.closed && q.acked as usize >= q.requests.len() {
                got_final = true;
            }
            Ok(())
        });
        let q = shared.q.lock().unwrap();
        let all_acked = q.closed && q.acked as usize >= q.requests.len();
        drop(q);
        match res {
            Ok(()) if all_acked => return Ok(()),
            Ok(()) => {
                // Stream ended OK but acknowledgements are missing: resend.
            }
            Err(e) => {
                let retriable = RetryPolicy::is_retriable(&e);
                if !retriable || attempt >= BES_MAX_RETRIES {
                    return Err(format!("{e}"));
                }
            }
        }
        let _ = got_final;
        attempt += 1;
        if attempt > BES_MAX_RETRIES {
            return Err("BES upload failed: not all events were acknowledged".to_string());
        }
        std::thread::sleep(delay);
        delay *= 2;
        let _ = cfg;
    }
}

impl BuildEventTransport for BesTransport {
    fn name(&self) -> String {
        "BuildEventServiceTransport".into()
    }

    fn send(&self, event: &Msg) {
        let mut q = self.shared.q.lock().unwrap();
        if q.closed {
            return;
        }
        let seq = q.requests.len() as i64 + 1;
        let req = stream_request(&self.cfg, seq, Some(event)).encode_to_vec();
        q.requests.push(req);
        *self.started.lock().unwrap() = true;
        self.shared.cv.notify_all();
    }

    fn close(&self) -> Result<(), String> {
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
        let rx = self.done_rx.lock().unwrap().take();
        let mut result = Ok(());
        if let Some(rx) = rx {
            let deadline = self.cfg.timeout.map(|t| Instant::now() + t);
            let r = match deadline {
                Some(d) => rx.recv_timeout(d.saturating_duration_since(Instant::now())).map_err(|_| {
                    "Build Event Service upload timed out (--bes_timeout)".to_string()
                }),
                None => rx.recv().map_err(|_| "BES upload thread died".to_string()),
            };
            result = r.and_then(|x| x);
        }
        if result.is_ok() && self.cfg.lifecycle_events {
            let ok = *self.success.lock().unwrap();
            for kind in ["invocation_attempt_finished", "build_finished"] {
                if let Err(e) = self.publish_lifecycle(kind, ok) {
                    result = Err(format!("{e}"));
                    break;
                }
            }
        }
        if let Some(h) = self.worker.lock().unwrap().take() {
            if result.is_ok() {
                let _ = h.join();
            }
        }
        result
    }
}

/// Convenience: build a transport from options, or `None` when `--bes_backend` is unset.
pub fn transport_from_options(
    o: &crate::bep::BepOptions,
    build_id: &str,
    invocation_id: &str,
    command: &str,
) -> Result<Option<(Arc<BesTransport>, BesConfig)>, String> {
    let Some(cfg) = BesConfig::from_options(o, build_id, invocation_id, command) else {
        return Ok(None);
    };
    let ch = cfg.channel()?;
    let t = BesTransport::start(cfg.clone(), ch);
    Ok(Some((t, cfg)))
}

#[allow(unused)]
fn _connector_is_object_safe(_: &dyn Connector) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bep::{ids, BepBuilder, BuildEventStreamer};
    use std::io;
    use zel_remote::grpc::{frame_message, MessageParser};
    use zel_remote::http2::{Conn, Event, LocalSettings, Stream};
    use zel_remote::transport::{memory_pair, Transport};

    /// Minimal in-memory BES server recording every request.
    #[derive(Clone, Default)]
    struct FakeBes {
        stream_reqs: Arc<Mutex<Vec<Vec<u8>>>>,
        lifecycle: Arc<Mutex<Vec<Vec<u8>>>>,
        fail_first_stream: Arc<Mutex<bool>>,
        headers: Arc<Mutex<Vec<(String, String)>>>,
    }

    struct Conn2(FakeBes);

    impl Connector for Conn2 {
        fn connect(&self) -> io::Result<Transport> {
            let (c, s) = memory_pair();
            let me = self.0.clone();
            std::thread::spawn(move || {
                let Ok((_conn, rx)) = Conn::server_from(s, LocalSettings::default()) else { return };
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
            loop {
                match st.recv(None) {
                    Ok(Event::Headers(h)) => headers = h,
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
            let path = headers.iter().find(|h| h.0 == ":path").map(|h| h.1.clone()).unwrap_or_default();
            let ok_headers = [(":status".to_string(), "200".to_string()), ("content-type".to_string(), "application/grpc".to_string())];
            if path == PUBLISH_LIFECYCLE {
                self.lifecycle.lock().unwrap().push(msgs[0].clone());
                let _ = st.send_headers(&ok_headers, false);
                let _ = st.send_data(&frame_message(&[]), false, None);
                let _ = st.send_headers(&[("grpc-status".to_string(), "0".to_string())], true);
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
            for m in &msgs {
                let r = Msg::decode("google.devtools.build.v1.PublishBuildToolEventStreamRequest", m).unwrap();
                let o = r.get_msg("ordered_build_event").unwrap();
                let seq = o.get_i64("sequence_number");
                self.stream_reqs.lock().unwrap().push(m.clone());
                let mut resp = Msg::new("google.devtools.build.v1.PublishBuildToolEventStreamResponse");
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
            results_url: Some("https://results.example/invocations/".into()),
            timeout: Some(Duration::from_secs(10)),
            headers: vec![("x-token".into(), "abc".into())],
            lifecycle_events: true,
            build_id: "b-1".into(),
            invocation_id: "inv-1".into(),
            command_name: "build".into(),
            keywords: vec!["ci".into()],
            ..Default::default()
        }
    }

    fn run_build(srv: &FakeBes) -> Result<(), String> {
        let channel = Arc::new(Channel::new(Arc::new(Conn2(srv.clone())), 1, vec![("x-token".into(), "abc".into())]));
        let t = BesTransport::start(cfg(), channel);
        let streamer = BuildEventStreamer::new(vec![t.clone()], false);
        streamer.post(BepBuilder::build_started("inv-1", 0, "v", "", "build", "/w", "/w", 1, "h", "u", vec![ids::progress(0), ids::build_finished()]));
        streamer.post(BepBuilder::build_finished("SUCCESS", 0, 1, None));
        streamer.post(BepBuilder::build_metrics(Msg::new("build_event_stream.BuildMetrics")));
        streamer.post(BepBuilder::build_tool_logs(&[]));
        streamer.post(BepBuilder::convenience_symlinks(&[]));
        assert!(streamer.is_closed());
        let errs = streamer.close_errors();
        if errs.is_empty() { Ok(()) } else { Err(errs.join(";")) }
    }

    #[test]
    fn uploads_ordered_events_and_lifecycle() {
        let srv = FakeBes::default();
        run_build(&srv).unwrap();
        let reqs = srv.stream_reqs.lock().unwrap().clone();
        assert!(reqs.len() >= 5, "got {}", reqs.len());
        // Sequence numbers are 1..=n and the last event is ComponentStreamFinished.
        let decoded: Vec<Msg> = reqs
            .iter()
            .map(|m| Msg::decode("google.devtools.build.v1.PublishBuildToolEventStreamRequest", m).unwrap())
            .collect();
        for (i, d) in decoded.iter().enumerate() {
            assert_eq!(d.get_msg("ordered_build_event").unwrap().get_i64("sequence_number"), i as i64 + 1);
        }
        let first = decoded[0].get_msg("ordered_build_event").unwrap();
        assert_eq!(first.get_msg("stream_id").unwrap().get_str("invocation_id"), "inv-1");
        assert_eq!(first.get_msg("stream_id").unwrap().get_enum_name("component"), "TOOL");
        let any = first.get_msg("event").unwrap().get_msg("bazel_event").unwrap();
        assert_eq!(any.get_str("type_url"), BEP_TYPE_URL);
        let inner = Msg::decode("build_event_stream.BuildEvent", any.get_bytes("value")).unwrap();
        assert!(inner.get_msg("id").unwrap().has("started"));
        assert!(decoded[0].get_str("notification_keywords") == "command_name=build");
        assert!(decoded.last().unwrap().get_msg("ordered_build_event").unwrap().get_msg("event").unwrap().has("component_stream_finished"));
        let lc = srv.lifecycle.lock().unwrap().len();
        assert_eq!(lc, 4, "enqueued, attempt started, attempt finished, build finished");
        assert!(srv.headers.lock().unwrap().iter().any(|(k, v)| k == "x-token" && v == "abc"));
        assert_eq!(cfg().streaming_message().unwrap(), "Streaming build results to: https://results.example/invocations/inv-1");
    }

    #[test]
    fn retries_after_unavailable() {
        let srv = FakeBes::default();
        *srv.fail_first_stream.lock().unwrap() = true;
        run_build(&srv).unwrap();
        assert!(srv.stream_reqs.lock().unwrap().len() >= 5);
    }

    #[test]
    fn tls_endpoint_reports_clear_error() {
        let mut c = cfg();
        c.backend = "grpcs://bes.example".into();
        let e = c.channel().err().unwrap();
        assert!(e.contains("TLS"), "{e}");
    }
}
