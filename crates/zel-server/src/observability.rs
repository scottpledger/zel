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
    build_request_id: String,
    include_run_residue: bool,
    bes: Option<Arc<bes::BesTransport>>,
    bes_streaming_msg: Option<String>,
    artifact_uploader: Option<Arc<bes::ByteStreamBuildEventArtifactUploader>>,
    setup_errors: std::sync::Mutex<Vec<String>>,
    started: Instant,
    start_time_millis: i64,
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
    working_dir: std::sync::Mutex<Option<String>>,
    build_started: std::sync::atomic::AtomicBool,
    initial_args: std::sync::Mutex<Vec<String>>,
    initial_workspace_dir: std::sync::Mutex<String>,
    initial_working_dir: std::sync::Mutex<String>,
    initial_server_pid: std::sync::atomic::AtomicU32,
    analysis_complete: std::sync::atomic::AtomicBool,
    bep_enabled: bool,
    bep_opts: BepOptions,
    build_metadata: Vec<(String, String)>,
    convenience_symlinks_bep_event: bool,
    convenience_symlinks: std::sync::Mutex<Vec<(String, String, bool)>>,
}

pub(crate) fn uuid_like() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    // Version-4 shaped identifier derived from time + pid + monotonic counter (no RNG crate).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_else(|_| zel_events::now_millis() as u128 * 1_000_000);
    let pid = std::process::id() as u128;
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as u128;
    let x = nanos
        .wrapping_mul(0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C835)
        .wrapping_add(pid.rotate_left(17))
        .wrapping_add(seq.wrapping_mul(0xBF58_476D_1CE4_E5B9_94D0_49BB_1331_11EB));
    let x = (x ^ (x >> 64)).wrapping_mul(0x94D0_49BB_1331_11EB_BF58_476D_1CE4_E5B9);
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
        workspace_dir: &str,
        fallback_invocation_id: Option<&str>,
        client_env: &std::collections::BTreeMap<String, String>,
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
        crate::live::register_ui(ui.clone());
        let reporter = Arc::new(Reporter::new());
        reporter.add_handler(ui.clone());
        if let Some(pat) = get("output_filter") {
            match zel_events::OutputFilter::from_regex(&pat) {
                Ok(f) => reporter.set_output_filter(f),
                Err(e) => setup_errors.push(format!("--output_filter: {e}")),
            }
        }

        // Wait for any deferred background BES uploads from a previous invocation
        // (`BuildEventServiceModule.waitForPreviousInvocation`).
        let prev = bes::drain_previous_uploads_ext(false, bes::DEFAULT_PREVIOUS_UPLOAD_MAX_WAIT);
        if prev.warnings.is_empty() && prev.waited > std::time::Duration::from_millis(100) {
            let ms = prev.waited.as_millis();
            reporter.handle(Event::info(format!(
                "Waited for the background upload of the Build Event Protocol for {}.{:03} seconds.",
                ms / 1000,
                ms % 1000
            )));
        }
        for w in prev.warnings {
            reporter.handle(Event::warning(None, w));
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
        let workspace_path =
            (!workspace_dir.is_empty()).then(|| std::path::PathBuf::from(workspace_dir));
        let mut bes_t = None;
        let mut bes_streaming_msg = None;
        let mut artifact_uploader: Option<Arc<bes::ByteStreamBuildEventArtifactUploader>> = None;
        if bep_enabled {
            if let Some(p) = parsed {
                let uses_remote = opt_last(p, "remote_cache").is_some_and(|v| !v.is_empty())
                    || opt_last(p, "remote_executor").is_some_and(|v| !v.is_empty());
                if uses_remote {
                    if let Ok(mut ropts) =
                        zel_remote::options::RemoteOptions::from_source(&BepRemoteSource(p))
                    {
                        ropts.set_client_env(client_env.clone());
                        if ropts.build_request_id.is_none() {
                            ropts.build_request_id = Some(build_request_id.clone());
                        }
                        ropts.workspace = workspace_path.clone();
                        let target = ropts
                            .remote_cache
                            .as_deref()
                            .or(ropts.remote_executor.as_deref())
                            .unwrap_or("");
                        let authority = zel_remote::options::Endpoint::parse_cache(target)
                            .ok()
                            .map(|ep| match ep {
                                zel_remote::options::Endpoint::Tcp { host, port, .. }
                                | zel_remote::options::Endpoint::Http { host, port, .. } => {
                                    format!("{host}:{port}")
                                }
                                zel_remote::options::Endpoint::Unix(u) => u,
                            })
                            .unwrap_or_default();
                        let http_cache = ropts.build_http_cache().ok().flatten();
                        let reapi = if http_cache.is_none() {
                            ropts
                                .build_client(&invocation_id)
                                .ok()
                                .flatten()
                                .map(Arc::new)
                        } else {
                            None
                        };
                        if reapi.is_some() || http_cache.is_some() {
                            artifact_uploader =
                                Some(Arc::new(bes::ByteStreamBuildEventArtifactUploader::new(
                                    reapi,
                                    http_cache,
                                    ropts.digest_fn,
                                    ropts.build_event_upload_mode,
                                    &authority,
                                    &ropts.remote_instance_name,
                                    ropts.bytestream_uri_prefix.as_deref(),
                                )));
                        }
                    }
                }
            }
            match bes::transport_from_options_with_env_and_workspace(
                &bep_opts,
                &build_request_id,
                &invocation_id,
                command,
                client_env,
                workspace_path.as_deref(),
            ) {
                Ok(Some((t, cfg))) => {
                    if let Some(u) = &artifact_uploader {
                        t.set_artifact_uploader(u.clone());
                    }
                    if let Some(m) = cfg.streaming_message() {
                        reporter.handle(Event::info(m.clone()));
                        bes_streaming_msg = Some(m);
                    }
                    reporter.add_bep_transport(t.clone());
                    bes_t = Some(t);
                }
                Ok(None) => {}
                Err(e) => setup_errors.push(format!("--bes_backend: {e}")),
            }
        }
        reporter.set_invocation_id(&invocation_id);
        reporter.set_build_request_id(&build_request_id);

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
            build_request_id,
            include_run_residue,
            bes: bes_t,
            bes_streaming_msg,
            artifact_uploader,
            setup_errors: std::sync::Mutex::new(setup_errors),
            started,
            start_time_millis: zel_events::now_millis(),
            cpu_at_start,
            command: command.to_string(),
            perform_build,
            record_all_mnemonics,
            memory_profile,
            namer: std::sync::Mutex::new(bep::ArtifactGroupNamer::new()),
            workspace_dir: std::sync::Mutex::new(workspace_dir.to_string()),
            stats: Default::default(),
            late_nodes: Default::default(),
            patterns: Default::default(),
            working_dir: Default::default(),
            build_started: Default::default(),
            initial_args: Default::default(),
            initial_workspace_dir: Default::default(),
            initial_working_dir: Default::default(),
            initial_server_pid: Default::default(),
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

    pub(crate) fn invocation_id(&self) -> &str {
        &self.invocation_id
    }

    #[allow(dead_code)]
    pub(crate) fn build_request_id(&self) -> &str {
        &self.build_request_id
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

    /// Records command-line and workspace metadata up front so `finish` can still emit
    /// `BuildStarted`, `BuildMetadata`, `UnstructuredCommandLine`, `OptionsParsed`, and
    /// `StructuredCommandLine` when a command aborts before `begin_build`.
    pub(crate) fn set_initial_invocation_context(
        &self,
        args: &[String],
        workspace: &str,
        working_dir: &str,
        server_pid: u32,
    ) {
        *self.initial_args.lock().unwrap() = args.to_vec();
        *self.initial_workspace_dir.lock().unwrap() = workspace.to_string();
        *self.initial_working_dir.lock().unwrap() = working_dir.to_string();
        self.initial_server_pid
            .store(server_pid, std::sync::atomic::Ordering::SeqCst);
        *self.workspace_dir.lock().unwrap() = workspace.to_string();
        *self.working_dir.lock().unwrap() = Some(if !working_dir.is_empty() {
            working_dir.to_string()
        } else {
            workspace.to_string()
        });
    }

    fn ensure_build_started(&self) {
        if !self.bep_enabled || self.build_started.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let args = self.initial_args.lock().unwrap().clone();
        let workspace = self.initial_workspace_dir.lock().unwrap().clone();
        let working_dir = self.initial_working_dir.lock().unwrap().clone();
        let server_pid = self
            .initial_server_pid
            .load(std::sync::atomic::Ordering::SeqCst);
        self.begin_build(&[], &args, &workspace, &working_dir, server_pid, &args);
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
        if self
            .build_started
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        *self.working_dir.lock().unwrap() = Some(if !working_dir.is_empty() {
            working_dir.to_string()
        } else {
            workspace.to_string()
        });
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
            self.start_time_millis,
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
        if self.bep_enabled {
            self.ensure_build_started();
        }
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
            let base_dir = self.working_dir.lock().unwrap().clone().or_else(|| {
                let ws = self.workspace_dir.lock().unwrap().clone();
                (!ws.is_empty()).then_some(ws)
            });
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
                    let resolved = resolve_bep_file_path(raw, base_dir.as_deref());
                    let pf = PathFragment::create(&resolved);
                    let write_res = if let Some(u) = &self.artifact_uploader {
                        r.force_write_bep_file_with(fs, &pf, fmt, conv, Some(u.as_ref()))
                    } else {
                        r.force_write_bep_file_with_options(
                            fs,
                            &pf,
                            fmt,
                            conv,
                            self.bep_opts.remote_bytestream_uri_prefix.as_deref(),
                        )
                    };
                    match write_res {
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
            if let Some(u) = &self.artifact_uploader {
                for w in u.take_warnings() {
                    r.handle(Event::warning(None, w));
                }
            }
            let errs = s.close_errors();
            if let Some(m) = &self.bes_streaming_msg {
                r.handle(Event::info(m.clone()));
            }
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
                if !errs.is_empty() {
                    let bes_err = self.bes.as_ref().and_then(|b| b.last_error());
                    let timed_out = bes_err.as_ref().is_some_and(|e| e.timed_out);
                    if timed_out {
                        code = ExitCode::TRANSIENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR;
                    } else if code == ExitCode::SUCCESS {
                        code = match bes_err.map(|e| e.kind) {
                            Some(bes::BesErrorKind::Persistent) => {
                                ExitCode::PERSISTENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR
                            }
                            _ => ExitCode::TRANSIENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR,
                        };
                    }
                } else if code == ExitCode::SUCCESS {
                    code = ExitCode::LOCAL_ENVIRONMENTAL_ERROR;
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

struct BepRemoteSource<'a>(&'a ParsedCommandOptions);

impl zel_remote::options::OptionSource for BepRemoteSource<'_> {
    fn last(&self, name: &str) -> Option<String> {
        if name == "digest_function" {
            return Some(
                zel_exec::digest::server_digest_function()
                    .name()
                    .replace('-', ""),
            );
        }
        opt_last(self.0, name)
    }
    fn all(&self, name: &str) -> Vec<String> {
        opt_all(self.0, name)
    }
}

fn resolve_bep_file_path(raw: &str, working_dir: Option<&str>) -> String {
    if raw == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return home;
        }
    } else if let Some(rest) = raw.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return std::path::Path::new(&home)
                .join(rest)
                .to_string_lossy()
                .into_owned();
        }
    }
    if std::path::Path::new(raw).is_absolute() {
        return raw.to_string();
    }
    if let Some(wd) = working_dir.filter(|s| !s.is_empty()) {
        return std::path::Path::new(wd)
            .join(raw)
            .to_string_lossy()
            .into_owned();
    }
    raw.to_string()
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

    fn is_valid_uuid_v4(s: &str) -> bool {
        let bytes = s.as_bytes();
        if bytes.len() != 36 {
            return false;
        }
        for (i, &b) in bytes.iter().enumerate() {
            match i {
                8 | 13 | 18 | 23 => {
                    if b != b'-' {
                        return false;
                    }
                }
                14 => {
                    if b != b'4' {
                        return false;
                    }
                }
                19 => {
                    if !matches!(b, b'8' | b'9' | b'a' | b'b' | b'A' | b'B') {
                        return false;
                    }
                }
                _ => {
                    if !b.is_ascii_hexdigit() {
                        return false;
                    }
                }
            }
        }
        true
    }

    #[test]
    fn uuid_like_generates_distinct_v4_uuids() {
        let mut seen = HashSet::new();
        for _ in 0..16 {
            let id = uuid_like();
            assert!(is_valid_uuid_v4(&id), "not a valid v4 UUID: {id}");
            assert!(seen.insert(id.clone()), "duplicate uuid_like: {id}");
        }
    }

    #[test]
    fn build_observer_unifies_invocation_and_build_request_ids() {
        let env = std::collections::BTreeMap::new();
        let ob = std::env::temp_dir()
            .join(format!("zel-obs-inv-unit-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();

        // 1. Explicit --invocation_id and --build_request_id.
        let exp_inv = "11111111-2222-4333-8444-555555555555";
        let exp_req = "req-aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        let parsed = ParsedCommandOptions::parse_for_command(
            "build",
            &[
                format!("--invocation_id={exp_inv}"),
                format!("--build_request_id={exp_req}"),
            ],
        )
        .unwrap();
        let obs = BuildObserver::new_with_fallback_invocation_id(
            Some(&parsed),
            "build",
            &ob,
            "",
            Some("99999999-9999-4999-8999-999999999999"),
            &env,
        );
        assert_eq!(obs.invocation_id(), exp_inv);
        assert_eq!(obs.reporter.invocation_id(), exp_inv);
        assert_eq!(obs.build_request_id(), exp_req);
        assert_eq!(obs.reporter.build_request_id().as_deref(), Some(exp_req));

        // 2. Empty --invocation_id= falls back to BAZEL_INTERNAL_INVOCATION_ID.
        let env_inv = "22222222-3333-4444-8555-666666666666";
        let parsed_empty =
            ParsedCommandOptions::parse_for_command("build", &["--invocation_id=".to_string()])
                .unwrap();
        let obs_env = BuildObserver::new_with_fallback_invocation_id(
            Some(&parsed_empty),
            "build",
            &ob,
            "",
            Some(env_inv),
            &env,
        );
        assert_eq!(obs_env.invocation_id(), env_inv);
        assert_eq!(obs_env.reporter.invocation_id(), env_inv);
        assert!(is_valid_uuid_v4(obs_env.build_request_id()));
        assert_eq!(
            obs_env.reporter.build_request_id().as_deref(),
            Some(obs_env.build_request_id())
        );

        // 3. Auto-generated UUID when neither flag nor env var is set.
        let parsed_auto = ParsedCommandOptions::parse_for_command("build", &[]).unwrap();
        let obs_auto = BuildObserver::new_with_fallback_invocation_id(
            Some(&parsed_auto),
            "build",
            &ob,
            "",
            None,
            &env,
        );
        assert!(is_valid_uuid_v4(obs_auto.invocation_id()));
        assert_eq!(obs_auto.reporter.invocation_id(), obs_auto.invocation_id());
        assert!(is_valid_uuid_v4(obs_auto.build_request_id()));
        assert_eq!(
            obs_auto.reporter.build_request_id().as_deref(),
            Some(obs_auto.build_request_id())
        );
        assert_ne!(obs_auto.invocation_id(), obs_auto.build_request_id());
    }

    #[cfg(unix)]
    #[derive(Clone, Default)]
    struct TestBesServer {
        lifecycle_ids: Arc<std::sync::Mutex<Vec<(String, String)>>>,
        stream_ids: Arc<std::sync::Mutex<Vec<(String, String)>>>,
        lifecycle_rm: Arc<std::sync::Mutex<Vec<zel_proto::reapi::RequestMetadata>>>,
        stream_rm: Arc<std::sync::Mutex<Vec<zel_proto::reapi::RequestMetadata>>>,
    }

    #[cfg(unix)]
    impl TestBesServer {
        fn clear(&self) {
            self.lifecycle_ids.lock().unwrap().clear();
            self.stream_ids.lock().unwrap().clear();
            self.lifecycle_rm.lock().unwrap().clear();
            self.stream_rm.lock().unwrap().clear();
        }

        fn serve_tcp(&self, listener: std::net::TcpListener) {
            let me = self.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (Ok(r), Ok(w)) = (stream.try_clone(), stream.try_clone()) else {
                        continue;
                    };
                    let me2 = me.clone();
                    let t = zel_remote::transport::Transport {
                        reader: Box::new(r),
                        writer: Box::new(w),
                        shutdown: Box::new(move || {
                            let _ = stream.shutdown(std::net::Shutdown::Both);
                        }),
                    };
                    std::thread::spawn(move || {
                        let Ok((_conn, rx)) = zel_remote::http2::Conn::server_from(
                            t,
                            zel_remote::http2::LocalSettings::default(),
                        ) else {
                            return;
                        };
                        while let Ok(st) = rx.recv() {
                            let me3 = me2.clone();
                            std::thread::spawn(move || me3.handle(st));
                        }
                    });
                }
            });
        }

        fn handle(&self, st: zel_remote::http2::Stream) {
            use zel_proto::dynamic::Msg;
            use zel_remote::grpc::{frame_message, MessageParser};
            use zel_remote::http2::Event;

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
            let path = headers
                .iter()
                .find(|(k, _)| k == ":path")
                .map(|(_, v)| v.as_str())
                .unwrap_or("");
            let rm = decode_request_metadata(&headers);
            let ok_headers = [
                (":status".to_string(), "200".to_string()),
                ("content-type".to_string(), "application/grpc".to_string()),
            ];
            if path.ends_with("/PublishLifecycleEvent") {
                if let Some(m) = msgs.first() {
                    if let Ok(req) =
                        Msg::decode("google.devtools.build.v1.PublishLifecycleEventRequest", m)
                    {
                        if let Some(obe) = req.get_msg("build_event") {
                            if let Some(sid) = obe.get_msg("stream_id") {
                                self.lifecycle_ids.lock().unwrap().push((
                                    sid.get_str("invocation_id").to_string(),
                                    sid.get_str("build_id").to_string(),
                                ));
                            }
                        }
                    }
                }
                if let Some(rm) = rm {
                    self.lifecycle_rm.lock().unwrap().push(rm);
                }
                let _ = st.send_headers(&ok_headers, false);
                let _ = st.send_data(&frame_message(&[]), false, None);
                let _ = st.send_headers(&[("grpc-status".to_string(), "0".to_string())], true);
                return;
            }
            if let Some(rm) = rm {
                self.stream_rm.lock().unwrap().push(rm);
            }
            let _ = st.send_headers(&ok_headers, false);
            for m in &msgs {
                let r = Msg::decode(
                    "google.devtools.build.v1.PublishBuildToolEventStreamRequest",
                    m,
                )
                .unwrap();
                let o = r.get_msg("ordered_build_event").unwrap();
                let seq = o.get_i64("sequence_number");
                let mut resp =
                    Msg::new("google.devtools.build.v1.PublishBuildToolEventStreamResponse");
                resp.set_int("sequence_number", seq);
                if let Some(sid) = o.get_msg("stream_id") {
                    self.stream_ids.lock().unwrap().push((
                        sid.get_str("invocation_id").to_string(),
                        sid.get_str("build_id").to_string(),
                    ));
                    resp.set_msg("stream_id", sid.clone());
                }
                let _ = st.send_data(&frame_message(&resp.encode_to_vec()), false, None);
            }
            let _ = st.send_headers(&[("grpc-status".to_string(), "0".to_string())], true);
        }
    }

    #[cfg(unix)]
    fn decode_request_metadata(
        headers: &[(String, String)],
    ) -> Option<zel_proto::reapi::RequestMetadata> {
        use zel_proto::reapi::{Message, RequestMetadata};
        let raw = headers
            .iter()
            .find(|(k, _)| k == zel_remote::reapi_client::REQUEST_METADATA_HEADER)
            .map(|(_, v)| v.as_str())?;
        let bytes = zel_util::base64_decode(raw).ok()?;
        RequestMetadata::decode(&bytes).ok()
    }

    #[cfg(unix)]
    fn extract_info_invocation_id(stderr: &str) -> String {
        stderr
            .lines()
            .find_map(|l| l.strip_prefix("INFO: Invocation ID: "))
            .map(str::trim)
            .unwrap_or_else(|| panic!("missing 'INFO: Invocation ID:' in stderr:\n{stderr}"))
            .to_string()
    }

    #[cfg(unix)]
    fn extract_bep_started_uuid(bep_json: &str) -> String {
        let first = bep_json
            .lines()
            .next()
            .expect("BEP JSON must contain BuildStarted event");
        let needle = "\"uuid\":\"";
        let start = first
            .find(needle)
            .unwrap_or_else(|| panic!("missing uuid in BuildStarted: {first}"))
            + needle.len();
        let rest = &first[start..];
        let end = rest.find('"').expect("unterminated uuid string");
        rest[..end].to_string()
    }

    #[cfg(unix)]
    #[test]
    fn invocation_id_unified_across_bes_console_reapi_and_commands() {
        let reapi_listener = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("SKIPPED: loopback sockets not permitted ({e})");
                return;
            }
            Err(e) => panic!("{e}"),
        };
        let reapi_port = reapi_listener.local_addr().unwrap().port();
        if let Err(e) = std::net::TcpStream::connect(("127.0.0.1", reapi_port)) {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                eprintln!("SKIPPED: loopback connect not permitted ({e})");
                return;
            }
            panic!("{e}");
        }
        let bes_listener = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("SKIPPED: loopback sockets not permitted ({e})");
                return;
            }
            Err(e) => panic!("{e}"),
        };
        let bes_port = bes_listener.local_addr().unwrap().port();

        let reapi = zel_remote::fake_server::FakeReapiServer::new();
        reapi.serve_tcp(reapi_listener);
        let bes = TestBesServer::default();
        bes.serve_tcp(bes_listener);

        let root = std::env::temp_dir().join(format!("zel-obs-inv-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ws = root.join("ws");
        std::fs::create_dir_all(ws.join("p")).unwrap();
        std::fs::write(ws.join("MODULE.bazel"), "module(name = \"w\")\n").unwrap();
        std::fs::write(
            ws.join("p/defs.bzl"),
            concat!(
                "def _exe_impl(ctx):\n",
                "    exe = ctx.actions.declare_file(ctx.label.name + '.sh')\n",
                "    ctx.actions.run_shell(outputs = [exe], command = 'printf \"#!/bin/sh\\necho ok\\nexit 0\\n\" > ' + exe.path + ' && chmod +x ' + exe.path)\n",
                "    return [DefaultInfo(executable = exe)]\n",
                "my_test = rule(implementation = _exe_impl, test = True)\n",
                "my_bin = rule(implementation = _exe_impl, executable = True)\n",
            ),
        )
        .unwrap();
        std::fs::write(
            ws.join("p/BUILD"),
            concat!(
                "load('//p:defs.bzl', 'my_test', 'my_bin')\n",
                "genrule(name='g_explicit', outs=['g_explicit.txt'], cmd='echo explicit > $@')\n",
                "genrule(name='g_env', outs=['g_env.txt'], cmd='echo env > $@')\n",
                "genrule(name='g_auto1', outs=['g_auto1.txt'], cmd='echo auto1 > $@')\n",
                "genrule(name='g_auto2', outs=['g_auto2.txt'], cmd='echo auto2 > $@')\n",
                "my_test(name='t_auto')\n",
                "my_bin(name='bin_auto')\n",
            ),
        )
        .unwrap();

        let rt = runtime(&root, &ws);
        let bes_flag = format!("--bes_backend=grpc://127.0.0.1:{bes_port}");
        let results_url = "https://bes.example/invocation/";
        let results_flag = format!("--bes_results_url={results_url}");
        let cache_flag = format!("--remote_cache=grpc://127.0.0.1:{reapi_port}");
        let path_for = |n: &str| root.join(n).to_str().unwrap().to_string();

        let assert_all_match = |label: &str,
                                err: &str,
                                bep_path: &str,
                                expected_inv: Option<&str>,
                                expected_req: Option<&str>|
         -> (String, String) {
            let console_inv = extract_info_invocation_id(err);
            assert!(
                is_valid_uuid_v4(&console_inv) || expected_inv.is_some(),
                "[{label}] console invocation id not valid UUID: {console_inv}"
            );
            if let Some(exp) = expected_inv {
                assert_eq!(console_inv, exp, "[{label}] console invocation id mismatch");
            }
            assert!(
                err.contains(&format!(
                    "INFO: Streaming build results to: {results_url}{console_inv}"
                )),
                "[{label}] streaming URL mismatch in stderr:\n{err}"
            );

            let bep_json = std::fs::read_to_string(bep_path).unwrap();
            let bep_uuid = extract_bep_started_uuid(&bep_json);
            assert_eq!(
                bep_uuid, console_inv,
                "[{label}] BEP BuildStarted.uuid mismatch"
            );

            let lc_ids = bes.lifecycle_ids.lock().unwrap().clone();
            assert!(
                !lc_ids.is_empty(),
                "[{label}] no BES lifecycle events captured"
            );
            let bes_build_id = lc_ids[0].1.clone();
            if let Some(exp_r) = expected_req {
                assert_eq!(bes_build_id, exp_r, "[{label}] BES build_id mismatch");
            } else {
                assert!(
                    is_valid_uuid_v4(&bes_build_id),
                    "[{label}] BES auto build_id not valid UUID: {bes_build_id}"
                );
            }
            let mut saw_lifecycle_inv = 0usize;
            for (inv, bid) in &lc_ids {
                if !inv.is_empty() {
                    assert_eq!(
                        inv, &console_inv,
                        "[{label}] BES lifecycle invocation_id mismatch"
                    );
                    saw_lifecycle_inv += 1;
                }
                assert_eq!(
                    bid, &bes_build_id,
                    "[{label}] BES lifecycle build_id mismatch"
                );
            }
            assert!(
                saw_lifecycle_inv >= 2,
                "[{label}] expected invocation_attempt_started/finished with invocation_id, got {lc_ids:?}"
            );
            let st_ids = bes.stream_ids.lock().unwrap().clone();
            assert!(
                !st_ids.is_empty(),
                "[{label}] no BES stream events captured"
            );
            for (inv, bid) in &st_ids {
                assert_eq!(
                    inv, &console_inv,
                    "[{label}] BES stream invocation_id mismatch"
                );
                assert_eq!(bid, &bes_build_id, "[{label}] BES stream build_id mismatch");
            }
            for rm in bes
                .lifecycle_rm
                .lock()
                .unwrap()
                .iter()
                .chain(bes.stream_rm.lock().unwrap().iter())
            {
                assert_eq!(
                    rm.tool_invocation_id, console_inv,
                    "[{label}] BES RequestMetadata.tool_invocation_id mismatch"
                );
                assert_eq!(
                    rm.correlated_invocations_id, bes_build_id,
                    "[{label}] BES RequestMetadata.correlated_invocations_id mismatch"
                );
            }

            let reapi_headers = reapi
                .last_headers("GetActionResult")
                .unwrap_or_else(|| panic!("[{label}] no REAPI GetActionResult headers recorded"));
            let reapi_rm = decode_request_metadata(&reapi_headers)
                .unwrap_or_else(|| panic!("[{label}] missing REAPI RequestMetadata header"));
            assert_eq!(
                reapi_rm.tool_invocation_id, console_inv,
                "[{label}] REAPI RequestMetadata.tool_invocation_id mismatch"
            );
            assert_eq!(
                reapi_rm.correlated_invocations_id, bes_build_id,
                "[{label}] REAPI RequestMetadata.correlated_invocations_id mismatch"
            );

            (console_inv, bes_build_id)
        };

        // 1. Explicit --invocation_id and --build_request_id.
        bes.clear();
        let explicit_inv = "11111111-2222-4333-8444-555555555555";
        let explicit_req = "req-11111111-2222-4333-8444-555555555555";
        let bep1 = path_for("bep_explicit.json");
        let (code, err, _) = run(
            &rt,
            &[
                "build",
                "--nouse_action_cache",
                &format!("--invocation_id={explicit_inv}"),
                &format!("--build_request_id={explicit_req}"),
                &bes_flag,
                &results_flag,
                &cache_flag,
                &format!("--build_event_json_file={bep1}"),
                "//p:g_explicit",
            ],
        );
        assert_eq!(code, 0, "{err}");
        assert_all_match(
            "explicit",
            &err,
            &bep1,
            Some(explicit_inv),
            Some(explicit_req),
        );

        // 2. BAZEL_INTERNAL_INVOCATION_ID via --client_env (with empty --invocation_id=).
        bes.clear();
        let env_inv = "22222222-3333-4444-8555-666666666666";
        let bep2 = path_for("bep_env.json");
        let (code, err, _) = run(
            &rt,
            &[
                "build",
                "--nouse_action_cache",
                "--invocation_id=",
                &format!("--client_env=BAZEL_INTERNAL_INVOCATION_ID={env_inv}"),
                &bes_flag,
                &results_flag,
                &cache_flag,
                &format!("--build_event_json_file={bep2}"),
                "//p:g_env",
            ],
        );
        assert_eq!(code, 0, "{err}");
        assert_all_match("env", &err, &bep2, Some(env_inv), None);

        // 3. Auto-generated UUID (two consecutive builds produce distinct, internally consistent UUIDs).
        bes.clear();
        let bep3 = path_for("bep_auto1.json");
        let (code, err, _) = run(
            &rt,
            &[
                "build",
                "--nouse_action_cache",
                &bes_flag,
                &results_flag,
                &cache_flag,
                &format!("--build_event_json_file={bep3}"),
                "//p:g_auto1",
            ],
        );
        assert_eq!(code, 0, "{err}");
        let (auto_inv1, _) = assert_all_match("auto1", &err, &bep3, None, None);

        bes.clear();
        let bep4 = path_for("bep_auto2.json");
        let (code, err, _) = run(
            &rt,
            &[
                "build",
                "--nouse_action_cache",
                &bes_flag,
                &results_flag,
                &cache_flag,
                &format!("--build_event_json_file={bep4}"),
                "//p:g_auto2",
            ],
        );
        assert_eq!(code, 0, "{err}");
        let (auto_inv2, _) = assert_all_match("auto2", &err, &bep4, None, None);
        assert_ne!(
            auto_inv1, auto_inv2,
            "consecutive auto-generated invocations must have distinct UUIDs"
        );

        // 4. `test` command with auto-generated UUID.
        bes.clear();
        let bep5 = path_for("bep_test.json");
        let (code, err, _) = run(
            &rt,
            &[
                "test",
                "--nouse_action_cache",
                "--nocache_test_results",
                &bes_flag,
                &results_flag,
                &cache_flag,
                &format!("--build_event_json_file={bep5}"),
                "//p:t_auto",
            ],
        );
        assert_eq!(code, 0, "{err}");
        assert_all_match("test", &err, &bep5, None, None);

        // 5. `run --norun` command with auto-generated UUID: BUILD_ID in ExecRequest also matches.
        bes.clear();
        let bep6 = path_for("bep_run.json");
        let run_req = zel_proto::command_server::RunRequest {
            cookie: "c".into(),
            arg: [
                "run",
                "--norun",
                "--nouse_action_cache",
                &bes_flag,
                &results_flag,
                &cache_flag,
                &format!("--build_event_json_file={bep6}"),
                "//p:bin_auto",
            ]
            .iter()
            .map(|a| a.as_bytes().to_vec())
            .collect(),
            client_description: "t".into(),
            ..Default::default()
        };
        let run_resp = rt.dispatch_command(&run_req, "r");
        let run_err = String::from_utf8_lossy(&run_resp.standard_error).to_string();
        assert_eq!(run_resp.exit_code, 0, "{run_err}");
        let (run_inv, _) = assert_all_match("run", &run_err, &bep6, None, None);
        let exec = run_resp
            .exec_request
            .expect("ExecRequest must be populated for `run --norun`");
        let build_id_env = exec
            .environment_variable
            .iter()
            .find(|ev| ev.name == b"BUILD_ID")
            .map(|ev| String::from_utf8_lossy(&ev.value).into_owned());
        assert_eq!(
            build_id_env.as_deref(),
            Some(run_inv.as_str()),
            "run ExecRequest BUILD_ID must match canonical invocation_id"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[derive(Clone, Default)]
    struct MockBesServer {
        lifecycle: Arc<std::sync::Mutex<Vec<zel_proto::dynamic::Msg>>>,
        bep_events: Arc<std::sync::Mutex<Vec<zel_proto::dynamic::Msg>>>,
        stream_headers: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    }

    impl MockBesServer {
        fn start() -> (Self, String) {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let srv = Self::default();
            let me = srv.clone();
            std::thread::spawn(move || {
                for s in listener.incoming().flatten() {
                    let _ = s.set_nodelay(true);
                    let (Ok(r), Ok(w)) = (s.try_clone(), s.try_clone()) else {
                        continue;
                    };
                    let conn_srv = me.clone();
                    let t = zel_remote::transport::Transport {
                        reader: Box::new(r),
                        writer: Box::new(w),
                        shutdown: Box::new(move || {
                            let _ = s.shutdown(std::net::Shutdown::Both);
                        }),
                    };
                    std::thread::spawn(move || {
                        let Ok((_conn, rx)) = zel_remote::http2::Conn::server_from(
                            t,
                            zel_remote::http2::LocalSettings::default(),
                        ) else {
                            return;
                        };
                        while let Ok(stream) = rx.recv() {
                            let h = conn_srv.clone();
                            std::thread::spawn(move || h.handle(stream));
                        }
                    });
                }
            });
            (srv, format!("grpc://127.0.0.1:{}", addr.port()))
        }

        fn handle(&self, st: zel_remote::http2::Stream) {
            use zel_proto::dynamic::Msg;
            use zel_remote::grpc::{frame_message, MessageParser};
            use zel_remote::http2::Event as H2Event;

            let mut headers = Vec::new();
            let mut parser = MessageParser::default();
            let mut msgs = Vec::new();
            loop {
                match st.recv(None) {
                    Ok(H2Event::Headers(h)) => headers = h,
                    Ok(H2Event::Data(d)) => {
                        parser.push(&d);
                        while let Ok(Some(m)) = parser.next() {
                            msgs.push(m);
                        }
                    }
                    Ok(H2Event::End) => break,
                    _ => return,
                }
            }
            let path = headers
                .iter()
                .find(|(k, _)| k == ":path")
                .map(|(_, v)| v.as_str())
                .unwrap_or("");
            let ok_headers = [
                (":status".to_string(), "200".to_string()),
                ("content-type".to_string(), "application/grpc".to_string()),
            ];
            if path == bes::PUBLISH_LIFECYCLE {
                for m in &msgs {
                    if let Ok(req) =
                        Msg::decode("google.devtools.build.v1.PublishLifecycleEventRequest", m)
                    {
                        self.lifecycle.lock().unwrap().push(req);
                    }
                }
                let _ = st.send_headers(&ok_headers, false);
                let _ = st.send_data(&frame_message(&[]), false, None);
                let _ = st.send_headers(&[("grpc-status".to_string(), "0".to_string())], true);
                return;
            }
            if path == bes::PUBLISH_STREAM {
                *self.stream_headers.lock().unwrap() = headers;
                let _ = st.send_headers(&ok_headers, false);
                for m in &msgs {
                    let req = Msg::decode(
                        "google.devtools.build.v1.PublishBuildToolEventStreamRequest",
                        m,
                    )
                    .unwrap();
                    let obe = req.get_msg("ordered_build_event").unwrap();
                    let seq = obe.get_i64("sequence_number");
                    if let Some(ev) = obe.get_msg("event") {
                        if let Some(any) = ev.get_msg("bazel_event") {
                            if let Ok(be) =
                                Msg::decode("build_event_stream.BuildEvent", any.get_bytes("value"))
                            {
                                self.bep_events.lock().unwrap().push(be);
                            }
                        }
                    }
                    let mut resp =
                        Msg::new("google.devtools.build.v1.PublishBuildToolEventStreamResponse");
                    resp.set_int("sequence_number", seq);
                    if let Some(sid) = obe.get_msg("stream_id") {
                        resp.set_msg("stream_id", sid.clone());
                    }
                    let _ = st.send_data(&frame_message(&resp.encode_to_vec()), false, None);
                }
                let _ = st.send_headers(&[("grpc-status".to_string(), "0".to_string())], true);
            }
        }

        fn bep_jsons(&self) -> Vec<String> {
            self.bep_events
                .lock()
                .unwrap()
                .iter()
                .map(|m| m.to_json())
                .collect()
        }

        fn lifecycle_count(&self) -> usize {
            self.lifecycle.lock().unwrap().len()
        }
    }

    fn assert_well_formed_early_failure_bep(
        events: &[String],
        expected_exit_code: i32,
        expected_invocation_id: Option<&str>,
    ) {
        assert!(
            !events.is_empty(),
            "expected BES to receive BuildEvent stream"
        );
        assert!(
            events[0].contains("\"id\":{\"started\":{}}"),
            "first BEP event must be BuildStarted, got: {}",
            events[0]
        );
        if let Some(inv) = expected_invocation_id {
            assert!(
                events[0].contains(&format!("\"uuid\":\"{inv}\"")),
                "BuildStarted missing invocation_id {inv}: {}",
                events[0]
            );
        }
        let all = events.join("\n");
        for needle in [
            "\"buildMetadata\":{",
            "\"unstructuredCommandLine\":{",
            "\"optionsParsed\":{",
            "\"structuredCommandLine\":{\"commandLineLabel\":\"original\"",
            "\"structuredCommandLine\":{\"commandLineLabel\":\"tool\"",
            "\"structuredCommandLine\":{\"commandLineLabel\":\"canonical\"",
            "\"workspaceStatus\":{",
            "\"buildFinished\":{}",
            &format!("\"code\":{expected_exit_code}"),
        ] {
            assert!(all.contains(needle), "missing {needle} in BEP:\n{all}");
        }
        assert!(
            events.last().unwrap().contains("\"lastMessage\":true"),
            "final BEP event must set lastMessage=true: {}",
            events.last().unwrap()
        );
    }

    #[test]
    fn bes_streams_module_resolution_and_downloader_failures_and_registers_live_sink() {
        let root = std::env::temp_dir().join(format!("zel-bes-early-mod-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ws = root.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        // Syntax error in MODULE.bazel triggers `module_resolution_failure` (exit 48).
        std::fs::write(ws.join("MODULE.bazel"), "def broken(\n").unwrap();
        std::fs::write(ws.join("BUILD.bazel"), "").unwrap();

        let rt = runtime(&root, &ws);

        // 1. Module resolution failure on `build`: LiveSink is active and BES gets full stream.
        let (srv, backend) = MockBesServer::start();
        let live_sink = Arc::new(crate::live::LiveSink::default());
        crate::live::set_current(Some(Arc::clone(&live_sink)));
        let (code, err, _) = run(
            &rt,
            &[
                "build",
                &format!("--bes_backend={backend}"),
                "--bes_results_url=https://results.example/invocation/",
                "--invocation_id=11111111-1111-4111-8111-111111111111",
                "//...",
            ],
        );
        let (_, drained_err) = live_sink.drain();
        crate::live::set_current(None);

        assert_eq!(code, 48, "{err}");
        assert!(
            err.contains("Error computing the main repository mapping"),
            "{err}"
        );
        assert!(
            live_sink.ui_terminal_width().is_some(),
            "LiveSink UI should be registered during module resolution failure"
        );
        let _ = drained_err;
        assert_eq!(srv.lifecycle_count(), 4);
        let events = srv.bep_jsons();
        assert_well_formed_early_failure_bep(
            &events,
            48,
            Some("11111111-1111-4111-8111-111111111111"),
        );

        // 2. Module resolution failure on `run` also streams to BES.
        let (srv_run, backend_run) = MockBesServer::start();
        let (code_run, err_run, _) = run(
            &rt,
            &[
                "run",
                &format!("--bes_backend={backend_run}"),
                "--invocation_id=22222222-2222-4222-8222-222222222222",
                "//:x",
            ],
        );
        assert_eq!(code_run, 48, "{err_run}");
        assert_eq!(srv_run.lifecycle_count(), 4);
        assert_well_formed_early_failure_bep(
            &srv_run.bep_jsons(),
            48,
            Some("22222222-2222-4222-8222-222222222222"),
        );

        // 3. Downloader config failure on `build` streams to BES with exit code 2.
        std::fs::write(ws.join("MODULE.bazel"), "module(name = \"w\")\n").unwrap();
        let (srv_dl, backend_dl) = MockBesServer::start();
        let (code_dl, err_dl, _) = run(
            &rt,
            &[
                "build",
                &format!("--bes_backend={backend_dl}"),
                "--invocation_id=33333333-3333-4333-8333-333333333333",
                "--downloader_config=does_not_exist.cfg",
                "//...",
            ],
        );
        assert_eq!(code_dl, 2, "{err_dl}");
        assert_eq!(srv_dl.lifecycle_count(), 4);
        assert_well_formed_early_failure_bep(
            &srv_dl.bep_jsons(),
            2,
            Some("33333333-3333-4333-8333-333333333333"),
        );

        // 4. `not_in_workspace` keeps exact existing behavior without sending BES RPCs.
        let (srv_nowork, backend_nowork) = MockBesServer::start();
        let rt_nowork = runtime(&root.join("nowork"), std::path::Path::new(""));
        let (code_nw, err_nw, _) = run(
            &rt_nowork,
            &["build", &format!("--bes_backend={backend_nowork}"), "//..."],
        );
        assert_eq!(code_nw, 2, "{err_nw}");
        assert!(
            err_nw.contains("must be invoked from within a workspace"),
            "{err_nw}"
        );
        assert_eq!(srv_nowork.lifecycle_count(), 0);
        assert!(srv_nowork.bep_jsons().is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bes_streams_unrelated_flag_parse_failure() {
        let root = std::env::temp_dir().join(format!("zel-bes-flag-err-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ws = root.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("MODULE.bazel"), "module(name = \"w\")\n").unwrap();
        std::fs::write(ws.join("BUILD.bazel"), "").unwrap();

        let rt = runtime(&root, &ws);
        let (srv, backend) = MockBesServer::start();

        let (code, err, _) = run(
            &rt,
            &[
                "build",
                &format!("--bes_backend={backend}"),
                "--bes_header=x-buildbuddy-api-key=header-secret",
                "--remote_header=x-buildbuddy-api-key=remote-secret",
                "--build_metadata=REPO_URL=https://example.com/repo",
                "--invocation_id=44444444-4444-4444-8444-444444444444",
                "--totally_unknown_flag_for_test",
                "//...",
            ],
        );
        assert_eq!(code, 2, "{err}");
        assert!(
            err.contains("Unrecognized option: --totally_unknown_flag_for_test"),
            "{err}"
        );
        assert_eq!(srv.lifecycle_count(), 4);
        let events = srv.bep_jsons();
        assert_well_formed_early_failure_bep(
            &events,
            2,
            Some("44444444-4444-4444-8444-444444444444"),
        );
        let all = events.join("\n");
        assert!(
            all.contains("x-buildbuddy-api-key=remote-secret"),
            "OptionsParsed/StructuredCommandLine should carry --remote_header: {all}"
        );
        assert!(
            all.contains("https://example.com/repo"),
            "BuildMetadata should carry --build_metadata: {all}"
        );
        assert!(
            all.contains("Unrecognized option: --totally_unknown_flag_for_test"),
            "BEP progress should include the flag parse error stderr: {all}"
        );
        let hdrs = srv.stream_headers.lock().unwrap().clone();
        assert!(
            hdrs.iter()
                .any(|(k, v)| k == "x-buildbuddy-api-key" && v == "header-secret"),
            "BES gRPC stream should carry --bes_header: {hdrs:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bes_streams_pre_begin_build_validation_failure() {
        let root = std::env::temp_dir().join(format!("zel-bes-pre-begin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ws = root.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("MODULE.bazel"), "module(name = \"w\")\n").unwrap();
        std::fs::write(ws.join("BUILD.bazel"), "").unwrap();

        let rt = runtime(&root, &ws);
        let (srv, backend) = MockBesServer::start();

        // Fails in `validate_exec_options` before `obs.begin_build(...)`.
        let (code, err, _) = run(
            &rt,
            &[
                "build",
                &format!("--bes_backend={backend}"),
                "--remote_header=x-buildbuddy-api-key=pre-begin-secret",
                "--invocation_id=55555555-5555-4555-8555-555555555555",
                "--spawn_strategy=definitely_not_a_real_strategy",
                "//...",
            ],
        );
        assert_eq!(code, 2, "{err}");
        assert!(
            err.contains("no strategy with that identifier was registered"),
            "{err}"
        );
        assert_eq!(srv.lifecycle_count(), 4);
        let events = srv.bep_jsons();
        assert_well_formed_early_failure_bep(
            &events,
            2,
            Some("55555555-5555-4555-8555-555555555555"),
        );
        let all = events.join("\n");
        assert!(
            all.contains("x-buildbuddy-api-key=pre-begin-secret"),
            "OptionsParsed/StructuredCommandLine should carry --remote_header: {all}"
        );
        assert!(
            all.contains("no strategy with that identifier was registered"),
            "BEP progress should include the validation error stderr: {all}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn mobile_install_skips_early_repo_and_downloader_checks() {
        let root = std::env::temp_dir().join(format!(
            "zel-mobile-install-no-early-checks-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let ws = root.join("ws");
        std::fs::create_dir_all(ws.join("p")).unwrap();
        // Deliberately invalid MODULE.bazel so `open_now()` fails if called.
        std::fs::write(ws.join("MODULE.bazel"), "def broken(\n").unwrap();
        std::fs::write(
            ws.join("p/mi.bzl"),
            "def _impl(target, ctx):\n    return []\nMIASPECT = aspect(implementation = _impl)\n",
        )
        .unwrap();
        std::fs::write(
            ws.join("p/BUILD"),
            "exports_files([\"mi.bzl\"])\nfilegroup(name = \"app\", srcs = [])\n",
        )
        .unwrap();

        let rt = runtime(&root, &ws);

        // First run `build` to install the broken MODULE.bazel into `rt.repo_resolver` so any
        // accidental `open_now()` call from a subsequent `mobile-install` would fail.
        let (build_code, build_err, _) = run(&rt, &["build", "//p:app"]);
        assert_eq!(build_code, 48, "{build_err}");

        // `mobile-install` is neither in `uses_external_repos` nor in the downloader config check
        // command set, so neither `--downloader_config=does_not_exist.cfg` nor the broken
        // MODULE.bazel should fail it.
        let (mi_code, mi_err, _) = run(
            &rt,
            &[
                "mobile-install",
                "--downloader_config=does_not_exist.cfg",
                "--mobile_install_aspect=//p:mi.bzl",
                "--mobile_install_supported_rules=filegroup",
                "--nobuild",
                "//p:app",
            ],
        );
        assert_eq!(mi_code, 0, "{mi_err}");
        assert!(
            !mi_err.contains("downloader") && !mi_err.contains("main repository mapping"),
            "mobile-install should not run downloader or repo checks: {mi_err}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn build_observer_expands_workspace_for_bes_and_artifact_uploader_credential_helper() {
        use std::os::unix::fs::PermissionsExt;
        use zel_proto::dynamic::Msg;
        use zel_remote::grpc::{frame_message, MessageParser};
        use zel_remote::http2::{Conn, Event, LocalSettings};
        use zel_remote::transport::Transport;

        let cas_listener = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("SKIPPED: loopback sockets are not permitted ({e})");
                return;
            }
            Err(e) => panic!("{e}"),
        };
        let cas_port = cas_listener.local_addr().unwrap().port();
        if let Err(e) = std::net::TcpStream::connect(("127.0.0.1", cas_port)) {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                eprintln!("SKIPPED: loopback connect not permitted ({e})");
                return;
            }
            panic!("{e}");
        }
        let cas_server = zel_remote::fake_server::FakeReapiServer::new();
        cas_server.serve_tcp(cas_listener);

        let bes_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let bes_port = bes_listener.local_addr().unwrap().port();
        let bes_auth_headers: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let bes_auth_clone = bes_auth_headers.clone();
        std::thread::spawn(move || {
            for tcp in bes_listener.incoming().flatten() {
                let (Ok(r), Ok(w)) = (tcp.try_clone(), tcp.try_clone()) else {
                    continue;
                };
                let t = Transport {
                    reader: Box::new(r),
                    writer: Box::new(w),
                    shutdown: Box::new(move || {
                        let _ = tcp.shutdown(std::net::Shutdown::Both);
                    }),
                };
                let auth_log = bes_auth_clone.clone();
                std::thread::spawn(move || {
                    let Ok((_conn, rx)) = Conn::server_from(t, LocalSettings::default()) else {
                        return;
                    };
                    while let Ok(st) = rx.recv() {
                        let auth_log = auth_log.clone();
                        std::thread::spawn(move || {
                            let mut path = String::new();
                            let mut parser = MessageParser::default();
                            let mut msgs = Vec::new();
                            loop {
                                match st.recv(None) {
                                    Ok(Event::Headers(h)) => {
                                        path = h
                                            .iter()
                                            .find(|kv| kv.0 == ":path")
                                            .map(|kv| kv.1.clone())
                                            .unwrap_or_default();
                                        if let Some((_, v)) =
                                            h.iter().find(|kv| kv.0 == "authorization")
                                        {
                                            auth_log.lock().unwrap().push(v.clone());
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
                            let ok_headers = [
                                (":status".to_string(), "200".to_string()),
                                ("content-type".to_string(), "application/grpc".to_string()),
                            ];
                            let _ = st.send_headers(&ok_headers, false);
                            if path == bes::PUBLISH_LIFECYCLE {
                                let _ = st.send_data(&frame_message(&[]), false, None);
                            } else if path == bes::PUBLISH_STREAM {
                                for m in &msgs {
                                    let r = Msg::decode(
                                        "google.devtools.build.v1.PublishBuildToolEventStreamRequest",
                                        m,
                                    )
                                    .unwrap();
                                    let o = r.get_msg("ordered_build_event").unwrap();
                                    let mut resp = Msg::new(
                                        "google.devtools.build.v1.PublishBuildToolEventStreamResponse",
                                    );
                                    resp.set_int("sequence_number", o.get_i64("sequence_number"));
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
                            let _ = st.send_headers(
                                &[("grpc-status".to_string(), "0".to_string())],
                                true,
                            );
                        });
                    }
                });
            }
        });

        let root = std::env::temp_dir().join(format!(
            "zel-obs-cred-{}-{}",
            std::process::id(),
            zel_util::uuid_v4()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let ws = root.join("ws");
        std::fs::create_dir_all(ws.join("tools")).unwrap();
        std::fs::create_dir_all(ws.join("p")).unwrap();
        std::fs::write(ws.join("WORKSPACE"), "").unwrap();
        std::fs::write(
            ws.join("p/BUILD"),
            "genrule(name='out', outs=['out.txt'], cmd='printf payload > $@')\n",
        )
        .unwrap();

        let helper_log = root.join("helper.log");
        let helper_script = ws.join("tools/cred_helper.sh");
        std::fs::write(
            &helper_script,
            format!(
                "#!/bin/sh\nread req\nprintf '%s|%s\\n' \"$req\" \"$(pwd -P)\" >> {}\nprintf '{{\"headers\":{{\"Authorization\":[\"Bearer %s\"]}}}}\\n' \"$OBS_CLIENT_TOK\"\n",
                helper_log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&helper_script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let rt = runtime(&root, &ws);
        let path_env = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
        let (code, err, _) = run(
            &rt,
            &[
                "build",
                "--noenable_bzlmod",
                "--noremote_accept_cached",
                "--noremote_upload_local_results",
                &format!("--client_env=PATH={path_env}"),
                "--client_env=OBS_CLIENT_TOK=ws-cred-ok",
                "--credential_helper=127.0.0.1=%workspace%/tools/cred_helper.sh",
                &format!("--bes_backend=grpc://127.0.0.1:{bes_port}"),
                &format!("--remote_cache=grpc://127.0.0.1:{cas_port}"),
                "//p:out",
            ],
        );
        assert_eq!(
            code, 0,
            "build with %workspace% credential helper failed: {err}"
        );

        // 1. BES server received Bearer token from `%workspace%` helper with `client_env`.
        let bes_auths = bes_auth_headers.lock().unwrap().clone();
        assert!(
            bes_auths.iter().any(|v| v == "Bearer ws-cred-ok"),
            "BES did not receive credential helper auth: {bes_auths:?}"
        );

        // 2. CAS server (used by ByteStreamBuildEventArtifactUploader and remote cache) received
        //    Bearer token from `%workspace%` helper with `client_env`.
        let cas_hdrs = cas_server
            .last_headers("FindMissingBlobs")
            .expect("CAS FindMissingBlobs should have been called");
        assert!(
            cas_hdrs.contains(&("authorization".to_string(), "Bearer ws-cred-ok".to_string())),
            "CAS did not receive credential helper auth: {cas_hdrs:?}"
        );

        // 3. Helper invocations received normalized `http://` URIs (not `grpc://`) and ran with
        //    workspace as cwd.
        let log_text = std::fs::read_to_string(&helper_log).unwrap();
        let expected_ws = std::fs::canonicalize(&ws).unwrap();
        let expected_bes_uri = format!(r#"{{"uri":"http://127.0.0.1:{bes_port}"}}"#);
        let expected_cas_uri = format!(r#"{{"uri":"http://127.0.0.1:{cas_port}"}}"#);
        assert!(
            log_text.contains(&expected_bes_uri),
            "helper log missing normalized BES URI ({expected_bes_uri}): {log_text}"
        );
        assert!(
            log_text.contains(&expected_cas_uri),
            "helper log missing normalized CAS URI ({expected_cas_uri}): {log_text}"
        );
        assert!(
            !log_text.contains("grpc://"),
            "helper must never receive non-standard grpc:// URI: {log_text}"
        );
        for line in log_text.lines() {
            let cwd = line.split('|').nth(1).unwrap_or("");
            assert_eq!(
                std::fs::canonicalize(cwd).unwrap(),
                expected_ws,
                "helper did not run with workspace as cwd: {line}"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }
}
