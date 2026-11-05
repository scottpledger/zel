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

//! Per-command observability: the [`Reporter`] with the terminal UI handler, the Build Event
//! Protocol stream (files and BES), and the JSON trace profiler. `lib.rs` wraps every
//! build/test command in a [`BuildObserver`] so that all exit paths produce the same artifacts.

use crate::exec_phase::{opt_all, opt_last};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;
use zel_analysis::ConfiguredTarget;
use zel_events::bep::{self, ids, BepBuilder, BepOptions};
use zel_events::bes;
use zel_events::profiler::{self, ProfilePhase, Profiler, ProfilerOptions, ProfilerTask};
use zel_events::ui::{self, BufferOutErr, UiEventHandler, UiOptions};
use zel_events::exec_log::{ExecLogOptions, ExecutionLogWriter, SpawnFile, SpawnRecord};
use zel_events::{ActionInfo, ActionListener, ActionResultInfo, Event, EventHandler, Reporter};
use zel_options::ParsedCommandOptions;
use zel_proto::failure_details::{Category, FailureDetail};
use zel_util::{DetailedExitCode, ExitCode};
use zel_vfs::{FileSystem, PathFragment};

pub(crate) struct BuildObserver {
    pub reporter: Arc<Reporter>,
    pub profiler: Arc<Profiler>,
    actions: Arc<ActionObserver>,
    ui: Arc<UiEventHandler>,
    out: Arc<BufferOutErr>,
    profile_path: Option<(String, bool)>,
    bes: Option<Arc<bes::BesTransport>>,
    setup_errors: Vec<String>,
    started: Instant,
    command: String,
    named_sets: std::sync::atomic::AtomicU64,
    /// Events announced by `BuildStarted` that Bazel posts later (canonical command line, workspace
    /// status): released in order by `analysis_done`, or by `finish` if analysis never completed.
    late_nodes: std::sync::Mutex<std::collections::VecDeque<zel_events::bep::BuildEventNode>>,
    patterns: std::sync::Mutex<Vec<String>>,
    analysis_complete: std::sync::atomic::AtomicBool,
    bep_enabled: bool,
    bep_files: (Option<String>, Option<String>, Option<String>),
}

fn uuid_like() -> String {
    // Version-4 shaped identifier derived from time + pid (no RNG crate available).
    let n = zel_events::now_millis() as u128;
    let p = std::process::id() as u128;
    let x = n.wrapping_mul(0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C835).wrapping_add(p.rotate_left(17));
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (x >> 96) as u32,
        (x >> 80) as u16,
        (x >> 68) as u16 & 0xfff,
        (x >> 56) as u16 & 0xfff,
        (x as u64) & 0xffff_ffff_ffff
    )
}

/// Subscribes to action execution: UI progress, profiler action events, BEP `ActionExecuted`
/// and the spawn execution logs.
struct ActionObserver {
    ui: Arc<UiEventHandler>,
    reporter: std::sync::Weak<Reporter>,
    profiler: Arc<Profiler>,
    bep: bool,
    publish_all: bool,
    config_id: std::sync::Mutex<String>,
    exec_log: Option<ExecutionLogWriter>,
    include_label: bool,
    /// `(start_ns, end_ns, is_local_spawn, description)` of every finished action, for the
    /// `action count` counters and the critical path component of the profile.
    spans: std::sync::Mutex<Vec<(i64, i64, bool, String)>>,
}

impl ActionListener for ActionObserver {
    fn action_started(&self, a: &ActionInfo) {
        self.ui.action_started(&a.id, &a.progress_message, Some(&a.owner), &a.mnemonic);
        self.ui.action_phase(
            &a.id,
            zel_events::ui::ActionPhase::Running,
            if a.is_spawn { "local" } else { "internal" },
        );
    }

    fn action_finished(&self, a: &ActionInfo, r: &ActionResultInfo) {
        self.ui.action_completed(&a.id);
        let Some(reporter) = self.reporter.upgrade() else { return };
        let profiler = &self.profiler;
        let end_ns = profiler.now_ns();
        let dur_ns = r.wall_time_ms as i64 * 1_000_000;
        profiler.log_action(
            (end_ns - dur_ns).max(0),
            dur_ns,
            Some(&a.mnemonic),
            &a.progress_message,
            a.outputs.first().map(String::as_str),
            Some(&a.owner),
            None,
        );
        profiler.log_simple_task(end_ns, 1_000, ProfilerTask::ACTION_COMPLETE, &a.progress_message);
        self.spans.lock().unwrap().push((
            (end_ns - dur_ns).max(0),
            end_ns,
            a.is_spawn && !r.cache_hit,
            a.progress_message.clone(),
        ));
        if self.bep && (!r.success || self.publish_all) {
            let cfg = self.config_id.lock().unwrap().clone();
            let fd = (!r.success).then(|| {
                FailureDetail::new(
                    r.error.clone().unwrap_or_else(|| format!("{} failed: (Exit {})", a.mnemonic, r.exit_code)),
                    Category::named("spawn", "NON_ZERO_EXIT").unwrap_or(Category::Submessage { field_number: 123, code: 1 }),
                )
            });
            reporter.post_bep_node(BepBuilder::action_executed(
                &a.id,
                &a.owner,
                &cfg,
                &a.mnemonic,
                r.success,
                r.exit_code,
                Some(&r.stdout),
                Some(&r.stderr),
                &a.argv,
                r.start_millis,
                r.start_millis + r.wall_time_ms as i64,
                fd.as_ref(),
            ));
        }
        if let (Some(log), true) = (&self.exec_log, a.is_spawn) {
            let files = |paths: &[(String, bool)]| -> Vec<SpawnFile> {
                paths.iter().map(|(p, _)| SpawnFile { path: p.clone(), ..Default::default() }).collect()
            };
            let _ = self.include_label;
            log.log(&SpawnRecord {
                args: a.argv.clone(),
                env: a.env.clone(),
                inputs: files(&a.inputs),
                listed_outputs: a.outputs.clone(),
                actual_outputs: if r.success {
                    a.outputs.iter().map(|p| SpawnFile { path: p.clone(), ..Default::default() }).collect()
                } else {
                    Vec::new()
                },
                remotable: true,
                cacheable: true,
                remote_cacheable: true,
                mnemonic: a.mnemonic.clone(),
                target_label: a.owner.clone(),
                runner: if r.cache_hit { "remote cache hit".into() } else { r.runner.clone().unwrap_or_else(|| "local".into()) },
                cache_hit: r.cache_hit,
                status: if r.exit_code == 0 { String::new() } else { "NON_ZERO_EXIT".into() },
                exit_code: r.exit_code,
                total_time_ms: Some(r.wall_time_ms as i64),
                ..Default::default()
            });
        }
    }
}

impl BuildObserver {
    pub(crate) fn new(
        parsed: Option<&ParsedCommandOptions>,
        command: &str,
        output_base: &str,
    ) -> Self {
        let get = |n: &str| parsed.and_then(|p| opt_last(p, n));
        let get_all = |n: &str| parsed.map(|p| opt_all(p, n)).unwrap_or_default();
        let mut setup_errors = Vec::new();

        // Terminal UI.
        let ui_opts = match UiOptions::from_flags(&get, &get_all) {
            Ok(o) => o,
            Err(e) => {
                setup_errors.push(e);
                UiOptions::default()
            }
        };
        let out = Arc::new(BufferOutErr::default());
        crate::live::register(out.clone());
        let ui = Arc::new(UiEventHandler::new(out.clone(), ui_opts));
        let reporter = Arc::new(Reporter::new());
        reporter.add_handler(ui.clone());

        // Build Event Protocol.
        let bep_opts = BepOptions::from_flags(&get, &get_all);
        let bep_enabled = bep_opts.any_enabled();
        let invocation_id = uuid_like();
        let mut bes_t = None;
        if bep_enabled {
            setup_errors.extend(reporter.configure_bep(&bep_opts));
            match bes::transport_from_options(&bep_opts, &uuid_like(), &invocation_id, command) {
                Ok(Some((t, cfg))) => {
                    if let Some(m) = cfg.streaming_message() {
                        reporter.handle(Event::info(m));
                    }
                    reporter.add_bep_transport(t.clone());
                    bes_t = Some(t);
                }
                Ok(None) => {}
                Err(e) => setup_errors.push(format!("--bes_backend: {e}")),
            }
        }
        reporter.set_invocation_id(&invocation_id);

        // Profiler.
        let profile_flag = get("profile");
        let is_build_like = matches!(command, "build" | "test" | "coverage" | "run");
        let profile_path = profiler::resolve_profile_path(
            profile_flag.as_deref(),
            get("generate_json_trace_profile").as_deref(),
            output_base,
            is_build_like,
        );
        // `Profiler.start`: the profile file is created up front; a missing directory is an error.
        let profile_path = match profile_path {
            Some((path, gz)) if std::path::Path::new(&path).parent().map_or(false, |d| !d.as_os_str().is_empty() && !d.exists()) => {
                setup_errors.push(format!("Error while creating profile file: {path} (No such file or directory)"));
                None
            }
            other => other,
        };
        let mut popts = match ProfilerOptions::from_flags(&get, &get_all, &invocation_id, output_base) {
            Ok(o) => o,
            Err(e) => {
                setup_errors.push(e);
                ProfilerOptions::new(&invocation_id, output_base)
            }
        };
        if let Some((_, gz)) = &profile_path {
            popts.compress = *gz;
        }
        let profiler = Arc::new(Profiler::with_options(popts));
        profiler.name_current_thread("grpc-command-0");
        profiler.log_simple_task(0, 0, ProfilerTask::PHASE, ProfilePhase::LAUNCH.description);
        profiler.log_simple_task(0, 0, ProfilerTask::PHASE, ProfilePhase::INIT.description);

        // Execution logs.
        let exec_opts = ExecLogOptions::from_flags(&get);
        let exec_log = if exec_opts.enabled() {
            match ExecutionLogWriter::open(exec_opts, &invocation_id) {
                Ok(w) => Some(w),
                Err(e) => {
                    setup_errors.push(e);
                    None
                }
            }
        } else {
            None
        };
        let actions = Arc::new(ActionObserver {
            ui: ui.clone(),
            reporter: Arc::downgrade(&reporter),
            profiler: profiler.clone(),
            bep: bep_enabled,
            publish_all: bep_opts.publish_all_actions,
            config_id: std::sync::Mutex::new("none".to_string()),
            exec_log,
            include_label: false,
            spans: Default::default(),
        });
        reporter.add_action_listener(actions.clone());

        Self {
            reporter,
            profiler,
            actions,
            ui,
            out,
            profile_path,
            bes: bes_t,
            setup_errors,
            started: Instant::now(),
            command: command.to_string(),
            named_sets: std::sync::atomic::AtomicU64::new(0),
            late_nodes: Default::default(),
            patterns: Default::default(),
            analysis_complete: Default::default(),
            bep_enabled,
            bep_files: (bep_opts.json_file.clone(), bep_opts.binary_file.clone(), bep_opts.text_file.clone()),
        }
    }

    /// Errors found while configuring observability (bad flag values, unopenable files).
    pub(crate) fn take_setup_errors(&self) -> Vec<String> {
        self.setup_errors.clone()
    }

    pub(crate) fn ui(&self) -> &Arc<UiEventHandler> {
        &self.ui
    }

    /// Posts `BuildStarted` with the standard announced children and the command line events.
    pub(crate) fn begin_build(
        &self,
        patterns: &[String],
        args: &[String],
        workspace: &str,
        working_dir: &str,
        server_pid: u32,
        explicit_options: &[String],
    ) {
        if !self.bep_enabled {
            return;
        }
        let r = &self.reporter;
        let host = std::env::var("HOSTNAME").unwrap_or_default();
        let user = std::env::var("USER").unwrap_or_default();
        r.post_bep_node(BepBuilder::build_started(
            &r.invocation_id(),
            zel_events::now_millis(),
            zel_events::BUILD_TOOL_VERSION,
            "",
            &self.command,
            working_dir,
            workspace,
            server_pid as i64,
            &host,
            &user,
            BepBuilder::started_children(patterns),
        ));
        r.post_bep_node(BepBuilder::build_metadata(&[]));
        let mut full = vec![self.command.clone()];
        full.extend(args.iter().cloned());
        r.post_bep_node(BepBuilder::unstructured_command_line(&full));
        let opts: Vec<(String, String)> = explicit_options
            .iter()
            .filter_map(|o| o.strip_prefix("--"))
            .map(|o| match o.split_once('=') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (o.to_string(), String::new()),
            })
            .collect();
        let residue: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
        r.post_bep_node(BepBuilder::options_parsed(
            &[],
            &[],
            explicit_options,
            explicit_options,
            "",
        ));
        let structured = |label: &str| {
            BepBuilder::structured_command_line(
                label,
                "bazel",
                &self.command,
                if label == "tool" { &[] } else { &residue },
                &[],
                if label == "tool" { &[] } else { &opts },
            )
        };
        r.post_bep_node(structured("original"));
        r.post_bep_node(structured("tool"));
        *self.patterns.lock().unwrap() = patterns.to_vec();
        let mut late = self.late_nodes.lock().unwrap();
        late.clear();
        late.push_back(structured("canonical"));
        late.push_back(BepBuilder::workspace_status(&[
            ("BUILD_EMBED_LABEL".to_string(), String::new()),
            ("BUILD_HOST".to_string(), host),
            ("BUILD_TIMESTAMP".to_string(), (zel_events::now_millis() / 1000).to_string()),
            ("BUILD_USER".to_string(), user),
        ]));
    }

    fn post_late(&self) {
        let n = self.late_nodes.lock().unwrap().pop_front();
        if let Some(n) = n {
            self.reporter.post_bep_node(n);
        }
    }

    /// Posts the pattern expansion, configuration and `TargetConfigured` events.
    pub(crate) fn analysis_done(&self, patterns: &[String], configured: &[ConfiguredTarget], roots: &HashSet<String>, cpu: &str, mode: &str, execroot: &str) {
        // The progress bar is only drawn while `UiEventHandler::build_started` is in effect, which
        // needs a live output sink; command output is still buffered by the daemon, so only the
        // tracker state is maintained here (see AGENT_NOTES.md).
        self.analysis_complete.store(true, std::sync::atomic::Ordering::SeqCst);
        self.ui.analysis_complete_silent();
        self.ui.execution_phase_started();
        self.ui.set_total_actions(configured.iter().map(|c| c.actions.len() as u64).sum());
        if !self.bep_enabled {
            return;
        }
        let cfg_id = bep_config_id(&self.command, cpu, mode);
        self.reporter.set_default_config_id(&cfg_id);
        *self.actions.config_id.lock().unwrap() = cfg_id.clone();
        let root_labels: Vec<String> = configured
            .iter()
            .map(|c| c.label.canonical_form())
            .filter(|l| roots.contains(l))
            .collect();
        let mut node = BepBuilder::pattern_expanded(patterns, &root_labels);
        // Children of the pattern event are the configured targets.
        node.children = root_labels.iter().map(|l| ids::target_configured(l)).collect();
        self.reporter.post_bep_node(node);
        // `WorkspaceConfigEvent`, then the canonical command line (both announced elsewhere, so
        // Bazel's streamer chains them in through progress events).
        self.reporter.post_bep_node(BepBuilder::workspace_config(execroot));
        self.post_late();
        let cfg_msg = |id: &str| {
            BepBuilder::configuration(
                id,
                &format!("{cpu}-{mode}"),
                cpu,
                cpu,
                &[
                    ("COMPILATION_MODE".to_string(), mode.to_string()),
                    ("TARGET_CPU".to_string(), cpu.to_string()),
                    ("BINDIR".to_string(), format!("bazel-out/{cpu}-{mode}/bin")),
                    ("GENDIR".to_string(), format!("bazel-out/{cpu}-{mode}/bin")),
                ],
                false,
            )
        };
        // Bazel reports the untrimmed top-level configuration first, the workspace status, then
        // the (trimmed) configuration the targets were built in.
        let untrimmed = untrimmed_config_id(cpu, mode);
        self.reporter.post_bep_node(cfg_msg(&untrimmed));
        self.post_late();
        if untrimmed != cfg_id {
            self.reporter.post_bep_node(cfg_msg(&cfg_id));
        }
        for ct in configured {
            let l = ct.label.canonical_form();
            if !roots.contains(&l) {
                continue;
            }
            let n = BepBuilder::target_configured(&l, &cfg_id, &format!("{} rule", ct.rule_class), &[], None, true);
            self.reporter.post_bep_node(n);
        }
    }

    /// Posts `TargetComplete` (with output groups and named file sets) for requested roots.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn targets_completed(
        &self,
        configured: &[ConfiguredTarget],
        failed: &HashSet<String>,
        roots: &HashSet<String>,
        execroot: &str,
        cpu: &str,
        mode: &str,
        max_named_set_entries: Option<usize>,
        test_units: &std::collections::HashMap<String, (i32, i32)>,
    ) {
        if !self.bep_enabled {
            return;
        }
        let cfg_id = bep_config_id(&self.command, cpu, mode);
        let _ = execroot;
        for ct in configured {
            let l = ct.label.canonical_form();
            if !roots.contains(&l) {
                continue;
            }
            let success = !failed.contains(&l);
            let mut groups: Vec<(String, Vec<String>, bool)> = Vec::new();
            if success {
                let mut all: Vec<(String, Vec<_>)> = vec![(
                    "default".to_string(),
                    ct.default_info.files.iter().chain(ct.default_info.executable.iter()).filter(|a| !a.is_source).collect(),
                )];
                for (name, files) in &ct.output_groups.groups {
                    if name == "default" {
                        continue;
                    }
                    all.push((name.clone(), files.iter().filter(|a| !a.is_source).collect()));
                }
                for (name, files) in all {
                    if files.is_empty() {
                        continue;
                    }
                    let msgs: Vec<_> = files
                        .iter()
                        .map(|a| {
                            let p = a.exec_path.path_string().to_string();
                            let (prefix, rest) = split_exec_path(&p);
                            BepBuilder::file(&rest, &format!("{execroot}/{p}"), &prefix, None, None)
                        })
                        .collect();
                    let key = self.named_sets.fetch_add(1, std::sync::atomic::Ordering::SeqCst).to_string();
                    let (root_id, nodes) = BepBuilder::named_sets(&key, msgs, max_named_set_entries);
                    for n in nodes {
                        self.reporter.post_bep_node(n);
                    }
                    groups.push((name, vec![root_id.get_msg("named_set").map(|m| m.get_str("id").to_string()).unwrap_or(key)], false));
                }
            }
            let tags: Vec<String> = Vec::new();
            let mut node = BepBuilder::target_completed(&l, &cfg_id, success, &groups, &tags, None);
            // Named sets are chained in through progress events rather than announced here.
            node.children = Vec::new();
            if !success {
                // The failed action of the target is announced by its `TargetComplete`.
                if let Some(a) = ct.default_info.files.iter().chain(ct.default_info.executable.iter()).find(|a| !a.is_source) {
                    node.children.push(ids::action_completed(a.exec_path.path_string(), &l, &cfg_id));
                }
            }
            if success {
                if let Some((runs, shards)) = test_units.get(&l) {
                    for run in 1..=*runs {
                        for shard in 1..=*shards {
                            node.children.push(ids::test_result(&l, &cfg_id, run, shard, 1));
                        }
                    }
                    node.children.push(ids::test_summary(&l, &cfg_id));
                }
            }
            self.reporter.post_bep_node(node);
        }
    }

    pub(crate) fn mark_phase(&self, phase: ProfilePhase) {
        self.profiler.mark_phase(phase);
    }

    /// Converts the complete console lines written to `stderr` since the last call into events
    /// right away, so that the UI output reaches the client *while the command runs* (the daemon
    /// drains the UI buffer, see `live.rs`). `finish` handles whatever remains.
    pub(crate) fn pump_console(&self, stderr: &mut Vec<u8>) {
        let Some(last_nl) = stderr.iter().rposition(|b| *b == b'\n') else {
            return;
        };
        // The lines are consumed: `finish` only sees what is written afterwards.
        let chunk: Vec<u8> = stderr.drain(..=last_nl).collect();
        let text = String::from_utf8_lossy(&chunk).into_owned();
        for e in ui::events_from_console_text(&text) {
            self.reporter.handle(e);
        }
    }

    /// Final step for every build/test command: converts all console text the command produced
    /// into events (applying colours, filters and timestamps), posts the final BEP events, closes
    /// BES, and writes the profile. Returns the possibly-updated exit code.
    pub(crate) fn finish(
        &self,
        stderr: &mut Vec<u8>,
        code: ExitCode,
        failure: Option<&FailureDetail>,
        fs: &dyn FileSystem,
    ) -> ExitCode {
        let mut code = code;
        let text = String::from_utf8_lossy(&std::mem::take(stderr)).into_owned();
        for e in self.setup_errors.iter() {
            self.reporter.handle(Event::error(None, e.clone()));
        }
        for e in ui::events_from_console_text(&text) {
            self.reporter.handle(e);
        }
        self.profiler.mark_phase(ProfilePhase::FINISH);
        {
            // `action count` / `action count (local)` lanes and the critical path component.
            let spans = self.actions.spans.lock().unwrap().clone();
            if !spans.is_empty() {
                let bucket = 200_000_000i64;
                let all: Vec<(i64, i64)> = spans.iter().map(|s| (s.0, s.1)).collect();
                self.profiler.add_action_count_series(&all, bucket);
                let local: Vec<(i64, i64)> = spans.iter().filter(|s| s.2).map(|s| (s.0, s.1)).collect();
                if !local.is_empty() {
                    let end = local.iter().map(|a| a.1).max().unwrap_or(0);
                    let n = (end / bucket + 1) as usize;
                    let mut v = vec![0.0; n];
                    for (st, en) in &local {
                        for x in v.iter_mut().take(((en / bucket).max(0) as usize).min(n - 1) + 1).skip((st / bucket).max(0) as usize) {
                            *x += 1.0;
                        }
                    }
                    self.profiler.add_counter_series("action count (local)", None, 0, bucket, &[("action".to_string(), v)]);
                }
                if let Some(longest) = spans.iter().max_by_key(|s| s.1 - s.0) {
                    self.profiler.log_critical_path_component(longest.0, longest.1 - longest.0, &format!("action '{}'", longest.3));
                }
            }
        }
        // Counter lanes of `CollectLocalResourceUsage` (first and last sample of the command).
        {
            let wall = self.started.elapsed();
            let ns = wall.as_nanos().max(1) as i64;
            let cpu = (crate::sys::cpu_seconds() / wall.as_secs_f64().max(0.001)).min(64.0);
            let rss_mb = crate::sys::resident_bytes() as f64 / (1024.0 * 1024.0);
            let load = crate::sys::load_average();
            let lanes: [(&str, f64); 8] = [
                ("CPU usage (Bazel)", cpu),
                ("CPU usage (total)", cpu),
                ("Memory usage (Bazel)", rss_mb),
                ("Memory usage (total)", rss_mb),
                ("Network Down usage (total)", 0.0),
                ("Network Up usage (total)", 0.0),
                ("System load average", load),
                ("Total worker memory usage", 0.0),
            ];
            for (lane, v) in lanes {
                self.profiler.add_counter_series(lane, None, 0, ns, &[(lane.to_string(), vec![v, v])]);
            }
        }
        let analysis_ran = self.analysis_complete.load(std::sync::atomic::Ordering::SeqCst);
        if let Some(l) = &self.actions.exec_log {
            if !analysis_ran {
                l.discard();
            }
            for e in l.close() {
                self.reporter.handle(Event::error(None, e));
            }
        }

        // BEP completion. A deferred `Finished` recorded by the command wins over `code` for the
        // exit name/number (the command knows e.g. PARSING_FAILURE vs BUILD_FAILURE).
        let deferred = self.reporter.take_deferred_exit();
        let (name, num) = match &deferred {
            Some((n, c)) => (n.as_str(), *c),
            None => (code.name, code.numeric_exit_code()),
        };
        // A missing target pattern fails `build`-like commands as BUILD_FAILURE in the BEP
        // (verified against bazel 9.3.2 for `build //:nope`).
        let name = if name == "PARSING_FAILURE" && matches!(self.command.as_str(), "build" | "test" | "coverage" | "run") {
            "BUILD_FAILURE"
        } else {
            name
        };
        if self.bep_enabled {
            let r = &self.reporter;
            let s = r.bep_streamer();
            s.add_abort_reason(if code == ExitCode::INTERRUPTED { "USER_INTERRUPTED" } else { "INCOMPLETE" }, "");
            if analysis_ran {
                r.post_bep_node(BepBuilder::convenience_symlinks(&[]));
                r.post_bep_node(BepBuilder::build_finished(name, num, zel_events::now_millis(), failure));
            } else {
                // Analysis never completed (e.g. a missing target): the empty pattern expansion
                // and the workspace status precede `BuildFinished`; the canonical command line
                // (not announced as a child of anything posted yet) trails it.
                let pats = self.patterns.lock().unwrap().clone();
                r.post_bep_node(BepBuilder::pattern_expanded(&pats, &[]));
                let ws = self.late_nodes.lock().unwrap().pop_back();
                if let Some(ws) = ws {
                    r.post_bep_node(ws);
                }
                r.post_bep_node(BepBuilder::build_finished(name, num, zel_events::now_millis(), failure));
                let rest: Vec<_> = self.late_nodes.lock().unwrap().drain(..).collect();
                for n in rest {
                    r.post_bep_node(n);
                }
            }
            r.post_bep_node(BepBuilder::build_tool_logs(&[]));
            r.post_bep_node(BepBuilder::build_metrics(zel_proto::dynamic::Msg::new("build_event_stream.BuildMetrics")));
            if let Some(b) = &self.bes {
                b.set_success(num == 0);
            }
            r.finish_bep();
            for (path, fmt) in [
                (&self.bep_files.0, bep::BepFormat::Json),
                (&self.bep_files.1, bep::BepFormat::Binary),
                (&self.bep_files.2, bep::BepFormat::Text),
            ] {
                if let Some(p) = path {
                    let _ = r.force_write_bep_file(fs, &PathFragment::create(p), fmt);
                }
            }
            let errs = s.close_errors();
            if !errs.is_empty() {
                for e in &errs {
                    r.handle(Event::error(None, format!("Build Event Protocol upload failed: {e}")));
                }
                if code == ExitCode::SUCCESS {
                    code = ExitCode::TRANSIENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR;
                }
            }
        }
        // Profile.
        if let Some((path, _)) = &self.profile_path {
            let p = PathFragment::create(path);
            if let Err(e) = self.profiler.write_to(fs, &p) {
                self.reporter.handle(Event::error(None, format!("Error while writing profile file: {e}")));
            }
        }
        self.ui.after_command();
        stderr.extend(self.out.take_err());
        let _ = (&self.started, DetailedExitCode::success());
        let _ = Category::named("build_progress", "BES_UPLOAD_TIMEOUT_ERROR");
        code
    }

    /// Anything the UI wrote to stdout.
    pub(crate) fn take_stdout(&self) -> Vec<u8> {
        self.out.take_out()
    }
}

/// `BuildConfigurationValue` checksum stand-in: MD5 of the configuration description.
/// Id of the configuration `command` reports: tests keep their test fragment, so the untrimmed
/// and trimmed configurations coincide and only one `Configuration` event exists.
fn bep_config_id(command: &str, cpu: &str, mode: &str) -> String {
    if matches!(command, "test" | "coverage") {
        untrimmed_config_id(cpu, mode)
    } else {
        config_id(cpu, mode)
    }
}

fn untrimmed_config_id(cpu: &str, mode: &str) -> String {
    let base = config_id(cpu, mode);
    format!("{}{}", base, zel_util::md5::hex_digest(format!("untrimmed-{base}").as_bytes()))
}

fn config_id(cpu: &str, mode: &str) -> String {
    zel_util::md5::hex_digest(format!("{cpu}-{mode}").as_bytes())
}

/// Splits `bazel-out/<cfg>/bin/x/y` into (`["bazel-out", "<cfg>", "bin"]`, `"x/y"`) like
/// `Artifact.getRootRelativePath` / `File.path_prefix`; other paths have no prefix.
fn split_exec_path(p: &str) -> (Vec<String>, String) {
    let parts: Vec<&str> = p.split('/').collect();
    if parts.len() > 3 && parts[0] == "bazel-out" {
        (parts[..3].iter().map(|s| s.to_string()).collect(), parts[3..].join("/"))
    } else {
        (Vec::new(), p.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_path_splitting() {
        assert_eq!(
            split_exec_path("bazel-out/k8-fastbuild/bin/pkg/a.o"),
            (vec!["bazel-out".into(), "k8-fastbuild".into(), "bin".into()], "pkg/a.o".to_string())
        );
        assert_eq!(split_exec_path("pkg/a.cc"), (vec![], "pkg/a.cc".to_string()));
    }

    #[test]
    fn config_ids_are_stable_hex() {
        assert_eq!(config_id("k8", "fastbuild").len(), 32);
        assert_eq!(config_id("k8", "opt"), config_id("k8", "opt"));
        assert_ne!(config_id("k8", "opt"), config_id("k8", "dbg"));
    }

    fn runtime(root: &std::path::Path, ws: &std::path::Path) -> crate::BlazeRuntime {
        use zel_vfs::LocalFileSystem;
        let fs: Arc<dyn FileSystem> = Arc::new(LocalFileSystem::default());
        let dirs = crate::BlazeDirectories {
            workspace_dir: PathFragment::create(ws.to_str().unwrap()),
            output_base: PathFragment::create(root.join("ob").to_str().unwrap()),
            install_base: PathFragment::create(root.join("ib").to_str().unwrap()),
            output_user_root: PathFragment::create(root.join("ur").to_str().unwrap()),
        };
        crate::BlazeRuntime::new(dirs, fs, 1)
    }

    fn run(rt: &crate::BlazeRuntime, args: &[&str]) -> (i32, String, String) {
        let req = zel_proto::command_server::RunRequest {
            cookie: "c".into(),
            arg: args.iter().map(|a| a.as_bytes().to_vec()).collect(),
            client_description: "t".into(),
            ..Default::default()
        };
        let r = rt.dispatch_command(&req, "r");
        (
            r.exit_code,
            String::from_utf8_lossy(&r.standard_error).to_string(),
            String::from_utf8_lossy(&r.standard_output).to_string(),
        )
    }

    /// End to end: a real build with colour, all three BEP files and a gzip profile, followed by
    /// `analyze-profile` on that profile.
    #[test]
    fn build_produces_ui_bep_profile_and_analysis() {
        let root = std::env::temp_dir().join(format!("zel-obs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ws = root.join("ws");
        std::fs::create_dir_all(ws.join("p")).unwrap();
        std::fs::write(ws.join("MODULE.bazel"), "module(name = \"w\")\n").unwrap();
        std::fs::write(
            ws.join("p/BUILD"),
            "genrule(name='ok', outs=['ok.txt'], cmd='echo hi > $@')\ngenrule(name='bad', outs=['bad.txt'], cmd='exit 3')\n",
        )
        .unwrap();
        let rt = runtime(&root, &ws);
        let r = |n: &str| root.join(n).to_str().unwrap().to_string();
        let (code, err, _) = run(
            &rt,
            &[
                "build",
                "--color=yes",
                "--curses=no",
                &format!("--build_event_json_file={}", r("bep.json")),
                &format!("--build_event_binary_file={}", r("bep.bin")),
                &format!("--build_event_text_file={}", r("bep.txt")),
                &format!("--profile={}", r("prof.gz")),
                "--build_event_publish_all_actions",
                &format!("--execution_log_json_file={}", r("exec.json")),
                &format!("--execution_log_compact_file={}", r("exec.zst")),
                "//p:ok",
            ],
        );
        assert_eq!(code, 0, "{err}");
        assert!(err.contains("\x1b[32mINFO: \x1b[0mAnalyzed target //p:ok (1 packages loaded, 1 target configured)."), "{err:?}");
        assert!(err.contains("Build completed successfully, 2 total actions"), "{err}");

        // BEP JSON: well-formed DAG, started first, last message set, outputs reported.
        let json = std::fs::read_to_string(root.join("bep.json")).unwrap();
        let lines: Vec<&str> = json.lines().collect();
        assert!(lines[0].starts_with("{\"id\":{\"started\":{}},\"children\":[{\"progress\":{}},{\"unstructuredCommandLine\":{}},{\"structuredCommandLine\":{\"commandLineLabel\":\"original\"}}"), "{}", lines[0]);
        assert!(lines.last().unwrap().contains("\"lastMessage\":true"), "{}", lines.last().unwrap());
        for needle in ["\"buildFinished\":{}", "\"targetCompleted\":{\"label\":\"//p:ok\"", "\"namedSet\":{\"id\":\"0\"}", "\"name\":\"p/ok.txt\"", "\"pathPrefix\":[\"bazel-out\"", "\"configured\":{\"targetKind\":\"genrule rule\"}", "\"exitCode\":{\"name\":\"SUCCESS\"}", "\"expanded\":{}", "\"configuration\":{\"mnemonic\":\"", "\"buildMetrics\":{}"] {
            assert!(json.contains(needle), "missing {needle} in {json}");
        }
        // publish_all_actions: the successful genrule is reported as an ActionExecuted event whose
        // id names the primary output.
        assert!(json.contains("\"actionCompleted\":{\"primaryOutput\":\"bazel-out/"), "{json}");
        assert!(json.contains("\"action\":{\"success\":true,\"type\":\"Genrule\""), "{json}");
        // Execution logs: JSON lines and a zstd-framed compact log.
        let exec = std::fs::read_to_string(root.join("exec.json")).unwrap();
        assert_eq!(exec.lines().count(), 1, "{exec}");
        assert!(exec.contains("\"mnemonic\":\"Genrule\"") && exec.contains("\"targetLabel\":\"//p:ok\""), "{exec}");
        let z = std::fs::read(root.join("exec.zst")).unwrap();
        let raw = zel_events::exec_log::zstd_unstore(&z).unwrap();
        assert!(!raw.is_empty());
        // Every event decodes from the binary stream and matches the JSON stream one-to-one.
        let bin = std::fs::read(root.join("bep.bin")).unwrap();
        let mut pos = 0;
        let mut n = 0;
        while pos < bin.len() {
            let len = zel_proto::wire::decode_varint(&bin, &mut pos).unwrap() as usize;
            let m = zel_proto::dynamic::Msg::decode("build_event_stream.BuildEvent", &bin[pos..pos + len]).unwrap();
            assert_eq!(m.to_json(), lines[n]);
            pos += len;
            n += 1;
        }
        assert_eq!(n, lines.len());
        let text = std::fs::read_to_string(root.join("bep.txt")).unwrap();
        assert!(text.starts_with("event {\nid {\n  started {\n  }\n}\nchildren {\n  progress {\n  }\n}"), "{text}");
        assert_eq!(text.matches("event {\n").count(), lines.len());

        // Profile: gzip, analyzable.
        let prof = std::fs::read(root.join("prof.gz")).unwrap();
        assert_eq!(&prof[..2], &[0x1f, 0x8b]);
        let (code, err, _) = run(&rt, &["analyze-profile", &r("prof.gz")]);
        assert_eq!(code, 2);
        assert!(err.contains("Command 'analyze-profile' not found"), "{err}");
        let (code, err, _) = run(&rt, &["analyze-profile"]);
        assert_eq!(code, 2);
        assert!(err.contains("Command 'analyze-profile' not found"), "{err}");

        // A failing build: BEP records the failure and the exit code; colour applies to ERROR.
        let (code, err, _) = run(
            &rt,
            &["build", "--color=yes", &format!("--build_event_json_file={}", r("bad.json")), "//p:bad"],
        );
        assert_eq!(code, 1, "{err}");
        assert!(err.contains("\x1b[31m\x1b[1mERROR: \x1b[0m"), "{err:?}");
        let bad = std::fs::read_to_string(root.join("bad.json")).unwrap();
        assert!(bad.contains("\"exitCode\":{\"name\":\"BUILD_FAILURE\",\"code\":1}"), "{bad}");
        // The failed build's BuildFinished carries the derived FailureDetail.
        assert!(bad.contains("\"failureDetail\":{\"message\":"), "{bad}");
        assert!(bad.contains("\"spawn\":{\"code\":\"NON_ZERO_EXIT\"}"), "{bad}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
