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

//! Bazel Event reporting (`Event`, `EventKind`, `Reporter`, `EventBus`, and Build Event Protocol).
//! Mirrors `src/main/java/com/google/devtools/build/lib/events` and `buildeventstream`.

pub mod bep;
pub mod bes;
pub mod exec_log;
pub mod profiler;
pub mod ui;

use bep::{ids, BepBuilder, BuildEventNode, BuildEventStreamer, BuildEventTransport, MemoryTransport};
use std::fmt;
use std::io;
use std::sync::{Arc, Mutex};
use zel_proto::dynamic::Msg;
use zel_vfs::{FileSystem, PathFragment};

/// `EventKind.java`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    Fatal,
    Error,
    Warning,
    Info,
    Debug,
    Progress,
    Start,
    Finish,
    Subcommand,
    Stdout,
    Stderr,
    Pass,
    Fail,
    Timeout,
    Cancelled,
    Depchecker,
}

impl EventKind {
    /// `EventKind.ALL_EVENTS`.
    pub const ALL: [EventKind; 16] = [
        Self::Fatal,
        Self::Error,
        Self::Warning,
        Self::Info,
        Self::Debug,
        Self::Progress,
        Self::Start,
        Self::Finish,
        Self::Subcommand,
        Self::Stdout,
        Self::Stderr,
        Self::Pass,
        Self::Fail,
        Self::Timeout,
        Self::Cancelled,
        Self::Depchecker,
    ];

    /// Upper-case enum name (what `toString()` prints before `": "`).
    pub fn name(&self) -> &'static str {
        match self {
            Self::Fatal => "FATAL",
            Self::Error => "ERROR",
            Self::Warning => "WARNING",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
            Self::Progress => "PROGRESS",
            Self::Start => "START",
            Self::Finish => "FINISH",
            Self::Subcommand => "SUBCOMMAND",
            Self::Stdout => "STDOUT",
            Self::Stderr => "STDERR",
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Timeout => "TIMEOUT",
            Self::Cancelled => "CANCELLED",
            Self::Depchecker => "DEPCHECKER",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        let u = s.to_ascii_uppercase();
        Self::ALL.into_iter().find(|k| k.name() == u)
    }

    /// Console prefix including the trailing `": "` (empty for stdout/stderr).
    pub fn prefix(&self) -> String {
        match self {
            Self::Stdout | Self::Stderr => String::new(),
            k => format!("{}: ", k.name()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub file: String,
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.column)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub kind: EventKind,
    pub location: Option<Location>,
    pub message: String,
    pub tag: Option<String>,
}

impl fmt::Display for Event {
    /// `Event.toString()`: `KIND: location: message`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.format_console_line())
    }
}

impl Event {
    fn of(kind: EventKind, location: Option<Location>, message: impl Into<String>) -> Self {
        Self {
            kind,
            location,
            message: message.into(),
            tag: None,
        }
    }

    pub fn error(location: Option<Location>, message: impl Into<String>) -> Self {
        Self::of(EventKind::Error, location, message)
    }

    pub fn warning(location: Option<Location>, message: impl Into<String>) -> Self {
        Self::of(EventKind::Warning, location, message)
    }

    pub fn info(message: impl Into<String>) -> Self {
        Self::of(EventKind::Info, None, message)
    }

    pub fn debug(message: impl Into<String>) -> Self {
        Self::of(EventKind::Debug, None, message)
    }

    pub fn fail(location: Option<Location>, message: impl Into<String>) -> Self {
        Self::of(EventKind::Fail, location, message)
    }

    pub fn pass(message: impl Into<String>) -> Self {
        Self::of(EventKind::Pass, None, message)
    }

    pub fn progress(message: impl Into<String>) -> Self {
        Self::of(EventKind::Progress, None, message)
    }

    pub fn subcommand(message: impl Into<String>) -> Self {
        Self::of(EventKind::Subcommand, None, message)
    }

    pub fn stdout(message: impl Into<String>) -> Self {
        Self::of(EventKind::Stdout, None, message)
    }

    pub fn stderr(message: impl Into<String>) -> Self {
        Self::of(EventKind::Stderr, None, message)
    }

    pub fn format_console_line(&self) -> String {
        match &self.location {
            Some(loc) => format!("{}{}: {}", self.kind.prefix(), loc, self.message),
            None => format!("{}{}", self.kind.prefix(), self.message),
        }
    }
}

/// What an executed action looked like (a plain-data mirror of the analysis action, so this crate
/// does not depend on the analysis crate).
#[derive(Debug, Clone, Default)]
pub struct ActionInfo {
    /// Primary output exec path (the action's identity in the UI and in BEP).
    pub id: String,
    pub owner: String,
    pub mnemonic: String,
    pub progress_message: String,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    /// `(exec path, is_source)`.
    pub inputs: Vec<(String, bool)>,
    pub outputs: Vec<String>,
    pub is_spawn: bool,
}

/// Outcome of an executed action.
#[derive(Debug, Clone, Default)]
pub struct ActionResultInfo {
    pub cache_hit: bool,
    pub success: bool,
    pub exit_code: i32,
    pub runner: Option<String>,
    pub wall_time_ms: u64,
    pub start_millis: i64,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub error: Option<String>,
}

/// Observes action execution (UI progress, BEP `ActionExecuted`, execution logs, profiling).
pub trait ActionListener: Send + Sync {
    fn action_started(&self, _action: &ActionInfo) {}
    fn action_finished(&self, _action: &ActionInfo, _result: &ActionResultInfo) {}
}

/// Receives events as they are reported (`EventHandler`).
pub trait EventHandler: Send + Sync {
    fn handle(&self, event: &Event);
    /// Flushes any buffered output (partial lines, progress bar).
    fn flush(&self) {}
}

struct BepState {
    streamer: Option<Arc<BuildEventStreamer>>,
    history: Arc<MemoryTransport>,
    transports: Vec<Arc<dyn BuildEventTransport>>,
    publish_all_actions: bool,
}

/// Collects events, dispatches them to handlers while the build progresses, and owns the
/// Build Event Protocol stream.
pub struct Reporter {
    events: Mutex<Vec<Event>>,
    handlers: Mutex<Vec<Arc<dyn EventHandler>>>,
    bep: Mutex<BepState>,
    invocation_id: Mutex<String>,
    config_id: Mutex<String>,
    deferred_finish: std::sync::atomic::AtomicBool,
    deferred_exit: Mutex<Option<(String, i32)>>,
    action_listeners: Mutex<Vec<Arc<dyn ActionListener>>>,
}

impl Default for Reporter {
    fn default() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            handlers: Mutex::new(Vec::new()),
            bep: Mutex::new(BepState {
                streamer: None,
                history: Arc::new(MemoryTransport::default()),
                transports: Vec::new(),
                publish_all_actions: false,
            }),
            invocation_id: Mutex::new("cmd-0".to_string()),
            config_id: Mutex::new("none".to_string()),
            deferred_finish: std::sync::atomic::AtomicBool::new(false),
            deferred_exit: Mutex::new(None),
            action_listeners: Mutex::new(Vec::new()),
        }
    }
}

/// Value of `build_tool_version` in `BuildStarted`.
pub const BUILD_TOOL_VERSION: &str = zel_util::BAZEL_COMPAT_RELEASE_NAME;

impl fmt::Debug for Reporter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reporter").finish_non_exhaustive()
    }
}

impl Reporter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an action execution listener.
    pub fn add_action_listener(&self, l: Arc<dyn ActionListener>) {
        self.action_listeners.lock().unwrap().push(l);
    }

    pub fn has_action_listeners(&self) -> bool {
        !self.action_listeners.lock().unwrap().is_empty()
    }

    pub fn action_started(&self, a: &ActionInfo) {
        let ls: Vec<_> = self.action_listeners.lock().unwrap().clone();
        for l in ls {
            l.action_started(a);
        }
    }

    pub fn action_finished(&self, a: &ActionInfo, r: &ActionResultInfo) {
        let ls: Vec<_> = self.action_listeners.lock().unwrap().clone();
        for l in ls {
            l.action_finished(a, r);
        }
    }

    /// Sets the invocation id (`BuildStarted.uuid`, BES `invocation_id`).
    pub fn set_invocation_id(&self, id: &str) {
        *self.invocation_id.lock().unwrap() = id.to_string();
    }

    pub fn invocation_id(&self) -> String {
        self.invocation_id.lock().unwrap().clone()
    }

    /// When set, [`BepEvent::Finished`] only records the exit code and the BEP file writers are
    /// no-ops: the owner finalizes the stream (see `BuildObserver::finish` in `zel-server`).
    pub fn set_deferred_finish(&self, on: bool) {
        self.deferred_finish.store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// Exit code recorded by a deferred [`BepEvent::Finished`], if any.
    pub fn take_deferred_exit(&self) -> Option<(String, i32)> {
        self.deferred_exit.lock().unwrap().take()
    }

    fn is_deferred(&self) -> bool {
        self.deferred_finish.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Configuration id used by the coarse [`BepEvent`] variants (`ConfigurationId.id`).
    pub fn set_default_config_id(&self, id: &str) {
        *self.config_id.lock().unwrap() = id.to_string();
    }

    pub fn default_config_id(&self) -> String {
        self.config_id.lock().unwrap().clone()
    }

    /// Registers a handler that receives every subsequently reported event immediately.
    pub fn add_handler(&self, h: Arc<dyn EventHandler>) {
        self.handlers.lock().unwrap().push(h);
    }

    /// Reports an event: records it and streams it to all handlers right away.
    pub fn handle(&self, event: Event) {
        let handlers: Vec<Arc<dyn EventHandler>> = self.handlers.lock().unwrap().clone();
        for h in &handlers {
            h.handle(&event);
        }
        if event.kind == EventKind::Stdout || event.kind == EventKind::Stderr {
            if let Some(s) = self.bep.lock().unwrap().streamer.clone() {
                if event.kind == EventKind::Stdout {
                    s.report_stdout(&event.message);
                } else {
                    s.report_stderr(&event.message);
                }
            }
        } else if matches!(
            event.kind,
            EventKind::Error | EventKind::Warning | EventKind::Info | EventKind::Debug | EventKind::Fail | EventKind::Subcommand | EventKind::Cancelled | EventKind::Fatal
        ) {
            // The terminal text of these events also rides on BEP progress events.
            if let Some(s) = self.bep.lock().unwrap().streamer.clone() {
                let mut line = event.format_console_line();
                if !line.ends_with('\n') {
                    line.push('\n');
                }
                s.report_stderr(&line);
            }
        }
        self.events.lock().unwrap().push(event);
    }

    pub fn flush_handlers(&self) {
        let handlers: Vec<Arc<dyn EventHandler>> = self.handlers.lock().unwrap().clone();
        for h in &handlers {
            h.flush();
        }
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    pub fn has_errors(&self) -> bool {
        self.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.kind == EventKind::Error)
    }

    // ------------------------------------------------------------------ BEP

    /// Adds a BEP transport (must be called before the first BEP event).
    pub fn add_bep_transport(&self, t: Arc<dyn BuildEventTransport>) {
        self.bep.lock().unwrap().transports.push(t);
    }

    /// Opens `--build_event_{json,binary,text}_file` transports; returns error messages for the
    /// files that could not be opened.
    pub fn configure_bep(&self, opts: &bep::BepOptions) -> Vec<String> {
        let mut errors = Vec::new();
        let mut st = self.bep.lock().unwrap();
        st.publish_all_actions = opts.publish_all_actions;
        for r in opts.open_file_transports() {
            match r {
                Ok(t) => st.transports.push(t),
                Err(e) => errors.push(e),
            }
        }
        errors
    }

    /// The BEP streamer, created on first use with the in-memory history transport and all
    /// configured transports.
    pub fn bep_streamer(&self) -> Arc<BuildEventStreamer> {
        let mut st = self.bep.lock().unwrap();
        if let Some(s) = &st.streamer {
            return s.clone();
        }
        let mut ts: Vec<Arc<dyn BuildEventTransport>> = vec![st.history.clone()];
        ts.extend(st.transports.iter().cloned());
        let s = Arc::new(BuildEventStreamer::new(ts, st.publish_all_actions));
        st.streamer = Some(s.clone());
        s
    }

    /// Posts a fully built BEP node.
    pub fn post_bep_node(&self, node: BuildEventNode) {
        self.bep_streamer().post(node);
    }

    /// Compatibility entry point mapping the coarse [`BepEvent`] variants onto real
    /// `build_event_stream.BuildEvent` protos.
    pub fn post_bep(&self, bep: BepEvent) {
        let s = self.bep_streamer();
        let cfg = self.default_config_id();
        match bep {
            BepEvent::Started {
                uuid,
                command,
                workspace_directory,
                server_pid,
            } => {
                let now = now_millis();
                let host = std::env::var("HOSTNAME").unwrap_or_default();
                let user = std::env::var("USER").unwrap_or_default();
                s.post(BepBuilder::build_started(
                    &uuid,
                    now,
                    BUILD_TOOL_VERSION,
                    "",
                    &command,
                    &workspace_directory,
                    &workspace_directory,
                    server_pid as i64,
                    &host,
                    &user,
                    vec![ids::progress(0), ids::build_finished()],
                ));
            }
            BepEvent::Progress { stdout, stderr } => {
                s.report_stdout(&stdout);
                s.report_stderr(&stderr);
            }
            BepEvent::TargetConfigured { label, rule_kind } => {
                s.post(BepBuilder::target_configured(
                    &label,
                    &cfg,
                    &format!("{rule_kind} rule"),
                    &[],
                    None,
                    true,
                ));
            }
            BepEvent::TargetComplete { label, success } => {
                s.post(BepBuilder::target_completed(&label, &cfg, success, &[], &[], None));
            }
            BepEvent::TestResult {
                label,
                status,
                duration_ms,
            } => {
                let st = if ["NO_STATUS", "PASSED", "FLAKY", "TIMEOUT", "FAILED", "INCOMPLETE", "REMOTE_FAILURE", "FAILED_TO_BUILD", "TOOL_HALTED_BEFORE_TESTING"]
                    .contains(&status.as_str())
                {
                    status
                } else {
                    "NO_STATUS".to_string()
                };
                s.post(BepBuilder::test_result(
                    &label,
                    &cfg,
                    1,
                    1,
                    1,
                    &st,
                    false,
                    now_millis() - duration_ms as i64,
                    duration_ms as i64,
                    &[],
                    "local",
                    0,
                ));
            }
            BepEvent::Finished {
                exit_code_name,
                exit_code,
            } => {
                if self.is_deferred() {
                    *self.deferred_exit.lock().unwrap() = Some((exit_code_name, exit_code));
                    return;
                }
                s.post(BepBuilder::build_finished(&exit_code_name, exit_code, now_millis(), None));
                s.post(BepBuilder::build_metrics(Msg::new("build_event_stream.BuildMetrics")));
                s.post(BepBuilder::build_tool_logs(&[]));
                s.post(BepBuilder::convenience_symlinks(&[]));
            }
        }
    }

    /// Closes the BEP stream (aborting unposted announced events) and its transports.
    pub fn finish_bep(&self) {
        let s = self.bep.lock().unwrap().streamer.clone();
        if let Some(s) = s {
            s.close();
        }
    }

    /// All BEP events emitted so far, as full `BuildEvent` protos.
    pub fn bep_history(&self) -> Vec<Msg> {
        self.bep.lock().unwrap().history.events.lock().unwrap().clone()
    }

    /// Writes BEP JSON lines (`--build_event_json_file`) from the recorded history.
    pub fn write_bep_json_file(&self, fs: &dyn FileSystem, path: &PathFragment) -> io::Result<()> {
        if self.is_deferred() {
            return Ok(());
        }
        self.finish_bep();
        let mut out = String::new();
        for ev in self.bep_history() {
            out.push_str(&ev.to_json());
            out.push('\n');
        }
        fs.write_file(path, out.as_bytes())
    }

    /// Writes BEP JSON lines regardless of deferral (after the stream has been finalized).
    pub fn force_write_bep_file(&self, fs: &dyn FileSystem, path: &PathFragment, format: bep::BepFormat) -> io::Result<()> {
        self.finish_bep();
        let mut out = Vec::new();
        for ev in self.bep_history() {
            out.extend(bep::serialize_event(format, &ev));
        }
        fs.write_file(path, &out)
    }

    /// Writes the delimited binary BEP stream (`--build_event_binary_file`).
    pub fn write_bep_binary_file(&self, fs: &dyn FileSystem, path: &PathFragment) -> io::Result<()> {
        if self.is_deferred() {
            return Ok(());
        }
        self.finish_bep();
        let mut out = Vec::new();
        for ev in self.bep_history() {
            out.extend(bep::serialize_event(bep::BepFormat::Binary, &ev));
        }
        fs.write_file(path, &out)
    }

    /// Writes the text-format BEP stream (`--build_event_text_file`).
    pub fn write_bep_text_file(&self, fs: &dyn FileSystem, path: &PathFragment) -> io::Result<()> {
        if self.is_deferred() {
            return Ok(());
        }
        self.finish_bep();
        let mut out = Vec::new();
        for ev in self.bep_history() {
            out.extend(bep::serialize_event(bep::BepFormat::Text, &ev));
        }
        fs.write_file(path, &out)
    }
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// High-level Build Event Protocol (`build_event_stream.proto`) event variants (compat layer).
/// New code should build events with [`bep::BepBuilder`] and call [`Reporter::post_bep_node`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BepEvent {
    Started {
        uuid: String,
        command: String,
        workspace_directory: String,
        server_pid: u32,
    },
    Progress {
        stdout: String,
        stderr: String,
    },
    TargetConfigured {
        label: String,
        rule_kind: String,
    },
    TargetComplete {
        label: String,
        success: bool,
    },
    TestResult {
        label: String,
        status: String,
        duration_ms: u64,
    },
    Finished {
        exit_code_name: String,
        exit_code: i32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_vfs::InMemoryFileSystem;

    #[test]
    fn test_reporter_and_bep_json() {
        let reporter = Reporter::new();
        reporter.handle(Event::info("Loading package //src"));
        reporter.post_bep(BepEvent::Started {
            uuid: "uuid-1".to_string(),
            command: "build".to_string(),
            workspace_directory: "/workspace".to_string(),
            server_pid: 1234,
        });
        reporter.post_bep(BepEvent::TargetConfigured {
            label: "//a:b".into(),
            rule_kind: "cc_library".into(),
        });
        reporter.post_bep(BepEvent::TargetComplete {
            label: "//a:b".into(),
            success: true,
        });
        reporter.post_bep(BepEvent::Finished {
            exit_code_name: "SUCCESS".to_string(),
            exit_code: 0,
        });

        let fs = InMemoryFileSystem::default();
        let bep_path = PathFragment::create("/bep.json");
        reporter.write_bep_json_file(&fs, &bep_path).unwrap();
        let content = String::from_utf8(fs.read_file(&bep_path).unwrap()).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert!(lines[0].starts_with("{\"id\":{\"started\":{}},\"children\":[{\"progress\":{}}"), "{}", lines[0]);
        assert!(content.contains("\"command\":\"build\""));
        assert!(content.contains("\"exitCode\":{\"name\":\"SUCCESS\"}"), "{content}");
        assert!(content.contains("\"targetKind\":\"cc_library rule\""));
        assert!(lines.last().unwrap().contains("\"lastMessage\":true"));
    }

    #[test]
    fn handlers_stream_immediately() {
        let out = Arc::new(ui::BufferOutErr::default());
        let ui = ui::UiEventHandler::new(
            out.clone(),
            ui::UiOptions { color: ui::TriState::No, curses: ui::TriState::No, ..Default::default() },
        );
        let r = Reporter::new();
        r.add_handler(Arc::new(ui));
        r.handle(Event::info("first"));
        // Visible before the reporter is dropped or the command finishes: streaming.
        assert_eq!(out.err_string(), "INFO: first\n");
        r.handle(Event::error(None, "second"));
        assert_eq!(out.err_string(), "INFO: first\nERROR: second\n");
        assert_eq!(r.events().len(), 2);
        assert!(r.has_errors());
    }

    #[test]
    fn event_kind_names() {
        assert_eq!(EventKind::from_name("warning"), Some(EventKind::Warning));
        assert_eq!(EventKind::Subcommand.prefix(), "SUBCOMMAND: ");
        assert_eq!(Event::error(Some(Location { file: "a".into(), line: 1, column: 2 }), "m").to_string(), "ERROR: a:1:2: m");
    }
}
