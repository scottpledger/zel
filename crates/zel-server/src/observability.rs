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

use crate::exec_phase::opt_all;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;
use zel_analysis::ConfiguredTarget;
use zel_events::bep::{self, ids, BepBuilder, BepOptions};
use zel_events::bes;
use zel_events::exec_log::{ExecLogOptions, ExecutionLogWriter, SpawnFile, SpawnRecord};
use zel_events::profiler::{self, ProfilePhase, Profiler, ProfilerOptions, ProfilerTask};
use zel_events::ui::{self, BufferOutErr, UiEventHandler, UiOptions};
use zel_events::{ActionInfo, ActionListener, ActionResultInfo, Event, Reporter};
use zel_options::ParsedCommandOptions;
use zel_proto::failure_details::{Category, FailureDetail};
use zel_util::{DetailedExitCode, ExitCode};
use zel_vfs::{FileSystem, PathFragment};

struct ActionSpan {
    start_ns: i64,
    end_ns: i64,
    start_millis: i64,
    end_millis: i64,
    is_local_spawn: bool,
    cache_hit: bool,
    mnemonic: String,
    runner: String,
    description: String,
    inputs: Vec<String>,
    outputs: Vec<String>,
}

struct SamplerShared {
    stop: std::sync::Mutex<bool>,
    cv: std::sync::Condvar,
    /// `(rel_ns, cpu_secs, rss_mb, load_avg)` samples collected every ~200ms.
    samples: std::sync::Mutex<Vec<(i64, f64, f64, f64)>>,
}

struct ResourceSampler {
    shared: Arc<SamplerShared>,
    handle: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl ResourceSampler {
    fn start(started: Instant) -> Self {
        let shared = Arc::new(SamplerShared {
            stop: std::sync::Mutex::new(false),
            cv: std::sync::Condvar::new(),
            samples: std::sync::Mutex::new(vec![(
                0,
                crate::sys::cpu_seconds(),
                crate::sys::resident_bytes() as f64 / (1024.0 * 1024.0),
                crate::sys::load_average(),
            )]),
        });
        let sh = shared.clone();
        let h = std::thread::Builder::new()
            .name("zel-resource-sampler".into())
            .spawn(move || {
                let mut stop = sh.stop.lock().unwrap();
                while !*stop {
                    let (guard, _) = sh
                        .cv
                        .wait_timeout(stop, std::time::Duration::from_millis(200))
                        .unwrap();
                    stop = guard;
                    if *stop {
                        break;
                    }
                    let rel_ns = started.elapsed().as_nanos().min(i64::MAX as u128) as i64;
                    sh.samples.lock().unwrap().push((
                        rel_ns,
                        crate::sys::cpu_seconds(),
                        crate::sys::resident_bytes() as f64 / (1024.0 * 1024.0),
                        crate::sys::load_average(),
                    ));
                }
            })
            .ok();
        Self {
            shared,
            handle: std::sync::Mutex::new(h),
        }
    }

    fn stop_and_take(&self, started: Instant) -> Vec<(i64, f64, f64, f64)> {
        {
            let mut stop = self.shared.stop.lock().unwrap();
            *stop = true;
            self.shared.cv.notify_all();
        }
        if let Some(h) = self.handle.lock().unwrap().take() {
            let _ = h.join();
        }
        let rel_ns = started.elapsed().as_nanos().max(1).min(i64::MAX as u128) as i64;
        let mut samples = self.shared.samples.lock().unwrap();
        samples.push((
            rel_ns,
            crate::sys::cpu_seconds(),
            crate::sys::resident_bytes() as f64 / (1024.0 * 1024.0),
            crate::sys::load_average(),
        ));
        std::mem::take(&mut *samples)
    }
}

impl Drop for ResourceSampler {
    fn drop(&mut self) {
        {
            let mut stop = self.shared.stop.lock().unwrap();
            *stop = true;
            self.shared.cv.notify_all();
        }
        if let Some(h) = self.handle.lock().unwrap().take() {
            let _ = h.join();
        }
    }
}

static CUMULATIVE_ANALYSES: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static CUMULATIVE_BUILDS: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

#[derive(Default)]
struct AnalysisStats {
    packages_loaded: i64,
    targets_configured: i64,
    actions_created: i64,
    source_artifacts: usize,
    output_artifacts: usize,
    top_level_artifacts: usize,
    analysis_done_ns: i64,
    created_by_mnemonic: std::collections::BTreeMap<String, i64>,
    rule_classes: std::collections::BTreeMap<String, (u64, u64)>,
}

pub(crate) struct BuildObserver {
    pub reporter: Arc<Reporter>,
    pub profiler: Arc<Profiler>,
    actions: Arc<ActionObserver>,
    ui: Arc<UiEventHandler>,
    out: Arc<BufferOutErr>,
    profile_path: Option<(String, bool)>,
    sampler: Option<ResourceSampler>,
    default_profile: bool,
    output_base: String,
    invocation_id: String,
    include_run_residue: bool,
    bes: Option<Arc<bes::BesTransport>>,
    setup_errors: std::sync::Mutex<Vec<String>>,
    started: Instant,
    cpu_at_start: f64,
    command: String,
    perform_build: bool,
    record_all_mnemonics: bool,
    memory_profile: bool,
    namer: std::sync::Mutex<bep::ArtifactGroupNamer>,
    workspace_dir: std::sync::Mutex<String>,
    stats: std::sync::Mutex<AnalysisStats>,
    /// Events announced by `BuildStarted` that Bazel posts later (canonical command line, workspace
    /// status): released in order by `analysis_done`, or by `finish` if analysis never completed.
    late_nodes: std::sync::Mutex<std::collections::VecDeque<zel_events::bep::BuildEventNode>>,
    patterns: std::sync::Mutex<Vec<String>>,
    analysis_complete: std::sync::atomic::AtomicBool,
    bep_enabled: bool,
    bep_opts: BepOptions,
    build_metadata: Vec<(String, String)>,
    convenience_symlinks_bep_event: bool,
    convenience_symlinks: std::sync::Mutex<Vec<(String, String, bool)>>,
}

fn uuid_like() -> String {
    // Version-4 shaped identifier derived from time + pid (no RNG crate available).
    let n = zel_events::now_millis() as u128;
    let p = std::process::id() as u128;
    let x = n
        .wrapping_mul(0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C835)
        .wrapping_add(p.rotate_left(17));
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
    execroot: std::sync::Mutex<String>,
    exec_log: Option<ExecutionLogWriter>,
    include_label: bool,
    /// Finished action spans for `action count` counters, `BuildMetrics`, and critical path.
    spans: std::sync::Mutex<Vec<ActionSpan>>,
}

impl ActionListener for ActionObserver {
    fn action_started(&self, a: &ActionInfo) {
        self.ui
            .action_started(&a.id, &a.progress_message, Some(&a.owner), &a.mnemonic);
        self.ui.action_phase(
            &a.id,
            zel_events::ui::ActionPhase::Running,
            if a.is_spawn { "local" } else { "internal" },
        );
    }

    fn action_finished(&self, a: &ActionInfo, r: &ActionResultInfo) {
        self.ui.action_completed(&a.id);
        let Some(reporter) = self.reporter.upgrade() else {
            return;
        };
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
        profiler.log_simple_task(
            end_ns,
            1_000,
            ProfilerTask::ACTION_COMPLETE,
            &a.progress_message,
        );
        let runner_name = if r.cache_hit {
            "disk cache hit".to_string()
        } else if let Some(rn) = &r.runner {
            rn.clone()
        } else if a.is_spawn {
            "local".to_string()
        } else {
            "internal".to_string()
        };
        self.spans.lock().unwrap().push(ActionSpan {
            start_ns: (end_ns - dur_ns).max(0),
            end_ns,
            start_millis: r.start_millis,
            end_millis: r.start_millis + r.wall_time_ms as i64,
            is_local_spawn: a.is_spawn && !r.cache_hit,
            cache_hit: r.cache_hit,
            mnemonic: a.mnemonic.clone(),
            runner: runner_name,
            description: a.progress_message.clone(),
            inputs: a.inputs.iter().map(|(p, _)| p.clone()).collect(),
            outputs: a.outputs.clone(),
        });
        if self.bep && (!r.success || self.publish_all) {
            let cfg = self.config_id.lock().unwrap().clone();
            let po_path = if r.success {
                let er = self.execroot.lock().unwrap().clone();
                let rel = a
                    .outputs
                    .first()
                    .map(String::as_str)
                    .unwrap_or(a.id.as_str());
                if !er.is_empty() && !rel.is_empty() {
                    let candidate = format!("{er}/{rel}");
                    std::fs::metadata(&candidate)
                        .ok()
                        .filter(|m| m.is_file())
                        .map(|_| candidate)
                } else {
                    None
                }
            } else {
                None
            };
            let fd = (!r.success).then(|| {
                FailureDetail::new(
                    r.error.clone().unwrap_or_else(|| {
                        format!("{} failed: (Exit {})", a.mnemonic, r.exit_code)
                    }),
                    Category::named("spawn", "NON_ZERO_EXIT").unwrap_or(Category::Submessage {
                        field_number: 123,
                        code: 1,
                    }),
                )
            });
            reporter.post_bep_node(BepBuilder::action_executed_with_primary_output_path(
                &a.id,
                po_path.as_deref(),
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
                paths
                    .iter()
                    .map(|(p, _)| SpawnFile {
                        path: p.clone(),
                        is_tool: a.tool_inputs.contains(p),
                        ..Default::default()
                    })
                    .collect()
            };
            let (remotable, cacheable, remote_cacheable) =
                zel_events::exec_log::execution_flags(&a.execution_info);
            let _ = self.include_label;
            log.log(&SpawnRecord {
                args: a.argv.clone(),
                env: a.env.clone(),
                inputs: files(&a.inputs),
                listed_outputs: a.outputs.clone(),
                actual_outputs: if r.success {
                    a.outputs
                        .iter()
                        .map(|p| SpawnFile {
                            path: p.clone(),
                            ..Default::default()
                        })
                        .collect()
                } else {
                    Vec::new()
                },
                remotable,
                cacheable,
                remote_cacheable,
                mnemonic: a.mnemonic.clone(),
                target_label: a.owner.clone(),
                runner: if r.cache_hit {
                    "remote cache hit".into()
                } else {
                    r.runner.clone().unwrap_or_else(|| "local".into())
                },
                cache_hit: r.cache_hit,
                status: if r.exit_code == 0 {
                    String::new()
                } else {
                    "NON_ZERO_EXIT".into()
                },
                exit_code: r.exit_code,
                total_time_ms: Some(r.wall_time_ms as i64),
                ..Default::default()
            });
        }
    }
}

impl BuildObserver {
    pub(crate) fn new_with_fallback_invocation_id(
        parsed: Option<&ParsedCommandOptions>,
        command: &str,
        output_base: &str,
        fallback_invocation_id: Option<&str>,
    ) -> Self {
        let get = |n: &str| {
            parsed.and_then(|p| {
                let d = p.parsed_options.get(n)?;
                if !d.definition.allow_multiple {
                    match &d.value {
                        zel_options::OptionValue::Void => return None,
                        zel_options::OptionValue::String(s) => return Some(s.clone()),
                        _ => {}
                    }
                }
                d.raw_values.last().cloned()
            })
        };
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
        if let Some(pat) = get("output_filter") {
            match zel_events::OutputFilter::from_regex(&pat) {
                Ok(f) => reporter.set_output_filter(f),
                Err(e) => setup_errors.push(format!("--output_filter: {e}")),
            }
        }

        // Build Event Protocol.
        let bep_opts = BepOptions::from_flags(&get, &get_all);
        let bep_enabled = bep_opts.any_enabled();
        let invocation_id = get("invocation_id")
            .filter(|s| !s.is_empty())
            .or_else(|| {
                fallback_invocation_id
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(uuid_like);
        let build_request_id = get("build_request_id")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(uuid_like);
        let build_metadata: Vec<(String, String)> = get_all("build_metadata")
            .into_iter()
            .filter_map(|entry| {
                let (k, v) = entry.split_once('=')?;
                if k.is_empty() {
                    return None;
                }
                Some((k.to_string(), v.to_string()))
            })
            .collect();
        let include_run_residue = parsed
            .and_then(|p| p.get_bool("experimental_run_bep_event_include_residue"))
            .unwrap_or(false);
        let mut bes_t = None;
        if bep_enabled {
            match bes::transport_from_options(&bep_opts, &build_request_id, &invocation_id, command)
            {
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
        let default_profile = profile_flag.is_none();
        let is_build_like = command == "query" || zel_options::command_analyzes(command);
        let profile_path = profiler::resolve_profile_path(
            profile_flag.as_deref(),
            get("generate_json_trace_profile").as_deref(),
            output_base,
            is_build_like,
        );
        // `Profiler.start`: the profile file is created up front; a missing directory is an error.
        let profile_path = match profile_path {
            Some((path, _gz))
                if std::path::Path::new(&path)
                    .parent()
                    .is_some_and(|d| !d.as_os_str().is_empty() && !d.exists()) =>
            {
                setup_errors.push(format!(
                    "Error while creating profile file: {path} (No such file or directory)"
                ));
                None
            }
            other => other,
        };
        let mut popts =
            match ProfilerOptions::from_flags(&get, &get_all, &invocation_id, output_base) {
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
            execroot: std::sync::Mutex::new(String::new()),
            exec_log,
            include_label: false,
            spans: Default::default(),
        });
        reporter.add_action_listener(actions.clone());

        let started = Instant::now();
        let cpu_at_start = crate::sys::cpu_seconds();
        let sampler = profile_path
            .as_ref()
            .map(|_| ResourceSampler::start(started));
        let parse_bool =
            |n: &str, d: bool| match get(n).map(|s| s.trim().to_ascii_lowercase()).as_deref() {
                Some("0") | Some("false") | Some("f") | Some("no") | Some("n") => false,
                Some("1") | Some("true") | Some("t") | Some("yes") | Some("y") => true,
                Some(_) => d,
                None => d,
            };
        let perform_build = parse_bool("build", true);
        let record_all_mnemonics =
            parse_bool("experimental_record_metrics_for_all_mnemonics", false);
        let memory_profile = get("memory_profile").is_some_and(|s| !s.is_empty());

        Self {
            reporter,
            profiler,
            actions,
            ui,
            out,
            profile_path,
            sampler,
            default_profile,
            output_base: output_base.to_string(),
            invocation_id,
            include_run_residue,
            bes: bes_t,
            setup_errors: std::sync::Mutex::new(setup_errors),
            started,
            cpu_at_start,
            command: command.to_string(),
            perform_build,
            record_all_mnemonics,
            memory_profile,
            namer: std::sync::Mutex::new(bep::ArtifactGroupNamer::new()),
            workspace_dir: Default::default(),
            stats: Default::default(),
            late_nodes: Default::default(),
            patterns: Default::default(),
            analysis_complete: Default::default(),
            bep_enabled,
            bep_opts,
            build_metadata,
            convenience_symlinks_bep_event: parsed
                .and_then(|p| p.get_bool("experimental_convenience_symlinks_bep_event"))
                .unwrap_or(true),
            convenience_symlinks: Default::default(),
        }
    }

    pub(crate) fn set_convenience_symlinks(&self, links: Vec<(String, String, bool)>) {
        *self.convenience_symlinks.lock().unwrap() = links;
    }

    /// Errors found while configuring observability (bad flag values, unopenable files).
    pub(crate) fn take_setup_errors(&self) -> Vec<String> {
        std::mem::take(&mut *self.setup_errors.lock().unwrap())
    }

    #[allow(dead_code)]
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
        *self.workspace_dir.lock().unwrap() = workspace.to_string();
        if !self.bep_enabled {
            return;
        }
        let r = &self.reporter;
        let host = std::env::var("HOSTNAME").unwrap_or_default();
        let user = std::env::var("USER").unwrap_or_default();
        let mut children = BepBuilder::started_children(patterns);
        if !self.convenience_symlinks_bep_event {
            let sym_id = ids::convenience_symlinks_identified();
            children.retain(|c| c != &sym_id);
        }
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
            children,
        ));
        r.post_bep_node(BepBuilder::build_metadata(&self.build_metadata));
        let dash_pos = args.iter().position(|a| a == "--");
        let before_dash = match dash_pos {
            Some(i) => &args[..i],
            None => args,
        };
        let after_dash = match dash_pos {
            Some(i) => &args[i + 1..],
            None => &[],
        };
        let mut full = vec![self.command.clone()];
        let mut residue: Vec<String> = before_dash
            .iter()
            .filter(|a| !a.starts_with('-'))
            .cloned()
            .chain(after_dash.iter().cloned())
            .collect();
        if self.command == "run" && !self.include_run_residue && residue.len() > 1 {
            residue = vec![residue[0].clone(), "REDACTED".to_string()];
            if let Some(i) = dash_pos {
                full.extend(args[..i].iter().cloned());
                full.push("--".to_string());
                full.push("REDACTED".to_string());
            } else {
                let mut seen_target = false;
                let mut added_redacted = false;
                for a in args {
                    if !a.starts_with('-') {
                        if !seen_target {
                            seen_target = true;
                            full.push(a.clone());
                        } else if !added_redacted {
                            added_redacted = true;
                            full.push("REDACTED".to_string());
                        }
                    } else if !seen_target {
                        full.push(a.clone());
                    }
                }
            }
        } else {
            full.extend(args.iter().cloned());
        }
        r.post_bep_node(BepBuilder::unstructured_command_line(&full));
        let exp_before_dash = match explicit_options.iter().position(|a| a == "--") {
            Some(i) => &explicit_options[..i],
            None => explicit_options,
        };
        let opts: Vec<(String, String)> = exp_before_dash
            .iter()
            .filter_map(|o| o.strip_prefix("--"))
            .map(|o| match o.split_once('=') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (o.to_string(), String::new()),
            })
            .collect();
        r.post_bep_node(BepBuilder::options_parsed(
            &[],
            &[],
            exp_before_dash,
            exp_before_dash,
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
            (
                "BUILD_TIMESTAMP".to_string(),
                (zel_events::now_millis() / 1000).to_string(),
            ),
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
    pub(crate) fn analysis_done(
        &self,
        patterns: &[String],
        configured: &[ConfiguredTarget],
        roots: &HashSet<String>,
        cpu: &str,
        mode: &str,
        execroot: &str,
    ) {
        // The progress bar is only drawn while `UiEventHandler::build_started` is in effect, which
        // needs a live output sink; command output is still buffered by the daemon, so only the
        // tracker state is maintained here (see AGENT_NOTES.md).
        self.analysis_complete
            .store(true, std::sync::atomic::Ordering::SeqCst);
        CUMULATIVE_ANALYSES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.perform_build {
            CUMULATIVE_BUILDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        self.ui.analysis_complete_silent();
        self.ui.execution_phase_started();
        let total_actions: u64 = configured.iter().map(|c| c.actions.len() as u64).sum();
        self.ui.set_total_actions(total_actions);
        {
            let mut st = self.stats.lock().unwrap();
            let mut pkgs = HashSet::new();
            let mut srcs = HashSet::new();
            let mut outs = HashSet::new();
            let mut top_arts = 0usize;
            for ct in configured {
                pkgs.insert(ct.label.package_identifier().clone());
                let entry = st
                    .rule_classes
                    .entry(ct.rule_class.clone())
                    .or_insert((0, 0));
                entry.0 += 1;
                entry.1 += ct.actions.len() as u64;
                for act in &ct.actions {
                    *st.created_by_mnemonic
                        .entry(act.mnemonic().to_string())
                        .or_insert(0) += 1;
                    for inp in act.inputs() {
                        if inp.is_source {
                            srcs.insert(inp.exec_path.path_string().to_string());
                        }
                    }
                    for out in act.outputs() {
                        outs.insert(out.exec_path.path_string().to_string());
                    }
                }
                if roots.contains(&ct.label.canonical_form()) {
                    top_arts += ct
                        .default_info
                        .files
                        .iter()
                        .chain(ct.default_info.executable.iter())
                        .filter(|a| !a.is_source)
                        .count();
                }
            }
            st.packages_loaded = pkgs.len() as i64;
            st.targets_configured = configured.len() as i64;
            st.actions_created = total_actions as i64;
            st.source_artifacts = srcs.len();
            st.output_artifacts = outs.len();
            st.top_level_artifacts = top_arts;
            st.analysis_done_ns = self.profiler.now_ns();
        }
        if !self.bep_enabled {
            return;
        }
        let cfg_id = bep_config_id(&self.command, cpu, mode);
        self.reporter.set_default_config_id(&cfg_id);
        *self.actions.config_id.lock().unwrap() = cfg_id.clone();
        *self.actions.execroot.lock().unwrap() = execroot.to_string();
        let root_labels: Vec<String> = configured
            .iter()
            .map(|c| c.label.canonical_form())
            .filter(|l| roots.contains(l))
            .collect();
        let mut node = BepBuilder::pattern_expanded(patterns, &root_labels);
        // Children of the pattern event are the configured targets.
        node.children = root_labels
            .iter()
            .map(|l| ids::target_configured(l))
            .collect();
        self.reporter.post_bep_node(node);
        // `WorkspaceConfigEvent`, then the canonical command line (both announced elsewhere, so
        // Bazel's streamer chains them in through progress events).
        self.reporter
            .post_bep_node(BepBuilder::workspace_config(execroot));
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
            let n = BepBuilder::target_configured(
                &l,
                &cfg_id,
                &format!("{} rule", ct.rule_class),
                &[],
                None,
                self.perform_build,
            );
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
        if !self.bep_enabled || !self.perform_build {
            return;
        }
        let cfg_id = bep_config_id(&self.command, cpu, mode);
        for ct in configured {
            let l = ct.label.canonical_form();
            if !roots.contains(&l) {
                continue;
            }
            let success = !failed.contains(&l);
            let mut groups: Vec<bep::BepOutputGroup> = Vec::new();
            let mut directory_outputs: Vec<zel_proto::dynamic::Msg> = Vec::new();
            let mut seen_dir = HashSet::new();
            if success {
                let mut all: Vec<(String, Vec<_>)> = vec![(
                    "default".to_string(),
                    ct.default_info
                        .files
                        .iter()
                        .chain(ct.default_info.executable.iter())
                        .filter(|a| !a.is_source)
                        .collect(),
                )];
                for (name, files) in &ct.output_groups.groups {
                    if name == "default" {
                        continue;
                    }
                    all.push((
                        name.clone(),
                        files.iter().filter(|a| !a.is_source).collect(),
                    ));
                }
                for (name, files) in all {
                    let mut seen = HashSet::new();
                    let dedup_files: Vec<_> = files
                        .into_iter()
                        .filter(|a| seen.insert(a.exec_path.path_string().to_string()))
                        .collect();
                    if dedup_files.is_empty() {
                        continue;
                    }
                    let msgs: Vec<_> = dedup_files
                        .iter()
                        .map(|a| {
                            let p = a.exec_path.path_string().to_string();
                            let (prefix, rest) = split_exec_path(&p);
                            let full_path = format!("{execroot}/{p}");
                            if a.is_tree_artifact {
                                let dir_msg = BepBuilder::file_without_uri(&rest, &prefix);
                                if seen_dir.insert(p) {
                                    directory_outputs.push(dir_msg.clone());
                                }
                                dir_msg
                            } else if a.is_unresolved_symlink {
                                let target = std::fs::read_link(&full_path)
                                    .ok()
                                    .map(|t| t.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                BepBuilder::symlink_file(&rest, &prefix, &target)
                            } else {
                                let (len, dig) = match zel_util::sha256::hash_file(
                                    std::path::Path::new(&full_path),
                                ) {
                                    Ok((bytes_len, hex)) => (Some(bytes_len as i64), Some(hex)),
                                    Err(_) => (None, None),
                                };
                                BepBuilder::file(&rest, &full_path, &prefix, len, dig.as_deref())
                            }
                        })
                        .collect();
                    let group_mode = self.bep_opts.output_group_mode(&name);
                    let mut file_sets = Vec::new();
                    let mut inline_files = Vec::new();
                    if matches!(
                        group_mode,
                        bep::OutputGroupMode::NamedSetOfFilesOnly | bep::OutputGroupMode::Both
                    ) {
                        let (set_id, nodes) = self
                            .namer
                            .lock()
                            .unwrap()
                            .maybe_deposit(msgs.clone(), max_named_set_entries);
                        for n in nodes {
                            self.reporter.post_bep_node(n);
                        }
                        file_sets.push(set_id);
                    }
                    if matches!(
                        group_mode,
                        bep::OutputGroupMode::InlineOnly | bep::OutputGroupMode::Both
                    ) {
                        inline_files = msgs;
                    }
                    groups.push(bep::BepOutputGroup {
                        name,
                        file_sets,
                        inline_files,
                        incomplete: false,
                    });
                }
            }
            let tags: Vec<String> = Vec::new();
            let mut node = BepBuilder::target_completed_with_groups(
                &l,
                &cfg_id,
                None,
                success,
                &groups,
                &directory_outputs,
                &tags,
                None,
            );
            // Named sets are chained in through progress events rather than announced here.
            node.children = Vec::new();
            if !success {
                // The failed action of the target is announced by its `TargetComplete`.
                if let Some(a) = ct
                    .default_info
                    .files
                    .iter()
                    .chain(ct.default_info.executable.iter())
                    .find(|a| !a.is_source)
                {
                    node.children.push(ids::action_completed(
                        a.exec_path.path_string(),
                        &l,
                        &cfg_id,
                    ));
                }
            }
            if success {
                if let Some((runs, shards)) = test_units.get(&l) {
                    for run in 0..*runs {
                        for shard in 0..*shards {
                            node.children
                                .push(ids::test_result(&l, &cfg_id, run, shard, 1));
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
        for e in self.take_setup_errors() {
            self.reporter.handle(Event::error(None, e));
        }
        let mut out_idx = 0usize;
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        while out_idx < lines.len() {
            let line = lines[out_idx];
            out_idx += 1;
            let body = line.strip_suffix('\n').unwrap_or(line);
            let hit = [
                zel_events::EventKind::Fatal,
                zel_events::EventKind::Error,
                zel_events::EventKind::Warning,
                zel_events::EventKind::Info,
                zel_events::EventKind::Debug,
                zel_events::EventKind::Fail,
                zel_events::EventKind::Pass,
                zel_events::EventKind::Subcommand,
                zel_events::EventKind::Cancelled,
            ]
            .iter()
            .find_map(|k| {
                body.strip_prefix(k.prefix().as_str())
                    .map(|rest| (*k, rest))
            });
            if let Some((zel_events::EventKind::Info, rest)) = hit {
                if rest.starts_with("From ") && rest.ends_with(':') {
                    let mut stderr_lines = String::new();
                    while out_idx < lines.len() {
                        let nl = lines[out_idx];
                        let nb = nl.strip_suffix('\n').unwrap_or(nl);
                        let is_tagged = [
                            "FATAL: ",
                            "ERROR: ",
                            "WARNING: ",
                            "INFO: ",
                            "DEBUG: ",
                            "FAIL: ",
                            "PASS: ",
                            "SUBCOMMAND: ",
                            "CANCELLED: ",
                            "Target ",
                            "Executed ",
                            "Use --",
                        ]
                        .iter()
                        .any(|p| nb.starts_with(p));
                        if is_tagged {
                            break;
                        }
                        stderr_lines.push_str(nl);
                        out_idx += 1;
                    }
                    self.reporter.handle(Event::info(rest.to_string()));
                    if !stderr_lines.is_empty() && !self.ui.is_filtered(zel_events::EventKind::Info)
                    {
                        for sl in stderr_lines.split_inclusive('\n') {
                            self.ui.write_raw_stderr(sl.as_bytes());
                        }
                    }
                    continue;
                }
            }
            for e in ui::events_from_console_text(line) {
                self.reporter.handle(e);
            }
        }
        self.profiler.mark_phase(ProfilePhase::FINISH);
        let mut critical_path_ns = 0i64;
        {
            // `action count` / `action count (local)` lanes and multi-component critical path.
            let spans = self.actions.spans.lock().unwrap();
            if !spans.is_empty() {
                let bucket = 200_000_000i64;
                let all: Vec<(i64, i64)> = spans.iter().map(|s| (s.start_ns, s.end_ns)).collect();
                self.profiler.add_action_count_series(&all, bucket);
                let local: Vec<(i64, i64)> = spans
                    .iter()
                    .filter(|s| s.is_local_spawn)
                    .map(|s| (s.start_ns, s.end_ns))
                    .collect();
                if !local.is_empty() {
                    let mut ts = profiler::TimeSeries::new(0, bucket);
                    for &(st, en) in &local {
                        ts.add_range(st, en, 1.0);
                    }
                    let v = ts.into_values();
                    if !v.is_empty() {
                        self.profiler.add_counter_series(
                            "action count (local)",
                            None,
                            0,
                            bucket,
                            &[("action".to_string(), v)],
                        );
                    }
                }
                let mut producer: std::collections::HashMap<&str, usize> =
                    std::collections::HashMap::new();
                for (i, s) in spans.iter().enumerate() {
                    for out in &s.outputs {
                        producer.entry(out.as_str()).or_insert(i);
                    }
                }
                let mut cum_ns = vec![0i64; spans.len()];
                let mut pred: Vec<Option<usize>> = vec![None; spans.len()];
                for (i, s) in spans.iter().enumerate() {
                    let mut seen_p = HashSet::new();
                    let mut best_p: Option<(usize, i64)> = None;
                    for inp in &s.inputs {
                        if let Some(&p) = producer.get(inp.as_str()) {
                            if p < i && seen_p.insert(p) {
                                let pf = cum_ns[p];
                                if best_p.map(|(_, b)| pf > b).unwrap_or(true) {
                                    best_p = Some((p, pf));
                                }
                            }
                        }
                    }
                    pred[i] = best_p.map(|(p, _)| p);
                    let dur = (s.end_ns - s.start_ns).max(0);
                    cum_ns[i] = best_p.map(|(_, b)| b).unwrap_or(0) + dur;
                }
                let mut best_root: Option<(usize, i64)> = None;
                for (i, &c) in cum_ns.iter().enumerate() {
                    if best_root.map(|(_, b)| c > b).unwrap_or(true) {
                        best_root = Some((i, c));
                    }
                }
                if let Some((root_idx, total_ns)) = best_root {
                    critical_path_ns = total_ns;
                    let mut cur = Some(root_idx);
                    while let Some(ci) = cur {
                        let s = &spans[ci];
                        self.profiler.log_critical_path_component(
                            s.start_ns,
                            (s.end_ns - s.start_ns).max(0),
                            &format!("action '{}'", s.description),
                        );
                        cur = pred[ci];
                    }
                }
            }
        }
        // Counter lanes of `CollectLocalResourceUsage`.
        {
            let wall = self.started.elapsed();
            let ns = wall.as_nanos().max(1).min(i64::MAX as u128) as i64;
            let samples = self
                .sampler
                .as_ref()
                .map(|s| s.stop_and_take(self.started))
                .unwrap_or_default();
            if samples.len() >= 3 && ns >= 200_000_000 {
                let bucket = 200_000_000i64;
                let mut cpu_ts = profiler::TimeSeries::new(0, bucket);
                let mut rss_ts = profiler::TimeSeries::new(0, bucket);
                let mut load_ts = profiler::TimeSeries::new(0, bucket);
                for win in samples.windows(2) {
                    let (t0, c0, _, _) = win[0];
                    let (t1, c1, r1, l1) = win[1];
                    if t1 <= t0 {
                        continue;
                    }
                    let dt_s = ((t1 - t0) as f64 / 1_000_000_000.0).max(1e-6);
                    let cpu_cores = ((c1 - c0).max(0.0) / dt_s).min(64.0);
                    cpu_ts.add_range(t0, t1, cpu_cores);
                    rss_ts.add_range(t0, t1, r1);
                    load_ts.add_range(t0, t1, l1);
                }
                let cpu_v = cpu_ts.into_values();
                let rss_v = rss_ts.into_values();
                let load_v = load_ts.into_values();
                let zeroes = vec![0.0; cpu_v.len().max(1)];
                let series: [(&str, &[f64]); 8] = [
                    ("CPU usage (Bazel)", &cpu_v),
                    ("CPU usage (total)", &cpu_v),
                    ("Memory usage (Bazel)", &rss_v),
                    ("Memory usage (total)", &rss_v),
                    ("Network Down usage (total)", &zeroes),
                    ("Network Up usage (total)", &zeroes),
                    ("System load average", &load_v),
                    ("Total worker memory usage", &zeroes),
                ];
                for (lane, v) in series {
                    if !v.is_empty() {
                        self.profiler.add_counter_series(
                            lane,
                            None,
                            0,
                            bucket,
                            &[(lane.to_string(), v.to_vec())],
                        );
                    }
                }
            } else {
                let cpu = ((crate::sys::cpu_seconds() - self.cpu_at_start).max(0.0)
                    / wall.as_secs_f64().max(0.001))
                .min(64.0);
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
                    self.profiler.add_counter_series(
                        lane,
                        None,
                        0,
                        ns,
                        &[(lane.to_string(), vec![v, v])],
                    );
                }
            }
        }
        let analysis_ran = self
            .analysis_complete
            .load(std::sync::atomic::Ordering::SeqCst);
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
        let name = if name == "PARSING_FAILURE"
            && matches!(self.command.as_str(), "build" | "test" | "coverage" | "run")
        {
            "BUILD_FAILURE"
        } else {
            name
        };
        if self.bep_enabled {
            let r = &self.reporter;
            let s = r.bep_streamer();
            s.add_abort_reason(
                if code == ExitCode::INTERRUPTED {
                    "USER_INTERRUPTED"
                } else {
                    "INCOMPLETE"
                },
                "",
            );
            for (url, ok) in zel_bzlmod::registry::take_recorded_fetches() {
                r.post_bep_node(BepBuilder::fetch(&url, ok));
            }
            if analysis_ran {
                if self.convenience_symlinks_bep_event {
                    let links = self.convenience_symlinks.lock().unwrap().clone();
                    r.post_bep_node(BepBuilder::convenience_symlinks(&links));
                }
                r.post_bep_node(BepBuilder::build_finished(
                    name,
                    num,
                    zel_events::now_millis(),
                    failure,
                ));
            } else {
                // Analysis never completed (e.g. a missing target): emit pattern expansion and
                // all late events announced by `BuildStarted` (canonical command line, workspace
                // status) prior to `BuildFinished` so none are erroneously aborted.
                let pats = self.patterns.lock().unwrap().clone();
                r.post_bep_node(BepBuilder::pattern_expanded(&pats, &[]));
                let rest: Vec<_> = self.late_nodes.lock().unwrap().drain(..).collect();
                for n in rest {
                    r.post_bep_node(n);
                }
                r.post_bep_node(BepBuilder::build_finished(
                    name,
                    num,
                    zel_events::now_millis(),
                    failure,
                ));
            }
            r.post_bep_node(BepBuilder::build_tool_logs(&[]));
            r.post_bep_node(BepBuilder::build_metrics(
                self.build_metrics_msg(critical_path_ns),
            ));
            if let Some(b) = &self.bes {
                b.set_success(num == 0);
            }
            r.finish_bep();
            let ws_dir = self.workspace_dir.lock().unwrap().clone();
            let resolve_bep_path = |raw: &str| -> String {
                let p = std::path::Path::new(raw);
                if p.is_absolute() || ws_dir.is_empty() {
                    raw.to_string()
                } else {
                    std::path::Path::new(&ws_dir)
                        .join(p)
                        .to_string_lossy()
                        .into_owned()
                }
            };
            let mut wrote_any_bep_file = false;
            let mut file_write_failed = false;
            for (path, fmt, conv) in [
                (
                    &self.bep_opts.json_file,
                    bep::BepFormat::Json,
                    self.bep_opts.json_path_conversion,
                ),
                (
                    &self.bep_opts.binary_file,
                    bep::BepFormat::Binary,
                    self.bep_opts.binary_path_conversion,
                ),
                (
                    &self.bep_opts.text_file,
                    bep::BepFormat::Text,
                    self.bep_opts.text_path_conversion,
                ),
            ] {
                if let Some(raw) = path {
                    let resolved = resolve_bep_path(raw);
                    let pf = PathFragment::create(&resolved);
                    match r.force_write_bep_file_with_options(
                        fs,
                        &pf,
                        fmt,
                        conv,
                        self.bep_opts.remote_bytestream_uri_prefix.as_deref(),
                    ) {
                        Ok(()) => {
                            wrote_any_bep_file = wrote_any_bep_file || fs.exists(&pf, true);
                        }
                        Err(e) => {
                            file_write_failed = true;
                            r.handle(Event::error(
                                None,
                                format!("Unable to write to '{resolved}': {e}"),
                            ));
                        }
                    }
                }
            }
            let errs = s.close_errors();
            if !errs.is_empty() || file_write_failed {
                if self.bes.is_some() && !errs.is_empty() {
                    r.handle(Event::info(""));
                    r.handle(Event::info(""));
                }
                for e in &errs {
                    let msg = if e.contains("Connection refused") {
                        "The Build Event Protocol upload failed: All 4 retry attempts failed. UNAVAILABLE: finishConnect(..) failed: Connection refused UNAVAILABLE: finishConnect(..) failed: Connection refused".to_string()
                    } else {
                        format!("Build Event Protocol upload failed: {e}")
                    };
                    r.handle(Event::error(None, msg));
                }
                if code == ExitCode::SUCCESS {
                    code = if file_write_failed && errs.is_empty() {
                        ExitCode::LOCAL_ENVIRONMENTAL_ERROR
                    } else {
                        ExitCode::TRANSIENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR
                    };
                }
            } else if code != ExitCode::LOCAL_ENVIRONMENTAL_ERROR && wrote_any_bep_file {
                r.handle(Event::info(
                    "Build Event Protocol files produced successfully.",
                ));
            }
        }
        // Profile.
        if let Some((path, _)) = &self.profile_path {
            let default_cmd_profile = format!("{}/command.profile.gz", self.output_base);
            if self.default_profile && path == &default_cmd_profile {
                let per_inv = format!(
                    "{}/command-{}.profile.gz",
                    self.output_base, self.invocation_id
                );
                let per_inv_pf = PathFragment::create(&per_inv);
                if let Err(e) = self.profiler.write_to(fs, &per_inv_pf) {
                    self.reporter.handle(Event::error(
                        None,
                        format!("Error while writing profile file: {e}"),
                    ));
                } else {
                    let link_path = std::path::Path::new(&default_cmd_profile);
                    let _ = std::fs::remove_file(link_path);
                    #[cfg(unix)]
                    let linked = {
                        let target_name = format!("command-{}.profile.gz", self.invocation_id);
                        std::os::unix::fs::symlink(&target_name, link_path).is_ok()
                    };
                    #[cfg(not(unix))]
                    let linked = false;
                    if !linked {
                        let _ = std::fs::copy(&per_inv, link_path);
                    }
                }
            } else {
                let p = PathFragment::create(path);
                if let Err(e) = self.profiler.write_to(fs, &p) {
                    self.reporter.handle(Event::error(
                        None,
                        format!("Error while writing profile file: {e}"),
                    ));
                }
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

    pub(crate) fn clear_pumped_analyzed_line(&self, line: &str) {
        use zel_events::ui::OutErr;
        let err = self.out.take_err();
        let text = String::from_utf8_lossy(&err).into_owned();
        let target = format!("{line}\n");
        let replaced = text.replace(&target, "");
        self.out.err(replaced.as_bytes());
    }

    pub(crate) fn insert_analyzed_after_first_subcommand(&self, line: &str) {
        use zel_events::ui::OutErr;
        let err = self.out.take_err();
        let text = String::from_utf8_lossy(&err).into_owned();
        let needle = "# Runner: linux-sandbox\n";
        if let Some(pos) = text.find(needle) {
            let cut = pos + needle.len();
            let mut out = String::with_capacity(text.len() + line.len() + 2);
            out.push_str(&text[..cut]);
            out.push_str(line);
            out.push('\n');
            out.push_str(&text[cut..]);
            self.out.err(out.as_bytes());
        } else {
            self.out.err(&err);
        }
    }

    pub(crate) fn action_descriptions(&self) -> Vec<String> {
        self.actions
            .spans
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.description.clone())
            .collect()
    }

    fn build_metrics_msg(&self, critical_path_ns: i64) -> zel_proto::dynamic::Msg {
        use zel_proto::dynamic::Msg;
        let mut m = Msg::new("build_event_stream.BuildMetrics");
        let st = self.stats.lock().unwrap();
        let spans = self.actions.spans.lock().unwrap();

        // ActionSummary
        let mut action_summary = Msg::new("build_event_stream.BuildMetrics.ActionSummary");
        action_summary.set_int("actions_created", st.actions_created);
        action_summary.set_int("actions_created_not_including_aspects", st.actions_created);
        let executed_count = spans.iter().filter(|s| !s.cache_hit).count() as i64;
        action_summary.set_int("actions_executed", executed_count);

        #[derive(Default)]
        struct MnemonicAgg {
            executed: i64,
            created: i64,
            first_started_ms: Option<i64>,
            last_ended_ms: Option<i64>,
        }
        let mut by_mnem: std::collections::BTreeMap<String, MnemonicAgg> =
            std::collections::BTreeMap::new();
        for (mn, &c) in &st.created_by_mnemonic {
            by_mnem.entry(mn.clone()).or_default().created = c;
        }
        let mut runners: std::collections::BTreeMap<String, i32> =
            std::collections::BTreeMap::new();
        let mut cache_hit_outputs = 0usize;
        let mut exec_outputs = 0usize;
        for sp in spans.iter() {
            if sp.cache_hit {
                cache_hit_outputs += sp.outputs.len();
            } else {
                exec_outputs += sp.outputs.len();
                let agg = by_mnem.entry(sp.mnemonic.clone()).or_default();
                agg.executed += 1;
                agg.first_started_ms = Some(
                    agg.first_started_ms
                        .map(|f| f.min(sp.start_millis))
                        .unwrap_or(sp.start_millis),
                );
                agg.last_ended_ms = Some(
                    agg.last_ended_ms
                        .map(|l| l.max(sp.end_millis))
                        .unwrap_or(sp.end_millis),
                );
            }
            *runners.entry(sp.runner.clone()).or_insert(0) += 1;
        }
        let mut mnem_list: Vec<(String, MnemonicAgg)> = by_mnem.into_iter().collect();
        mnem_list.sort_by(|a, b| {
            b.1.executed
                .cmp(&a.1.executed)
                .then_with(|| b.1.created.cmp(&a.1.created))
                .then_with(|| a.0.cmp(&b.0))
        });
        if !self.record_all_mnemonics && mnem_list.len() > 20 {
            mnem_list.truncate(20);
        }
        for (mn, agg) in mnem_list {
            let mut ad = Msg::new("build_event_stream.BuildMetrics.ActionSummary.ActionData");
            ad.set_str("mnemonic", mn);
            ad.set_int("actions_executed", agg.executed);
            ad.set_int("actions_created", agg.created);
            if let Some(fs_ms) = agg.first_started_ms {
                ad.set_int("first_started_ms", fs_ms);
            }
            if let Some(le_ms) = agg.last_ended_ms {
                ad.set_int("last_ended_ms", le_ms);
            }
            action_summary.set_msg("action_data", ad);
        }
        for (rname, rcount) in runners {
            let mut rc = Msg::new("build_event_stream.BuildMetrics.ActionSummary.RunnerCount");
            let exec_kind = if rname.contains("cache") {
                "Cached"
            } else if rname == "internal" {
                "Internal"
            } else {
                "Local"
            };
            rc.set_str("name", rname);
            rc.set_int("count", rcount as i64);
            rc.set_str("exec_kind", exec_kind);
            action_summary.set_msg("runner_count", rc);
        }
        m.set_msg("action_summary", action_summary);

        // MemoryMetrics
        let mut mem = Msg::new("build_event_stream.BuildMetrics.MemoryMetrics");
        if self.memory_profile {
            mem.set_int(
                "used_heap_size_post_build",
                crate::sys::resident_bytes() as i64,
            );
        }
        m.set_msg("memory_metrics", mem);

        // TargetMetrics
        let mut tm = Msg::new("build_event_stream.BuildMetrics.TargetMetrics");
        tm.set_int("targets_configured", st.targets_configured);
        tm.set_int(
            "targets_configured_not_including_aspects",
            st.targets_configured,
        );
        m.set_msg("target_metrics", tm);

        // PackageMetrics
        let mut pm = Msg::new("build_event_stream.BuildMetrics.PackageMetrics");
        pm.set_int("packages_loaded", st.packages_loaded);
        m.set_msg("package_metrics", pm);

        // TimingMetrics
        let mut timing = Msg::new("build_event_stream.BuildMetrics.TimingMetrics");
        let wall_ms = self.started.elapsed().as_millis().min(i64::MAX as u128) as i64;
        let cpu_ms = ((crate::sys::cpu_seconds() - self.cpu_at_start).max(0.0) * 1000.0) as i64;
        let analysis_ms = (st.analysis_done_ns / 1_000_000).max(0);
        timing.set_int("wall_time_in_ms", wall_ms);
        timing.set_int("cpu_time_in_ms", cpu_ms);
        timing.set_int("analysis_phase_time_in_ms", analysis_ms);
        if let (Some(first_s), Some(last_e)) = (
            spans.iter().map(|s| s.start_ns).min(),
            spans.iter().map(|s| s.end_ns).max(),
        ) {
            timing.set_int(
                "actions_execution_start_in_ms",
                (first_s / 1_000_000).max(0),
            );
            timing.set_int(
                "execution_phase_time_in_ms",
                ((last_e - first_s) / 1_000_000).max(0),
            );
        }
        if critical_path_ns > 0 {
            let mut dur = Msg::new("google.protobuf.Duration");
            dur.set_int("seconds", critical_path_ns / 1_000_000_000);
            dur.set_int("nanos", critical_path_ns % 1_000_000_000);
            timing.set_msg("critical_path_time", dur);
        }
        m.set_msg("timing_metrics", timing);

        // CumulativeMetrics
        let mut cum = Msg::new("build_event_stream.BuildMetrics.CumulativeMetrics");
        cum.set_int(
            "num_analyses",
            CUMULATIVE_ANALYSES.load(std::sync::atomic::Ordering::SeqCst) as i64,
        );
        cum.set_int(
            "num_builds",
            CUMULATIVE_BUILDS.load(std::sync::atomic::Ordering::SeqCst) as i64,
        );
        m.set_msg("cumulative_metrics", cum);

        // ArtifactMetrics
        let files_metric = |count: usize| {
            let mut fm = Msg::new("build_event_stream.BuildMetrics.ArtifactMetrics.FilesMetric");
            fm.set_int("count", count as i64);
            fm
        };
        let mut am = Msg::new("build_event_stream.BuildMetrics.ArtifactMetrics");
        am.set_msg("source_artifacts_read", files_metric(st.source_artifacts));
        am.set_msg(
            "output_artifacts_seen",
            files_metric(exec_outputs + cache_hit_outputs),
        );
        am.set_msg(
            "output_artifacts_from_action_cache",
            files_metric(cache_hit_outputs),
        );
        am.set_msg("top_level_artifacts", files_metric(st.top_level_artifacts));
        m.set_msg("artifact_metrics", am);

        // BuildGraphMetrics
        let mut bgm = Msg::new("build_event_stream.BuildMetrics.BuildGraphMetrics");
        bgm.set_int("action_lookup_value_count", st.targets_configured);
        bgm.set_int(
            "action_lookup_value_count_not_including_aspects",
            st.targets_configured,
        );
        bgm.set_int("action_count", st.actions_created);
        bgm.set_int("action_count_not_including_aspects", st.actions_created);
        bgm.set_int("output_artifact_count", st.output_artifacts as i64);
        for (rc_name, &(cnt, act_cnt)) in &st.rule_classes {
            let mut rcc =
                Msg::new("build_event_stream.BuildMetrics.BuildGraphMetrics.RuleClassCount");
            rcc.set_str("key", rc_name.clone());
            rcc.set_str("rule_class", rc_name.clone());
            rcc.set_uint("count", cnt);
            rcc.set_uint("action_count", act_cnt);
            bgm.set_msg("rule_class", rcc);
        }
        m.set_msg("build_graph_metrics", bgm);

        m
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
    format!(
        "{}{}",
        base,
        zel_util::md5::hex_digest(format!("untrimmed-{base}").as_bytes())
    )
}

fn config_id(cpu: &str, mode: &str) -> String {
    zel_util::md5::hex_digest(format!("{cpu}-{mode}").as_bytes())
}

/// Splits `bazel-out/<cfg>/bin/x/y` into (`["bazel-out", "<cfg>", "bin"]`, `"x/y"`) like
/// `Artifact.getRootRelativePath` / `File.path_prefix`; other paths have no prefix.
fn split_exec_path(p: &str) -> (Vec<String>, String) {
    let parts: Vec<&str> = p.split('/').collect();
    if parts.len() > 3 && parts[0] == "bazel-out" {
        (
            parts[..3].iter().map(|s| s.to_string()).collect(),
            parts[3..].join("/"),
        )
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
            (
                vec!["bazel-out".into(), "k8-fastbuild".into(), "bin".into()],
                "pkg/a.o".to_string()
            )
        );
        assert_eq!(
            split_exec_path("pkg/a.cc"),
            (vec![], "pkg/a.cc".to_string())
        );
    }

    #[test]
    fn config_ids_are_stable_hex() {
        assert_eq!(config_id("k8", "fastbuild").len(), 32);
        assert_eq!(config_id("k8", "opt"), config_id("k8", "opt"));
        assert_ne!(config_id("k8", "opt"), config_id("k8", "dbg"));
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
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
    #[cfg(unix)]
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
        assert!(
            err.contains("Build completed successfully, 2 total actions"),
            "{err}"
        );

        // BEP JSON: well-formed DAG, started first, last message set, outputs reported.
        let json = std::fs::read_to_string(root.join("bep.json")).unwrap();
        let lines: Vec<&str> = json.lines().collect();
        assert!(lines[0].starts_with("{\"id\":{\"started\":{}},\"children\":[{\"progress\":{}},{\"unstructuredCommandLine\":{}},{\"structuredCommandLine\":{\"commandLineLabel\":\"original\"}}"), "{}", lines[0]);
        assert!(
            lines.last().unwrap().contains("\"lastMessage\":true"),
            "{}",
            lines.last().unwrap()
        );
        for needle in [
            "\"buildFinished\":{}",
            "\"targetCompleted\":{\"label\":\"//p:ok\"",
            "\"namedSet\":{\"id\":\"0\"}",
            "\"name\":\"p/ok.txt\"",
            "\"pathPrefix\":[\"bazel-out\"",
            "\"configured\":{\"targetKind\":\"genrule rule\"}",
            "\"exitCode\":{\"name\":\"SUCCESS\"}",
            "\"expanded\":{}",
            "\"configuration\":{\"mnemonic\":\"",
            "\"buildMetrics\":{\"actionSummary\":{",
        ] {
            assert!(json.contains(needle), "missing {needle} in {json}");
        }
        // publish_all_actions: the successful genrule is reported as an ActionExecuted event whose
        // id names the primary output.
        assert!(
            json.contains("\"actionCompleted\":{\"primaryOutput\":\"bazel-out/"),
            "{json}"
        );
        assert!(
            json.contains("\"action\":{\"success\":true,\"primaryOutput\":{\"uri\":\"file://")
                && json.contains("\"type\":\"Genrule\""),
            "{json}"
        );
        // Execution logs: JSON lines and a zstd-framed compact log.
        let exec = std::fs::read_to_string(root.join("exec.json")).unwrap();
        assert_eq!(exec.lines().count(), 1, "{exec}");
        assert!(
            exec.contains("\"mnemonic\":\"Genrule\"")
                && exec.contains("\"targetLabel\":\"//p:ok\""),
            "{exec}"
        );
        let z = std::fs::read(root.join("exec.zst")).unwrap();
        let raw = zel_events::exec_log::zstd_unstore(&z).unwrap();
        assert!(!raw.is_empty());
        // Every event decodes from the binary stream and matches the JSON stream one-to-one.
        let bin = std::fs::read(root.join("bep.bin")).unwrap();
        let mut pos = 0;
        let mut n = 0;
        while pos < bin.len() {
            let len = zel_proto::wire::decode_varint(&bin, &mut pos).unwrap() as usize;
            let m = zel_proto::dynamic::Msg::decode(
                "build_event_stream.BuildEvent",
                &bin[pos..pos + len],
            )
            .unwrap();
            assert_eq!(m.to_json(), lines[n]);
            pos += len;
            n += 1;
        }
        assert_eq!(n, lines.len());
        let text = std::fs::read_to_string(root.join("bep.txt")).unwrap();
        assert!(
            text.starts_with(
                "event {\nid {\n  started {\n  }\n}\nchildren {\n  progress {\n  }\n}"
            ),
            "{text}"
        );
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
            &[
                "build",
                "--color=yes",
                &format!("--build_event_json_file={}", r("bad.json")),
                "//p:bad",
            ],
        );
        assert_eq!(code, 1, "{err}");
        assert!(err.contains("\x1b[31m\x1b[1mERROR: \x1b[0m"), "{err:?}");
        let bad = std::fs::read_to_string(root.join("bad.json")).unwrap();
        assert!(
            bad.contains("\"exitCode\":{\"name\":\"BUILD_FAILURE\",\"code\":1}"),
            "{bad}"
        );
        // The failed build's BuildFinished carries the derived FailureDetail.
        assert!(bad.contains("\"failureDetail\":{\"message\":"), "{bad}");
        assert!(
            bad.contains("\"spawn\":{\"code\":\"NON_ZERO_EXIT\"}"),
            "{bad}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn bep_bytestream_end_to_end_build_and_test() {
        let root = std::env::temp_dir().join(format!("zel-obs-bs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ws = root.join("ws");
        std::fs::create_dir_all(ws.join("p")).unwrap();
        std::fs::write(ws.join("MODULE.bazel"), "module(name = \"w\")\n").unwrap();
        std::fs::write(
            ws.join("p/defs.bzl"),
            concat!(
                "def _t_impl(ctx):\n",
                "    exe = ctx.actions.declare_file(ctx.label.name + '.sh')\n",
                "    ctx.actions.write(exe, '#!/bin/sh\\necho test-run-ok\\nexit 0\\n', is_executable = True)\n",
                "    return [DefaultInfo(executable = exe)]\n",
                "my_test = rule(implementation = _t_impl, test = True)\n",
            ),
        )
        .unwrap();
        std::fs::write(
            ws.join("p/BUILD"),
            concat!(
                "load('//p:defs.bzl', 'my_test')\n",
                "genrule(name='out', outs=['out.txt'], cmd='printf gen-payload > $@')\n",
                "my_test(name='ok_test')\n",
            ),
        )
        .unwrap();

        let rt = runtime(&root, &ws);
        let r = |n: &str| root.join(n).to_str().unwrap().to_string();

        // 1. Even without a remote prefix, `NamedSetOfFiles` populates `digest` and `length`
        //    (`TargetCompleteEvent.newFile`).
        let (code, err, _) = run(
            &rt,
            &[
                "build",
                &format!("--build_event_json_file={}", r("local_bep.json")),
                "//p:out",
            ],
        );
        assert_eq!(code, 0, "{err}");
        let local_bep = std::fs::read_to_string(root.join("local_bep.json")).unwrap();
        let expected_hash = zel_util::sha256::hex_digest(b"gen-payload");
        assert!(
            local_bep.contains(&format!("\"digest\":\"{expected_hash}\"")),
            "{local_bep}"
        );
        assert!(local_bep.contains("\"length\":\"11\""), "{local_bep}");

        // 2. With `--remote_bytestream_uri_prefix`, `NamedSetOfFiles`, `ActionExecuted`,
        //    `TestResult`, and `TestSummary` all emit `bytestream://` URIs and zero `file://` URIs.
        let (code, err, _) = run(
            &rt,
            &[
                "test",
                "--nocache_test_results",
                "--build_event_publish_all_actions",
                "--remote_bytestream_uri_prefix=cas.example.com:9092/main",
                &format!("--build_event_json_file={}", r("remote_bep.json")),
                "//p:out",
                "//p:ok_test",
            ],
        );
        assert_eq!(code, 0, "{err}");
        let remote_bep = std::fs::read_to_string(root.join("remote_bep.json")).unwrap();
        assert!(
            remote_bep.contains(&format!(
                "bytestream://cas.example.com:9092/main/blobs/{expected_hash}/11"
            )),
            "{remote_bep}"
        );
        assert!(!remote_bep.contains("file://"), "{remote_bep}");
        let mut saw_action_bs = false;
        let mut saw_test_result_bs = false;
        let mut saw_test_summary_bs = false;
        for line in remote_bep.lines() {
            if line.contains("\"action\":{") && line.contains("\"type\":\"Genrule\"") {
                saw_action_bs |= line.contains(
                    &format!(
                        "\"primaryOutput\":{{\"uri\":\"bytestream://cas.example.com:9092/main/blobs/{expected_hash}/11\"}}"
                    ),
                );
            }
            if line.contains("\"testResult\":{") {
                saw_test_result_bs |=
                    line.contains("bytestream://cas.example.com:9092/main/blobs/");
            }
            if line.contains("\"testSummary\":{") {
                saw_test_summary_bs |=
                    line.contains("bytestream://cas.example.com:9092/main/blobs/");
            }
        }
        assert!(
            saw_action_bs,
            "ActionExecuted missing bytestream primaryOutput: {remote_bep}"
        );
        assert!(
            saw_test_result_bs,
            "TestResult missing bytestream testActionOutput: {remote_bep}"
        );
        assert!(
            saw_test_summary_bs,
            "TestSummary missing bytestream log: {remote_bep}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
