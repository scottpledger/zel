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

//! Terminal UI: `UiOptions`, `UiStateTracker` and `UiEventHandler`
//! (`runtime/UiOptions.java`, `UiStateTracker.java`, `UiEventHandler.java`).
//!
//! Output is streamed to the configured [`OutErr`] as events arrive. With cursor control
//! (`--curses`) the progress bar is redrawn in place (`CR`, `ESC[1A`, `ESC[K`) and rate limited by
//! `--show_progress_rate_limit`; without it, progress lines are rate limited to at most one per
//! second and printed in the short form.

use crate::{Event, EventHandler, EventKind};
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ELLIPSIS: &str = "...";
const NO_STATUS: &str = "-----";
const STATUS_LENGTH: usize = 5;
const SHOW_TIME_THRESHOLD_SECONDS: u64 = 3;
const MINIMAL_UPDATE_INTERVAL_MILLIS: i64 = 200;
const NO_CURSES_MINIMAL_PROGRESS_RATE_LIMIT: i64 = 1000;
const SHORT_REFRESH_MILLIS: i64 = 1000;

/// `StringUtil.formatCount`: thousands separators from 10,000 upwards.
pub fn format_count(n: u64) -> String {
    if n >= 10_000 {
        group(n)
    } else {
        n.to_string()
    }
}

/// `NumberFormat.getIntegerInstance(ENGLISH)` with grouping (used by `[N / M]`).
pub fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn pluralize(noun: &str, n: u64) -> String {
    format!("{noun}{}", if n == 1 { "" } else { "s" })
}

// ---------------------------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriState {
    Yes,
    No,
    Auto,
}

impl TriState {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "yes" | "true" | "1" => Some(Self::Yes),
            "no" | "false" | "0" => Some(Self::No),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

/// `--ui_event_filters` semantic (`UiOptions.EventFiltersConverter`).
pub fn parse_event_filters(values: &[String]) -> Result<HashSet<EventKind>, String> {
    let mut filtered: HashSet<EventKind> = HashSet::new();
    for v in values {
        let mut f: HashSet<EventKind> = HashSet::new();
        let mut unf: HashSet<EventKind> = HashSet::new();
        if v.is_empty() {
            f.extend(EventKind::ALL);
        } else {
            for part in v.split(',') {
                if !part.starts_with('+') && !part.starts_with('-') {
                    f.extend(EventKind::ALL);
                    unf.clear();
                }
                if part.is_empty() {
                    continue;
                }
                let name = part.trim_start_matches(['+', '-']);
                let kind = EventKind::from_name(name)
                    .ok_or_else(|| format!("Not a valid event kind: '{name}'"))?;
                if part.starts_with('-') {
                    f.insert(kind);
                    unf.remove(&kind);
                } else {
                    unf.insert(kind);
                    f.remove(&kind);
                }
            }
        }
        filtered.extend(f);
        for k in unf {
            filtered.remove(&k);
        }
    }
    Ok(filtered)
}

/// Terminal UI options (`UiOptions.java`).
#[derive(Debug, Clone)]
pub struct UiOptions {
    pub show_progress: bool,
    pub show_progress_rate_limit: f64,
    pub color: TriState,
    pub curses: TriState,
    pub terminal_columns: usize,
    pub isatty: bool,
    pub emacs: bool,
    pub show_timestamps: bool,
    pub progress_in_terminal_title: bool,
    pub attempt_to_print_relative_paths: bool,
    pub debug_all_events: bool,
    pub filtered_event_kinds: HashSet<EventKind>,
    pub ui_actions_shown: usize,
    pub max_stdouterr_bytes: usize,
    pub terminal_hyperlinks: TriState,
    pub quiet: bool,
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            show_progress: true,
            show_progress_rate_limit: 0.2,
            color: TriState::Auto,
            curses: TriState::Auto,
            terminal_columns: 80,
            isatty: false,
            emacs: false,
            show_timestamps: false,
            progress_in_terminal_title: false,
            attempt_to_print_relative_paths: false,
            debug_all_events: false,
            filtered_event_kinds: HashSet::new(),
            ui_actions_shown: 8,
            max_stdouterr_bytes: 1_048_576,
            terminal_hyperlinks: TriState::No,
            quiet: false,
        }
    }
}

impl UiOptions {
    /// Builds options from a flag lookup (`name` without leading dashes, last value wins).
    /// `emacs` implies no color/curses like `--emacs` in Bazel's `UiOptions`.
    pub fn from_flags(
        get: &dyn Fn(&str) -> Option<String>,
        get_all: &dyn Fn(&str) -> Vec<String>,
    ) -> Result<Self, String> {
        let mut o = Self::default();
        let b = |s: Option<String>, d: bool| match s.as_deref() {
            Some("0") | Some("false") | Some("no") => false,
            Some(_) => true,
            None => d,
        };
        o.show_progress = b(get("show_progress"), true);
        if let Some(v) = get("show_progress_rate_limit") {
            o.show_progress_rate_limit =
                v.parse().map_err(|_| format!("bad --show_progress_rate_limit: {v}"))?;
        }
        if let Some(v) = get("color") {
            o.color = TriState::parse(&v).ok_or_else(|| format!("bad --color value: {v}"))?;
        }
        if let Some(v) = get("curses") {
            o.curses = TriState::parse(&v).ok_or_else(|| format!("bad --curses value: {v}"))?;
        }
        if let Some(v) = get("terminal_columns").and_then(|v| v.parse().ok()) {
            o.terminal_columns = v;
        }
        o.isatty = b(get("isatty"), false);
        o.emacs = b(get("emacs"), false);
        o.show_timestamps = b(get("show_timestamps"), false);
        o.progress_in_terminal_title = b(get("progress_in_terminal_title"), false);
        o.attempt_to_print_relative_paths = b(get("attempt_to_print_relative_paths"), false);
        o.debug_all_events = b(get("experimental_ui_debug_all_events"), false);
        if let Some(v) = get("ui_actions_shown").and_then(|v| v.parse().ok()) {
            o.ui_actions_shown = v;
        }
        if let Some(v) = get("experimental_ui_max_stdouterr_bytes") {
            let n: i64 = v
                .parse()
                .map_err(|_| format!("bad --experimental_ui_max_stdouterr_bytes: {v}"))?;
            if n < -1 {
                return Err(format!("--experimental_ui_max_stdouterr_bytes out of range: {n}"));
            }
            o.max_stdouterr_bytes = if n >= 0 { n as usize } else { ((i32::MAX - 8) >> 1) as usize };
        }
        if let Some(v) = get("terminal_hyperlinks") {
            o.terminal_hyperlinks = TriState::parse(&v).unwrap_or(TriState::No);
        }
        o.quiet = b(get("quiet"), false);
        let filters = get_all("ui_event_filters");
        o.filtered_event_kinds = parse_event_filters(&filters)?;
        if o.emacs {
            o.color = TriState::No;
            o.curses = TriState::No;
        }
        Ok(o)
    }

    pub fn use_color(&self) -> bool {
        self.color == TriState::Yes || (self.color == TriState::Auto && self.isatty)
    }

    pub fn use_cursor_control(&self) -> bool {
        self.curses == TriState::Yes || (self.curses == TriState::Auto && self.isatty)
    }

    pub fn use_hyperlinks(&self) -> bool {
        self.terminal_hyperlinks == TriState::Yes
            || (self.terminal_hyperlinks == TriState::Auto && self.use_color() && self.isatty)
    }

    /// Fills `isatty`/`terminal_columns` from the real terminal when the client did not provide
    /// them (what the Bazel C++ client does).
    pub fn detect_terminal(&mut self) {
        self.isatty = zel_util::terminal::isatty(2) && !zel_util::terminal::term_is_dumb();
        if let Some(c) = zel_util::terminal::terminal_columns() {
            self.terminal_columns = c;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------------------------

/// Destination for UI output (`OutErr`).
pub trait OutErr: Send + Sync {
    fn out(&self, bytes: &[u8]);
    fn err(&self, bytes: &[u8]);
    fn flush(&self) {}
}

/// Buffers output in memory; drain with [`BufferOutErr::take_err`]/[`take_out`].
#[derive(Default)]
pub struct BufferOutErr {
    out: Mutex<Vec<u8>>,
    err: Mutex<Vec<u8>>,
}

impl BufferOutErr {
    pub fn take_out(&self) -> Vec<u8> {
        std::mem::take(&mut self.out.lock().unwrap())
    }
    pub fn take_err(&self) -> Vec<u8> {
        std::mem::take(&mut self.err.lock().unwrap())
    }
    pub fn err_string(&self) -> String {
        String::from_utf8_lossy(&self.err.lock().unwrap()).into_owned()
    }
    pub fn out_string(&self) -> String {
        String::from_utf8_lossy(&self.out.lock().unwrap()).into_owned()
    }
}

impl OutErr for BufferOutErr {
    fn out(&self, b: &[u8]) {
        self.out.lock().unwrap().extend_from_slice(b);
    }
    fn err(&self, b: &[u8]) {
        self.err.lock().unwrap().extend_from_slice(b);
    }
}

/// Writes straight to the process stdout/stderr (real streaming).
pub struct StdOutErr;

impl OutErr for StdOutErr {
    fn out(&self, b: &[u8]) {
        use std::io::Write;
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(b);
        let _ = o.flush();
    }
    fn err(&self, b: &[u8]) {
        use std::io::Write;
        let mut o = std::io::stderr().lock();
        let _ = o.write_all(b);
        let _ = o.flush();
    }
}

/// Forwards each write to caller-provided closures (e.g. gRPC response streaming).
pub struct CallbackOutErr {
    pub out_cb: Box<dyn Fn(&[u8]) + Send + Sync>,
    pub err_cb: Box<dyn Fn(&[u8]) + Send + Sync>,
}

impl OutErr for CallbackOutErr {
    fn out(&self, b: &[u8]) {
        (self.out_cb)(b)
    }
    fn err(&self, b: &[u8]) {
        (self.err_cb)(b)
    }
}

/// Time source for the UI (injectable for tests).
pub trait UiClock: Send + Sync {
    fn millis(&self) -> i64;
    fn nanos(&self) -> u64;
    /// Offset of local time from UTC at `millis` (Bazel formats `--show_timestamps` in local time).
    fn utc_offset_ms(&self, _millis: i64) -> i64 {
        0
    }
}

#[repr(C)]
struct CTm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
    tm_gmtoff: i64,
    tm_zone: *const u8,
}

extern "C" {
    fn localtime_r(t: *const i64, out: *mut CTm) -> *mut CTm;
}

/// Local-time offset from UTC in seconds at the given epoch second (0 if unavailable).
pub fn local_utc_offset_secs(epoch_secs: i64) -> i64 {
    let mut tm = CTm { tm_sec: 0, tm_min: 0, tm_hour: 0, tm_mday: 0, tm_mon: 0, tm_year: 0, tm_wday: 0, tm_yday: 0, tm_isdst: 0, tm_gmtoff: 0, tm_zone: std::ptr::null() };
    // SAFETY: `tm` is a valid, properly laid out `struct tm`; `localtime_r` is thread-safe.
    let r = unsafe { localtime_r(&epoch_secs, &mut tm) };
    if r.is_null() { 0 } else { tm.tm_gmtoff }
}

pub struct SystemUiClock {
    start: Instant,
}

impl Default for SystemUiClock {
    fn default() -> Self {
        Self { start: Instant::now() }
    }
}

impl UiClock for SystemUiClock {
    fn millis(&self) -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
    }
    fn nanos(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }
    fn utc_offset_ms(&self, millis: i64) -> i64 {
        local_utc_offset_secs(millis.div_euclid(1000)) * 1000
    }
}

/// Manually advanced clock for tests.
#[derive(Default)]
pub struct ManualClock {
    nanos: Mutex<u64>,
}

impl ManualClock {
    pub fn advance(&self, d: Duration) {
        *self.nanos.lock().unwrap() += d.as_nanos() as u64;
    }
}

impl UiClock for ManualClock {
    fn millis(&self) -> i64 {
        // 2024-01-02 03:04:05 UTC + elapsed
        1_704_164_645_000 + (*self.nanos.lock().unwrap() / 1_000_000) as i64
    }
    fn nanos(&self) -> u64 {
        *self.nanos.lock().unwrap()
    }
}

// ---------------------------------------------------------------------------------------------
// Terminal writer
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Normal,
    Ok,
    Fail,
}

#[derive(Debug, Clone)]
pub enum Seg {
    Text(String),
    Style(Style),
    Newline,
}

/// Position-aware progress document (`PositionAwareAnsiTerminalWriter`).
#[derive(Debug, Default, Clone)]
pub struct ProgressDoc {
    pub segs: Vec<Seg>,
    pos: usize,
}

impl ProgressDoc {
    pub fn append(&mut self, s: &str) -> &mut Self {
        self.pos += s.chars().count();
        self.segs.push(Seg::Text(s.to_string()));
        self
    }
    pub fn newline(&mut self) -> &mut Self {
        self.pos = 0;
        self.segs.push(Seg::Newline);
        self
    }
    pub fn ok(&mut self) -> &mut Self {
        self.segs.push(Seg::Style(Style::Ok));
        self
    }
    pub fn fail(&mut self) -> &mut Self {
        self.segs.push(Seg::Style(Style::Fail));
        self
    }
    pub fn normal(&mut self) -> &mut Self {
        self.segs.push(Seg::Style(Style::Normal));
        self
    }
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Plain text transcript (`LoggingTerminalWriter` with highlights discarded).
    pub fn plain(&self) -> String {
        let mut s = String::new();
        for g in &self.segs {
            match g {
                Seg::Text(t) => s.push_str(t),
                Seg::Newline => s.push('\n'),
                Seg::Style(_) => {}
            }
        }
        s
    }
}

/// Low-level ANSI output buffer (`AnsiTerminal`); emits styling only when `color`.
struct Term {
    buf: Vec<u8>,
    color: bool,
    cursor: bool,
}

impl Term {
    fn new(color: bool, cursor: bool) -> Self {
        Self { buf: Vec::new(), color, cursor }
    }
    fn raw(&mut self, s: &str) {
        self.buf.extend_from_slice(s.as_bytes());
    }
    fn esc(&mut self, s: &str) {
        if self.color {
            self.raw(s);
        }
    }
    fn red(&mut self) {
        self.esc("\x1b[31m");
    }
    fn green(&mut self) {
        self.esc("\x1b[32m");
    }
    fn yellow(&mut self) {
        self.esc("\x1b[33m");
    }
    fn blue(&mut self) {
        self.esc("\x1b[34m");
    }
    fn magenta(&mut self) {
        self.esc("\x1b[35m");
    }
    fn bold(&mut self) {
        self.esc("\x1b[1m");
    }
    fn reset(&mut self) {
        self.esc("\x1b[0m");
    }
    fn style(&mut self, s: Style) {
        match s {
            Style::Ok => self.green(),
            Style::Fail => {
                self.red();
                self.bold();
            }
            Style::Normal => self.reset(),
        }
    }
    fn cursor_up_clear_line(&mut self) {
        if self.cursor {
            self.raw("\r");
            self.raw("\x1b[1A");
            self.raw("\x1b[K");
        }
    }
    fn set_title(&mut self, t: &str) {
        if self.cursor {
            self.raw(&format!("\x1b]0;{t}\x1b\\"));
        }
    }
    fn nl(&mut self) {
        self.raw("\n");
    }
}

// ---------------------------------------------------------------------------------------------
// State tracker
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionPhase {
    Preparing,
    Scanning,
    Caching,
    Uploading,
    Scheduling,
    Running,
}

impl ActionPhase {
    fn describe(self) -> Option<&'static str> {
        Some(match self {
            Self::Preparing => "Preparing",
            Self::Scanning => "Scanning",
            Self::Caching => "Caching",
            Self::Uploading => "Uploading",
            Self::Scheduling => "Scheduling",
            Self::Running => "Running",
        })
    }
}

#[derive(Debug, Clone)]
struct ActionState {
    /// Progress message such as `Compiling foo.cc` (`action.getProgressMessage()`).
    message: String,
    owner_label: Option<String>,
    mnemonic: String,
    start_nanos: u64,
    phase: ActionPhase,
    strategies: Vec<String>,
    progress: Option<(String, String)>,
    seq: u64,
}

#[derive(Debug, Clone, Default)]
struct DownloadData {
    start_nanos: u64,
    bytes: u64,
}

/// Tracks build state to render the progress bar (`UiStateTracker`).
pub struct UiStateTracker {
    status: Option<String>,
    ok: bool,
    additional_message: String,
    packages_completed: u64,
    pending_packages: Vec<String>,
    package_progress: bool,
    analysis_progress: bool,
    targets_configured: u64,
    aspects_configured: u64,
    execution_progress: bool,
    actions_completed: u64,
    enqueued_actions: u64,
    exclusive_tests: u64,
    active: BTreeMap<String, ActionState>,
    next_seq: u64,
    completed_tests: u64,
    total_tests: u64,
    failed_tests: u64,
    test_actions: BTreeMap<String, HashSet<String>>,
    downloads: BTreeMap<String, DownloadData>,
    bep_transports: Vec<String>,
    build_complete: bool,
    build_complete_at_nanos: Option<u64>,
    target_width: usize,
    progress_sample_size: usize,
    new_stats_summary: bool,
    build_started: bool,
    execution_started: bool,
    hint_nanos: u64,
}

impl UiStateTracker {
    pub fn new(target_width: usize) -> Self {
        Self {
            status: None,
            ok: true,
            additional_message: String::new(),
            packages_completed: 0,
            pending_packages: Vec::new(),
            package_progress: false,
            analysis_progress: false,
            targets_configured: 0,
            aspects_configured: 0,
            execution_progress: false,
            actions_completed: 0,
            enqueued_actions: 0,
            exclusive_tests: 0,
            active: BTreeMap::new(),
            next_seq: 0,
            completed_tests: 0,
            total_tests: 0,
            failed_tests: 0,
            test_actions: BTreeMap::new(),
            downloads: BTreeMap::new(),
            bep_transports: Vec::new(),
            build_complete: false,
            build_complete_at_nanos: None,
            target_width,
            progress_sample_size: 8,
            new_stats_summary: false,
            build_started: false,
            execution_started: false,
            hint_nanos: 0,
        }
    }

    pub fn set_target_width(&mut self, w: usize) {
        self.target_width = w;
    }
    pub fn set_progress_sample_size(&mut self, n: usize) {
        self.progress_sample_size = n;
    }
    pub fn set_new_stats_summary(&mut self, b: bool) {
        self.new_stats_summary = b;
    }
    pub fn actions_completed(&self) -> u64 {
        self.actions_completed
    }
    pub fn failed_tests(&self) -> u64 {
        self.failed_tests
    }

    pub fn main_repo_mapping_computation_started(&mut self) {
        self.status = Some("Computing main repo mapping".into());
        self.additional_message.clear();
    }

    pub fn build_started(&mut self) {
        self.build_started = true;
        self.status = Some("Loading".into());
        self.additional_message.clear();
    }

    pub fn loading_started(&mut self, with_package_progress: bool) {
        self.package_progress = with_package_progress;
        self.status = if with_package_progress { None } else { Some("Loading".into()) };
    }

    pub fn configuration_started(&mut self) {
        self.analysis_progress = true;
    }

    pub fn package_loading_started(&mut self, pkg: &str) {
        if !self.pending_packages.iter().any(|p| p == pkg) {
            self.pending_packages.push(pkg.to_string());
        }
    }

    pub fn package_loaded(&mut self, pkg: &str) {
        self.pending_packages.retain(|p| p != pkg);
        self.packages_completed += 1;
    }

    pub fn target_configured_progress(&mut self, n: u64) {
        self.targets_configured += n;
    }

    pub fn aspect_configured_progress(&mut self, n: u64) {
        self.aspects_configured += n;
    }

    pub fn loading_complete(&mut self, labels: &[String]) {
        self.status = Some("Analyzing".into());
        self.additional_message = if labels.len() == 1 {
            format!("target {}", labels[0])
        } else {
            format!("{} targets", format_count(labels.len() as u64))
        };
    }

    pub fn execution_phase_started(&mut self) {
        self.execution_started = true;
        self.execution_progress = true;
    }

    /// `AnalysisProgressReceiver.getProgressString`.
    fn analysis_progress_string(&self) -> String {
        let mut s = format!(
            "{} {} configured",
            format_count(self.targets_configured),
            if self.targets_configured == 1 { "target" } else { "targets" }
        );
        if self.aspects_configured > 0 {
            s.push_str(&format!(
                ", {} {}",
                format_count(self.aspects_configured),
                if self.aspects_configured == 1 { "aspect application" } else { "aspect applications" }
            ));
        }
        s
    }

    fn package_progress_state(&self) -> (String, String) {
        let progress = format!("{} packages loaded", format_count(self.packages_completed));
        let mut activity = String::new();
        if let Some(first) = self.pending_packages.first() {
            activity.push_str(&format!("currently loading: {first}"));
            if self.pending_packages.len() > 1 {
                activity.push_str(&format!(
                    " ... ({} packages)",
                    format_count(self.pending_packages.len() as u64)
                ));
            }
        }
        (progress, activity)
    }

    /// Summary of the analysis phase, e.g. `Analyzed 2 targets (5 packages loaded, 7 targets
    /// configured).`
    pub fn analysis_complete(&mut self) -> String {
        let mut work = format!("Analyzed {}", self.additional_message);
        if self.package_progress {
            let (p, _) = self.package_progress_state();
            work.push_str(&format!(" ({p}"));
            if self.analysis_progress {
                work.push_str(&format!(", {}", self.analysis_progress_string()));
            }
            work.push(')');
        }
        work.push('.');
        self.status = None;
        self.package_progress = false;
        self.analysis_progress = false;
        work
    }

    /// Returns the final event (`Build completed successfully, N total actions` or the error).
    pub fn build_complete(&mut self, success: bool, now_nanos: u64) -> Event {
        self.build_complete = true;
        self.build_complete_at_nanos = Some(now_nanos);
        self.execution_progress = false;
        self.status = None;
        self.additional_message.clear();
        if success {
            let mut s = String::from("Build completed");
            if self.failed_tests == 0 {
                s.push_str(" successfully");
            } else {
                s.push_str(&format!(
                    ", {}{} FAILED",
                    self.failed_tests,
                    pluralize(" test", self.failed_tests)
                ));
            }
            if !self.new_stats_summary {
                s.push_str(&format!(
                    ", {}{}",
                    self.actions_completed,
                    pluralize(" total action", self.actions_completed)
                ));
            }
            Event::info(s)
        } else {
            self.ok = false;
            Event::error(None, "Build did NOT complete successfully")
        }
    }

    pub fn build_is_complete(&self) -> bool {
        self.build_complete
    }

    // --- actions ---

    fn action_mut(&mut self, id: &str) -> Option<&mut ActionState> {
        self.active.get_mut(id)
    }

    /// `ActionStartedEvent`: begin tracking an action. `message` is the progress message.
    pub fn action_started(&mut self, id: &str, message: &str, owner: Option<&str>, mnemonic: &str) {
        let nanos = self.now_hint();
        let seq = self.next_seq;
        self.next_seq += 1;
        self.enqueued_actions = self.enqueued_actions.max(self.actions_completed + self.active.len() as u64 + 1);
        if mnemonic == "TestRunner" {
            if let Some(o) = owner {
                self.test_actions.entry(o.to_string()).or_default().insert(id.to_string());
            }
        }
        self.active.insert(
            id.to_string(),
            ActionState {
                message: message.to_string(),
                owner_label: owner.map(str::to_string),
                mnemonic: mnemonic.to_string(),
                start_nanos: nanos,
                phase: ActionPhase::Preparing,
                strategies: Vec::new(),
                progress: None,
                seq,
            },
        );
    }

    fn now_hint(&self) -> u64 {
        self.hint_nanos
    }

    pub fn set_now(&mut self, nanos: u64) {
        self.hint_nanos = nanos;
    }
}

// The tracker needs the current time to stamp action starts; kept as a plain field to avoid a
// clock dependency in the pure state machine.
impl UiStateTracker {
    pub fn scanning_action(&mut self, id: &str) {
        if let Some(a) = self.action_mut(id) {
            if a.phase < ActionPhase::Scanning {
                a.phase = ActionPhase::Scanning;
            }
        }
    }
    pub fn stop_scanning_action(&mut self, id: &str) {
        if let Some(a) = self.action_mut(id) {
            if a.phase < ActionPhase::Caching {
                a.phase = ActionPhase::Preparing;
            }
        }
    }
    pub fn checking_action_cache(&mut self, id: &str) {
        if let Some(a) = self.action_mut(id) {
            if a.phase < ActionPhase::Caching {
                a.phase = ActionPhase::Caching;
            }
        }
    }
    pub fn uploading_action(&mut self, id: &str, strategy: &str) {
        if let Some(a) = self.action_mut(id) {
            if a.phase < ActionPhase::Uploading {
                a.phase = ActionPhase::Uploading;
            }
            if !a.strategies.iter().any(|s| s == strategy) {
                a.strategies.push(strategy.to_string());
            }
        }
    }
    pub fn scheduling_action(&mut self, id: &str, strategy: &str) {
        if let Some(a) = self.action_mut(id) {
            if a.phase < ActionPhase::Scheduling {
                a.phase = ActionPhase::Scheduling;
            }
            if !a.strategies.iter().any(|s| s == strategy) {
                a.strategies.push(strategy.to_string());
            }
        }
    }
    pub fn running_action(&mut self, id: &str, strategy: &str) {
        if let Some(a) = self.action_mut(id) {
            if a.phase < ActionPhase::Running {
                a.phase = ActionPhase::Running;
            }
            if !a.strategies.iter().any(|s| s == strategy) {
                a.strategies.push(strategy.to_string());
            }
        }
    }
    pub fn action_progress(&mut self, id: &str, progress_id: &str, message: &str, finished: bool) {
        if let Some(a) = self.action_mut(id) {
            if finished {
                if a.progress.as_ref().map(|p| p.0 == progress_id).unwrap_or(false) {
                    a.progress = None;
                }
            } else {
                a.progress = Some((progress_id.to_string(), message.to_string()));
            }
        }
    }
    pub fn action_completed(&mut self, id: &str) {
        if self.active.remove(id).is_some() {
            self.actions_completed += 1;
            self.enqueued_actions = self.enqueued_actions.max(self.actions_completed);
        }
    }
    pub fn set_total_actions(&mut self, n: u64) {
        self.enqueued_actions = n;
    }
    pub fn set_exclusive_tests(&mut self, n: u64) {
        self.exclusive_tests = n;
    }
    pub fn test_filtering_complete(&mut self, num_tests: u64) {
        self.total_tests = num_tests;
    }
    pub fn test_summary(&mut self, passed: bool) {
        self.completed_tests += 1;
        if !passed {
            self.failed_tests += 1;
        }
    }
    pub fn download_started(&mut self, url: &str, nanos: u64) {
        self.downloads.insert(url.to_string(), DownloadData { start_nanos: nanos, bytes: 0 });
    }
    pub fn download_progress(&mut self, url: &str, bytes: u64) {
        if let Some(d) = self.downloads.get_mut(url) {
            d.bytes = bytes;
        }
    }
    pub fn download_finished(&mut self, url: &str) {
        self.downloads.remove(url);
    }
    pub fn bep_transports_announced(&mut self, names: Vec<String>) {
        self.bep_transports = names;
    }
    pub fn bep_transport_closed(&mut self, name: &str) {
        self.bep_transports.retain(|n| n != name);
    }

    pub fn has_activities(&self) -> bool {
        self.status.is_some()
            || self.package_progress
            || !self.active.is_empty()
            || (self.execution_progress && !self.build_complete)
            || !self.downloads.is_empty()
            || (self.build_complete && !self.bep_transports.is_empty())
    }

    pub fn progress_bar_time_dependent(&self) -> bool {
        !self.active.is_empty() || !self.downloads.is_empty() || !self.bep_transports.is_empty()
    }

    // --- rendering ---

    fn shortened(s: &str, max: usize) -> String {
        if max <= 3 * ELLIPSIS.len() || s.chars().count() <= max {
            return s.to_string();
        }
        let keep = max - ELLIPSIS.len();
        let mut out: String = s.chars().take(keep).collect();
        out.push_str(ELLIPSIS);
        out
    }

    fn suffix(s: &str, n: usize) -> String {
        let chars: Vec<char> = s.chars().collect();
        chars[chars.len().saturating_sub(n)..].iter().collect()
    }

    fn describe_test_group(&self, owner: &str, now: u64, desired: usize, all: &HashSet<String>) -> String {
        let prefix = "Testing ";
        let label_sep = " [";
        let postfix = format!(" ({} actions)]", all.len());
        let label_width = desired as i64
            - prefix.len() as i64
            - label_sep.len() as i64
            - postfix.len() as i64
            - 12;
        let mut message = format!(
            "{prefix}{}{label_sep}",
            Self::shortened(owner, label_width.max(0) as usize)
        );
        let mut remaining = desired as i64 - message.len() as i64 - postfix.len() as i64;
        if remaining < 0 {
            remaining = "[1s], ".len() as i64 + 1;
        }
        let mut sep = "";
        let mut all_reported = true;
        let mut ids: Vec<&String> = all.iter().collect();
        ids.sort();
        for id in ids {
            let Some(a) = self.active.get(id) else { continue };
            let secs = now.saturating_sub(a.start_nanos) / 1_000_000_000;
            let text = if a.phase < ActionPhase::Running {
                format!("{sep}[{secs}s]")
            } else {
                format!("{sep}{secs}s")
            };
            if remaining < text.len() as i64 {
                all_reported = false;
                break;
            }
            message.push_str(&text);
            remaining -= text.len() as i64;
            sep = ", ";
        }
        message.push_str(if all_reported { "]" } else { &postfix });
        message
    }

    fn describe_action_progress(a: &ActionState, desired: usize) -> String {
        let Some((id, msg)) = &a.progress else { return String::new() };
        let m = if msg.is_empty() { id.clone() } else { msg.clone() };
        let m = format!("; {m}");
        if desired == 0 || m.chars().count() <= desired {
            return m;
        }
        let keep = desired.saturating_sub(ELLIPSIS.len());
        let mut out: String = m.chars().take(keep).collect();
        out.push_str(ELLIPSIS);
        out
    }

    /// `describeAction`: `[Prepa] Compiling foo.cc; 5s (sandbox)` shortened to `desired` columns
    /// (0 = unlimited).
    fn describe_action(&self, a: &ActionState, now: u64, desired: usize, mut skip: Option<&mut HashSet<String>>) -> String {
        if a.mnemonic == "TestRunner" {
            if let Some(owner) = &a.owner_label {
                if let Some(all) = self.test_actions.get(owner) {
                    if all.len() > 1 {
                        if let Some(s) = skip.as_deref_mut() {
                            s.extend(all.iter().cloned());
                        }
                        return self.describe_test_group(owner, now, desired, all);
                    }
                }
            }
        }
        let mut prefix = String::new();
        let mut postfix = String::new();
        let secs = now.saturating_sub(a.start_nanos) / 1_000_000_000;
        let strategy = if a.strategies.is_empty() { None } else { Some(a.strategies.join(", ")) };
        if a.phase != ActionPhase::Caching && a.phase != ActionPhase::Running {
            let mut status = a.phase.describe().unwrap_or(NO_STATUS).to_string();
            if status.len() > STATUS_LENGTH {
                status.truncate(STATUS_LENGTH);
            }
            prefix = format!("[{status}] ");
        }
        if strategy.is_some() || secs > SHOW_TIME_THRESHOLD_SECONDS {
            postfix = format!("; {secs}s");
        }
        if let Some(s) = &strategy {
            postfix.push_str(&format!(" ({s})"));
        }
        let message = a.message.clone();
        let progress = Self::describe_action_progress(a, 0);
        let len = |s: &str| s.chars().count();
        if desired == 0 || len(&prefix) + len(&message) + len(&progress) + len(&postfix) <= desired {
            return format!("{prefix}{message}{progress}{postfix}");
        }
        let remaining = desired as i64 - len(&prefix) as i64 - len(&message) as i64 - len(&postfix) as i64;
        if remaining >= 7 {
            let p = Self::describe_action_progress(a, remaining as usize);
            return format!("{prefix}{message}{p}{postfix}");
        }
        if len(&prefix) + len(&message) + len(&postfix) <= desired {
            return format!("{prefix}{message}{postfix}");
        }
        // Shorten the message: first the package path, then the label, then the tail.
        if let Some(label) = &a.owner_label {
            let pkg = label
                .trim_start_matches('@')
                .split("//")
                .nth(1)
                .and_then(|r| r.split(':').next())
                .unwrap_or("");
            if !pkg.is_empty() {
                if let Some(idx) = message.find(pkg) {
                    let start = &message[..idx];
                    let end = &message[idx + pkg.len()..];
                    let target = desired as i64
                        - len(start) as i64
                        - len(end) as i64
                        - len(&postfix) as i64
                        - len(&prefix) as i64;
                    if target >= 3 * ELLIPSIS.len() as i64 {
                        let short = Self::suffix(pkg, target as usize - ELLIPSIS.len());
                        if let Some(slash) = short.find('/') {
                            return format!("{prefix}{start}{ELLIPSIS}{}{end}{postfix}", &short[slash..]);
                        }
                    }
                }
            }
            let short_label = Self::shortened_label(label, desired.saturating_sub(len(&postfix) + len(&prefix)));
            if len(&prefix) + len(&short_label) + len(&postfix) <= desired {
                return format!("{prefix}{short_label}{postfix}");
            }
        }
        let mut message = message;
        if 3 * ELLIPSIS.len() + len(&postfix) + len(&prefix) <= desired {
            message = format!(
                "{ELLIPSIS}{}",
                Self::suffix(&message, desired - ELLIPSIS.len() - len(&postfix) - len(&prefix))
            );
        }
        format!("{prefix}{message}{postfix}")
    }

    fn shortened_label(label: &str, max: usize) -> String {
        if label.chars().count() <= max || max <= 3 * ELLIPSIS.len() {
            return label.to_string();
        }
        // Keep the target name, elide the middle of the package path.
        let (pkg, name) = label.rsplit_once(':').unwrap_or((label, ""));
        let tail = if name.is_empty() { String::new() } else { format!(":{name}") };
        let budget = max.saturating_sub(tail.chars().count() + ELLIPSIS.len());
        let kept = Self::suffix(pkg, budget);
        format!("{ELLIPSIS}{kept}{tail}")
    }

    fn oldest_action(&self) -> Option<&ActionState> {
        let mut oldest_running: Option<&ActionState> = None;
        let mut oldest_scheduled: Option<&ActionState> = None;
        for a in self.active.values() {
            let slot = if a.phase == ActionPhase::Running { &mut oldest_running } else { &mut oldest_scheduled };
            if slot.map(|s| (a.start_nanos, a.seq) < (s.start_nanos, s.seq)).unwrap_or(true) {
                *slot = Some(a);
            }
        }
        oldest_running.or(oldest_scheduled)
    }

    fn count_actions(&self) -> String {
        let total = self.active.len();
        let running = self.active.values().filter(|a| a.phase == ActionPhase::Running).count();
        if total == 1 {
            " 1 action running".to_string()
        } else if total == running {
            format!("{total} actions running")
        } else {
            format!("{total} actions, {running} running")
        }
    }

    fn sample_oldest_actions(&self, doc: &mut ProgressDoc, now: u64) {
        let mut v: Vec<(&String, &ActionState)> = self.active.iter().collect();
        // Running first, then oldest start.
        v.sort_by(|a, b| {
            (b.1.phase == ActionPhase::Running)
                .cmp(&(a.1.phase == ActionPhase::Running))
                .then(a.1.start_nanos.cmp(&b.1.start_nanos))
                .then(a.1.seq.cmp(&b.1.seq))
        });
        let total = v.len();
        let sample = self.progress_sample_size.max(1);
        let mut skip: HashSet<String> = HashSet::new();
        let mut shown = 0;
        let mut skipped_count = 0;
        for (id, a) in v {
            if skip.contains(id.as_str()) {
                continue;
            }
            if shown >= sample {
                skipped_count += 1;
                continue;
            }
            // The last allowed line is reserved for the "... N more" line when more remain.
            let width = self.target_width.saturating_sub(4);
            let line = self.describe_action(a, now, width, Some(&mut skip));
            doc.newline().append(&format!("    {line}"));
            shown += 1;
        }
        let hidden = total.saturating_sub(shown);
        let _ = skipped_count;
        if hidden > 0 && shown >= sample {
            doc.newline().append(&format!("  ... and {} more", format_count(hidden as u64)));
        }
    }

    fn write_execution_progress(&self, doc: &mut ProgressDoc, short: bool, now: u64) {
        let actions = self.active.len();
        if self.execution_progress {
            doc.ok().append(&format!(
                "[{} / {}]",
                group(self.actions_completed),
                group(self.exclusive_tests + self.enqueued_actions.max(self.actions_completed))
            ));
        }
        if self.completed_tests > 0 {
            doc.normal().append(&format!(" {} / {} tests", self.completed_tests, self.total_tests));
            if self.failed_tests > 0 {
                doc.append(", ").fail().append(&format!("{} failed", self.failed_tests)).normal();
            }
            doc.append(";");
        }
        let width_left = |doc: &ProgressDoc| self.target_width.saturating_sub(doc.position());
        let oldest = self.oldest_action();
        match (actions, oldest) {
            (0, _) | (_, None) => {
                if self.execution_progress && self.enqueued_actions > self.actions_completed {
                    doc.normal().append(" checking cached actions");
                } else {
                    doc.normal().append(" no actions running");
                }
            }
            (1, Some(a)) => {
                let w = width_left(doc).saturating_sub(1);
                let msg = self.describe_action(a, now, if self.target_width == 0 { 0 } else { w }, None);
                doc.normal().append(&format!(" {msg}"));
            }
            (_, Some(a)) => {
                if short {
                    let w = width_left(doc);
                    let mut msg = self.describe_action(a, now, if self.target_width == 0 { 0 } else { w }, None);
                    msg.push_str(&format!(" ... ({})", self.count_actions().trim_start()));
                    doc.normal().append(&format!(" {msg}"));
                } else {
                    doc.normal().append(&format!(" {}", self.count_actions().trim_start()));
                    self.sample_oldest_actions(doc, now);
                }
            }
        }
    }

    fn report_downloads(&self, doc: &mut ProgressDoc, now: u64) {
        if self.downloads.is_empty() {
            return;
        }
        let mut v: Vec<(&String, &DownloadData)> = self.downloads.iter().collect();
        v.sort_by_key(|(_, d)| d.start_nanos);
        let count = v.len();
        let sample = self.progress_sample_size.max(1);
        for (i, (url, d)) in v.iter().enumerate() {
            if i >= sample {
                doc.newline().append(&format!("    ... and {} more", count - i));
                break;
            }
            let secs = now.saturating_sub(d.start_nanos) / 1_000_000_000;
            let line = format!("    Fetching {url}");
            let postfix = format!("; {secs}s");
            let width = self.target_width;
            let line = if width > 0 && line.chars().count() + postfix.len() > width {
                let keep = width.saturating_sub(postfix.len() + ELLIPSIS.len());
                let mut t: String = line.chars().take(keep).collect();
                t.push_str(ELLIPSIS);
                t
            } else {
                line
            };
            doc.newline().append(&format!("{line}{postfix}"));
        }
    }

    fn report_bep(&self, doc: &mut ProgressDoc, now: u64) {
        let Some(at) = self.build_complete_at_nanos else { return };
        if !self.build_complete || self.bep_transports.is_empty() {
            return;
        }
        let wait = now.saturating_sub(at) / 1_000_000_000;
        if wait == 0 {
            return;
        }
        let max_width = self.target_width;
        let name = &self.bep_transports[0];
        let line = format!("Waiting for build events upload: {name} {wait}s");
        if self.bep_transports.len() == 1 && (max_width == 0 || line.chars().count() <= max_width) {
            doc.newline().append(&line);
        } else if self.bep_transports.len() == 1 {
            let wait_message = "Waiting for: ";
            let secs = format!(" {wait}s");
            let max_name = max_width.saturating_sub(wait_message.len() + secs.len());
            doc.newline().append(&format!("{wait_message}{}{secs}", Self::shortened(name, max_name)));
        } else {
            doc.newline().append(&format!("Waiting for build events upload: {wait}s"));
            for t in &self.bep_transports {
                doc.newline().append(&Self::shortened(&format!("  {t}"), max_width));
            }
        }
    }

    /// Renders the progress bar document (`writeProgressBar`).
    pub fn write_progress_bar(&self, short: bool, timestamp: Option<&str>, now: u64) -> ProgressDoc {
        let mut doc = ProgressDoc::default();
        if let Some(t) = timestamp {
            doc.append(t);
        }
        if let Some(status) = &self.status {
            if self.ok {
                doc.ok();
            } else {
                doc.fail();
            }
            doc.append(&format!("{status}:")).normal().append(&format!(" {}", self.additional_message));
            if self.package_progress || self.analysis_progress {
                doc.append(" (");
                let mut first = true;
                if self.package_progress {
                    doc.append(&self.package_progress_state().0);
                    first = false;
                }
                if self.analysis_progress {
                    if !first {
                        doc.append(", ");
                    }
                    doc.append(&self.analysis_progress_string());
                }
                doc.append(")");
                if self.package_progress {
                    let (_, act) = self.package_progress_state();
                    if !act.is_empty() && !short {
                        doc.newline().append(&format!("    {act}"));
                    }
                }
            }
            if !short {
                self.report_downloads(&mut doc, now);
                self.report_bep(&mut doc, now);
            }
            return doc;
        }
        if self.package_progress {
            let (p, act) = self.package_progress_state();
            doc.ok().append("Loading:").normal().append(&format!(" {p}"));
            if !act.is_empty() {
                doc.newline().append(&format!("    {act}"));
            }
            if !short {
                self.report_downloads(&mut doc, now);
            }
            return doc;
        }
        if !self.build_complete {
            self.write_execution_progress(&mut doc, short, now);
        }
        if !short {
            self.report_downloads(&mut doc, now);
            self.report_bep(&mut doc, now);
        }
        doc
    }
}

// ---------------------------------------------------------------------------------------------
// Event handler
// ---------------------------------------------------------------------------------------------

struct HandlerState {
    tracker: UiStateTracker,
    stdout_buf: Vec<u8>,
    stderr_buf: Vec<u8>,
    lines_in_bar: usize,
    build_running: bool,
    show_progress: bool,
    date_shown: bool,
    last_refresh: i64,
    must_refresh_after: i64,
    needs_refresh: bool,
    terminal_width: usize,
    fatal: bool,
}

struct Shared {
    out: Arc<dyn OutErr>,
    opts: UiOptions,
    clock: Arc<dyn UiClock>,
    cursor: bool,
    color: bool,
    progress_rate_limit_ms: i64,
    minimal_update_interval: i64,
    state: Mutex<HandlerState>,
    shutdown: AtomicBool,
    thread_started: AtomicBool,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// Renders events and the progress bar to an [`OutErr`] as they arrive.
#[derive(Clone)]
pub struct UiEventHandler {
    s: Arc<Shared>,
}

fn civil_hms(ms: i64) -> (u32, u32, u32) {
    // Callers pass epoch millis already shifted by the local UTC offset.
    let secs = ms.div_euclid(1000).rem_euclid(86400);
    ((secs / 3600) as u32, (secs % 3600 / 60) as u32, (secs % 60) as u32)
}

fn civil_date(ms: i64) -> String {
    let (y, m, d) = {
        let z = ms.div_euclid(86_400_000) + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        (if m <= 2 { y + 1 } else { y }, m, d)
    };
    format!("{y:04}-{m:02}-{d:02}")
}

impl UiEventHandler {
    pub fn new(out: Arc<dyn OutErr>, opts: UiOptions) -> Self {
        Self::with_clock(out, opts, Arc::new(SystemUiClock::default()))
    }

    pub fn with_clock(out: Arc<dyn OutErr>, opts: UiOptions, clock: Arc<dyn UiClock>) -> Self {
        let cursor = opts.use_cursor_control();
        let terminal_width = if opts.terminal_columns > 0 { opts.terminal_columns } else { 80 };
        let target = if cursor { terminal_width.saturating_sub(2) } else { 0 };
        let mut tracker = UiStateTracker::new(target);
        tracker.set_progress_sample_size(opts.ui_actions_shown);
        let rate = (opts.show_progress_rate_limit * 1000.0).round() as i64;
        let progress_rate_limit_ms = if cursor { rate } else { rate.max(NO_CURSES_MINIMAL_PROGRESS_RATE_LIMIT) };
        let last_refresh = clock.millis() - progress_rate_limit_ms - 1;
        let state = HandlerState {
            tracker,
            stdout_buf: Vec::new(),
            stderr_buf: Vec::new(),
            lines_in_bar: 0,
            build_running: false,
            show_progress: opts.show_progress,
            date_shown: false,
            last_refresh,
            must_refresh_after: 0,
            needs_refresh: false,
            terminal_width,
            fatal: false,
        };
        Self {
            s: Arc::new(Shared {
                out,
                color: opts.use_color(),
                cursor,
                progress_rate_limit_ms,
                minimal_update_interval: progress_rate_limit_ms.max(MINIMAL_UPDATE_INTERVAL_MILLIS),
                opts,
                clock,
                state: Mutex::new(state),
                shutdown: AtomicBool::new(false),
                thread_started: AtomicBool::new(false),
                thread: Mutex::new(None),
            }),
        }
    }

    pub fn options(&self) -> &UiOptions {
        &self.s.opts
    }

    fn term(&self) -> Term {
        Term::new(self.s.color, self.s.cursor)
    }

    fn timestamp(&self) -> String {
        let now = self.s.clock.millis();
        let (h, m, s) = civil_hms(now + self.s.clock.utc_offset_ms(now));
        format!("({h:02}:{m:02}:{s:02}) ")
    }

    fn wrapping_width(st: &HandlerState) -> usize {
        st.terminal_width.saturating_sub(1).max(1)
    }

    /// Applies `LineWrappingAnsiTerminalWriter` semantics and returns the rendered bytes and the
    /// number of newlines written.
    fn render_doc(&self, doc: &ProgressDoc, width: Option<usize>) -> (Vec<u8>, usize) {
        let mut t = self.term();
        let mut lines = 0;
        let mut pos = 0usize;
        for seg in &doc.segs {
            match seg {
                Seg::Style(s) => t.style(*s),
                Seg::Newline => {
                    t.nl();
                    lines += 1;
                    pos = 0;
                }
                Seg::Text(text) => {
                    for c in text.chars() {
                        match width {
                            Some(w) if c != '\n' && pos + 1 >= w => {
                                t.raw("\\");
                                t.nl();
                                lines += 1;
                                t.raw(&c.to_string());
                                pos = 1;
                            }
                            _ => {
                                if c == '\n' {
                                    t.nl();
                                    lines += 1;
                                    pos = 0;
                                } else {
                                    t.raw(&c.to_string());
                                    pos += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        (t.buf, lines)
    }

    fn clear_bar(&self, st: &mut HandlerState, t: &mut Term) {
        if !self.s.cursor {
            return;
        }
        for _ in 0..st.lines_in_bar {
            t.cursor_up_clear_line();
        }
        st.lines_in_bar = 0;
    }

    fn add_bar(&self, st: &mut HandlerState, t: &mut Term) {
        if self.s.opts.quiet {
            return;
        }
        st.last_refresh = self.s.clock.millis();
        let now = self.s.clock.nanos();
        st.tracker.set_now(now);
        let ts = if self.s.opts.show_timestamps { Some(self.timestamp()) } else { None };
        let mut lines = 0;
        if st.tracker.has_activities() {
            let mut doc = st.tracker.write_progress_bar(!self.s.cursor, ts.as_deref(), now);
            doc.newline();
            let wrap = if self.s.cursor { Some(Self::wrapping_width(st)) } else { None };
            let (bytes, n) = self.render_doc(&doc, wrap);
            t.buf.extend_from_slice(&bytes);
            lines = n;
        }
        st.lines_in_bar = lines;
        if self.s.opts.progress_in_terminal_title && self.s.cursor {
            let doc = st.tracker.write_progress_bar(true, None, now);
            let plain = doc.plain();
            let first = plain.split('\n').next().unwrap_or("");
            t.set_title(first);
        }
    }

    fn flush_buffers(st: &mut HandlerState, out: &dyn OutErr) -> bool {
        let mut did = false;
        if !st.stdout_buf.is_empty() {
            out.out(&std::mem::take(&mut st.stdout_buf));
            did = true;
        }
        if !st.stderr_buf.is_empty() {
            out.err(&std::mem::take(&mut st.stderr_buf));
            did = true;
        }
        did
    }

    fn write_to_stream(&self, st: &mut HandlerState, t: &mut Term, kind: EventKind, msg: &[u8]) -> bool {
        let eol = msg.iter().rposition(|&b| b == b'\n');
        let Some(eol) = eol else {
            let buf = if kind == EventKind::Stdout { &mut st.stdout_buf } else { &mut st.stderr_buf };
            buf.extend_from_slice(msg);
            return false;
        };
        self.clear_bar(st, t);
        // Flush what the terminal buffer collected so far, including the progress bar clearing.
        self.s.out.err(&std::mem::take(&mut t.buf));
        let buf = if kind == EventKind::Stdout { &mut st.stdout_buf } else { &mut st.stderr_buf };
        let mut line = std::mem::take(buf);
        line.extend_from_slice(&msg[..=eol]);
        if kind == EventKind::Stdout {
            self.s.out.out(&line);
            st.stdout_buf.extend_from_slice(&msg[eol + 1..]);
        } else {
            self.s.out.err(&line);
            st.stderr_buf.extend_from_slice(&msg[eol + 1..]);
        }
        true
    }

    fn set_kind_color(t: &mut Term, kind: EventKind) {
        match kind {
            EventKind::Fatal | EventKind::Error | EventKind::Fail => {
                t.red();
                t.bold();
            }
            EventKind::Warning | EventKind::Cancelled => t.magenta(),
            EventKind::Info => t.green(),
            EventKind::Debug => t.yellow(),
            EventKind::Subcommand => t.blue(),
            _ => t.reset(),
        }
    }

    fn maybe_add_date(&self, st: &mut HandlerState) -> Option<Event> {
        if !self.s.opts.show_timestamps || !st.build_running || st.date_shown {
            return None;
        }
        st.date_shown = true;
        Some(Event::info(format!("Current date is {}", {
            let now = self.s.clock.millis();
            civil_date(now + self.s.clock.utc_offset_ms(now))
        })))
    }

    fn handle_locked(&self, st: &mut HandlerState, event: &Event) {
        if let Some(d) = self.maybe_add_date(st) {
            self.handle_locked(st, &d);
        }
        let mut t = self.term();
        match event.kind {
            EventKind::Stdout | EventKind::Stderr => {
                let bytes = event.message.as_bytes();
                if !st.build_running {
                    if event.kind == EventKind::Stdout {
                        self.s.out.out(bytes);
                    } else {
                        self.s.out.err(bytes);
                    }
                } else {
                    let cleared = self.write_to_stream(st, &mut t, event.kind, bytes);
                    if cleared && st.show_progress && self.s.cursor {
                        self.add_bar(st, &mut t);
                    }
                    self.s.out.err(&std::mem::take(&mut t.buf));
                }
            }
            EventKind::Fatal
            | EventKind::Error
            | EventKind::Fail
            | EventKind::Warning
            | EventKind::Cancelled
            | EventKind::Info
            | EventKind::Debug
            | EventKind::Subcommand => {
                if st.show_progress && st.build_running {
                    self.clear_bar(st, &mut t);
                }
                // Anything buffered must land before this line; the bar clearing goes first.
                self.s.out.err(&std::mem::take(&mut t.buf));
                let incomplete = Self::flush_buffers(st, self.s.out.as_ref());
                if incomplete {
                    t.nl();
                }
                if self.s.opts.show_timestamps {
                    t.raw(&self.timestamp());
                }
                Self::set_kind_color(&mut t, event.kind);
                t.raw(&format!("{}: ", event.kind.name()));
                t.reset();
                if let Some(loc) = &event.location {
                    t.raw(&format!("{loc}: "));
                }
                let mut incomplete = true;
                if !event.message.is_empty() || event.location.is_none() {
                    t.raw(&event.message);
                    incomplete = !event.message.ends_with('\n');
                }
                if incomplete {
                    t.nl();
                }
                if st.show_progress && st.build_running && self.s.cursor {
                    self.add_bar(st, &mut t);
                }
                self.s.out.err(&std::mem::take(&mut t.buf));
            }
            EventKind::Progress => {
                if st.tracker.progress_bar_time_dependent() {
                    self.refresh_locked(st);
                }
            }
            EventKind::Start | EventKind::Finish | EventKind::Pass | EventKind::Timeout | EventKind::Depchecker => {}
        }
    }

    fn handle_internal(&self, event: &Event) {
        let kind = event.kind;
        if self.s.opts.quiet {
            match kind {
                EventKind::Error | EventKind::Fatal | EventKind::Stdout | EventKind::Stderr => {}
                _ => return,
            }
        }
        if self.s.opts.filtered_event_kinds.contains(&kind) {
            return;
        }
        let mut st = self.s.state.lock().unwrap();
        if self.s.opts.debug_all_events {
            let mut t = self.term();
            self.clear_bar(&mut st, &mut t);
            t.raw(&format!("{event}\n"));
            self.add_bar(&mut st, &mut t);
            self.s.out.err(&t.buf);
            return;
        }
        self.handle_locked(&mut st, event);
    }

    fn refresh_locked(&self, st: &mut HandlerState) {
        if !st.show_progress {
            return;
        }
        st.needs_refresh = true;
        self.do_refresh(st, false);
    }

    fn time_based_refresh(&self, st: &HandlerState) -> bool {
        if !st.tracker.progress_bar_time_dependent() || !self.s.cursor {
            return false;
        }
        let now = self.s.clock.millis();
        let (last, must) = (st.last_refresh, st.must_refresh_after);
        if last < must && must < now + self.s.progress_rate_limit_ms {
            return false;
        }
        last + SHORT_REFRESH_MILLIS < now
    }

    fn do_refresh(&self, st: &mut HandlerState, from_update_thread: bool) {
        if !st.build_running {
            return;
        }
        let now = self.s.clock.millis();
        if st.last_refresh + self.s.progress_rate_limit_ms < now {
            if st.show_progress && (std::mem::replace(&mut st.needs_refresh, false) || self.time_based_refresh(st)) {
                let mut t = self.term();
                self.clear_bar(st, &mut t);
                self.add_bar(st, &mut t);
                self.s.out.err(&t.buf);
            }
        } else if !from_update_thread {
            self.start_update_thread();
        }
    }

    fn ignore_refresh_limit_once(&self, st: &mut HandlerState) {
        st.last_refresh = self.s.clock.millis() - self.s.progress_rate_limit_ms - 1;
    }

    fn start_update_thread(&self) {
        // Without a cursor a refresh thread would only add more progress lines, which the
        // rate limit forbids; keep it for interactive terminals like Bazel's `cli-update-thread`.
        if !self.s.cursor || self.s.shutdown.load(Ordering::SeqCst) {
            return;
        }
        if self.s.thread_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        let handle = std::thread::Builder::new()
            .name("cli-update-thread".into())
            .spawn(move || {
                while !me.s.shutdown.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(me.s.minimal_update_interval as u64));
                    let mut st = me.s.state.lock().unwrap();
                    let must = st.must_refresh_after;
                    if st.last_refresh < must && must < me.s.clock.millis() {
                        st.needs_refresh = true;
                    }
                    me.do_refresh(&mut st, true);
                }
            })
            .ok();
        *self.s.thread.lock().unwrap() = handle;
    }

    fn stop_update_thread(&self) {
        self.s.shutdown.store(true, Ordering::SeqCst);
        let h = self.s.thread.lock().unwrap().take();
        if let Some(h) = h {
            if h.thread().id() != std::thread::current().id() {
                let _ = h.join();
            }
        }
    }

    // ----- lifecycle notifications (event bus subscribers in UiEventHandler.java) -----

    fn with_state(&self, f: impl FnOnce(&mut HandlerState)) {
        let mut st = self.s.state.lock().unwrap();
        f(&mut st);
    }

    fn phase_changed(&self, f: impl FnOnce(&mut UiStateTracker)) {
        let mut st = self.s.state.lock().unwrap();
        f(&mut st.tracker);
        self.ignore_refresh_limit_once(&mut st);
        self.refresh_locked(&mut st);
        drop(st);
        self.start_update_thread();
    }

    pub fn main_repo_mapping_computation_started(&self) {
        self.with_state(|st| st.build_running = true);
        self.phase_changed(|t| t.main_repo_mapping_computation_started());
    }

    pub fn build_started(&self) {
        self.with_state(|st| st.build_running = true);
        self.phase_changed(|t| t.build_started());
    }

    pub fn loading_started(&self, with_package_progress: bool) {
        self.phase_changed(|t| t.loading_started(with_package_progress));
    }

    pub fn configuration_started(&self) {
        self.phase_changed(|t| t.configuration_started());
    }

    pub fn loading_complete(&self, labels: &[String]) {
        self.phase_changed(|t| t.loading_complete(labels));
    }

    /// Reports the analysis summary as an INFO event and clears the analysis status.
    pub fn analysis_complete(&self) {
        let msg = {
            let mut st = self.s.state.lock().unwrap();
            st.tracker.analysis_complete()
        };
        self.handle(&Event::info(msg));
    }

    /// Like [`Self::analysis_complete`] without reporting the summary line (the caller already
    /// printed it).
    pub fn analysis_complete_silent(&self) {
        let mut st = self.s.state.lock().unwrap();
        let _ = st.tracker.analysis_complete();
    }

    pub fn execution_phase_started(&self) {
        self.phase_changed(|t| t.execution_phase_started());
    }

    pub fn package_loading_started(&self, pkg: &str) {
        self.with_state(|st| st.tracker.package_loading_started(pkg));
        self.refresh_soon();
    }

    pub fn package_loaded(&self, pkg: &str) {
        self.with_state(|st| st.tracker.package_loaded(pkg));
        self.refresh_soon();
    }

    pub fn target_configured(&self) {
        self.with_state(|st| st.tracker.target_configured_progress(1));
        self.refresh_soon();
    }

    fn refresh_soon(&self) {
        let mut st = self.s.state.lock().unwrap();
        let now = self.s.clock.millis();
        let last = st.last_refresh;
        if st.must_refresh_after <= last {
            st.must_refresh_after = (now + 1).max(last + self.s.minimal_update_interval);
        }
        st.needs_refresh = true;
        self.do_refresh(&mut st, false);
        drop(st);
        self.start_update_thread();
    }

    pub fn action_started(&self, id: &str, message: &str, owner: Option<&str>, mnemonic: &str) {
        let now = self.s.clock.nanos();
        self.with_state(|st| {
            st.tracker.set_now(now);
            st.tracker.action_started(id, message, owner, mnemonic);
        });
        self.refresh_soon();
    }

    pub fn action_phase(&self, id: &str, phase: ActionPhase, strategy: &str) {
        self.with_state(|st| match phase {
            ActionPhase::Scanning => st.tracker.scanning_action(id),
            ActionPhase::Caching => st.tracker.checking_action_cache(id),
            ActionPhase::Uploading => st.tracker.uploading_action(id, strategy),
            ActionPhase::Scheduling => st.tracker.scheduling_action(id, strategy),
            ActionPhase::Running => st.tracker.running_action(id, strategy),
            ActionPhase::Preparing => st.tracker.stop_scanning_action(id),
        });
        self.refresh_soon();
    }

    pub fn action_progress(&self, id: &str, progress_id: &str, message: &str, finished: bool) {
        self.with_state(|st| st.tracker.action_progress(id, progress_id, message, finished));
        self.refresh_soon();
    }

    pub fn action_completed(&self, id: &str) {
        self.with_state(|st| st.tracker.action_completed(id));
        self.refresh_soon();
    }

    pub fn set_total_actions(&self, n: u64) {
        self.with_state(|st| st.tracker.set_total_actions(n));
    }

    pub fn test_filtering_complete(&self, num_tests: u64) {
        self.with_state(|st| st.tracker.test_filtering_complete(num_tests));
    }

    /// Reports a finished test target; failing summaries and new information are shown by the
    /// caller as events.
    pub fn test_summary(&self, passed: bool) {
        self.with_state(|st| st.tracker.test_summary(passed));
        self.refresh_soon();
    }

    pub fn download_started(&self, url: &str) {
        let now = self.s.clock.nanos();
        self.with_state(|st| st.tracker.download_started(url, now));
        self.refresh_soon();
    }

    pub fn download_finished(&self, url: &str) {
        self.with_state(|st| st.tracker.download_finished(url));
        self.refresh_soon();
    }

    pub fn bep_transports_announced(&self, names: Vec<String>) {
        let msg = if names.is_empty() { None } else { Some(format!("Streaming build results to: {}", "")) };
        let _ = msg;
        self.with_state(|st| st.tracker.bep_transports_announced(names));
    }

    pub fn bep_transport_closed(&self, name: &str) {
        self.with_state(|st| st.tracker.bep_transport_closed(name));
    }

    /// `buildComplete`: emits the completion INFO/ERROR line and stops progress.
    pub fn build_complete(&self, success: bool) {
        let ev = {
            let mut st = self.s.state.lock().unwrap();
            let now = self.s.clock.nanos();
            st.tracker.set_now(now);
            st.tracker.build_complete(success, now)
        };
        self.complete_build(ev);
    }

    fn complete_build(&self, ev: Event) {
        self.stop_update_thread();
        let mut st = self.s.state.lock().unwrap();
        // Clear the bar, then print the final line without a new bar.
        let mut t = self.term();
        self.clear_bar(&mut st, &mut t);
        self.s.out.err(&std::mem::take(&mut t.buf));
        st.build_running = false;
        drop(st);
        self.handle(&ev);
        self.flush_all();
    }

    /// `afterCommand`: flush buffered partial lines and stop the update thread.
    pub fn after_command(&self) {
        self.stop_update_thread();
        self.flush_all();
    }

    fn flush_all(&self) {
        let mut st = self.s.state.lock().unwrap();
        let mut t = self.term();
        self.clear_bar(&mut st, &mut t);
        self.s.out.err(&std::mem::take(&mut t.buf));
        st.build_running = false;
        Self::flush_buffers(&mut st, self.s.out.as_ref());
        self.s.out.flush();
    }

    /// Temporarily suppresses progress, clearing the bar; returns whether it was enabled.
    pub fn disable_progress(&self) -> bool {
        let mut st = self.s.state.lock().unwrap();
        if !st.show_progress {
            return false;
        }
        let mut t = self.term();
        self.clear_bar(&mut st, &mut t);
        self.s.out.err(&t.buf);
        st.show_progress = false;
        true
    }

    pub fn enable_progress(&self) {
        let mut st = self.s.state.lock().unwrap();
        if st.show_progress || !self.s.opts.show_progress {
            return;
        }
        st.show_progress = true;
        let mut t = self.term();
        self.add_bar(&mut st, &mut t);
        self.s.out.err(&t.buf);
    }

    /// Terminal resize notification (`terminalSizeChanged`).
    pub fn terminal_size_changed(&self, columns: usize) {
        let new = if columns > 0 { columns } else { 80 };
        let mut st = self.s.state.lock().unwrap();
        if st.terminal_width == new {
            return;
        }
        let mut t = self.term();
        if st.show_progress && st.build_running && self.s.cursor {
            self.clear_bar(&mut st, &mut t);
        }
        st.terminal_width = new;
        if self.s.cursor {
            st.tracker.set_target_width(new.saturating_sub(2));
        }
        self.ignore_refresh_limit_once(&mut st);
        st.needs_refresh = true;
        if st.show_progress && st.build_running && self.s.cursor {
            self.add_bar(&mut st, &mut t);
        }
        self.s.out.err(&t.buf);
    }

    /// Current plain-text progress bar (for tests and `--progress_in_terminal_title`).
    pub fn progress_text(&self, short: bool) -> String {
        let mut st = self.s.state.lock().unwrap();
        let now = self.s.clock.nanos();
        st.tracker.set_now(now);
        st.tracker.write_progress_bar(short, None, now).plain()
    }
}

impl EventHandler for UiEventHandler {
    fn handle(&self, event: &Event) {
        if !self.s.opts.debug_all_events
            && !self.s.opts.show_timestamps
            && matches!(
                event.kind,
                EventKind::Start | EventKind::Finish | EventKind::Pass | EventKind::Timeout | EventKind::Depchecker
            )
        {
            return;
        }
        if event.kind == EventKind::Fatal {
            self.s.state.lock().unwrap().fatal = true;
            self.s.state.lock().unwrap().build_running = false;
            self.stop_update_thread();
        }
        self.handle_internal(event);
    }

    fn flush(&self) {
        self.flush_all();
    }
}

/// Converts pre-formatted console text (`KIND: message` lines, as produced by emitters that print
/// directly) back into [`Event`]s so that colour, `--ui_event_filters`, `--show_timestamps` and
/// the rest of the UI pipeline apply uniformly. Lines without a recognised kind prefix become
/// `STDERR` events carrying the line verbatim.
pub fn events_from_console_text(text: &str) -> Vec<Event> {
    const KINDS: [EventKind; 9] = [
        EventKind::Fatal,
        EventKind::Error,
        EventKind::Warning,
        EventKind::Info,
        EventKind::Debug,
        EventKind::Fail,
        EventKind::Pass,
        EventKind::Subcommand,
        EventKind::Cancelled,
    ];
    let mut out = Vec::new();
    for line in text.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let hit = KINDS.iter().find_map(|k| body.strip_prefix(k.prefix().as_str()).map(|rest| (*k, rest)));
        match hit {
            Some((k, rest)) if line.ends_with('\n') => {
                out.push(Event { kind: k, location: None, message: rest.to_string(), tag: None })
            }
            _ => out.push(Event::stderr(line)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler(opts: UiOptions) -> (UiEventHandler, Arc<BufferOutErr>, Arc<ManualClock>) {
        let out = Arc::new(BufferOutErr::default());
        let clock = Arc::new(ManualClock::default());
        let h = UiEventHandler::with_clock(out.clone(), opts, clock.clone());
        (h, out, clock)
    }

    fn curses_opts() -> UiOptions {
        UiOptions {
            curses: TriState::Yes,
            color: TriState::Yes,
            terminal_columns: 80,
            ..Default::default()
        }
    }

    #[test]
    fn no_color_plain_lines() {
        let (h, out, _) = handler(UiOptions { color: TriState::No, curses: TriState::No, ..Default::default() });
        h.handle(&Event::info("hello"));
        h.handle(&Event::warning(None, "careful"));
        h.handle(&Event::error(None, "bad"));
        h.handle(&Event::debug("dbg"));
        assert_eq!(out.err_string(), "INFO: hello\nWARNING: careful\nERROR: bad\nDEBUG: dbg\n");
    }

    #[test]
    fn color_sequences_match_bazel() {
        let (h, out, _) = handler(UiOptions { color: TriState::Yes, curses: TriState::No, ..Default::default() });
        h.handle(&Event::error(None, "x"));
        h.handle(&Event::info("y"));
        assert_eq!(
            out.err_string(),
            "\x1b[31m\x1b[1mERROR: \x1b[0mx\n\x1b[32mINFO: \x1b[0my\n"
        );
    }

    #[test]
    fn auto_color_follows_isatty() {
        let (h, out, _) = handler(UiOptions { isatty: true, curses: TriState::No, ..Default::default() });
        h.handle(&Event::warning(None, "w"));
        assert!(out.err_string().starts_with("\x1b[35mWARNING: "));
        let (h, out, _) = handler(UiOptions { isatty: false, ..Default::default() });
        h.handle(&Event::warning(None, "w"));
        assert_eq!(out.err_string(), "WARNING: w\n");
    }

    #[test]
    fn event_filters_and_timestamps() {
        let mut o = UiOptions { color: TriState::No, curses: TriState::No, show_timestamps: true, ..Default::default() };
        o.filtered_event_kinds = parse_event_filters(&["-info".to_string()]).unwrap();
        let (h, out, _) = handler(o);
        h.handle(&Event::info("hidden"));
        h.handle(&Event::error(None, "shown"));
        assert_eq!(out.err_string(), "(03:04:05) ERROR: shown\n");
        assert!(parse_event_filters(&["bogus".to_string()]).is_err());
        let f = parse_event_filters(&["error".to_string()]).unwrap();
        assert!(!f.contains(&EventKind::Error) && f.contains(&EventKind::Info));
        let f = parse_event_filters(&["-info,-warning".to_string()]).unwrap();
        assert!(f.contains(&EventKind::Info) && f.contains(&EventKind::Warning) && !f.contains(&EventKind::Error));
    }

    #[test]
    fn date_line_with_timestamps_while_building() {
        let (h, out, _) = handler(UiOptions { color: TriState::No, curses: TriState::No, show_timestamps: true, show_progress: false, ..Default::default() });
        h.build_started();
        h.handle(&Event::info("a"));
        let s = out.err_string();
        assert!(s.starts_with("(03:04:05) INFO: Current date is 2024-01-02\n(03:04:05) INFO: a\n"), "{s}");
    }

    #[test]
    fn progress_text_and_curses_redraw() {
        let (h, out, clock) = handler(curses_opts());
        h.build_started();
        h.loading_started(true);
        h.package_loading_started("//foo");
        let t = h.progress_text(false);
        assert_eq!(t, "Loading: 0 packages loaded\n    currently loading: //foo");
        h.package_loaded("//foo");
        h.loading_complete(&["//foo:bar".into()]);
        h.configuration_started();
        h.target_configured();
        clock.advance(Duration::from_millis(300));
        h.handle(&Event::info("tick"));
        h.analysis_complete();
        let e = out.err_string();
        assert!(e.contains("Analyzed target //foo:bar (1 packages loaded, 1 target configured)."), "{e}");
        // A line event while a bar is displayed clears it with CR, up, erase-line.
        assert!(e.contains("\r\x1b[1A\x1b[K"), "{e:?}");
    }

    #[test]
    fn action_progress_text() {
        let (h, _, clock) = handler(curses_opts());
        h.build_started();
        h.analysis_complete();
        h.execution_phase_started();
        h.set_total_actions(10);
        h.action_started("a1", "Compiling foo.cc", Some("//x:y"), "CppCompile");
        h.action_phase("a1", ActionPhase::Running, "sandbox");
        clock.advance(Duration::from_secs(5));
        let t = h.progress_text(false);
        assert_eq!(t, "[0 / 10] Compiling foo.cc; 5s (sandbox)");
        h.action_started("a2", "Linking bar", Some("//x:y"), "CppLink");
        h.action_phase("a2", ActionPhase::Scheduling, "local");
        let t = h.progress_text(false);
        assert!(t.starts_with("[0 / 10] 2 actions, 1 running\n    Compiling foo.cc; 5s (sandbox)\n    [Sched] Linking bar; 0s (local)"), "{t}");
        h.action_completed("a1");
        h.action_completed("a2");
        let t = h.progress_text(true);
        assert_eq!(t, "[2 / 10] checking cached actions");
        h.set_total_actions(2);
        assert_eq!(h.progress_text(true), "[2 / 2] no actions running");
        h.build_complete(true);
    }

    #[test]
    fn completion_line_counts_actions() {
        let (h, out, _) = handler(UiOptions { color: TriState::No, curses: TriState::No, ..Default::default() });
        h.build_started();
        h.analysis_complete();
        h.execution_phase_started();
        h.action_started("a", "x", None, "M");
        h.action_completed("a");
        h.build_complete(true);
        assert!(out.err_string().contains("INFO: Build completed successfully, 1 total action\n"), "{}", out.err_string());
        let (h, out, _) = handler(UiOptions { color: TriState::No, curses: TriState::No, ..Default::default() });
        h.build_started();
        h.build_complete(false);
        assert!(out.err_string().contains("ERROR: Build did NOT complete successfully\n"));
    }

    #[test]
    fn stdout_stderr_line_buffering_and_partial_flush() {
        let (h, out, _) = handler(UiOptions { color: TriState::No, curses: TriState::No, show_progress: false, ..Default::default() });
        h.build_started();
        h.handle(&Event::stderr("par"));
        h.handle(&Event::stderr("tial\nrest"));
        assert_eq!(out.err_string(), "partial\n");
        // An INFO line forces the incomplete line out first, with a newline.
        h.handle(&Event::info("x"));
        assert_eq!(out.err_string(), "partial\nrest\nINFO: x\n");
    }

    #[test]
    fn rate_limit_without_curses_is_one_second() {
        let (h, out, clock) = handler(UiOptions { color: TriState::No, curses: TriState::No, ..Default::default() });
        h.build_started();
        h.analysis_complete();
        h.execution_phase_started();
        h.set_total_actions(3);
        h.action_started("a", "Compiling a.cc", None, "M");
        h.action_phase("a", ActionPhase::Running, "local");
        let n1 = out.err_string().matches("[0 / 3]").count();
        h.action_started("b", "Compiling b.cc", None, "M");
        h.action_started("c", "Compiling c.cc", None, "M");
        // Within the limit: nothing more is printed.
        assert_eq!(out.err_string().matches("[0 / 3]").count(), n1);
        let _ = clock;
    }

    #[test]
    fn options_from_flags() {
        let map: std::collections::HashMap<&str, &str> = [
            ("color", "yes"),
            ("curses", "no"),
            ("show_progress", "false"),
            ("show_progress_rate_limit", "1.5"),
            ("experimental_ui_max_stdouterr_bytes", "-1"),
            ("terminal_columns", "120"),
        ]
        .into_iter()
        .collect();
        let o = UiOptions::from_flags(&|n| map.get(n).map(|s| s.to_string()), &|n| {
            if n == "ui_event_filters" { vec!["-debug".to_string()] } else { vec![] }
        })
        .unwrap();
        assert!(o.use_color() && !o.use_cursor_control() && !o.show_progress);
        assert_eq!(o.terminal_columns, 120);
        assert_eq!(o.max_stdouterr_bytes, ((i32::MAX - 8) >> 1) as usize);
        assert!(o.filtered_event_kinds.contains(&EventKind::Debug));
        assert!(UiOptions::from_flags(&|n| (n == "color").then(|| "maybe".to_string()), &|_| vec![]).is_err());
        assert_eq!(format_count(9_999), "9999");
        assert_eq!(format_count(12_345), "12,345");
        assert_eq!(group(1_234), "1,234");
    }

    #[test]
    fn console_text_roundtrips_through_events() {
        let text = "INFO: a\nTarget //x up-to-date:\n  bazel-bin/x\nERROR: boom\nlast partial";
        let evs = events_from_console_text(text);
        assert_eq!(evs.len(), 5);
        assert_eq!(evs[0].kind, EventKind::Info);
        assert_eq!(evs[1].kind, EventKind::Stderr);
        assert_eq!(evs[3].message, "boom");
        let (h, out, _) = handler(UiOptions { color: TriState::No, curses: TriState::No, ..Default::default() });
        for e in &evs {
            h.handle(e);
        }
        h.after_command();
        assert_eq!(out.err_string(), "INFO: a\nTarget //x up-to-date:\n  bazel-bin/x\nERROR: boom\nlast partial");
        let (h, out, _) = handler(UiOptions { color: TriState::Yes, curses: TriState::No, ..Default::default() });
        for e in &evs {
            h.handle(e);
        }
        assert!(out.err_string().starts_with("\x1b[32mINFO: \x1b[0ma\n"));
    }

    #[test]
    fn wrapping_inserts_continuation() {
        let (h, _, _) = handler(UiOptions { curses: TriState::Yes, color: TriState::No, terminal_columns: 10, ..Default::default() });
        let mut doc = ProgressDoc::default();
        doc.append("0123456789ab");
        let (bytes, lines) = h.render_doc(&doc, Some(9));
        assert_eq!(String::from_utf8(bytes).unwrap(), "01234567\\\n89ab");
        assert_eq!(lines, 1);
    }
}
