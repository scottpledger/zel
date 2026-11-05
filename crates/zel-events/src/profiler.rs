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

//! JSON trace profiler (`profiler/Profiler.java`, `JsonTraceFileWriter.java`) and the
//! `analyze-profile` report.
//!
//! The writer reproduces Bazel's trace layout: an `otherData` header, `thread_name` /
//! `thread_sort_index` metadata for the fake critical-path thread and every recording thread,
//! `X`/`i` task events with `cat`, `out` and `args` fields, `C` counter events, optional
//! slim-profile merging, and gzip output.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Mutex;
use std::time::Instant;
use zel_util::json::{self, Json};
use zel_vfs::{FileSystem, PathFragment};

macro_rules! tasks {
    ($( $v:ident = $desc:expr, $min:expr, $slow:expr; )*) => {
        /// `ProfilerTask.java`.
        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum ProfilerTask { $( $v, )* }
        impl ProfilerTask {
            pub const ALL: &'static [ProfilerTask] = &[ $( ProfilerTask::$v, )* ];
            pub fn description(&self) -> &'static str { match self { $( ProfilerTask::$v => $desc, )* } }
            /// Minimum duration in nanoseconds below which events are dropped (-1 = no limit).
            pub fn min_duration_nanos(&self) -> i64 { match self { $( ProfilerTask::$v => $min, )* } }
            pub fn collects_slowest_instances(&self) -> bool { match self { $( ProfilerTask::$v => $slow, )* } }
            pub fn enum_name(&self) -> &'static str { match self { $( ProfilerTask::$v => stringify!($v), )* } }
        }
    };
}

const TEN: i64 = 10_000_000;
const FIFTY: i64 = 50_000_000;

tasks! {
    PHASE = "build phase marker", -1, false;
    ACTION = "action processing", -1, false;
    DISCOVER_INPUTS = "discover inputs", -1, false;
    ACTION_CHECK = "action dependency checking", TEN, false;
    ACTION_LOCK = "action resource lock", TEN, false;
    ACTION_UPDATE = "update action information", TEN, false;
    ACTION_COMPLETE = "complete action execution", -1, false;
    ACTION_REWINDING = "prepare action rewind plan", -1, false;
    BZLMOD = "bazel module processing", -1, false;
    INFO = "general information", -1, false;
    CREATE_PACKAGE = "package creation", -1, false;
    REMOTE_EXECUTION = "remote action execution", -1, false;
    LOCAL_EXECUTION = "local action execution", -1, false;
    SCANNER = "include scanner", -1, false;
    LOCAL_PARSE = "Local parse to prepare for remote execution", FIFTY, true;
    UPLOAD_TIME = "Remote execution upload time", FIFTY, false;
    REMOTE_PROCESS_TIME = "Remote execution process wall time", FIFTY, false;
    REMOTE_QUEUE = "Remote execution queuing time", FIFTY, false;
    REMOTE_SETUP = "Remote execution setup", FIFTY, false;
    FETCH = "Remote execution file fetching", FIFTY, false;
    LOCAL_PROCESS_TIME = "Local execution process wall time", FIFTY, false;
    VFS_STAT = "VFS stat", TEN, true;
    VFS_DIR = "VFS readdir", TEN, true;
    VFS_READLINK = "VFS readlink", TEN, true;
    VFS_MD5 = "VFS md5", TEN, true;
    VFS_XATTR = "VFS xattr", TEN, true;
    VFS_DELETE = "VFS delete", TEN, false;
    VFS_OPEN = "VFS open", TEN, true;
    VFS_READ = "VFS read", TEN, true;
    VFS_WRITE = "VFS write", TEN, true;
    VFS_GLOB = "globbing", -1, true;
    VFS_VMFS_STAT = "VMFS stat", TEN, false;
    VFS_VMFS_DIR = "VMFS readdir", TEN, false;
    VFS_VMFS_READ = "VMFS read", TEN, false;
    WAIT = "thread wait", TEN, false;
    THREAD_NAME = "thread name", -1, false;
    THREAD_SORT_INDEX = "thread sort index", -1, false;
    SKYFRAME_EVAL = "skyframe evaluator", -1, false;
    SKYFUNCTION = "skyfunction", -1, false;
    CRITICAL_PATH = "critical path", -1, false;
    CRITICAL_PATH_COMPONENT = "critical path component", -1, false;
    MANUAL_GC = "manual GC", -1, false;
    HANDLE_GC_NOTIFICATION = "gc notification", -1, false;
    LOCAL_ACTION_COUNTS = "action count (local)", -1, false;
    STARLARK_PARSER = "Starlark Parser", FIFTY, false;
    STARLARK_USER_FN = "Starlark user function call", FIFTY, false;
    STARLARK_BUILTIN_FN = "Starlark builtin function call", FIFTY, false;
    STARLARK_USER_COMPILED_FN = "Starlark compiled user function call", FIFTY, false;
    STARLARK_REPOSITORY_FN = "Starlark repository function call", FIFTY, false;
    STARLARK_THREAD_CONTEXT = "Starlark thread context", FIFTY, false;
    ACTION_FS_STAGING = "Staging per-action file system", -1, false;
    REMOTE_CACHE_CHECK = "remote action cache check", -1, false;
    REMOTE_DOWNLOAD = "remote output download", TEN, false;
    REMOTE_NETWORK = "remote network", -1, false;
    FILESYSTEM_TRAVERSAL = "filesystem traversal", -1, false;
    WORKER_EXECUTION = "local execution in worker", -1, false;
    WORKER_SETUP = "setting up inputs for worker", -1, false;
    WORKER_BORROW = "borrowing a worker", -1, false;
    WORKER_WORKING = "waiting for response from worker", -1, false;
    WORKER_COPYING_OUTPUTS = "copying outputs from worker", -1, false;
    SANDBOX_SETUP = "Sandbox setup", FIFTY, false;
    SANDBOX_PROCESS_TIME = "Sandbox execution process wall time", FIFTY, false;
    SANDBOX_OUTPUTS = "Sandbox copying outputs", FIFTY, false;
    CREDENTIAL_HELPER = "calling credential helper", -1, false;
    CONFLICT_CHECK = "Conflict checking", -1, false;
    DYNAMIC_LOCK = "Acquiring dynamic execution output lock", FIFTY, false;
    REPOSITORY_FETCH = "Fetching repository", -1, false;
    REPOSITORY_VENDOR = "Vendoring repository", -1, false;
    REPO_CACHE_GC_WAIT = "blocked on repo contents cache GC", TEN, false;
    SPAWN_LOG = "logging spawn", TEN, false;
    RPC = "RPC", -1, false;
    SKYCACHE = "Skycache", -1, false;
    WASM_LOAD = "load WebAssembly module", -1, false;
    WASM_EXEC = "execute WebAssembly function", -1, false;
    UNKNOWN = "Unknown event", -1, false;
}

impl ProfilerTask {
    pub fn is_vfs(&self) -> bool {
        self.enum_name().starts_with("VFS")
    }

    /// Parses an enum value name case-insensitively (`--experimental_profile_additional_tasks`).
    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| t.enum_name().eq_ignore_ascii_case(s))
    }

    pub fn from_description(d: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| t.description() == d)
    }
}

/// `ProfilePhase.java`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfilePhase {
    pub nick: &'static str,
    pub description: &'static str,
}

impl ProfilePhase {
    pub const LAUNCH: Self = Self { nick: "launch", description: "Launch Blaze" };
    pub const INIT: Self = Self { nick: "init", description: "Initialize command" };
    pub const TARGET_PATTERN_EVAL: Self = Self { nick: "target pattern evaluation", description: "Evaluate target patterns" };
    pub const ANALYZE: Self = Self { nick: "interleaved loading-and-analysis", description: "Load and analyze dependencies" };
    pub const ANALYZE_AND_EXECUTE: Self = Self {
        nick: "interleaved loading, analysis and execution",
        description: "Load, analyze dependencies and build artifacts",
    };
    pub const LICENSE: Self = Self { nick: "license checking", description: "Analyze licenses" };
    pub const PREPARE: Self = Self { nick: "preparation", description: "Prepare for build" };
    pub const EXECUTE: Self = Self { nick: "execution", description: "Build artifacts" };
    pub const FINISH: Self = Self { nick: "finish", description: "Complete build" };
    pub const UNKNOWN: Self = Self { nick: "unknown", description: "unknown" };
    pub const ALL: [Self; 10] = [
        Self::LAUNCH,
        Self::INIT,
        Self::TARGET_PATTERN_EVAL,
        Self::ANALYZE,
        Self::ANALYZE_AND_EXECUTE,
        Self::LICENSE,
        Self::PREPARE,
        Self::EXECUTE,
        Self::FINISH,
        Self::UNKNOWN,
    ];

    pub fn from_description(d: &str) -> Self {
        Self::ALL.iter().copied().find(|p| p.description == d).unwrap_or(Self::UNKNOWN)
    }
}

/// `--[no]slim_profile` / `--experimental_slim_json_profile`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlimProfile {
    pub enabled: bool,
    /// Bytes after which merging starts; `<= 0` means merge after 10,000 events.
    pub size_limit: i64,
}

impl SlimProfile {
    pub const DISABLED: Self = Self { enabled: false, size_limit: -1 };
    pub const ALWAYS: Self = Self { enabled: true, size_limit: -1 };

    pub fn has_size_limit(&self) -> bool {
        self.enabled && self.size_limit > 0
    }

    /// Parses `true`/`false`/`yes`/`no`/`1`/`0` or `<n>[k|m|g]`/`<n>` byte size.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" | "always" => return Ok(Self::ALWAYS),
            "false" | "no" | "0" | "never" | "" => return Ok(Self::DISABLED),
            _ => {}
        }
        let l = s.to_ascii_lowercase();
        let (num, mul) = if let Some(n) = l.strip_suffix('k') {
            (n, 1024)
        } else if let Some(n) = l.strip_suffix('m') {
            (n, 1024 * 1024)
        } else if let Some(n) = l.strip_suffix('g') {
            (n, 1024 * 1024 * 1024)
        } else {
            (l.as_str(), 1)
        };
        num.parse::<i64>()
            .map(|n| Self { enabled: true, size_limit: n * mul })
            .map_err(|_| format!("Invalid --slim_profile value: {s}"))
    }
}

const SLIM_EVENT_THRESHOLD: u64 = 10_000;
const SLIM_MAX_PAUSE_NS: i64 = 100_000_000;
const SLIM_MAX_DURATION_NS: i64 = 250_000_000;
pub const CRITICAL_PATH_THREAD_ID: u64 = 0;

/// Options controlling what is profiled and how it is written.
#[derive(Debug, Clone)]
pub struct ProfilerOptions {
    pub profiled_tasks: HashSet<ProfilerTask>,
    pub compress: bool,
    pub slim: SlimProfile,
    pub include_primary_output: bool,
    pub include_target_label: bool,
    pub include_configuration: bool,
    pub bazel_version: String,
    pub build_id: String,
    pub output_base: String,
}

impl ProfilerOptions {
    /// The default task set when profiling is on (`BlazeRuntime.initProfiler`): everything
    /// except VFS tasks, `CRITICAL_PATH` and `SKYFUNCTION`, plus `additional` and, if
    /// `record_full`, all tasks.
    pub fn default_tasks(additional: &[ProfilerTask], record_full: bool) -> HashSet<ProfilerTask> {
        let mut s: HashSet<ProfilerTask> = ProfilerTask::ALL
            .iter()
            .copied()
            .filter(|t| !t.is_vfs() && *t != ProfilerTask::CRITICAL_PATH && *t != ProfilerTask::SKYFUNCTION)
            .collect();
        s.extend(additional.iter().copied());
        if record_full {
            s.extend(ProfilerTask::ALL.iter().copied());
        }
        s
    }

    pub fn new(build_id: &str, output_base: &str) -> Self {
        Self {
            profiled_tasks: Self::default_tasks(&[], false),
            compress: true,
            slim: SlimProfile::ALWAYS,
            include_primary_output: false,
            include_target_label: false,
            include_configuration: false,
            bazel_version: crate::BUILD_TOOL_VERSION.to_string(),
            build_id: build_id.to_string(),
            output_base: output_base.to_string(),
        }
    }

    /// Builds options from flags: `--profile`, `--slim_profile`, `--experimental_profile_*`,
    /// `--record_full_profiler_data`, `--experimental_json_trace_compression`.
    pub fn from_flags(
        get: &dyn Fn(&str) -> Option<String>,
        get_all: &dyn Fn(&str) -> Vec<String>,
        build_id: &str,
        output_base: &str,
    ) -> Result<Self, String> {
        let b = |n: &str, d: bool| match get(n).as_deref() {
            Some("0") | Some("false") | Some("no") => false,
            Some(_) => true,
            None => d,
        };
        let mut additional = Vec::new();
        for v in get_all("experimental_profile_additional_tasks") {
            for name in v.split(',').filter(|s| !s.is_empty()) {
                additional.push(
                    ProfilerTask::from_name(name)
                        .ok_or_else(|| format!("Unknown profiler task: {name}"))?,
                );
            }
        }
        let mut o = Self::new(build_id, output_base);
        o.profiled_tasks = Self::default_tasks(&additional, b("record_full_profiler_data", false));
        let slim = get("slim_profile").or_else(|| get("experimental_slim_json_profile"));
        o.slim = match slim {
            Some(s) => SlimProfile::parse(&s)?,
            None => SlimProfile::ALWAYS,
        };
        o.include_primary_output = b("experimental_profile_include_primary_output", false);
        o.include_target_label = b("experimental_profile_include_target_label", false);
        o.include_configuration = b("experimental_profile_include_target_configuration", false);
        o.compress = match get("profile") {
            Some(p) if !p.is_empty() => p.ends_with(".gz"),
            _ => true,
        };
        Ok(o)
    }
}

/// Decides where the profile goes (`--profile`, `--generate_json_trace_profile`).
///
/// Returns `(path, compressed)`. With `--profile` unset and generation `auto`/`true`, the profile
/// goes to `<output_base>/command.profile.gz`.
pub fn resolve_profile_path(
    profile_flag: Option<&str>,
    generate_json_trace_profile: Option<&str>,
    output_base: &str,
    command_is_build_like: bool,
) -> Option<(String, bool)> {
    let enabled = match generate_json_trace_profile.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("true") | Some("yes") | Some("1") => true,
        Some("false") | Some("no") | Some("0") => false,
        _ => profile_flag.map(|p| !p.is_empty()).unwrap_or(command_is_build_like),
    };
    if !enabled {
        return None;
    }
    match profile_flag {
        Some(p) if !p.is_empty() => Some((p.to_string(), p.ends_with(".gz"))),
        _ => Some((format!("{output_base}/command.profile.gz"), true)),
    }
}

#[derive(Debug, Clone)]
struct ActionData {
    mnemonic: Option<String>,
    primary_output: Option<String>,
    target_label: Option<String>,
    configuration: Option<String>,
}

#[derive(Debug, Clone)]
enum Rec {
    Thread { tid: u64, name: String, sort: i64 },
    Task { tid: u64, start_ns: i64, dur_ns: i64, task: ProfilerTask, desc: String, action: Option<ActionData> },
    Counter { json: String },
}

#[derive(Default)]
struct Inner {
    recs: Vec<Rec>,
    threads: HashMap<std::thread::ThreadId, u64>,
    next_tid: u64,
}

/// A recording profiler. Cheap to call from many threads.
pub struct Profiler {
    opts: ProfilerOptions,
    start: Instant,
    start_epoch_ms: i64,
    inner: Mutex<Inner>,
}

fn thread_sort_index(name: &str) -> i64 {
    if name.starts_with("grpc-command") || name == "Main Thread" {
        return 1;
    }
    if name == "Notification Thread" || name == "Garbage Collector" {
        return 2;
    }
    let digits: String = name.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    let Ok(n) = digits.parse::<i64>() else { return 1_000_000 };
    if name.starts_with("skyframe-evaluator") {
        let shift = if name.starts_with("skyframe-evaluator-cpu-heavy") {
            20_000
        } else if name.starts_with("skyframe-evaluator-execution") {
            30_000
        } else if name.starts_with("skyframe-evaluator-memoizing") {
            40_000
        } else {
            10_000
        };
        return shift + n;
    }
    if name.starts_with("dynamic-execution") {
        return 100_000 + n;
    }
    if name.starts_with("Include scanner") {
        return 200_000 + n;
    }
    1_000_000
}

fn readable_thread_name(name: &str) -> String {
    if name.starts_with("grpc-command") {
        "Main Thread".into()
    } else if name == "Notification Thread" {
        "Garbage Collector".into()
    } else {
        name.to_string()
    }
}

impl Profiler {
    pub fn new() -> Self {
        Self::with_options(ProfilerOptions::new(&uuid_like(), ""))
    }

    pub fn with_options(opts: ProfilerOptions) -> Self {
        Self {
            opts,
            start: Instant::now(),
            start_epoch_ms: crate::now_millis(),
            inner: Mutex::new(Inner { next_tid: 1, ..Default::default() }),
        }
    }

    pub fn options(&self) -> &ProfilerOptions {
        &self.opts
    }

    pub fn is_profiling(&self, t: ProfilerTask) -> bool {
        self.opts.profiled_tasks.contains(&t)
    }

    /// Nanoseconds since profile start (monotonic).
    pub fn now_ns(&self) -> i64 {
        self.start.elapsed().as_nanos() as i64
    }

    fn tid(&self, inner: &mut Inner) -> u64 {
        let id = std::thread::current().id();
        if let Some(t) = inner.threads.get(&id) {
            return *t;
        }
        let tid = inner.next_tid;
        inner.next_tid += 1;
        inner.threads.insert(id, tid);
        let name = std::thread::current().name().map(str::to_string).unwrap_or_else(|| format!("Thread-{tid}"));
        let sort = thread_sort_index(&name);
        inner.recs.push(Rec::Thread { tid, name: readable_thread_name(&name), sort });
        tid
    }

    /// Names the calling thread in the profile (e.g. `"Main Thread"`).
    pub fn name_current_thread(&self, name: &str) {
        let mut inner = self.inner.lock().unwrap();
        let id = std::thread::current().id();
        let tid = match inner.threads.get(&id) {
            Some(t) => *t,
            None => {
                let t = inner.next_tid;
                inner.next_tid += 1;
                inner.threads.insert(id, t);
                t
            }
        };
        inner.recs.push(Rec::Thread { tid, name: readable_thread_name(name), sort: thread_sort_index(name) });
    }

    fn push_task(&self, task: ProfilerTask, start_ns: i64, dur_ns: i64, desc: &str, action: Option<ActionData>) {
        if !self.is_profiling(task) {
            return;
        }
        let min = task.min_duration_nanos();
        if min >= 0 && dur_ns < min {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        let tid = self.tid(&mut inner);
        inner.recs.push(Rec::Task { tid, start_ns, dur_ns, task, desc: desc.to_string(), action });
    }

    /// `logSimpleTaskDuration`.
    pub fn log_simple_task(&self, start_ns: i64, dur_ns: i64, task: ProfilerTask, desc: &str) {
        self.push_task(task, start_ns, dur_ns, desc, None);
    }

    /// Records an instantaneous event (`logEvent`).
    pub fn log_event(&self, task: ProfilerTask, desc: &str) {
        self.push_task(task, self.now_ns(), 0, desc, None);
    }

    /// Starts a scoped task that is recorded when the guard drops (`Profiler.profile`).
    pub fn profile(&self, task: ProfilerTask, desc: impl Into<String>) -> ProfileScope<'_> {
        ProfileScope { p: self, task, desc: desc.into(), start: self.now_ns() }
    }

    /// Starts a phase marker (`markPhase`): an instantaneous `PHASE` event.
    pub fn mark_phase(&self, phase: ProfilePhase) {
        self.push_task(ProfilerTask::PHASE, self.now_ns(), 0, phase.description, None);
    }

    /// Records a completed action (`ACTION` task with optional `out`/`args`).
    #[allow(clippy::too_many_arguments)]
    pub fn log_action(
        &self,
        start_ns: i64,
        dur_ns: i64,
        mnemonic: Option<&str>,
        description: &str,
        primary_output: Option<&str>,
        target_label: Option<&str>,
        configuration: Option<&str>,
    ) {
        let a = ActionData {
            mnemonic: mnemonic.filter(|_| true).map(str::to_string),
            primary_output: primary_output.filter(|_| self.opts.include_primary_output).map(str::to_string),
            target_label: target_label.filter(|_| self.opts.include_target_label).map(str::to_string),
            configuration: configuration.filter(|_| self.opts.include_configuration).map(str::to_string),
        };
        self.push_task(ProfilerTask::ACTION, start_ns, dur_ns, description, Some(a));
    }

    /// Records one critical path component (rendered on the fake "Critical Path" thread).
    pub fn log_critical_path_component(&self, start_ns: i64, dur_ns: i64, desc: &str) {
        if !self.is_profiling(ProfilerTask::CRITICAL_PATH_COMPONENT) && !self.is_profiling(ProfilerTask::CRITICAL_PATH) {
            // Critical path is always written when the profiler is on.
        }
        let mut inner = self.inner.lock().unwrap();
        let tid = self.tid(&mut inner);
        inner.recs.push(Rec::Task {
            tid,
            start_ns,
            dur_ns,
            task: ProfilerTask::CRITICAL_PATH_COMPONENT,
            desc: desc.to_string(),
            action: None,
        });
    }

    /// Adds a counter lane (`CounterSeriesTraceData`): `series` are `(series name, values)` with one
    /// value per bucket of `bucket_ns` starting at `start_ns`.
    pub fn add_counter_series(&self, lane: &str, color: Option<&str>, start_ns: i64, bucket_ns: i64, series: &[(String, Vec<f64>)]) {
        let Some(len) = series.first().map(|s| s.1.len()) else { return };
        let mut inner = self.inner.lock().unwrap();
        let tid = self.tid(&mut inner);
        let mut recorded = false;
        for i in 0..len {
            let ts = (start_ns + bucket_ns * i as i64) / 1000;
            let mut s = String::from("{");
            s.push_str(&format!("\"name\":{},\"pid\":1,\"tid\":{tid}", json::escape(lane)));
            if let Some(c) = color {
                s.push_str(&format!(",\"cname\":{}", json::escape(c)));
            }
            s.push_str(&format!(",\"ph\":\"C\",\"ts\":{ts},\"args\":{{"));
            let mut first = true;
            for (name, vals) in series {
                let v = vals.get(i).copied().unwrap_or(0.0);
                if v.abs() > 0.00001 || recorded {
                    if !first {
                        s.push(',');
                    }
                    first = false;
                    s.push_str(&format!("{}:{}", json::escape(name), fmt_double(v)));
                    recorded = true;
                }
            }
            s.push_str("}}");
            inner.recs.push(Rec::Counter { json: s });
        }
    }

    /// `action count` counter lane from `(start_ns, end_ns)` pairs of local actions.
    pub fn add_action_count_series(&self, actions: &[(i64, i64)], bucket_ns: i64) {
        if actions.is_empty() || bucket_ns <= 0 {
            return;
        }
        let end = actions.iter().map(|a| a.1).max().unwrap_or(0);
        let n = (end / bucket_ns + 1) as usize;
        let mut v = vec![0.0; n];
        for (s, e) in actions {
            let (a, b) = ((s / bucket_ns).max(0) as usize, (e / bucket_ns).max(0) as usize);
            for x in v.iter_mut().take(b.min(n - 1) + 1).skip(a) {
                *x += 1.0;
            }
        }
        self.add_counter_series("action count", None, 0, bucket_ns, &[("action".to_string(), v)]);
    }

    /// Legacy API: records a pre-timed coarse event (`category` is looked up as a task
    /// description, defaulting to `UNKNOWN`/`PHASE` for known phase descriptions).
    pub fn record(&self, name: impl Into<String>, category: impl Into<String>, start_us: u64, duration_us: u64) {
        let name = name.into();
        let category = category.into();
        let task = ProfilerTask::from_description(&category).unwrap_or(ProfilerTask::PHASE);
        let mut inner = self.inner.lock().unwrap();
        let tid = self.tid(&mut inner);
        inner.recs.push(Rec::Task {
            tid,
            start_ns: start_us as i64 * 1000,
            dur_ns: duration_us as i64 * 1000,
            task,
            desc: name,
            action: None,
        });
    }

    fn write_task(&self, rec: &Rec) -> String {
        let Rec::Task { tid, start_ns, dur_ns, task, desc, action } = rec else { return String::new() };
        let ph = if *dur_ns == 0 { "i" } else { "X" };
        let mut s = String::from("{");
        s.push_str(&format!("\"cat\":{},", json::escape(task.description())));
        s.push_str(&format!("\"name\":{},\"ph\":\"{ph}\",\"ts\":{}", json::escape(desc), start_ns / 1000));
        if *dur_ns != 0 {
            s.push_str(&format!(",\"dur\":{}", dur_ns / 1000));
        }
        s.push_str(",\"pid\":1");
        if let Some(a) = action {
            if let Some(o) = &a.primary_output {
                s.push_str(&format!(",\"out\":{}", json::escape(o)));
            }
        }
        let critical = *task == ProfilerTask::CRITICAL_PATH_COMPONENT;
        let has_args = action.as_ref().map(|a| a.target_label.is_some() || a.mnemonic.is_some() || a.configuration.is_some()).unwrap_or(false);
        if critical || has_args {
            s.push_str(",\"args\":{");
            let mut first = true;
            let mut put = |k: &str, v: String| {
                if !first {
                    s.push(',');
                }
                first = false;
                s.push_str(&format!("\"{k}\":{v}"));
            };
            if critical {
                put("tid", tid.to_string());
            }
            if let Some(a) = action {
                if let Some(t) = &a.target_label {
                    put("target", json::escape(t));
                }
                if let Some(m) = &a.mnemonic {
                    put("mnemonic", json::escape(m));
                }
                if let Some(c) = &a.configuration {
                    put("configuration", json::escape(c));
                }
            }
            s.push('}');
        }
        s.push_str(&format!(",\"tid\":{}}}", if critical { CRITICAL_PATH_THREAD_ID } else { *tid }));
        s
    }

    fn write_thread(tid: u64, name: &str, sort: i64) -> [String; 2] {
        [
            format!("{{\"name\":\"thread_name\",\"ph\":\"M\",\"pid\":1,\"tid\":{tid},\"args\":{{\"name\":{}}}}}", json::escape(name)),
            format!("{{\"name\":\"thread_sort_index\",\"ph\":\"M\",\"pid\":1,\"tid\":{tid},\"args\":{{\"sort_index\":{sort}}}}}"),
        ]
    }

    /// Serializes the profile (uncompressed JSON text).
    pub fn to_json(&self) -> String {
        let inner = self.inner.lock().unwrap();
        let mut out = String::new();
        out.push_str("{\"otherData\":{");
        out.push_str(&format!(
            "\"bazel_version\":{},\"build_id\":{},\"output_base\":{},\"date\":{},\"profile_start_ts\":{}",
            json::escape(&self.opts.bazel_version),
            json::escape(&self.opts.build_id),
            json::escape(&self.opts.output_base),
            json::escape(&zel_proto::dynamic::format_timestamp(self.start_epoch_ms.div_euclid(1000), (self.start_epoch_ms.rem_euclid(1000) * 1_000_000) as i32)),
            self.start_epoch_ms
        ));
        out.push_str("},\"traceEvents\":[");
        let mut first = true;
        let mut emit = |out: &mut String, e: &str| {
            out.push_str(if first { "\n  " } else { ",\n  " });
            first = false;
            out.push_str(e);
        };
        for e in Self::write_thread(CRITICAL_PATH_THREAD_ID, "Critical Path", 0) {
            emit(&mut out, &e);
        }
        let slim = self.opts.slim;
        let mut merged: HashMap<u64, Merged> = HashMap::new();
        let mut event_count: u64 = 0;
        for rec in &inner.recs {
            match rec {
                Rec::Thread { tid, name, sort } => {
                    for e in Self::write_thread(*tid, name, *sort) {
                        emit(&mut out, &e);
                    }
                }
                Rec::Counter { json } => {
                    event_count += 1;
                    emit(&mut out, json);
                }
                Rec::Task { tid, start_ns, dur_ns, task, desc, action } => {
                    event_count += 1;
                    let should_slim = if !slim.enabled {
                        false
                    } else if slim.has_size_limit() {
                        out.len() as i64 > slim.size_limit
                    } else {
                        event_count > SLIM_EVENT_THRESHOLD
                    };
                    let candidate = *dur_ns > 0 && *dur_ns < SLIM_MAX_DURATION_NS && *task != ProfilerTask::CRITICAL_PATH_COMPONENT;
                    if should_slim && candidate {
                        let m = merged.entry(*tid).or_default();
                        if let Some(flushed) = m.maybe_merge(*tid, *start_ns, *dur_ns, desc, *task, action) {
                            let s = self.write_task(&flushed);
                            emit(&mut out, &s);
                        }
                    } else {
                        let s = self.write_task(rec);
                        emit(&mut out, &s);
                    }
                }
            }
        }
        let mut leftovers: Vec<_> = merged.into_iter().collect();
        leftovers.sort_by_key(|(k, _)| *k);
        for (_, mut m) in leftovers {
            if let Some(r) = m.get_and_reset() {
                let s = self.write_task(&r);
                emit(&mut out, &s);
            }
        }
        out.push_str("\n]\n}");
        out
    }

    /// Serializes the profile, gzip-compressing when configured.
    pub fn to_bytes(&self) -> Vec<u8> {
        let j = self.to_json();
        if self.opts.compress {
            zel_util::deflate::gzip(j.as_bytes())
        } else {
            j.into_bytes()
        }
    }

    pub fn write_to(&self, fs: &dyn FileSystem, path: &PathFragment) -> io::Result<()> {
        if let Some(parent) = path.parent_directory() {
            let _ = fs.create_directory_and_parents(&parent);
        }
        fs.write_file(path, &self.to_bytes())
    }

    /// Legacy name used by the server.
    pub fn write_profile(&self, fs: &dyn FileSystem, path: &PathFragment) -> io::Result<()> {
        self.write_to(fs, path)
    }
}

impl Default for Profiler {
    fn default() -> Self {
        Self::new()
    }
}

/// Guard recording a task on drop.
pub struct ProfileScope<'a> {
    p: &'a Profiler,
    task: ProfilerTask,
    desc: String,
    start: i64,
}

impl Drop for ProfileScope<'_> {
    fn drop(&mut self) {
        let dur = self.p.now_ns() - self.start;
        self.p.push_task(self.task, self.start, dur, &self.desc, None);
    }
}

#[derive(Default)]
struct Merged {
    count: u64,
    start_ns: i64,
    end_ns: i64,
    tid: u64,
    task: Option<ProfilerTask>,
    first_desc: String,
    description: Option<String>,
    first_action: Option<ActionData>,
}

impl Merged {
    /// `MergedEvent.maybeMerge`: returns the previously merged event if `data` is not mergeable.
    fn maybe_merge(&mut self, tid: u64, start: i64, dur: i64, desc: &str, task: ProfilerTask, action: &Option<ActionData>) -> Option<Rec> {
        let end = start + dur;
        if self.count > 0 && start >= self.start_ns && end <= self.end_ns {
            return None;
        }
        if self.count == 0 {
            self.begin(tid, start, end, desc, task, action);
            return None;
        }
        if start <= self.end_ns + SLIM_MAX_PAUSE_NS {
            if self.description.as_deref() != Some(desc) {
                self.description = None;
            }
            self.end_ns = end;
            self.count += 1;
            return None;
        }
        let ret = self.get_and_reset();
        self.begin(tid, start, end, desc, task, action);
        self.count = 1;
        ret
    }

    fn begin(&mut self, tid: u64, start: i64, end: i64, desc: &str, task: ProfilerTask, action: &Option<ActionData>) {
        self.tid = tid;
        self.task = Some(task);
        self.first_desc = desc.to_string();
        self.description = Some(desc.to_string());
        self.first_action = action.clone();
        self.start_ns = start;
        self.end_ns = end;
        self.count = 1;
    }

    fn get_and_reset(&mut self) -> Option<Rec> {
        if self.count == 0 {
            return None;
        }
        let rec = if self.count <= 1 {
            Rec::Task {
                tid: self.tid,
                start_ns: self.start_ns,
                dur_ns: self.end_ns - self.start_ns,
                task: self.task.unwrap_or(ProfilerTask::UNKNOWN),
                desc: self.first_desc.clone(),
                action: self.first_action.clone(),
            }
        } else {
            let d = match &self.description {
                Some(d) => format!("{}x {}", self.count, d),
                None => format!("{}x various events", self.count),
            };
            // Merged events are `TaskData(threadId, start, dur, description)` => UNKNOWN type.
            Rec::Task {
                tid: self.tid,
                start_ns: self.start_ns,
                dur_ns: self.end_ns - self.start_ns,
                task: ProfilerTask::UNKNOWN,
                desc: d,
                action: None,
            }
        };
        self.count = 0;
        Some(rec)
    }
}

fn fmt_double(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e7 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

fn uuid_like() -> String {
    let n = crate::now_millis() as u128;
    let p = std::process::id() as u128;
    let x = n.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(p);
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (x >> 96) as u32,
        (x >> 80) as u16,
        (x >> 68) as u16 & 0xfff,
        (x >> 56) as u16 & 0xfff,
        (x as u64) & 0xffff_ffff_ffff
    )
}

// ---------------------------------------------------------------------------------------------
// analyze-profile
// ---------------------------------------------------------------------------------------------

/// One parsed trace event (`TraceEvent.java`).
#[derive(Debug, Clone, PartialEq)]
pub struct TraceEvent {
    pub category: Option<String>,
    pub name: String,
    pub ph: Option<String>,
    pub ts_us: Option<i64>,
    pub dur_us: Option<i64>,
    pub pid: i64,
    pub tid: i64,
    pub out: Option<String>,
    pub target: Option<String>,
    pub mnemonic: Option<String>,
    pub configuration: Option<String>,
}

/// Parsed profile (`JsonProfile.java`).
#[derive(Debug, Clone)]
pub struct JsonProfile {
    pub build_id: Option<String>,
    pub date: Option<String>,
    pub output_base: Option<String>,
    pub events: Vec<TraceEvent>,
    /// Phase durations in nanoseconds, in encounter order.
    pub phases: Vec<(ProfilePhase, i64)>,
}

impl JsonProfile {
    /// Parses profile bytes (gzip detected by magic number).
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let raw;
        let data: &[u8] = if bytes.starts_with(&[0x1f, 0x8b]) {
            raw = zel_util::deflate::gunzip(bytes)?;
            &raw
        } else {
            bytes
        };
        let j = json::parse(data).map_err(|e| format!("Corrupted profile file: {e}"))?;
        let events_j = j
            .get("traceEvents")
            .and_then(Json::as_array)
            .ok_or("Corrupted profile file: couldn't find 'traceEvents'.")?;
        let other = j.get("otherData");
        let s = |k: &str| other.and_then(|o| o.get(k)).and_then(Json::as_str).map(str::to_string);
        let mut events = Vec::new();
        for e in events_j {
            let args = e.get("args");
            let astr = |k: &str| args.and_then(|a| a.get(k)).and_then(Json::as_str).map(str::to_string);
            events.push(TraceEvent {
                category: e.get("cat").and_then(Json::as_str).map(str::to_string),
                name: e.get("name").and_then(Json::as_str).unwrap_or("").to_string(),
                ph: e.get("ph").and_then(Json::as_str).map(str::to_string),
                ts_us: e.get("ts").and_then(Json::as_i64),
                dur_us: e.get("dur").and_then(Json::as_i64),
                pid: e.get("pid").and_then(Json::as_i64).unwrap_or(-1),
                tid: e.get("tid").and_then(Json::as_i64).unwrap_or(-1),
                out: e.get("out").and_then(Json::as_str).map(str::to_string),
                target: astr("target"),
                mnemonic: astr("mnemonic"),
                configuration: astr("configuration"),
            });
        }
        let mut phases = Vec::new();
        let mut last_phase: Option<&TraceEvent> = None;
        let mut max_end = 0i64;
        for ev in &events {
            if let Some(ts) = ev.ts_us {
                let end = ts + ev.dur_us.unwrap_or(0);
                max_end = max_end.max(end);
            }
            if ev.category.as_deref() == Some(ProfilerTask::PHASE.description()) {
                if let Some(l) = last_phase {
                    phases.push((
                        ProfilePhase::from_description(&l.name),
                        (ev.ts_us.unwrap_or(0) - l.ts_us.unwrap_or(0)) * 1000,
                    ));
                }
                last_phase = Some(ev);
            }
        }
        if let Some(l) = last_phase {
            phases.push((ProfilePhase::from_description(&l.name), (max_end - l.ts_us.unwrap_or(0)) * 1000));
        }
        Ok(Self { build_id: s("build_id"), date: s("date"), output_base: s("output_base"), events, phases })
    }
}

/// `TimeUtilities.prettyTime`.
pub fn pretty_time(ns: f64) -> String {
    let ms = ns / 1_000_000.0;
    if ms < 10.0 {
        format!("{ms:.2} ms")
    } else if ms < 100.0 {
        format!("{ms:.1} ms")
    } else if ms < 1000.0 {
        format!("{ms:.0} ms")
    } else {
        format!("{:.3} s", ms / 1000.0)
    }
}

/// `TextPrinter.prettyPercentage`.
pub fn pretty_percentage(rel: f64) -> String {
    if rel.is_nan() {
        "N/A".into()
    } else {
        format!("{:.2}%", rel * 100.0)
    }
}

fn weekday_month(secs: i64) -> (&'static str, &'static str, i64, i64, i64, i64, i64) {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let wd = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize];
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let mon = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][(m - 1) as usize];
    (wd, mon, d, rem / 3600, rem % 3600 / 60, rem % 60, if m <= 2 { y + 1 } else { y })
}

/// `java.util.Date.toString()` in UTC (`EEE MMM dd HH:mm:ss zzz yyyy`) for an ISO instant.
fn java_date_string(iso: &str) -> String {
    // Parse `YYYY-MM-DDTHH:MM:SS[.fff]Z`.
    let parse = || -> Option<i64> {
        let (date, time) = iso.trim_end_matches('Z').split_once('T')?;
        let mut d = date.split('-');
        let (y, mo, da): (i64, i64, i64) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
        let t = time.split('.').next()?;
        let mut tp = t.split(':');
        let (h, mi, s): (i64, i64, i64) = (tp.next()?.parse().ok()?, tp.next()?.parse().ok()?, tp.next()?.parse().ok()?);
        let y2 = if mo <= 2 { y - 1 } else { y };
        let era = y2.div_euclid(400);
        let yoe = y2.rem_euclid(400);
        let mp = (mo + 9) % 12;
        let doy = (153 * mp + 2) / 5 + da - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        Some(days * 86400 + h * 3600 + mi * 60 + s)
    };
    match parse() {
        Some(secs) => {
            let (wd, mon, d, h, mi, s, y) = weekday_month(secs);
            format!("{wd} {mon} {d:02} {h:02}:{mi:02}:{s:02} UTC {y}")
        }
        None => iso.to_string(),
    }
}

/// Renders the `analyze-profile` report: header, phase summary and critical path.
pub fn render_analysis(p: &JsonProfile) -> String {
    let mut o = String::new();
    let none = "null".to_string();
    o.push_str(&format!(
        "INFO: Profile created on {}, build ID: {}, output base: {}\n",
        p.date.as_deref().map(java_date_string).unwrap_or_else(|| none.clone()),
        p.build_id.clone().unwrap_or_else(|| none.clone()),
        p.output_base.clone().unwrap_or(none),
    ));
    // Phase summary (THREE_COLUMN_FORMAT = "%-28s %10s %8s").
    let total: i64 = p.phases.iter().map(|(_, d)| *d).sum();
    o.push_str("\n=== PHASE SUMMARY INFORMATION ===\n\n");
    for (phase, d) in &p.phases {
        o.push_str(&format!(
            "{:<28} {:>10} {:>8}\n",
            format!("Total {} phase time", phase.nick),
            pretty_time(*d as f64),
            pretty_percentage(*d as f64 / total as f64)
        ));
    }
    o.push_str(&"-".repeat(48));
    o.push('\n');
    o.push_str(&format!("{:<28} {:>10} {:>8}\n", "Total run time", pretty_time(total as f64), "100.00%"));
    // Critical path.
    let crit: Vec<&TraceEvent> = p
        .events
        .iter()
        .filter(|e| e.category.as_deref() == Some(ProfilerTask::CRITICAL_PATH_COMPONENT.description()))
        .collect();
    if !crit.is_empty() {
        let total_ns: i64 = crit.iter().map(|e| e.dur_us.unwrap_or(0) * 1000).sum();
        o.push_str(&format!("\nCritical path ({}):\n", pretty_time(total_ns as f64)));
        o.push_str(&format!("{:>11} {:>8}   {}\n", "Time", "Percentage", "Description"));
        for e in crit {
            let ns = e.dur_us.unwrap_or(0) * 1000;
            o.push_str(&format!(
                "{:>11} {:>8}   {}\n",
                pretty_time(ns as f64),
                pretty_percentage(ns as f64 / total_ns as f64),
                e.name.replace(':', " ")
            ));
        }
    }
    o
}

/// `--dump=text|raw` rendering: events sorted by start time (`text`) or in file order (`raw`).
pub fn render_dump(p: &JsonProfile, mode: &str) -> String {
    let mut evs: Vec<&TraceEvent> = p.events.iter().filter(|e| e.ph.as_deref() != Some("M")).collect();
    if mode != "raw" && mode != "unsorted" {
        evs.sort_by_key(|e| (e.ts_us.unwrap_or(0), e.tid));
    }
    let mut o = String::new();
    for e in evs {
        o.push_str(&format!(
            "{} thread={} ph={} cat={} dur={} {}\n",
            e.ts_us.map(|t| format!("{t}us")).unwrap_or_default(),
            e.tid,
            e.ph.as_deref().unwrap_or("?"),
            e.category.as_deref().unwrap_or(""),
            e.dur_us.map(|d| format!("{d}us")).unwrap_or_else(|| "-".into()),
            e.name
        ));
    }
    o
}

/// Result of running `analyze-profile`.
pub struct AnalyzeOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Implements `bazel analyze-profile [--dump=text|raw|unsorted] <profile> [<profile>...]`.
pub fn analyze_profile_command(args: &[String], read: &dyn Fn(&str) -> io::Result<Vec<u8>>) -> AnalyzeOutput {
    let mut dump: Option<String> = None;
    let mut files = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--dump=") {
            dump = Some(v.to_string());
        } else if a == "--dump" || a == "-d" {
            dump = it.next().cloned();
        } else if let Some(v) = a.strip_prefix("-d").filter(|v| !v.is_empty() && !a.starts_with("--")) {
            dump = Some(v.to_string());
        } else if a.starts_with("--") && !a.starts_with("--") || a.starts_with('-') && a.len() > 1 && a != "--" {
            return AnalyzeOutput {
                stdout: String::new(),
                stderr: format!("ERROR: Unrecognized option: {a}\n"),
                exit_code: 2,
            };
        } else if a != "--" {
            files.push(a.clone());
        }
    }
    if let Some(d) = &dump {
        if !["text", "raw", "unsorted"].contains(&d.as_str()) {
            return AnalyzeOutput {
                stdout: String::new(),
                stderr: format!("ERROR: Invalid --dump value '{d}': expected one of text, raw, unsorted\n"),
                exit_code: 2,
            };
        }
    }
    if files.is_empty() {
        return AnalyzeOutput {
            stdout: String::new(),
            stderr: "ERROR: Please specify a profile file to analyze.\n".to_string(),
            exit_code: 2,
        };
    }
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut code = 0;
    for f in &files {
        let parsed = read(f).map_err(|e| format!("Failed to read profile file {f}: {e}")).and_then(|b| JsonProfile::parse(&b));
        match parsed {
            Ok(p) => {
                if let Some(d) = &dump {
                    stdout.push_str(&render_dump(&p, d));
                } else {
                    stdout.push_str(&render_analysis(&p));
                }
            }
            Err(e) => {
                stderr.push_str(&format!("ERROR: Failed to analyze profile file(s): {e}\n"));
                code = 1;
            }
        }
    }
    AnalyzeOutput { stdout, stderr, exit_code: code }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(slim: SlimProfile) -> ProfilerOptions {
        let mut o = ProfilerOptions::new("bid-1", "/ob");
        o.slim = slim;
        o.compress = false;
        o.bazel_version = "release 9.0.0".into();
        o
    }

    #[test]
    fn task_table_matches_java() {
        assert_eq!(ProfilerTask::ALL.len(), 75);
        assert_eq!(ProfilerTask::PHASE.description(), "build phase marker");
        assert_eq!(ProfilerTask::VFS_STAT.min_duration_nanos(), 10_000_000);
        assert!(ProfilerTask::VFS_GLOB.collects_slowest_instances());
        assert_eq!(ProfilerTask::VFS_GLOB.min_duration_nanos(), -1);
        assert!(ProfilerTask::VFS_OPEN.is_vfs() && !ProfilerTask::ACTION.is_vfs());
        let d = ProfilerOptions::default_tasks(&[], false);
        assert!(d.contains(&ProfilerTask::ACTION) && !d.contains(&ProfilerTask::VFS_STAT));
        assert!(!d.contains(&ProfilerTask::SKYFUNCTION) && !d.contains(&ProfilerTask::CRITICAL_PATH));
        let d = ProfilerOptions::default_tasks(&[ProfilerTask::SKYFUNCTION], false);
        assert!(d.contains(&ProfilerTask::SKYFUNCTION));
    }

    #[test]
    fn writes_bazel_trace_layout() {
        let p = Profiler::with_options(opts(SlimProfile::DISABLED));
        p.name_current_thread("grpc-command-0");
        p.log_simple_task(0, 1_500_000, ProfilerTask::PHASE, ProfilePhase::LAUNCH.description);
        p.log_action(2_000_000, 3_000_000, Some("CppCompile"), "Compiling a.cc", Some("bazel-out/a.o"), Some("//a:a"), Some("abc"));
        p.log_critical_path_component(2_000_000, 3_000_000, "action 'Compiling a.cc'");
        // Below min duration: dropped.
        p.log_simple_task(0, 1_000, ProfilerTask::ACTION_CHECK, "tiny");
        let j = p.to_json();
        assert!(j.starts_with("{\"otherData\":{\"bazel_version\":\"release 9.0.0\",\"build_id\":\"bid-1\",\"output_base\":\"/ob\",\"date\":\""), "{j}");
        assert!(j.contains("\"traceEvents\":[\n  {\"name\":\"thread_name\",\"ph\":\"M\",\"pid\":1,\"tid\":0,\"args\":{\"name\":\"Critical Path\"}},\n  {\"name\":\"thread_sort_index\",\"ph\":\"M\",\"pid\":1,\"tid\":0,\"args\":{\"sort_index\":0}}"), "{j}");
        assert!(j.contains("\"args\":{\"name\":\"Main Thread\"}"));
        assert!(j.contains("{\"cat\":\"build phase marker\",\"name\":\"Launch Blaze\",\"ph\":\"X\",\"ts\":0,\"dur\":1500,\"pid\":1,\"tid\":1}"), "{j}");
        // include_* flags are off by default: no `out`, no args for actions.
        assert!(j.contains("{\"cat\":\"action processing\",\"name\":\"Compiling a.cc\",\"ph\":\"X\",\"ts\":2000,\"dur\":3000,\"pid\":1,\"args\":{\"mnemonic\":\"CppCompile\"},\"tid\":1}"), "{j}");
        assert!(j.contains("\"cat\":\"critical path component\",\"name\":\"action 'Compiling a.cc'\",\"ph\":\"X\",\"ts\":2000,\"dur\":3000,\"pid\":1,\"args\":{\"tid\":1},\"tid\":0}"), "{j}");
        assert!(!j.contains("tiny"));
        assert!(j.ends_with("\n]\n}"));
        let parsed = json::parse(j.as_bytes()).unwrap();
        assert!(parsed.get("traceEvents").unwrap().as_array().unwrap().len() >= 7);
    }

    #[test]
    fn include_flags_add_fields() {
        let mut o = opts(SlimProfile::DISABLED);
        o.include_primary_output = true;
        o.include_target_label = true;
        o.include_configuration = true;
        let p = Profiler::with_options(o);
        p.log_action(0, 5_000_000, Some("Javac"), "Compiling X", Some("bazel-out/x.jar"), Some("//x:x"), Some("cfg"));
        let j = p.to_json();
        assert!(j.contains("\"pid\":1,\"out\":\"bazel-out/x.jar\",\"args\":{\"target\":\"//x:x\",\"mnemonic\":\"Javac\",\"configuration\":\"cfg\"},\"tid\":"), "{j}");
    }

    #[test]
    fn slim_profile_merges_close_events() {
        let mut o = opts(SlimProfile { enabled: true, size_limit: 1 });
        o.profiled_tasks = ProfilerOptions::default_tasks(&[], true);
        let p = Profiler::with_options(o);
        for i in 0..5 {
            p.log_simple_task(i * 10_000_000, 5_000_000, ProfilerTask::ACTION, "same");
        }
        p.log_simple_task(900_000_000, 5_000_000, ProfilerTask::ACTION, "far away");
        let j = p.to_json();
        assert!(j.contains("\"name\":\"5x same\""), "{j}");
        assert!(j.contains("\"name\":\"far away\""), "{j}");
        let j2 = {
            let mut o = opts(SlimProfile::DISABLED);
            o.profiled_tasks = ProfilerOptions::default_tasks(&[], true);
            let p = Profiler::with_options(o);
            for i in 0..5 {
                p.log_simple_task(i * 10_000_000, 5_000_000, ProfilerTask::ACTION, "same");
            }
            p.to_json()
        };
        assert!(!j2.contains("5x"));
    }

    #[test]
    fn counter_series_skips_leading_zeroes() {
        let p = Profiler::with_options(opts(SlimProfile::DISABLED));
        p.add_counter_series("action count", None, 0, 1_000_000_000, &[("action".to_string(), vec![0.0, 2.0, 0.0])]);
        let j = p.to_json();
        assert!(j.contains("\"ph\":\"C\",\"ts\":0,\"args\":{}}"), "{j}");
        assert!(j.contains("\"ph\":\"C\",\"ts\":1000000,\"args\":{\"action\":2.0}}"), "{j}");
        // After the first non-zero value, zeros are recorded.
        assert!(j.contains("\"ts\":2000000,\"args\":{\"action\":0.0}}"), "{j}");
    }

    #[test]
    fn gzip_output_and_analyze_roundtrip() {
        let mut o = opts(SlimProfile::DISABLED);
        o.compress = true;
        let p = Profiler::with_options(o);
        p.log_simple_task(0, 1_000_000_000, ProfilerTask::PHASE, ProfilePhase::LAUNCH.description);
        p.log_simple_task(1_000_000_000, 0, ProfilerTask::PHASE, ProfilePhase::INIT.description);
        p.log_simple_task(1_500_000_000, 0, ProfilerTask::PHASE, ProfilePhase::EXECUTE.description);
        p.log_simple_task(1_500_000_000, 500_000_000, ProfilerTask::ACTION, "x");
        p.log_critical_path_component(1_500_000_000, 8_850_000, "_Ccompiler_Udeps for @local_config_cc//:compiler_deps");
        p.log_critical_path_component(1_510_000_000, 3_839_000_000, "action 'Compiling foo.cc'");
        let bytes = p.to_bytes();
        assert_eq!(&bytes[..2], &[0x1f, 0x8b]);
        let prof = JsonProfile::parse(&bytes).unwrap();
        assert_eq!(prof.build_id.as_deref(), Some("bid-1"));
        let out = analyze_profile_command(&["/p.gz".to_string()], &|_| Ok(bytes.clone()));
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        let s = out.stdout;
        assert!(s.starts_with("INFO: Profile created on "), "{s}");
        assert!(s.contains(", build ID: bid-1, output base: /ob\n\n=== PHASE SUMMARY INFORMATION ===\n\n"), "{s}");
        assert!(s.contains("Total launch phase time"), "{s}");
        assert!(s.contains("Total init phase time"), "{s}");
        assert!(s.contains("Total execution phase time"), "{s}");
        assert!(s.contains(&format!("{}\nTotal run time", "-".repeat(48))), "{s}");
        assert!(s.contains("Critical path (3.848 s):\n       Time Percentage   Description\n"), "{s}");
        assert!(s.contains("    8.85 ms    0.23%   _Ccompiler_Udeps for @local_config_cc// compiler_deps\n"), "{s}");
        assert!(s.contains("    3.839 s   99.77%   action 'Compiling foo.cc'\n"), "{s}");
    }

    #[test]
    fn analyze_errors_and_dump() {
        let none: &dyn Fn(&str) -> io::Result<Vec<u8>> = &|_| Err(io::Error::new(io::ErrorKind::NotFound, "nope"));
        assert_eq!(analyze_profile_command(&[], none).exit_code, 2);
        assert_eq!(analyze_profile_command(&["--bogus".into()], none).exit_code, 2);
        assert_eq!(analyze_profile_command(&["--dump=weird".into(), "f".into()], none).exit_code, 2);
        let r = analyze_profile_command(&["f".into()], none);
        assert_eq!(r.exit_code, 1);
        assert!(r.stderr.contains("Failed to analyze profile file(s)"));
        let bad = analyze_profile_command(&["f".into()], &|_| Ok(b"{}".to_vec()));
        assert_eq!(bad.exit_code, 1);
        assert!(bad.stderr.contains("couldn't find 'traceEvents'"));
        let p = Profiler::with_options(opts(SlimProfile::DISABLED));
        p.log_simple_task(5_000_000, 1_000_000, ProfilerTask::ACTION, "b");
        p.log_simple_task(1_000_000, 1_000_000, ProfilerTask::ACTION, "a");
        let bytes = p.to_bytes();
        let sorted = analyze_profile_command(&["--dump=text".into(), "f".into()], &|_| Ok(bytes.clone()));
        let raw = analyze_profile_command(&["--dump=raw".into(), "f".into()], &|_| Ok(bytes.clone()));
        assert!(sorted.stdout.find(" a\n").unwrap() < sorted.stdout.find(" b\n").unwrap());
        assert!(raw.stdout.find(" b\n").unwrap() < raw.stdout.find(" a\n").unwrap());
    }

    #[test]
    fn path_resolution_and_flags() {
        assert_eq!(resolve_profile_path(Some("/x/p.gz"), None, "/ob", true), Some(("/x/p.gz".into(), true)));
        assert_eq!(resolve_profile_path(Some("/x/p.json"), None, "/ob", true), Some(("/x/p.json".into(), false)));
        assert_eq!(resolve_profile_path(None, None, "/ob", true), Some(("/ob/command.profile.gz".into(), true)));
        assert_eq!(resolve_profile_path(None, Some("false"), "/ob", true), None);
        assert_eq!(resolve_profile_path(None, None, "/ob", false), None);
        assert_eq!(resolve_profile_path(None, Some("true"), "/ob", false), Some(("/ob/command.profile.gz".into(), true)));
        assert_eq!(SlimProfile::parse("5m").unwrap(), SlimProfile { enabled: true, size_limit: 5 * 1024 * 1024 });
        assert!(SlimProfile::parse("zzz").is_err());
        let m: std::collections::HashMap<&str, &str> = [("slim_profile", "false"), ("profile", "/p.json")].into_iter().collect();
        let o = ProfilerOptions::from_flags(&|n| m.get(n).map(|s| s.to_string()), &|n| if n == "experimental_profile_additional_tasks" { vec!["vfs_stat,skyfunction".into()] } else { vec![] }, "b", "/ob").unwrap();
        assert!(!o.slim.enabled && !o.compress);
        assert!(o.profiled_tasks.contains(&ProfilerTask::VFS_STAT));
        assert!(ProfilerOptions::from_flags(&|_| None, &|n| if n == "experimental_profile_additional_tasks" { vec!["nope".into()] } else { vec![] }, "b", "/ob").is_err());
    }

    #[test]
    fn thread_sort_indices() {
        assert_eq!(thread_sort_index("grpc-command-3"), 1);
        assert_eq!(thread_sort_index("skyframe-evaluator-cpu-heavy-7"), 20_007);
        assert_eq!(thread_sort_index("skyframe-evaluator-12"), 10_012);
        assert_eq!(thread_sort_index("dynamic-execution-4"), 100_004);
        assert_eq!(thread_sort_index("worker"), 1_000_000);
        assert_eq!(java_date_string("2020-06-16T06:59:40.123Z"), "Tue Jun 16 06:59:40 UTC 2020");
    }
}
