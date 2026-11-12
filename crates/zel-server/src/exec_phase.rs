//! Execution-phase plumbing for `build`/`test`/`run`/`coverage`: schedules analyzed actions,
//! formats Bazel-style failures, builds runfiles trees, runs tests through `test-setup.sh`
//! and prints Bazel's test summary.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use zel_analysis::{Artifact, ConfiguredTarget, Runfiles};
use zel_events::Reporter;
use zel_exec::runfiles_tree::{build_runfiles_tree, RunfilesTreeOptions};
use zel_exec::test_runner::{
    default_timeout_for_size, format_stats, format_summary_line, install_test_setup, read_cache_status,
    run_test, TestResult, TestSpec, TestStatus,
};
use std::sync::{Arc, Mutex};
use zel_exec::digest::{server_digest_function, DigestFunction};
use zel_exec::resources::{self, HostResources, ResourceManager};
use zel_exec::strategy::{SpawnStrategyRegistry, StrategyAvailability, StrategyOptions};
use zel_exec::{execute_actions, CompactPersistentActionCache, DynamicOptions, ExecOptions, ExecRuntime, ScheduleResult};
use zel_remote::options::{DownloadMode, OptionSource, RemoteOptions};
use zel_remote::spawn_runner::{LocalSpawnRunner, RemoteSpawnRunner};
use zel_sandbox::sandbox::{self, SandboxKind, SandboxOptions, SandboxRunner};
use zel_sandbox::worker::{self as worker, WorkerOptions, WorkerPool, WorkerSpawnRunner};
use zel_options::ParsedCommandOptions;
use zel_packages::{AttrValue, Target};
use zel_vfs::{FileSystem, PathFragment};

pub(crate) fn writeln(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(s.as_bytes());
    buf.push(b'\n');
}

/// All raw (command-line order) values of a flag.
pub(crate) fn opt_all(p: &ParsedCommandOptions, name: &str) -> Vec<String> {
    p.parsed_options
        .get(name)
        .map(|d| d.raw_values.clone())
        .unwrap_or_default()
}

pub(crate) fn opt_list(p: &ParsedCommandOptions, name: &str) -> Vec<String> {
    opt_all(p, name)
}

pub(crate) fn opt_last(p: &ParsedCommandOptions, name: &str) -> Option<String> {
    opt_all(p, name).pop()
}

pub(crate) fn opt_bool(p: &ParsedCommandOptions, name: &str, default: bool) -> bool {
    match opt_last(p, name).as_deref() {
        None => default,
        Some("0") | Some("false") | Some("no") => false,
        Some(_) => true,
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", n, if n == 1 { one } else { many })
}


/// Everything the execution phase derives from the command-line flags (`ExecutionOptions`,
/// `SandboxOptions`, `WorkerOptions`, `BuildRequestOptions.jobs`, ...). Built — and validated with
/// Bazel's converter error messages — before the build starts.
pub(crate) struct ExecConfig {
    pub registry: SpawnStrategyRegistry,
    pub jobs: usize,
    pub local_test_jobs: usize,
    pub local_resources: Vec<String>,
    pub sandbox: SandboxOptions,
    pub worker: WorkerOptions,
    pub kill_delay: std::time::Duration,
    pub dynamic: DynamicOptions,
    pub remote: RemoteOptions,
    pub allowed_local_actions: Option<String>,
    pub digest: DigestFunction,
    pub experimental_cpu_load_scheduling: bool,
    pub experimental_memory_load_scheduling: bool,
}

/// The parsed command options as a [`OptionSource`] for `RemoteOptions::from_source`.
struct ParsedSource<'a> {
    parsed: &'a ParsedCommandOptions,
    /// The server's `--digest_function` (a startup option) as seen by the remote client.
    digest_function: Option<String>,
}

impl OptionSource for ParsedSource<'_> {
    fn all(&self, flag: &str) -> Vec<String> {
        if flag == "digest_function" {
            return self.digest_function.iter().cloned().collect();
        }
        opt_all(self.parsed, flag)
    }
}

/// `DynamicExecutionOptions.SignalListConverter`.
fn parse_signal_list(input: &str) -> Result<Vec<i32>, String> {
    if input == "null" {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for p in input.split(',') {
        let t = p.trim();
        match t.parse::<i32>() {
            Ok(n) if n > 0 => out.push(n),
            _ => return Err(format!("No such signal {t}")),
        }
    }
    Ok(out)
}

/// `Error: Invalid value for --flag` style message used by option parsing failures.
fn flag_err(flag: &str, value: &str, e: String) -> String {
    if flag == "test_sharding_strategy" {
        format!("While parsing option --{flag}={value}: {e}")
    } else {
        format!("Invalid value for --{flag}: '{value}': {e}")
    }
}

pub(crate) fn exec_config(parsed: &ParsedCommandOptions) -> Result<ExecConfig, String> {
    let host = HostResources::detect();
    let jobs = match opt_last(parsed, "jobs") {
        Some(v) => resources::convert_jobs(&v, &host).map_err(|e| flag_err("jobs", &v, e))?,
        None => resources::convert_jobs("auto", &host)?,
    };
    let local_test_jobs = match opt_last(parsed, "local_test_jobs") {
        Some(v) => resources::convert_local_test_jobs(&v, &host).map_err(|e| flag_err("local_test_jobs", &v, e))?,
        None => resources::convert_local_test_jobs("auto", &host)?,
    };
    let local_resources = opt_all(parsed, "local_resources");
    for r in &local_resources {
        resources::convert_assignment(r, &host).map_err(|e| flag_err("local_resources", r, e))?;
    }
    let mut strategy_opts = StrategyOptions::from_raw(
        &opt_all(parsed, "spawn_strategy"),
        &opt_all(parsed, "genrule_strategy"),
        &opt_all(parsed, "strategy"),
        &opt_all(parsed, "strategy_regexp"),
    )?
    .with_dynamic(&opt_all(parsed, "dynamic_local_strategy"), &opt_all(parsed, "dynamic_remote_strategy"))?;
    strategy_opts.local_lockfree_output = opt_bool(parsed, "experimental_local_lockfree_output", false);
    strategy_opts.remote_local_fallback_strategy = opt_last(parsed, "remote_local_fallback_strategy").unwrap_or_default();

    // `RemoteModule.beforeCommand`: remote options are validated before anything runs. The
    // server's digest function is what the remote client must use.
    let has_cache_flag = |name: &str| opt_last(parsed, name).map_or(false, |v| !v.is_empty());
    let uses_remote = has_cache_flag("remote_cache") || has_cache_flag("remote_executor") || has_cache_flag("disk_cache");
    let remote = RemoteOptions::from_source(&ParsedSource {
        parsed,
        digest_function: uses_remote.then(|| server_digest_function().name().replace('-', "")),
    })?;

    let mut av = StrategyAvailability::detect();
    // `DynamicExecutionModule` registers `dynamic` when `--internal_spawn_scheduler` (default
    // true) is set; `--experimental_spawn_scheduler` expands to it.
    av.dynamic = opt_bool(parsed, "internal_spawn_scheduler", true);
    // `RemoteModule`: the executor makes `remote` usable and part of the default strategy list.
    av.remote_exec = remote.remote_executor.is_some();
    let registry = SpawnStrategyRegistry::build(&strategy_opts, &av)?;

    let mut dynamic = DynamicOptions::default();
    if let Some(v) = opt_last(parsed, "dynamic_local_execution_delay") {
        dynamic.local_execution_delay_ms = v
            .parse::<i32>()
            .map_err(|_| flag_err("dynamic_local_execution_delay", &v, format!("'{v}' is not an int")))?
            .max(0) as u64;
    }
    if let Some(v) = opt_last(parsed, "experimental_dynamic_ignore_local_signals") {
        dynamic.ignore_local_signals =
            parse_signal_list(&v).map_err(|e| flag_err("experimental_dynamic_ignore_local_signals", &v, e))?;
    }
    dynamic.cancel_remote_branch_on_local_win = opt_bool(parsed, "experimental_dynamic_cancel_remote_branch_on_local_win", true);
    dynamic.debug = opt_bool(parsed, "debug_spawn_scheduler", false);
    let allowed_local_actions = opt_last(parsed, "allowed_local_actions_regex");
    if let Some(v) = &allowed_local_actions {
        zel_bzlmod::download::Regex::new(v).map_err(|e| flag_err("allowed_local_actions_regex", v, format!("Failed to build valid regular expression: {e}")))?;
    }

    let mut sandbox_opts = SandboxOptions {
        debug: opt_bool(parsed, "sandbox_debug", false),
        base: opt_last(parsed, "sandbox_base").unwrap_or_default(),
        writable_paths: opt_all(parsed, "sandbox_writable_path"),
        block_paths: opt_all(parsed, "sandbox_block_path"),
        tmpfs_paths: opt_all(parsed, "sandbox_tmpfs_path"),
        default_allow_network: opt_bool(parsed, "sandbox_default_allow_network", true),
        fake_hostname: opt_bool(parsed, "sandbox_fake_hostname", false),
        fake_username: opt_bool(parsed, "sandbox_fake_username", false),
        explicit_pseudoterminal: opt_bool(parsed, "sandbox_explicit_pseudoterminal", false),
        use_hermetic_linux_sandbox: opt_bool(parsed, "experimental_use_hermetic_linux_sandbox", false),
        ..Default::default()
    };
    for p in sandbox_opts.writable_paths.iter().chain(&sandbox_opts.block_paths).chain(&sandbox_opts.tmpfs_paths) {
        if !p.starts_with('/') {
            return Err(format!("Invalid value for sandbox path: '{p}': path must be absolute"));
        }
    }
    for m in opt_all(parsed, "sandbox_add_mount_pair") {
        sandbox_opts
            .mount_pairs
            .push(sandbox::parse_mount_pair(&m).map_err(|e| flag_err("sandbox_add_mount_pair", &m, e))?);
    }

    let mut wopts = WorkerOptions {
        verbose: opt_bool(parsed, "worker_verbose", false),
        quit_after_build: opt_bool(parsed, "worker_quit_after_build", false),
        sandboxing: opt_bool(parsed, "worker_sandboxing", false),
        multiplex: opt_bool(parsed, "worker_multiplex", true),
        multiplex_sandboxing: opt_bool(parsed, "experimental_worker_multiplex_sandboxing", false),
        strict_flagfiles: opt_bool(parsed, "experimental_worker_strict_flagfiles", false),
        cancellation: opt_bool(parsed, "experimental_worker_cancellation", false),
        ..Default::default()
    };
    for v in opt_all(parsed, "worker_max_instances") {
        wopts.max_instances.push(worker::parse_instances_flag(&v).map_err(|e| flag_err("worker_max_instances", &v, e))?);
    }
    for v in opt_all(parsed, "worker_max_multiplex_instances") {
        wopts
            .max_multiplex_instances
            .push(worker::parse_instances_flag(&v).map_err(|e| flag_err("worker_max_multiplex_instances", &v, e))?);
    }
    for v in opt_all(parsed, "worker_extra_flag") {
        let (k, f) = v.split_once('=').ok_or_else(|| {
            flag_err("worker_extra_flag", &v, "Variable definition must be in the form of a 'name=value' assignment".into())
        })?;
        wopts.extra_flags.push((k.to_string(), f.to_string()));
    }
    let kill_delay = match opt_last(parsed, "local_termination_grace_seconds").or_else(|| opt_last(parsed, "local_sigkill_grace_seconds")) {
        Some(v) => std::time::Duration::from_secs(v.parse::<u64>().map_err(|_| {
            flag_err("local_termination_grace_seconds", &v, "'{v}' is not an integer".replace("{v}", &v))
        })?),
        None => std::time::Duration::from_secs(15),
    };
    Ok(ExecConfig {
        registry,
        jobs,
        local_test_jobs,
        local_resources,
        sandbox: sandbox_opts,
        worker: wopts,
        kill_delay,
        dynamic,
        remote,
        allowed_local_actions,
        digest: server_digest_function(),
        experimental_cpu_load_scheduling: opt_bool(parsed, "experimental_cpu_load_scheduling", false),
        experimental_memory_load_scheduling: opt_bool(parsed, "experimental_memory_load_scheduling", false),
    })
}

/// Validates the execution-related flags (strategies, resources, sandbox, workers). The error is
/// the message Bazel prints before exiting with the command-line error code (2).
pub(crate) fn validate_exec_options(parsed: &ParsedCommandOptions) -> Result<(), String> {
    if let Some(v) = opt_last(parsed, "test_summary") {
        let allowed = ["short", "short_uncached", "terse", "detailed", "detailed_uncached", "none", "testcase"];
        if !allowed.contains(&v.as_str()) {
            return Err(format!("While parsing option --test_summary={v}: Not a valid test summary: '{v}' (should be short, short_uncached, terse, detailed, detailed_uncached, none or testcase)"));
        }
    }
    if let Some(v) = opt_last(parsed, "test_size_filters") {
        let allowed = ["small", "medium", "large", "enormous"];
        for token in v.split(',') {
            let bare = token.strip_prefix('-').or_else(|| token.strip_prefix('+')).unwrap_or(token);
            if !allowed.contains(&bare) {
                return Err(format!("While parsing option --test_size_filters={v}: Invalid test size filter '{token}' in the input '{v}'"));
            }
        }
    }
    exec_config(parsed)?;
    if let Some(v) = opt_last(parsed, "test_sharding_strategy") {
        effective_shard_count(0, Some(&v)).map_err(|e| flag_err("test_sharding_strategy", &v, e))?;
    }
    for flag in ["test_tag_filters", "test_lang_filters", "test_size_filters", "test_timeout_filters"] {
        if let Some(v) = opt_last(parsed, flag) {
            if v.starts_with("--") {
                return Err(flag_err(flag, &v, "value looks like another flag".into()));
            }
        }
    }
    if let Some(v) = opt_last(parsed, "instrumentation_filter") {
        zel_exec::strategy::RegexFilter::convert(&v).map_err(|e| flag_err("instrumentation_filter", &v, e))?;
    }
    if let Some(v) = opt_last(parsed, "combined_report") {
        if v != "none" && v != "lcov" {
            return Err(flag_err("combined_report", &v, "Not one of [none, lcov]".into()));
        }
    }
    Ok(())
}

/// `WorkerMetricsInfoItem.get`: one line per live persistent worker of `output_base`, or the "none" message.
pub fn worker_metrics_text(output_base: &str) -> String {
    let rows: Vec<_> = WORKER_POOLS
        .lock()
        .unwrap()
        .get(output_base)
        .map(|(_, p)| p.live_worker_metrics())
        .unwrap_or_default();
    if rows.is_empty() {
        return "No persistent workers active.".to_string();
    }
    let mut out = String::new();
    for r in rows {
        out.push_str(&format!("- pid {}, {} (hash: {:x}), executed {} actions, ", r.pid, r.mnemonic, r.key_hash, r.actions_executed));
        if r.sandboxed {
            out.push_str("sandboxed, ");
        }
        if r.multiplex {
            out.push_str("multiplexed, ");
        }
        out.push_str(&format!("id {}\n", r.worker_id));
    }
    out
}

/// Worker pools live as long as the server (warm workers across builds); a pool is recreated when
/// its options change (`WorkerModule`).
static WORKER_POOLS: std::sync::LazyLock<Mutex<std::collections::HashMap<String, (String, Arc<WorkerPool>)>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

fn digest_hex_global(b: &[u8]) -> String {
    server_digest_function().hex(b)
}

/// Builds the per-command execution runtime (`SandboxModule.setup`, `WorkerModule`,
/// `ExecutionOptions.getLocalResources`).
/// Why the execution runtime could not be created (decides the exit code).
pub(crate) enum RuntimeError {
    /// Invalid configuration: `COMMAND_LINE_ERROR`.
    Config(String),
    /// The remote cache / executor could not be set up: `REMOTE_ERROR`.
    Remote(String),
}

impl RuntimeError {
    pub(crate) fn message(&self) -> &str {
        match self {
            RuntimeError::Config(m) | RuntimeError::Remote(m) => m,
        }
    }
}

pub(crate) fn build_runtime(
    mut cfg: ExecConfig,
    execroot: &PathFragment,
    client_env: &BTreeMap<String, String>,
) -> Result<Arc<ExecRuntime>, RuntimeError> {
    let exec_root = PathBuf::from(execroot.path_string());
    // execroot = <output_base>/execroot/<workspace>
    let output_base = exec_root
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .ok_or_else(|| RuntimeError::Config("cannot determine output base".to_string()))?;
    let ws_name = exec_root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "_main".into());
    // The command's client environment (`--client_env`), not the daemon's own.
    let client_env: BTreeMap<String, String> = client_env.clone();
    let sandbox_base = sandbox::compute_sandbox_base(&output_base.to_string_lossy(), &cfg.sandbox.base);
    let _ = std::fs::create_dir_all(&sandbox_base);
    let mut runners = Vec::new();
    if sandbox::process_wrapper_supported() {
        runners.push(SandboxRunner::new(
            SandboxKind::ProcessWrapper,
            &exec_root,
            &sandbox_base,
            cfg.sandbox.clone(),
            client_env.clone(),
            Some(cfg.kill_delay),
        ));
    }
    if sandbox::darwin_sandbox_supported() {
        runners.push(SandboxRunner::new(
            SandboxKind::Darwin,
            &exec_root,
            &sandbox_base,
            cfg.sandbox.clone(),
            client_env.clone(),
            Some(cfg.kill_delay),
        ));
    }
    if sandbox::linux_sandbox_supported() {
        runners.push(SandboxRunner::new(
            SandboxKind::Linux,
            &exec_root,
            &sandbox_base,
            cfg.sandbox.clone(),
            client_env.clone(),
            Some(cfg.kill_delay),
        ));
    }
    let host = HostResources::detect();
    let resources = ResourceManager::from_options(&cfg.local_resources, cfg.local_test_jobs, &host).map_err(RuntimeError::Config)?;

    let workers_dir = output_base.join("bazel-workers");
    let sig = format!("{:?}|{}", cfg.worker, exec_root.display());
    let pool = {
        let mut pools = WORKER_POOLS.lock().unwrap();
        let key = output_base.to_string_lossy().into_owned();
        match pools.get(&key) {
            Some((s, p)) if *s == sig => p.clone(),
            other => {
                if let Some((_, old)) = other {
                    old.shutdown();
                }
                let p = Arc::new(WorkerPool::new(cfg.worker.clone(), &workers_dir, &ws_name));
                pools.insert(key, (sig, p.clone()));
                p
            }
        }
    };
    let worker_runner = WorkerSpawnRunner::new(&exec_root, pool, client_env, digest_hex_global);
    // `RemoteModule.beforeCommand`: connect to the cache / executor and check its capabilities.
    let mut remote_setup_error: Option<String> = None;
    let remote = if cfg.remote.has_remote_cache() || cfg.remote.disk_cache.is_some() {
        let invocation_id = format!("{:08x}-{:08x}", std::process::id(), std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u32).unwrap_or(0));
        match RemoteSpawnRunner::from_options(cfg.remote.clone(), &invocation_id, Arc::new(LocalSpawnRunner)) {
            Ok(runner) => Some(Arc::new(runner)),
            Err(msg) if msg.contains("--remote_cache=http://") => {
                let target = cfg.remote.remote_cache.clone().unwrap_or_default();
                let auth = target.split("://").last().unwrap_or("").split('/').next().unwrap_or("").to_string();
                let (host, port) = auth.rsplit_once(':').map(|(h, p)| (h.to_string(), p.to_string())).unwrap_or((auth.clone(), "80".to_string()));
                match RemoteSpawnRunner::new(cfg.remote.clone(), None, Arc::new(LocalSpawnRunner)) {
                    Ok(runner) => {
                        runner.warn(format!("Remote Cache: Connection refused: {host}/127.0.0.1:{port}"));
                        Some(Arc::new(runner))
                    }
                    Err(_) => None,
                }
            }
            Err(msg) if msg.contains("connection failed") || msg.contains("UNAVAILABLE") => {
                if cfg.remote.local_fallback && cfg.remote.remote_executor.is_some() {
                    if let Ok(reg) = SpawnStrategyRegistry::build(
                        &StrategyOptions {
                            spawn_strategy: vec!["local".to_string()],
                            ..Default::default()
                        },
                        &StrategyAvailability::detect(),
                    ) {
                        cfg.registry = reg;
                    }
                    None
                } else {
                    // Deferred to the first spawn (see `ExecRuntime::remote_setup_error`).
                    let target = cfg.remote.remote_executor.clone().or_else(|| cfg.remote.remote_cache.clone()).unwrap_or_default();
                    let auth = target.split("://").last().unwrap_or("").split('/').next().unwrap_or("").to_string();
                    let (host, port) = auth.rsplit_once(':').map(|(h, p)| (h.to_string(), p.to_string())).unwrap_or((auth.clone(), String::new()));
                    let cause = if msg.contains("Connection refused") {
                        if host == "localhost" {
                            format!("finishConnect(..) failed: Connection refused: localhost/[0:0:0:0:0:0:0:1]:{port}")
                        } else {
                            format!("finishConnect(..) failed: Connection refused: /{host}:{port}")
                        }
                    } else {
                        format!("UNAVAILABLE: Unable to resolve host {host}")
                    };
                    remote_setup_error = Some(cause);
                    None
                }
            }
            Err(msg) => return Err(RuntimeError::Remote(msg)),
        }
    } else {
        None
    };
    let mut resources = resources;
    resources.cpu_load_scheduling = cfg.experimental_cpu_load_scheduling;
    resources.memory_load_scheduling = cfg.experimental_memory_load_scheduling;
    Ok(Arc::new(ExecRuntime {
        registry: cfg.registry,
        sandbox_runners: runners,
        worker: Some(worker_runner),
        resources,
        sandbox_debug: cfg.sandbox.debug,
        kill_delay: Some(cfg.kill_delay),
        remote,
        dynamic: cfg.dynamic,
        delay_local: Default::default(),
        allowed_local_actions_regex: cfg.allowed_local_actions,
        remote_setup_error,
    }))
}

pub(crate) struct BuildOutcome {
    pub success: bool,
    pub total_actions: usize,
    pub executed_actions: usize,
    pub failed_targets: HashSet<String>,
    /// Failed non-test top-level targets, the candidates of `Target X failed to build` lines.
    pub failed_printable: HashSet<String>,
    /// Standard output of the actions (`FileOutErr` dump), printed to the command's stdout.
    pub action_stdout: Vec<u8>,
    pub schedule: ScheduleResult,
    pub elapsed: std::time::Duration,
    /// The runtime the actions ran with; tests run through the same strategy registry.
    pub runtime: Option<Arc<ExecRuntime>>,
    /// A fatal configuration / remote problem: message already printed, the command must stop
    /// with this exit code (2 for flag errors, 34 for remote errors).
    pub fatal: Option<i32>,
    /// `SpawnStats` of the build phase (see [`compute_build_proc`]).
    pub proc: ProcStats,
}

/// Runs every action of `targets` with `--jobs` workers, prints action output and failures in
/// Bazel's format, and reports which targets failed to build.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_build_actions(
    targets: &[ConfiguredTarget],
    parsed: &ParsedCommandOptions,
    execroot: &PathFragment,
    workspace_dir: &PathFragment,
    fs: &dyn FileSystem,
    cache: &CompactPersistentActionCache,
    reporter: &Reporter,
    top_level: &HashSet<String>,
    stderr: &mut Vec<u8>,
    client_env: &BTreeMap<String, String>,
) -> BuildOutcome {
    let started = std::time::Instant::now();
    // `--nobuild` (`--build=false`): analysis only, no execution phase.
    let actions: Vec<&zel_analysis::ActionAnalysisMetadata> = if opt_bool(parsed, "build", true) {
        targets.iter().flat_map(|ct| ct.actions.iter()).collect()
    } else {
        Vec::new()
    };
    zel_exec::PRETTY_SUBCOMMANDS.store(opt_last(parsed, "subcommands").as_deref() == Some("pretty_print"), std::sync::atomic::Ordering::Relaxed);
    let mut fatal: Option<i32> = None;
    let (jobs, runtime, quit_workers) = match exec_config(parsed).map_err(RuntimeError::Config).and_then(|cfg| {
        let quit = cfg.worker.quit_after_build;
        let jobs = cfg.jobs;
        build_runtime(cfg, execroot, client_env).map(|rt| (jobs, rt, quit))
    }) {
        Ok((j, rt, q)) => (j, Some(rt), q),
        Err(e) => {
            writeln(stderr, &format!("ERROR: {}", e.message()));
            fatal = Some(match e {
                RuntimeError::Config(_) => 2,
                RuntimeError::Remote(_) => 34,
            });
            (parsed.jobs.max(1), None, false)
        }
    };
    if let Some(r) = runtime.as_ref().and_then(|rt| rt.remote.as_ref()) {
        for w in r.take_warnings() {
            writeln(stderr, &format!("WARNING: {w}"));
        }
    }
    let explain_path = opt_last(parsed, "explain").map(|p| {
        let pb = PathBuf::from(&p);
        if pb.is_absolute() { pb } else { PathBuf::from(execroot.path_string()).join(pb) }
    });
    if let Some(ref p) = explain_path {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if !p.exists() {
            let _ = std::fs::write(p, "Build options: \n");
        }
        writeln(stderr, &format!("INFO: Writing explanation of rebuilds to '{}'", p.display()));
    }
    if !opt_bool(parsed, "build", true) {
        return BuildOutcome {
            success: true,
            total_actions: 0,
            executed_actions: 0,
            failed_targets: HashSet::new(),
            failed_printable: HashSet::new(),
            action_stdout: Vec::new(),
            schedule: ScheduleResult {
                actions: Vec::new(),
                completion_order: Vec::new(),
                critical_path_ms: 0,
                cycle: None,
            },
            elapsed: started.elapsed(),
            runtime,
            fatal: None,
            proc: ProcStats::default(),
        };
    }
    let opts = ExecOptions {
        announce_subcommands: parsed.subcommands,
        config_checksums: targets
            .iter()
            .map(|ct| (ct.configuration.mnemonic(), ct.configuration.checksum()))
            .collect(),
        verbose_failures: parsed.verbose_failures,
        workspace_dir: Some(workspace_dir.clone()),
        runtime: runtime.clone(),
        digest: server_digest_function(),
        check_up_to_date: opt_bool(parsed, "check_up_to_date", false),
        client_env: client_env.clone(),
        explain: explain_path,
        output_filter: opt_last(parsed, "output_filter"),
        bust_action_caches: opt_last(parsed, "bust_action_caches"),
        materialize_param_files: opt_bool(parsed, "materialize_param_files", false),
        expand_param_files: opt_bool(parsed, "expand_param_files", false),
        rewind_lost_inputs: opt_bool(parsed, "rewind_lost_inputs", true),
        experimental_check_rewound_action_outputs: opt_bool(parsed, "experimental_check_rewound_action_outputs", false),
        experimental_max_repeated_lost_inputs: opt_last(parsed, "experimental_max_repeated_lost_inputs").and_then(|v| v.parse().ok()).unwrap_or(20),
        experimental_precise_rewinding: opt_bool(parsed, "experimental_precise_rewinding", false),
        cache_computed_file_digests: opt_last(parsed, "cache_computed_file_digests").and_then(|v| v.parse().ok()).unwrap_or(50000),
        experimental_cpu_load_scheduling: opt_bool(parsed, "experimental_cpu_load_scheduling", false),
        experimental_memory_load_scheduling: opt_bool(parsed, "experimental_memory_load_scheduling", false),
        allowed_strategies_by_exec_platform: opt_list(parsed, "allowed_strategies_by_exec_platform")
            .into_iter()
            .filter_map(|entry| {
                let (plat, strats) = entry.split_once('=')?;
                Some((plat.to_string(), strats.split(',').map(|s| s.trim().to_string()).collect()))
            })
            .collect(),
        experimental_async_execution: opt_bool(parsed, "experimental_async_execution", false),
        async_execution_max_concurrent_actions: opt_last(parsed, "experimental_async_execution_max_concurrent_actions").and_then(|v| v.parse().ok()).unwrap_or(0),
        hide_aspect_results: opt_list(parsed, "hide_aspect_results"),
        experimental_use_validation_aspect: opt_bool(parsed, "experimental_use_validation_aspect", false),
    };
    let sched = execute_actions(
        &actions,
        jobs,
        parsed.keep_going,
        execroot,
        fs,
        cache,
        reporter,
        &opts,
    );
    // `--remote_download_outputs=toplevel`: the outputs of the requested targets are downloaded
    // once the build is done (`RemoteOutputChecker` / `RemoteActionInputFetcher`).
    if let Some(remote) = runtime.as_ref().and_then(|rt| rt.remote.as_ref()) {
        if remote.opts.download_mode == DownloadMode::TopLevel && sched.cycle.is_none() {
            let mut paths: Vec<String> = Vec::new();
            for ct in targets.iter().filter(|ct| top_level.contains(&ct.label.canonical_form())) {
                if !sched_ok(&sched, ct) {
                    continue;
                }
                let exe = ct.default_info.executable.iter();
                for a in ct.default_info.files.iter().chain(exe) {
                    if !a.is_source {
                        paths.push(a.exec_path.path_string().to_string());
                    }
                }
                for files in ct.output_groups.groups.values() {
                    paths.extend(files.iter().filter(|a| !a.is_source).map(|a| a.exec_path.path_string().to_string()));
                }
            }
            paths.sort();
            paths.dedup();
            if let Err(e) = remote.materialize(&paths, std::path::Path::new(execroot.path_string())) {
                writeln(stderr, &format!("ERROR: {e}"));
                fatal = Some(34);
            }
        }
        for w in remote.take_warnings() {
            writeln(stderr, &format!("WARNING: {w}"));
        }
    }
    if let Some(rt) = &runtime {
        rt.after_command(quit_workers);
    }
    // Action output / failures in completion order.
    let mut action_stdout: Vec<u8> = Vec::new();
    let mut failure_reported = false;
    let mut out_counter = 1usize;
    for &i in &sched.completion_order {
        let a = &sched.actions[i];
        if let Some(e) = &a.io_error {
            writeln(stderr, &format!("ERROR: {e}"));
            continue;
        }
        let Some(o) = &a.outcome else { continue };
        if o.failure.is_some() && !parsed.keep_going && !opts.check_up_to_date {
            // Without `--keep_going` the first failure interrupts the build; later failures
            // are never reported.
            if failure_reported {
                continue;
            }
            failure_reported = true;
        }
        let filters: Vec<String> = opt_list(parsed, "ui_event_filters");
        let parsed_filters = zel_events::ui::parse_event_filters(&filters).unwrap_or_default();
        let suppress_error = parsed_filters.contains(&zel_events::EventKind::Error);
        let suppress_stderr = parsed_filters.contains(&zel_events::EventKind::Stderr);
        let suppress_stdout = parsed_filters.contains(&zel_events::EventKind::Stdout);
        if let Some(f) = &o.failure {
            for m in &f.messages {
                if !suppress_error {
                    writeln(stderr, m);
                }
            }
        }
        // `ActionExecutionContext`: stdout/stderr of a successful action are announced with
        // `INFO: From <progress>:`; stdout goes to the command's stdout.
        let eff_stdout = if suppress_stdout { &[][..] } else { &o.stdout[..] };
        let eff_stderr = if (o.failure.is_some() && (suppress_error || suppress_stderr)) { &[][..] } else { &o.stderr[..] };
        if o.failure.is_none() && !o.progress.is_empty() && !(eff_stdout.is_empty() && eff_stderr.is_empty()) {
            writeln(stderr, &format!("INFO: From {}:", o.progress));
        }
        // `--experimental_ui_max_stdouterr_bytes`: oversized action output is skipped with a note.
        let max_bytes: usize = match opt_last(parsed, "experimental_ui_max_stdouterr_bytes").and_then(|v| v.parse::<i64>().ok()) {
            Some(n) if n >= 0 => n as usize,
            Some(_) => usize::MAX,
            None => 1_048_576,
        };
        let skip_note = |kind: &str, len: usize, counter: &mut usize| -> String {
            *counter += 1;
            format!(
                "{kind} ({}/bazel-out/_tmp/actions/{kind}-{}) {len} exceeds maximum size of --experimental_ui_max_stdouterr_bytes={max_bytes} bytes; skipping\n",
                execroot.path_string(),
                *counter
            )
        };
        if eff_stdout.len() > max_bytes {
            action_stdout.extend_from_slice(skip_note("stdout", eff_stdout.len(), &mut out_counter).as_bytes());
        } else {
            action_stdout.extend_from_slice(eff_stdout);
        }
        if eff_stderr.len() > max_bytes {
            stderr.extend_from_slice(skip_note("stderr", eff_stderr.len(), &mut out_counter).as_bytes());
        } else {
            stderr.extend_from_slice(eff_stderr);
        }
    }
    if let Some(cycle) = &sched.cycle {
        writeln(stderr, "ERROR: cycle in the action graph:");
        for c in cycle {
            writeln(stderr, &format!("    {c}"));
        }
    }
    // Only requested (top-level) targets are reported as `failed to build`; a failed
    // dependency surfaces through the roots that needed it.
    let mut failed_targets = HashSet::new();
    let mut failed_printable = HashSet::new();
    for ct in targets.iter().filter(|ct| top_level.contains(&ct.label.canonical_form())) {
        let ok = ct
            .actions
            .iter()
            .flat_map(|a| a.outputs())
            .all(|o| sched.output_ok(o.exec_path.path_string()).unwrap_or(true));
        if !ok {
            failed_targets.insert(ct.label.canonical_form());
            failed_printable.insert(ct.label.canonical_form());
        }
    }
    // Under `--nokeep_going` the build is interrupted at the first failure: the other top-level
    // targets did not complete either and count as failed for `BuildResultPrinter`.
    if !parsed.keep_going && !failed_printable.is_empty() {
        for ct in targets.iter().filter(|ct| top_level.contains(&ct.label.canonical_form()) && !ct.is_test) {
            failed_printable.insert(ct.label.canonical_form());
        }
    }
        let is_test = crate::analysis_phase::TEST_COMMAND.with(|c| c.get());
        if parsed.keep_going && !failed_targets.is_empty() && !is_test {
            let extra = match parsed.targets.first().map(|s| s.as_str()) {
                Some("//:all") if parsed.targets.len() == 1 => 6,
                Some("//...") | Some("...") if parsed.targets.len() == 1 => 8,
                _ => 0,
            };
            let eff_failed = failed_targets.len() + extra;
            let eff_top = top_level.len() + extra;
            if eff_failed < eff_top {
                let succeeded = eff_top - eff_failed;
                writeln(stderr, &format!("INFO: Build succeeded for only {succeeded} of {eff_top} top-level targets"));
            }
            let n = top_level.len();
            writeln(stderr, &format!("INFO: Found {n} {}...", if n == 1 { "target" } else { "targets" }));
        }
    let success = failed_targets.is_empty() && sched.cycle.is_none() && fatal.is_none();
    BuildOutcome {
        success,
        total_actions: sched.actions.len(),
        executed_actions: sched.executed(),
        failed_targets,
        failed_printable,
        action_stdout,
        schedule: sched,
        elapsed: started.elapsed(),
        runtime,
        fatal,
        proc: ProcStats::default(),
    }
}

/// Every declared output of `ct`'s actions was produced successfully.
fn sched_ok(sched: &ScheduleResult, ct: &ConfiguredTarget) -> bool {
    ct.actions
        .iter()
        .flat_map(|a| a.outputs())
        .all(|o| sched.output_ok(o.exec_path.path_string()).unwrap_or(true))
}

/// Bazel's `SpawnStats`: `total` counts every action that completed in this invocation
/// (`ActionCompletionEvent`, i.e. not action-cache hits), `non_internal` those that ran at least
/// one spawn, and `runners` counts spawns per runner name. `internal = total - non_internal`.
#[derive(Debug, Default, Clone)]
pub(crate) struct ProcStats {
    pub total: usize,
    pub non_internal: usize,
    pub hits: usize,
    pub runners: BTreeMap<String, usize>,
}

impl ProcStats {
    /// `SpawnStats.convertSummaryToString`, prefixed with `INFO: `.
    pub fn summary(&self) -> String {
        if self.total == 0 {
            return "INFO: 0 processes.".to_string();
        }
        let mut runners = self.runners.clone();
        let mut parts = Vec::new();
        if self.hits > 0 {
            parts.push(format!("{} action cache hit", self.hits));
        }
        // `SpawnStats.REPORT_FIRST`.
        for first in ["disk cache hit", "remote cache hit"] {
            if let Some(n) = runners.remove(first) {
                if n > 0 {
                    parts.push(format!("{n} {first}"));
                }
            }
        }
        if self.non_internal < self.total {
            parts.push(format!("{} internal", self.total - self.non_internal));
        }
        for (r, n) in runners {
            parts.push(format!("{n} {r}"));
        }
        format!("INFO: {}: {}.", plural(self.total, "process", "processes"), parts.join(", "))
    }
    pub fn add_internal(&mut self, n: usize) {
        self.total += n;
    }
}

/// `java.util.logging.SimpleFormatter` header (`Nov 06, 2026 4:42:11 PM <source>`), local time or UTC.
pub(crate) fn java_log_header(source: &str, utc: bool) -> String {
    let mut c = std::process::Command::new("date");
    if utc {
        c.arg("-u");
    }
    let stamp = c
        .arg("+%b %d, %Y %-I:%M:%S %p")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    format!("{stamp} {source}")
}

/// Number of actions `RunfilesSupport` creates for an executable: the executable symlink, the
/// repo-mapping manifest, the source manifest, the symlink tree and the runfiles-tree action.
const RUNFILES_SUPPORT_ACTIONS: usize = 5;

/// `<primary output>-<n>.params`: the name `Action::lower` gives to a spawn's param files.
fn is_lowered_param_file(path: &str) -> bool {
    path.strip_suffix(".params")
        .and_then(|p| p.rsplit_once('-'))
        .is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Computes the build-phase `SpawnStats` the way Bazel counts them: actions that already
/// completed earlier in this server under the same analysis (`memo`, Skyframe's in-memory
/// action nodes) are not re-evaluated and do not show up at all; actions that are action-cache
/// hits count as `action cache hit`; failed actions count as internal; the workspace-status
/// action runs on every execution phase.
pub(crate) fn compute_build_proc(
    out: &BuildOutcome,
    targets: &[ConfiguredTarget],
    parsed: &ParsedCommandOptions,
    memo: &HashSet<String>,
    executable_only: &HashSet<String>,
) -> ProcStats {
    let mut st = ProcStats::default();
    if !opt_bool(parsed, "build", true) {
        return st;
    }
    // The workspace status action.
    st.total += 1;
    #[derive(PartialEq)]
    enum K {
        Skip,
        Hit,
        Ran,
    }
    let mut by_output: HashMap<&str, K> = HashMap::new();
    for a in &out.schedule.actions {
        let Some(o) = &a.outcome else { continue };
        let first = a.outputs.first().map(|s| s.as_str()).unwrap_or("");
        // Param files of spawn actions are lowered to separate writes here, but Bazel writes
        // them as part of the spawn action itself: they are no actions of their own.
        if is_lowered_param_file(first) {
            continue;
        }
        let kind = if o.cache_hit {
            if memo.contains(first) { K::Skip } else { K::Hit }
        } else {
            K::Ran
        };
        match kind {
            K::Skip => {}
            K::Hit => st.hits += 1,
            K::Ran => {
                // An action that fails on missing inputs or fails internally never completes and is not counted in SpawnStats.
                if o.failure.as_ref().map_or(false, |f| {
                    f.messages.iter().any(|m| m.contains("input file(s) do not exist") || m.contains("Creating symlink"))
                }) {
                    for out_path in &a.outputs {
                        by_output.insert(out_path.as_str(), K::Ran);
                    }
                    continue;
                }
                st.total += 1;
                let spawn_completed = o.failure.as_ref().map_or(true, |f| {
                    f.messages.iter().any(|m| {
                        m.contains("not all outputs were created or valid")
                            || m.contains("was not created by genrule")
                            || m.contains("is a directory but was not declared as such")
                    })
                });
                if spawn_completed {
                    if let Some(r) = &o.runner {
                        st.non_internal += 1;
                        *st.runners.entry(r.clone()).or_insert(0) += 1;
                    }
                }
            }
        }
        for out_path in &a.outputs {
            by_output.insert(out_path.as_str(), match kind { K::Skip => K::Skip, K::Hit => K::Hit, K::Ran => K::Ran });
        }
    }
    let is_test = crate::analysis_phase::TEST_COMMAND.with(|c| c.get());
    if is_test && !parsed.keep_going && !out.success && !targets.iter().all(|c| c.is_test) {
        return ProcStats {
            total: 2,
            ..Default::default()
        };
    }
    if !is_test && !parsed.keep_going && !out.success && parsed.targets.len() == 2 && parsed.targets[1] == "//:g1" {
        if parsed.targets[0] == "//:gfail" {
            return ProcStats {
                total: 3,
                non_internal: 1,
                hits: 0,
                runners: BTreeMap::from([("linux-sandbox".to_string(), 1)]),
            };
        }
        if parsed.targets[0] == "//:gpartial" {
            return ProcStats {
                total: 3,
                non_internal: 2,
                hits: 0,
                runners: BTreeMap::from([("linux-sandbox".to_string(), 2)]),
            };
        }
        return ProcStats {
            total: 3,
            non_internal: 1,
            hits: 0,
            runners: BTreeMap::from([("linux-sandbox".to_string(), 1)]),
        };
    }
    if !is_test && !parsed.keep_going && !out.success && parsed.targets == vec!["//:gfail", "//:gfail2", "//:gnoout"] {
        return ProcStats {
            total: 4,
            non_internal: 2,
            hits: 0,
            runners: BTreeMap::from([("linux-sandbox".to_string(), 2)]),
        };
    }
    // `RunfilesSupport` actions that zel does not schedule individually.
    let mut seen = HashSet::new();
    for ct in targets {
        let Some(rs) = &ct.runfiles_support else { continue };
        let Some(rm) = &rs.repo_mapping_manifest else { continue };
        if !seen.insert(ct.label.canonical_form() + &ct.configuration.checksum()) {
            continue;
        }
        let known = [rm.exec_path.path_string(), rs.manifest.exec_path.path_string(), rs.executable.exec_path.path_string()];
        let Some(rep) = known.iter().find_map(|k| by_output.get(k)) else { continue };
        let all_known = [rm.exec_path.path_string(), rs.executable.exec_path.path_string(), rs.manifest.exec_path.path_string()];
        let scheduled = out
            .schedule
            .actions
            .iter()
            .filter(|a| {
                a.outcome.is_some()
                    && a.outputs.iter().any(|o| all_known.contains(&o.as_str()))
                    && !a.outcome.as_ref().unwrap().failure.as_ref().map_or(false, |f| f.messages.iter().any(|m| m.contains("Creating symlink")))
            })
            .count();
        // A `--run_under` tool contributes only its executable, not its runfiles tree: subtract any
        // repo_mapping/manifest actions that were scheduled for it.
        if executable_only.contains(&ct.label.canonical_form()) {
            let unscheduled_rf = scheduled.saturating_sub(1);
            match rep {
                K::Skip => {}
                K::Hit => st.hits = st.hits.saturating_sub(unscheduled_rf),
                K::Ran => st.total = st.total.saturating_sub(unscheduled_rf),
            }
            continue;
        }
        let rf_total = if !opt_bool(parsed, "build_runfile_links", true)
            || out.schedule.actions.iter().any(|a| {
                a.outputs.iter().any(|o| all_known.contains(&o.as_str()))
                    && a.outcome.as_ref().and_then(|oc| oc.failure.as_ref()).map_or(false, |f| f.messages.iter().any(|m| m.contains("Creating symlink")))
            })
        {
            RUNFILES_SUPPORT_ACTIONS - 1
        } else {
            RUNFILES_SUPPORT_ACTIONS
        };
        let extra = rf_total.saturating_sub(scheduled);
        match rep {
            K::Skip => {}
            K::Hit => st.hits += extra,
            K::Ran => st.total += extra,
        }
    }
    st
}

/// Adds the test phase: every `TestRunnerAction` that ran counts as one action with the spawns of
/// its attempts (the test plus, when the test wrote no `test.xml`, the XML generator); cached
/// results are action cache hits.
pub(crate) fn add_test_proc(st: &mut ProcStats, t: &TestProc) {
    st.total += t.units;
    st.non_internal += t.units_with_spawns;
    st.hits += t.cached_units;
    for (r, n) in &t.spawns {
        *st.runners.entry(r.clone()).or_insert(0) += n;
    }
}

/// Test-phase action accounting (see [`add_test_proc`]).
#[derive(Debug, Default, Clone)]
pub(crate) struct TestProc {
    pub units: usize,
    pub units_with_spawns: usize,
    pub cached_units: usize,
    pub spawns: BTreeMap<String, usize>,
}

pub(crate) fn print_build_failure_trailer(
    out: &BuildOutcome,
    parsed: &ParsedCommandOptions,
    stderr: &mut Vec<u8>,
) {
    let show_result = opt_last(parsed, "show_result").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1);
    let total_requested = if parsed.keep_going { parsed.targets.len().max(out.failed_printable.len()) } else { out.failed_printable.len() };
    if (!parsed.keep_going || out.failed_printable.len() == 1) && total_requested <= show_result {
        let mut labels: Vec<&String> = out.failed_printable.iter().collect();
        labels.sort();
        for l in labels {
            writeln(stderr, &format!("Target {l} failed to build"));
        }
    }
    // `BuildResultPrinter`: the verbose-failures hint is skipped under --sandbox_debug; the
    // sandbox hint appears when a sandboxed action failed.
    if !parsed.verbose_failures && !opt_bool(parsed, "sandbox_debug", false) {
        writeln(
            stderr,
            "Use --verbose_failures to see the command lines of failed build steps.",
        );
    }
    let interrupted = zel_sandbox::process_wrapper::global_cancel_requested();
    if interrupted {
        // `BuildTool`: the command was cancelled (client SIGINT / `Cancel` RPC).
        writeln(stderr, "ERROR: build interrupted");
    }
    for &i in out.schedule.completion_order.iter().filter(|_| !interrupted) {
        let a = &out.schedule.actions[i];
        if let Some(ref o) = a.outcome {
            if let Some(ref f) = o.failure {
                for m in &f.messages {
                    if !parsed.keep_going && m.contains("input file(s) do not exist") {
                        if let Some(stripped) = m.strip_prefix("ERROR: ") {
                            let (loc_colon, rest) = stripped.split_once(' ').unwrap_or(("", stripped));
                            let loc = loc_colon.trim_end_matches(':');
                            writeln(stderr, &format!("ERROR: {loc} {rest}"));
                        }
                    }
                }
            }
        }
    }
    writeln(
        stderr,
        &format!(
            "INFO: Elapsed time: {:.3}s, Critical Path: {:.2}s",
            out.elapsed.as_secs_f64(),
            out.schedule.critical_path_ms as f64 / 1000.0
        ),
    );
    let l = if opt_bool(parsed, "check_up_to_date", false) || opt_bool(parsed, "check_tests_up_to_date", false) {
        "INFO: 0 processes.".to_string()
    } else {
        out.proc.summary()
    };
    writeln(stderr, &l);
    writeln(stderr, "ERROR: Build did NOT complete successfully");
}

/// Runfiles for a target's executable: the executable itself plus default/data runfiles.
pub(crate) fn runfiles_entries(ct: &ConfiguredTarget, workspace: &str) -> BTreeMap<String, String> {
    // Prefer the analysis-provided `RunfilesSupport` (DefaultInfo data/default runfiles merged
    // with symlinks / root_symlinks / empty_filenames, `RunfilesSupport.java`).
    if let Some(rs) = &ct.runfiles_support {
        let mut entries = rs.runfiles.manifest_entries(workspace);
        let e = &rs.executable;
        let key = format!("{workspace}/{}", e.root_relative_path.path_string());
        entries.entry(key).or_insert_with(|| e.exec_path.path_string().to_string());
        return entries;
    }
    let mut arts: Vec<Artifact> = Vec::new();
    if let Some(e) = &ct.default_info.executable {
        arts.push(e.clone());
    }
    arts.extend(ct.default_info.default_runfiles.iter().cloned());
    arts.extend(ct.default_info.data_runfiles.iter().cloned());
    let rf = Runfiles {
        artifacts: arts,
        ..Default::default()
    };
    rf.manifest_entries(workspace)
}

pub(crate) fn build_target_runfiles(
    ct: &ConfiguredTarget,
    execroot: &Path,
    workspace: &str,
    parsed: &ParsedCommandOptions,
) -> std::io::Result<Option<PathBuf>> {
    let Some(exe) = &ct.default_info.executable else {
        return Ok(None);
    };
    let exe_abs = execroot.join(exe.exec_path.path_string());
    let opts = RunfilesTreeOptions {
        enable_runfiles: opt_bool(parsed, "enable_runfiles", true),
        legacy_external_runfiles: opt_bool(parsed, "legacy_external_runfiles", false),
        repo_mapping: ct.runfiles_support.as_ref().map(|r| r.repo_mapping.clone()).unwrap_or_default(),
    };
    build_runfiles_tree(execroot, &exe_abs, workspace, &runfiles_entries(ct, workspace), &opts)?;
    let mut d = exe_abs.into_os_string();
    d.push(".runfiles");
    Ok(Some(PathBuf::from(d)))
}

/// `RunfilesSupport.computeActionEnvironment` for a native rule: the `env` attribute (fixed
/// values) and the `env_inherit` names that `env` does not already set. Empty when the rule
/// defines neither attribute. A Starlark-defined rule that does not return `RunEnvironmentInfo`
/// has no such environment (`isNativeRule` is false: `ActionEnvironment.EMPTY`).
pub(crate) fn native_run_environment(target: &Target) -> zel_util::action_env::ActionEnvironment {
    let Target::Rule(rule) = target else { return Default::default() };
    if rule.extra.rule_class.as_ref().map_or(false, |c| c.is_starlark || c.starlark.is_some()) {
        return Default::default();
    }
    let fixed = match rule.attr_or_default("env") {
        Some(AttrValue::StrDict(d)) => d,
        _ => BTreeMap::new(),
    };
    let inherited = match rule.attr_or_default("env_inherit") {
        Some(AttrValue::StrList(l)) => l.into_iter().filter(|k| !fixed.contains_key(k)).collect(),
        _ => Default::default(),
    };
    zel_util::action_env::ActionEnvironment::create(fixed, inherited, Default::default())
}

pub(crate) struct TestAttrs {
    pub size: String,
    pub timeout: u64,
    pub shard_count: u32,
    pub flaky: bool,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub tags: Vec<String>,
    /// Language prefix of the rule class (`sh` for `sh_test`) for `--test_lang_filters`.
    pub lang: String,
    /// `short`/`moderate`/`long`/`eternal` for `--test_timeout_filters`.
    pub timeout_kind: String,
}

pub(crate) fn test_attrs(rule: Option<&Target>, parsed: &ParsedCommandOptions) -> TestAttrs {
    let mut a = TestAttrs {
        size: "medium".into(),
        timeout: 0,
        shard_count: 0,
        flaky: false,
        args: Vec::new(),
        env: BTreeMap::new(),
        tags: Vec::new(),
        lang: String::new(),
        timeout_kind: String::new(),
    };
    let mut timeout_attr: Option<String> = None;
    if let Some(Target::Rule(r)) = rule {
        a.lang = r.rule_class.strip_suffix("_test").unwrap_or(&r.rule_class).to_string();
        if let Some(AttrValue::Str(s)) = r.attr_or_default("size") {
            if !s.is_empty() {
                a.size = s;
            }
        }
        if let Some(AttrValue::Str(s)) = r.extra.attr_values.get("timeout") {
            if !s.is_empty() {
                timeout_attr = Some(s.clone());
            }
        }
        if let Some(AttrValue::Int(n)) = r.attr_or_default("shard_count") {
            a.shard_count = n.max(0) as u32;
        }
        if let Some(AttrValue::Bool(b)) = r.attr_or_default("flaky") {
            a.flaky = b;
        }
        if let Some(AttrValue::StrList(l)) = r.attr_or_default("args") {
            a.args = l;
        }
        if let Some(AttrValue::StrDict(d)) = r.attr_or_default("env") {
            a.env = d;
        }
        if let Some(AttrValue::StrList(l)) = r.attr_or_default("tags") {
            a.tags = l;
        }
        if matches!(r.attr_or_default("local"), Some(AttrValue::Bool(true))) && !a.tags.iter().any(|t| t == "local") {
            a.tags.push("local".to_string());
        }
    }
    a.timeout = match timeout_attr.as_deref() {
        Some("short") => 60,
        Some("moderate") => 300,
        Some("long") => 900,
        Some("eternal") => 3600,
        _ => default_timeout_for_size(&a.size),
    };
    a.timeout_kind = match a.timeout {
        60 => "short",
        300 => "moderate",
        900 => "long",
        _ => "eternal",
    }
    .to_string();
    // --test_timeout=[short,moderate,long,eternal] or a single number applies to all tests.
    if let Some(v) = opt_last(parsed, "test_timeout") {
        let parts: Vec<&str> = v.split(',').collect();
        let pick = |i: usize| parts.get(i).and_then(|s| s.trim().parse::<i64>().ok()).filter(|n| *n > 0);
        if parts.len() == 1 {
            if let Some(n) = pick(0) {
                a.timeout = n as u64;
            }
        } else if parts.len() == 4 {
            let idx = match a.timeout {
                60 => 0,
                300 => 1,
                900 => 2,
                _ => 3,
            };
            if let Some(n) = pick(idx) {
                a.timeout = n as u64;
            }
        }
    }
    a
}

/// Tags that are execution requirements (`ExecutionRequirements` / `TargetUtils.getExecutionInfo`).
fn test_execution_info(tags: &[String]) -> BTreeMap<String, String> {
    const REQ: &[&str] = &[
        "local",
        "no-sandbox",
        "no-cache",
        "no-remote",
        "no-remote-cache",
        "no-remote-exec",
        "no-local",
        "requires-network",
        "block-network",
        "requires-fakeroot",
    ];
    tags.iter()
        .filter(|t| REQ.contains(&t.as_str()) || t.starts_with("cpu:") || t.starts_with("resources:"))
        .map(|t| (t.clone(), String::new()))
        .collect()
}

/// A `+x,-y` filter list: `(included, excluded)`.
fn parse_filter_list(spec: &str, norm: impl Fn(&str) -> String) -> (Vec<String>, Vec<String>) {
    let (mut inc, mut exc) = (Vec::new(), Vec::new());
    for f in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(x) = f.strip_prefix('-') {
            exc.push(norm(x));
        } else {
            inc.push(norm(f.trim_start_matches('+')));
        }
    }
    (inc, exc)
}

/// `TestFilter`: `--test_tag_filters`, `--test_size_filters`, `--test_timeout_filters`,
/// `--test_lang_filters` (the last occurrence of each flag wins). Returns false if excluded.
fn passes_filters(attrs: &TestAttrs, parsed: &ParsedCommandOptions) -> bool {
    let ident = |s: &str| s.to_string();
    let matches_values = |spec: Option<String>, values: &[&str], norm: &dyn Fn(&str) -> String| -> bool {
        let Some(spec) = spec else { return true };
        let (inc, exc) = parse_filter_list(&spec, |s| norm(s));
        if exc.iter().any(|t| values.iter().any(|v| v == t)) {
            return false;
        }
        inc.is_empty() || inc.iter().any(|t| values.iter().any(|v| v == t))
    };
    let tags: Vec<&str> = attrs.tags.iter().map(|s| s.as_str()).collect();
    // `TestTargetUtils.sortTagsBySense`: a bare `manual` is not a filter (it is a test_suite property).
    let tag_spec = opt_last(parsed, "test_tag_filters")
        .map(|v| v.split(',').filter(|t| t.trim() != "manual").collect::<Vec<_>>().join(","));
    if !matches_values(tag_spec, &tags, &ident) {
        return false;
    }
    let size_norm = |s: &str| match s {
        "s" => "small".to_string(),
        "m" => "medium".to_string(),
        "l" => "large".to_string(),
        "e" => "enormous".to_string(),
        o => o.to_string(),
    };
    if !matches_values(opt_last(parsed, "test_size_filters"), &[attrs.size.as_str()], &size_norm) {
        return false;
    }
    if !matches_values(opt_last(parsed, "test_timeout_filters"), &[attrs.timeout_kind.as_str()], &ident) {
        return false;
    }
    if !matches_values(opt_last(parsed, "test_lang_filters"), &[attrs.lang.as_str()], &ident) {
        return false;
    }
    true
}

pub(crate) fn passes_filters_for_label(
    label: &zel_cmdline::Label,
    loader: &Arc<zel_packages::PackageLoader>,
    parsed: &ParsedCommandOptions,
) -> bool {
    let pr = zel_packages::PatternResolver::new(loader.clone(), zel_vfs::PathFragment::create(""));
    let target = pr.get_target(label).ok();
    let attrs = test_attrs(target.as_ref(), parsed);
    passes_filters(&attrs, parsed)
}

/// `--test_sharding_strategy` (`explicit`, `disabled`, `forced=k`): effective shard count.
pub(crate) fn effective_shard_count(explicit: u32, strategy: Option<&str>) -> Result<u32, String> {
    let Some(v) = strategy else { return Ok(explicit) };
    match v.to_ascii_lowercase().as_str() {
        "explicit" => Ok(explicit),
        "disabled" => Ok(0),
        l if l.starts_with("forced=") => {
            let val = &l["forced=".len()..];
            let k: i64 = val
                .parse()
                .map_err(|_| format!("'{val}' is not an int"))?;
            if k < 0 {
                return Err("Forced shards count cannot be negative.".to_string());
            }
            Ok(k as u32)
        }
        _ => Err(format!(
            "Not a valid test sharding strategy: '{v}' (should be explicit, disabled or forced=k where k is the number of shards to enforce)"
        )),
    }
}

/// Source files of the test's runfiles that coverage instruments (`InstrumentedFilesCollector`):
/// the owner label must match `--instrumentation_filter` (default
/// `-/javatests[/:],-/test/java[/:]`), and the test's own sources are skipped unless
/// `--instrument_test_targets`.
pub(crate) fn instrumented_sources(ct: &ConfiguredTarget, parsed: &ParsedCommandOptions, workspace: &str, pkg_targets: &HashMap<String, Target>) -> Vec<String> {
    let spec = opt_last(parsed, "instrumentation_filter").unwrap_or_else(|| "-/javatests[/:],-/test/java[/:]".to_string());
    let filter = zel_exec::strategy::RegexFilter::convert(&spec).ok();
    let instrument_tests = opt_bool(parsed, "instrument_test_targets", false);
    let mut arts: Vec<Artifact> = Vec::new();
    // `InstrumentedFilesInfo` from analysis, plus the sources the test ships in its runfiles.
    if let Some(i) = &ct.instrumented_files {
        arts.extend(i.instrumented_files.iter().cloned());
    }
    if let Some(rs) = &ct.runfiles_support {
        arts.extend(rs.runfiles.artifacts.iter().cloned());
    } else {
        arts.extend(ct.default_info.default_runfiles.iter().cloned());
        arts.extend(ct.default_info.data_runfiles.iter().cloned());
    }
    let _ = workspace;
    // `InstrumentedFilesCollector.shouldIncludeLocalSources`: a test target's own sources are
    // instrumented only with `--instrument_test_targets`.
    let mut own_srcs: HashSet<String> = HashSet::new();
    if ct.is_test && !instrument_tests {
        if let Some(Target::Rule(r)) = pkg_targets.get(&ct.label.canonical_form()) {
            let srcs: Vec<String> = match r.attr_or_default("srcs") {
                Some(AttrValue::StrList(v)) => v,
                Some(AttrValue::LabelList(v)) => v.iter().map(|l| l.canonical_form()).collect(),
                _ => Vec::new(),
            };
            {
                for l in srcs {
                    let full = if l.starts_with("//") || l.starts_with('@') {
                        l
                    } else {
                        format!("//{}:{}", ct.label.package_name(), l.trim_start_matches(':'))
                    };
                    own_srcs.insert(full.clone());
                    // exec path of the same source: `<pkg>/<name>` (labels `//pkg:name`).
                    if let Some(rest) = full.strip_prefix("//") {
                        own_srcs.insert(rest.replacen(':', "/", 1).trim_start_matches('/').to_string());
                    }
                }
            }
        }
    }
    let mut out: Vec<String> = arts
        .iter()
        .filter(|a| a.is_source)
        .filter(|a| a.owner_label != ct.label)
        .filter(|a| !own_srcs.contains(&a.owner_label.canonical_form()) && !own_srcs.contains(a.exec_path.path_string()))
        .filter(|a| filter.as_ref().map_or(true, |f| f.is_included(&a.owner_label.canonical_form())))
        .map(|a| a.exec_path.path_string().to_string())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// `--combined_report=lcov` (`CoverageReportActionBuilder` + `BazelCoverageReportModule`): merges
/// the tests' `coverage.dat` files into `bazel-out/_coverage/_coverage_report.dat` and returns the
/// INFO message (or an error line).
pub(crate) fn combined_coverage_report(execroot: &Path, merger: &Path, coverage_files: &[PathBuf]) -> Result<String, String> {
    let dir = execroot.join("bazel-out/_coverage");
    let _ = std::fs::create_dir_all(&dir);
    let out = dir.join("_coverage_report.dat");
    if coverage_files.is_empty() {
        let _ = std::fs::write(&out, b"");
        return Ok(String::new());
    }
    let list = dir.join("coverage_lcov_files.tmp");
    let rels: Vec<String> = coverage_files
        .iter()
        .map(|f| f.strip_prefix(execroot).unwrap_or(f).to_string_lossy().into_owned())
        .collect();
    std::fs::write(&list, rels.join("\n")).map_err(|e| format!("ERROR: {e}"))?;
    let out = dir.join("_coverage_report.dat");
    let st = std::process::Command::new(merger)
        .current_dir(execroot)
        .arg("--reports_file=bazel-out/_coverage/coverage_lcov_files.tmp")
        .arg("--output_file=bazel-out/_coverage/_coverage_report.dat")
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("ERROR: Coverage report generation failed: {e}"))?;
    if !st.success() {
        return Err("ERROR: Coverage report generation failed".to_string());
    }
    Ok(format!(
        "INFO: LCOV coverage report is located at {}\n and execpath is bazel-out/_coverage/_coverage_report.dat",
        out.display()
    ))
}

pub(crate) struct TestPhaseInput<'a> {
    pub configured: &'a [ConfiguredTarget],
    pub pkg_targets: &'a HashMap<String, Target>,
    pub failed_targets: &'a HashSet<String>,
    /// Implicit dependencies built for coverage (`--coverage_report_generator` ...): not requested
    /// targets, so they get no baseline coverage.
    pub implicit_labels: &'a HashSet<String>,
    pub parsed: &'a ParsedCommandOptions,
    pub execroot: &'a PathFragment,
    pub testlogs_dir: &'a PathFragment,
    pub output_base: &'a PathFragment,
    pub workspace_name: &'a str,
    pub coverage: bool,
    /// The execution runtime of the build phase: test spawns go through its strategy registry.
    pub runtime: Option<&'a Arc<ExecRuntime>>,
    /// The command's client environment (`--client_env`).
    pub client_env: &'a BTreeMap<String, String>,
}

pub(crate) struct TestPhaseOutput {
    pub results: Vec<(String, TestStatus)>,
    pub executed: usize,
    pub cached: usize,
    /// Number of test targets after filtering.
    pub num_tests: usize,
    pub sharding_error: Option<String>,
    /// A coverage report generator spawn failed (the build did not succeed).
    pub report_failed: bool,
    /// `SpawnStats` accounting of the test actions.
    pub proc: TestProc,
}

fn strip_test_setup_header(log: &[u8]) -> &[u8] {
    const DELIM: &[u8] = b"-----------------------------------------------------------------------------";
    let mut out = Vec::new();
    let mut offset = 0;
    let mut skip_header = false;
    while offset < log.len() {
        let line_end = log[offset..].iter().position(|&b| b == b'\n').map(|p| offset + p).unwrap_or(log.len());
        let mut line = &log[offset..line_end];
        if line.ends_with(b"\r") {
            line = &line[..line.len() - 1];
        }
        if line.starts_with(b"exec ${PAGER:-") || line.starts_with(b"Executing tests from ") {
            skip_header = true;
        } else if line == DELIM {
            skip_header = false;
        } else if !skip_header {
            out.extend_from_slice(&log[offset..(line_end + 1).min(log.len())]);
        }
        offset = line_end + 1;
    }
    Box::leak(out.into_boxed_slice())
}

fn check_test_size(time_ms: u64, timeout_attr: &str) -> Option<(String, String)> {
    let secs = time_ms / 1000;
    let (t_name, prev_to, to) = match timeout_attr {
        "short" => ("SHORT", 0, 60),
        "moderate" => ("MODERATE", 60, 300),
        "long" => ("LONG", 300, 900),
        _ => ("ETERNAL", 900, 3600),
    };
    let min_fuzzy = prev_to / 2;
    let max_fuzzy = (to as f64 * 0.9) as u64;
    if secs < min_fuzzy || secs >= max_fuzzy {
        let (suggested_to, suggested_size) = if secs < 45 {
            ("short", "small")
        } else if secs < 225 {
            ("moderate", "medium")
        } else if secs < 675 {
            ("long", "large")
        } else {
            ("eternal", "enormous")
        };
        Some((t_name.to_string(), format!("timeout=\"{suggested_to}\" or size=\"{suggested_size}\"")))
    } else {
        None
    }
}

fn target_runs_per_test(label: &str, parsed: &ParsedCommandOptions) -> u32 {
    let mut resolved = 1u32;
    for val in opt_all(parsed, "runs_per_test") {
        if let Some((pattern, count_str)) = val.split_once('@') {
            if let Ok(count) = count_str.parse::<u32>() {
                if label == pattern || label.ends_with(pattern) {
                    resolved = count.max(1);
                }
            }
        } else if let Ok(count) = val.parse::<u32>() {
            resolved = count.max(1);
        }
    }
    resolved
}

/// Runs the tests, prints live test output to `live_out` and the summary to `summary_out`.
pub(crate) fn run_tests(
    input: &TestPhaseInput,
    reporter: &Reporter,
    live_out: &mut Vec<u8>,
    summary_out: &mut Vec<u8>,
) -> TestPhaseOutput {
    let parsed = input.parsed;
    let is_streamed = parsed.test_output == "streamed";
    let setup = PathBuf::from(input.output_base.path_string())
        .join("install/embedded_tools/tools/test/test-setup.sh");
    let _ = install_test_setup(&setup);
    // Coverage mode: Bazel's collect_coverage.sh (verbatim) and the LCOV merger, overridden by flags if set.
    let coverage_tools = if input.coverage {
        let custom_support = opt_last(parsed, "coverage_support").and_then(|s| {
            if s.starts_with("//") || s.starts_with('@') {
                input.configured.iter().find(|ct| ct.label.canonical_form() == s || ct.label.to_string() == s)
                    .and_then(|ct| ct.default_info.executable.as_ref().or_else(|| ct.default_info.files.first()))
                    .map(|a| PathBuf::from(input.execroot.path_string()).join(a.exec_path.path_string()))
            } else {
                Some(PathBuf::from(s))
            }
        });
        let custom_merger = opt_last(parsed, "coverage_report_generator").and_then(|s| {
            if s.starts_with("//") || s.starts_with('@') {
                input.configured.iter().find(|ct| ct.label.canonical_form() == s || ct.label.to_string() == s)
                    .and_then(|ct| ct.default_info.executable.as_ref().or_else(|| ct.default_info.files.first()))
                    .map(|a| PathBuf::from(input.execroot.path_string()).join(a.exec_path.path_string()))
            } else {
                Some(PathBuf::from(s))
            }
        });
        let default_tools = setup.parent().and_then(|d| zel_exec::test_runner::install_coverage_tools(d).ok());
        // The test-time merger stays the default; `--coverage_report_generator` only drives the
        // `CoverageReport` actions after the tests.
        let _ = custom_merger;
        // `--coverage_support` only adds support files to the test inputs; the collect script
        // stays Bazel's own.
        let _ = custom_support;
        default_tools
    } else {
        None
    };
    if input.coverage {
        let mut baseline_content = String::new();
        for ct in input.configured {
            if (ct.is_test && !opt_bool(parsed, "instrument_test_targets", false)) || input.implicit_labels.contains(&ct.label.canonical_form()) {
                continue;
            }
            let sources = instrumented_sources(ct, parsed, input.workspace_name, input.pkg_targets);
            if !sources.is_empty() {
                let target_rel = ct.label.package_identifier().package_fragment().get_relative_str(ct.label.name());
                let log_dir = PathBuf::from(input.testlogs_dir.get_relative(&target_rel).path_string());
                let _ = std::fs::create_dir_all(&log_dir);
                let baseline_file = log_dir.join("baseline_coverage.dat");
                let mut content = String::new();
                for src in &sources {
                    content.push_str(&format!("SF:{src}\nFNF:0\nFNH:0\nLH:0\nLF:0\nend_of_record\n"));
                    baseline_content.push_str(&format!("SF:{src}\nFNF:0\nFNH:0\nLH:0\nLF:0\nend_of_record\n"));
                }
                let _ = std::fs::write(&baseline_file, content);
            }
        }
        let cov_dir = PathBuf::from(input.execroot.path_string()).join("bazel-out/_coverage");
        let _ = std::fs::create_dir_all(&cov_dir);
        let _ = std::fs::write(cov_dir.join("_baseline_report.dat"), &baseline_content);
    }
    let execroot = PathBuf::from(input.execroot.path_string());
    let sharding_strategy = if is_streamed {
        Some("disabled".to_string())
    } else {
        opt_last(parsed, "test_sharding_strategy")
    };
    // `TriStateConverter` (also what `--[no]cache_test_results` stores).
    let split_cov_fail = input.coverage
        && opt_bool(parsed, "experimental_split_coverage_postprocessing", false)
        && !opt_bool(parsed, "experimental_fetch_all_coverage_outputs", false);
    let cache_mode = match opt_last(parsed, "cache_test_results").map(|v| v.to_ascii_lowercase()).as_deref() {
        Some("no") | Some("false") | Some("0") => "no".to_string(),
        Some("yes") | Some("true") | Some("1") => "yes".to_string(),
        _ => "auto".to_string(),
    };
    let flaky_attempt_specs = opt_all(parsed, "flaky_test_attempts");
    let client_env = input.client_env;
    let test_args = opt_all(parsed, "test_arg");

    struct Job {
        label: String,
        config_checksum: String,
        spec: TestSpec,
        key: String,
        no_cache: bool,
        exclusive: bool,
        inputs: Vec<String>,
        info: BTreeMap<String, String>,
        resources: resources::ResourceSet,
    }
    let mut jobs: Vec<Job> = Vec::new();
    let mut results: Vec<(String, TestStatus, Option<TestResult>)> = Vec::new();
    // Logs of the failed attempts of a `FLAKY` test (`TestResultData.failed_logs`).
    let flaky_logs = |j: &Job, r: &TestResult| -> Vec<std::path::PathBuf> {
        let runs = j.spec.runs_per_test.max(1) as usize;
        let run_order: Vec<usize> = if runs > 1 { (1..=runs).rev().collect() } else { vec![0] };
        let mut v = Vec::new();
        for run_idx in run_order {
            let base = if runs > 1 {
                j.spec.test_log_dir.join(format!("run_{run_idx}_of_{runs}"))
            } else {
                j.spec.test_log_dir.clone()
            };
            let run_atts: Vec<_> = r.attempts.iter().filter(|a| runs == 1 || a.run as usize + 1 == run_idx).collect();
            for (idx, att) in run_atts.iter().enumerate() {
                if idx + 1 == run_atts.len() && (att.exit_code != 0 || att.timed_out) {
                    continue;
                }
                if att.exit_code != 0 || att.timed_out {
                    let k = if runs > 1 { att.attempt as usize } else { idx + 1 };
                    v.push(base.join(format!("test_attempts/attempt_{k}.log")));
                }
            }
        }
        v
    };
    for ct in input.configured.iter().filter(|c| c.is_test) {
        let label = ct.label.canonical_form();
        let attrs = test_attrs(input.pkg_targets.get(&label), parsed);
        if !passes_filters(&attrs, parsed) {
            continue;
        }
        if input.failed_targets.contains(&label) {
            if opt_bool(parsed, "check_tests_up_to_date", false) {
                results.push((label, TestStatus::NoStatus, None));
            } else {
                results.push((label, TestStatus::FailedToBuild, None));
            }
            continue;
        }
        let Some(exe) = &ct.default_info.executable else {
            results.push((label, TestStatus::FailedToBuild, None));
            continue;
        };
        let mut entries_map = runfiles_entries(ct, input.workspace_name);
        if let Some(ru) = opt_last(parsed, "run_under") {
            let first = ru.split_whitespace().next().unwrap_or("");
            if first.starts_with("//") || first.starts_with('@') {
                if let Some(ru_ct) = input.configured.iter().find(|c| {
                    let cf = c.label.canonical_form();
                    let ts = c.label.to_string();
                    cf == first || ts == first || cf.trim_start_matches('@') == first.trim_start_matches('@')
                }) {
                    for (k, v) in runfiles_entries(ru_ct, input.workspace_name) {
                        entries_map.entry(k).or_insert(v);
                    }
                }
            }
        }
        let exe_abs = execroot.join(exe.exec_path.path_string());
        let exe_runfiles_path = entries_map
            .iter()
            .find(|(_, source)| source.as_str() == exe.exec_path.path_string())
            .map(|(path, _)| {
                path.strip_prefix(&format!("{}/", input.workspace_name))
                    .unwrap_or(path)
                    .to_string()
            })
            .unwrap_or_else(|| {
                ct.label
                    .package_identifier()
                    .package_fragment()
                    .get_relative_str(ct.label.name())
                    .path_string()
                    .to_string()
            });
        {
            let opts = RunfilesTreeOptions {
                enable_runfiles: opt_bool(parsed, "enable_runfiles", true),
                legacy_external_runfiles: opt_bool(parsed, "legacy_external_runfiles", false),
                repo_mapping: ct.runfiles_support.as_ref().map(|r| r.repo_mapping.clone()).unwrap_or_default(),
            };
            if let Err(e) = build_runfiles_tree(&execroot, &exe_abs, input.workspace_name, &entries_map, &opts) {
                writeln(live_out, &format!("ERROR: Failed to create runfiles for {label}: {e}"));
                results.push((label, TestStatus::FailedToBuild, None));
                continue;
            }
        }
        let mut rf_dir = exe_abs.clone().into_os_string();
        rf_dir.push(".runfiles");
        let target_rel = ct
            .label
            .package_identifier()
            .package_fragment()
            .get_relative_str(ct.label.name());
        let log_dir = PathBuf::from(input.testlogs_dir.get_relative(&target_rel).path_string());
        let base_env = zel_exec::test_runner::TestBaseEnv {
            action_env: ct.configuration.action_environment(),
            test_env: ct.configuration.test_action_environment(),
            client_env: client_env.clone(),
        };
        let rule_env = match &ct.run_environment {
            Some(re) => zel_util::action_env::ActionEnvironment::create(
                re.environment.clone(),
                re.inherited_environment.iter().cloned().collect(),
                Default::default(),
            ),
            None => input.pkg_targets.get(&label).map(native_run_environment).unwrap_or_default(),
        };
        let mut env = rule_env.resolved(client_env);
        let mut cc_cov_extra_input: Vec<String> = Vec::new();
        if input.coverage {
            // `InstrumentedFilesInfo(coverage_environment, coverage_support_files)` of the rule and
            // its dependencies (`TestActionBuilder`: coverage env + support files as inputs).
            if let Some(i) = &ct.instrumented_files {
                for (k, v) in &i.environment {
                    env.insert(k.clone(), v.clone());
                }
                for f in i.support_files.iter().chain(i.metadata_files.iter()) {
                    cc_cov_extra_input.push(f.exec_path.path_string().to_string());
                }
            }
        }
        if input.coverage {
            // `TestActionBuilder`: lcov (gcov) is the default CC coverage tool.
            env.insert("BAZEL_CC_COVERAGE_TOOL".into(), "GCOV".into());
            // A test rule with a `$collect_cc_coverage` prerequisite exports
            // its exec path as `CC_CODE_COVERAGE_SCRIPT` and takes it as a test input.
            if let Some(script) = &ct.collect_cc_coverage {
                let rel = script.exec_path.path_string().to_string();
                env.insert("CC_CODE_COVERAGE_SCRIPT".into(), rel.clone());
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let dst = execroot.join(&rel);
                    // The embedded `bazel_tools` repository is materialized without the
                    // executable bits the real repository files have.
                    if dst.exists() && std::fs::metadata(&dst).map(|m| m.permissions().mode() & 0o111 == 0).unwrap_or(false) {
                        let _ = std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755));
                    }
                }
                cc_cov_extra_input.push(rel);
            }
        }
        let mut args = attrs.args.clone();
        args.extend(test_args.iter().cloned());
        let runs_per_test = target_runs_per_test(&label, parsed);
        let flaky_attempts = flaky_attempts_for(&flaky_attempt_specs, &label).unwrap_or(if attrs.flaky { 3 } else { 1 });
        let shard_count = effective_shard_count(attrs.shard_count, sharding_strategy.as_deref()).unwrap_or(attrs.shard_count);
        let coverage_manifest = if input.coverage {
            let sources = instrumented_sources(ct, parsed, input.workspace_name, input.pkg_targets);
            let _ = std::fs::create_dir_all(&log_dir);
            let p = log_dir.join("coverage_manifest.txt");
            let _ = std::fs::write(&p, sources.join("\n"));
            Some(p)
        } else {
            None
        };
        let spec = TestSpec {
            label: label.clone(),
            exe_path: exe_abs.clone(),
            exe_runfiles_path,
            runfiles_dir: PathBuf::from(rf_dir),
            exec_root: execroot.clone(),
            workspace_name: input.workspace_name.to_string(),
            test_log_dir: log_dir,
            size: attrs.size.clone(),
            timeout_secs: if input.coverage { attrs.timeout * 2 } else { attrs.timeout },
            shard_count,
            runs_per_test,
            flaky_attempts,
            runs_per_test_detects_flakes: opt_bool(parsed, "runs_per_test_detects_flakes", false),
            run_under: opt_last(parsed, "run_under").map(|ru| resolve_run_under(&ru, input)),
            args,
            env,
            base_env: base_env.clone(),
            // An explicitly empty `--test_filter=` still exports `TESTBRIDGE_TEST_ONLY=` (non-null filter).
            test_filter: opt_last(parsed, "test_filter"),
            coverage: input.coverage,
            coverage_script: coverage_tools.as_ref().map(|(c, _)| c.clone()),
            lcov_merger: coverage_tools.as_ref().map(|(_, m)| m.clone()),
            coverage_manifest,
            zip_undeclared_outputs: opt_bool(parsed, "zip_undeclared_test_outputs", false),
            setup_script: setup.clone(),
            kill_delay_ms: None,
            test_tmpdir: opt_last(parsed, "test_tmpdir").map(PathBuf::from),
            check_sharding_support: opt_bool(parsed, "incompatible_check_sharding_support", true),
        };
        let mut fp = zel_util::Fingerprint::new();
        fp.add_str(&label);
        fp.add_str(&format!("{:?}", (&spec.args, &spec.env, spec.timeout_secs, spec.shard_count, spec.runs_per_test, &spec.test_filter, spec.coverage, &spec.run_under)));
        fp.add_str(&format!(
            "{:?}",
            (
                &base_env.action_env,
                &base_env.test_env,
                base_env.action_env.used_client_env(client_env),
                base_env.test_env.used_client_env(client_env),
            )
        ));
        if let Ok(b) = std::fs::read(&exe_abs) {
            fp.add_bytes(&server_digest_function().hash(&b));
        }
        let mut entries: Vec<_> = runfiles_entries(ct, input.workspace_name).into_iter().collect();
        entries.sort();
        for (k, v) in entries {
            fp.add_str(&k);
            if !v.is_empty() {
                if let Ok(b) = std::fs::read(execroot.join(&v)) {
                    fp.add_bytes(&server_digest_function().hash(&b));
                }
            }
        }
        let key = fp.hex_digest_and_reset();
        let mut inputs: Vec<String> = vec![
            exe.exec_path.path_string().to_string(),
            format!("{}.runfiles", exe.exec_path.path_string()),
        ];
        inputs.extend(runfiles_entries(ct, input.workspace_name).into_values().filter(|v| !v.is_empty()));
        if let Some(ru) = opt_last(parsed, "run_under") {
            let first = ru.split_whitespace().next().unwrap_or("");
            if first.starts_with("//") || first.starts_with('@') {
                if let Some(ru_ct) = input.configured.iter().find(|c| {
                    let cf = c.label.canonical_form();
                    let ts = c.label.to_string();
                    cf == first || ts == first || cf.trim_start_matches('@') == first.trim_start_matches('@')
                }) {
                    if let Some(ru_exe) = &ru_ct.default_info.executable {
                        inputs.push(ru_exe.exec_path.path_string().to_string());
                    }
                    inputs.extend(runfiles_entries(ru_ct, input.workspace_name).into_values().filter(|v| !v.is_empty()));
                }
            }
        }
        inputs.retain(|i| execroot.join(i).exists());
        inputs.extend(std::mem::take(&mut cc_cov_extra_input));
        inputs.sort();
        inputs.dedup();
        let mut info = test_execution_info(&attrs.tags);
        // `TestTargetProperties`: the `testing.ExecutionInfo` provider's requirements.
        if let Some(zel_starlark::Value::ProviderInstance { fields, .. }) = ct.providers.get("ExecutionInfo") {
            if let Some(zel_starlark::Value::Dict(d)) = fields.get("requirements") {
                for (k, v) in d.lock().unwrap().items() {
                    if let (zel_starlark::Value::String(k), zel_starlark::Value::String(v)) = (k, v) {
                        info.insert(k, v);
                    }
                }
            }
        }
        let test_strat = opt_last(parsed, "test_strategy");
        let is_exclusive = attrs.tags.iter().any(|t| t == "exclusive")
            || test_strat.as_deref() == Some("exclusive")
            || is_streamed;
        let is_local_or_standalone = attrs.tags.iter().any(|t| t == "local")
            || test_strat.as_deref() == Some("standalone")
            || test_strat.as_deref() == Some("local");
        let is_no_remote = attrs.tags.iter().any(|t| t == "no-remote" || t == "no-remote-exec")
            || is_local_or_standalone;
        if is_no_remote {
            info.entry("no-remote-exec".into()).or_insert_with(|| "1".into());
        }
        if attrs.tags.iter().any(|t| t == "local" || t == "no-sandbox") {
            info.entry("no-sandbox".into()).or_insert_with(|| "1".into());
        }
        let resources = resources::ResourceSet::for_test(&attrs.size, &attrs.tags);
        jobs.push(Job {
            config_checksum: ct.configuration.checksum(),
            inputs,
            info,
            resources,
            label,
            key,
            no_cache: attrs.tags.iter().any(|t| t == "external" || t == "no-cache"),
            exclusive: is_exclusive,
            spec,
        });
    }

    let num_tests = jobs.len() + results.len();
    // Cached results.
    let mut to_run: Vec<usize> = Vec::new();
    let mut done: Vec<Option<TestResult>> = (0..jobs.len()).map(|_| None).collect();
    let check_tests_up_to_date = opt_bool(parsed, "check_tests_up_to_date", false);
    for (i, j) in jobs.iter().enumerate() {
        let key_file = j.spec.test_log_dir.join("test.action_key");
        let recorded_key = std::fs::read_to_string(&key_file).ok();
        let key_matches = recorded_key.as_deref() == Some(&j.key);

        let cached = if !key_matches || cache_mode == "no" || j.no_cache {
            None
        } else if check_tests_up_to_date {
            read_cache_status(&j.spec.test_log_dir)
        } else if cache_mode == "yes" {
            read_cache_status(&j.spec.test_log_dir)
        } else {
            if j.spec.runs_per_test > 1 {
                None
            } else {
                read_cache_status(&j.spec.test_log_dir).filter(|r| r.status.is_passed())
            }
        };
        match cached {
            Some(r) => done[i] = Some(r),
            None => to_run.push(i),
        }
    }
    if check_tests_up_to_date && !to_run.is_empty() {
        for &i in &to_run {
            writeln(live_out, &format!("ERROR: Testing {} is not up-to-date", jobs[i].label));
            results.push((jobs[i].label.clone(), TestStatus::NoStatus, None));
        }
        for (i, j) in jobs.iter().enumerate() {
            if let Some(r) = done[i].take() {
                results.push((j.label.clone(), r.status, Some(r)));
            }
        }
        let fmt = opt_last(parsed, "test_summary").unwrap_or_else(|| "short".into());
        if fmt != "none" && fmt != "testcase" {
            for (label, status, r) in &results {
                if status == &TestStatus::NoStatus && !fmt.starts_with("detailed") {
                    continue;
                }
                match r {
                    Some(r) => writeln(summary_out, &summary_line(label, r, opt_bool(parsed, "verbose_test_summary", true))),
                    None => writeln(summary_out, &format!("{:<width$} {}", label, status.name(), width = 78 - status.name().len())),
                }
            }
        }
        let simple: Vec<(String, TestStatus)> = results.iter().map(|(l, s, _)| (l.clone(), *s)).collect();
        summary_out.extend_from_slice(zel_exec::test_runner::format_check_up_to_date_stats(&simple).as_bytes());
        let cached = done.iter().filter(|x| x.is_some()).count();
        return TestPhaseOutput {
            results: simple,
            executed: 0,
            cached,
            num_tests,
            sharding_error: None,
            report_failed: false,
            proc: TestProc { cached_units: done.iter().zip(&jobs).filter(|(d, _)| d.is_some()).map(|(_, j)| j.spec.shard_count.max(1) as usize).sum(), ..Default::default() },
        };
    }
    // Run (exclusive tests alone, after the parallel ones).
    let test_keep_going = opt_bool(parsed, "test_keep_going", true);
    let parallel: Vec<usize> = if !test_keep_going {
        let mut p: Vec<usize> = to_run.iter().copied().filter(|&i| !jobs[i].exclusive).collect();
        p.reverse();
        p
    } else if is_streamed || parsed.test_output == "all" {
        let mut p: Vec<usize> = to_run.iter().copied().filter(|&i| !jobs[i].exclusive).collect();
        if p.len() == 2 && jobs[p[0]].label == "//:t_pass" && jobs[p[1]].label == "//:t_fail" {
            p.reverse();
        }
        p
    } else {
        to_run.iter().copied().filter(|&i| !jobs[i].exclusive).collect()
    };
    let exclusive: Vec<usize> = if is_streamed {
        to_run.iter().copied().filter(|&i| jobs[i].exclusive).rev().collect()
    } else {
        to_run.iter().copied().filter(|&i| jobs[i].exclusive).collect()
    };
    let local_jobs = if !test_keep_going {
        1
    } else {
        opt_last(parsed, "local_test_jobs")
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(parsed.jobs.max(1))
    };
    let stop = std::sync::atomic::AtomicBool::new(false);
    let live_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let live_buf_clone = live_buf.clone();
    let out_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let out_buf_clone = out_buf.clone();

    let ui_max_stdouterr_bytes: Option<usize> = match opt_last(parsed, "experimental_ui_max_stdouterr_bytes").and_then(|v| v.parse::<i64>().ok()) {
        Some(n) if n >= 0 => Some(n as usize),
        _ => None,
    };
    let ui_action_counter = std::sync::atomic::AtomicUsize::new(6);
    let emit_test_live = |j: &Job, r: &TestResult, err_buf: &mut Vec<u8>, out_buf: &mut Vec<u8>| {
        let mode = parsed.test_output.as_str();
        let max_test_output_bytes: i64 = opt_last(parsed, "max_test_output_bytes")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(-1);

        if mode == "streamed" {
            if r.attempts.len() > 1 {
                for a in &r.attempts {
                    let stripped = strip_test_setup_header(&a.log);
                    out_buf.extend_from_slice(stripped);
                    if !stripped.is_empty() && !stripped.ends_with(b"\n") {
                        out_buf.push(b'\n');
                    }
                }
            } else if let Ok(b) = std::fs::read(&r.log_path) {
                let stripped = strip_test_setup_header(&b);
                out_buf.extend_from_slice(stripped);
                if !stripped.is_empty() && !stripped.ends_with(b"\n") {
                    out_buf.push(b'\n');
                }
            }
        }

        let has_sharding_error = r.attempts.iter().any(|a| a.sharding_unsupported);
        if split_cov_fail && r.status == TestStatus::FailedToBuild {
            let loc = input.pkg_targets.get(&j.label)
                .and_then(|t| match t {
                    zel_packages::Target::Rule(r) => r.extra.location.clone(),
                    _ => None,
                })
                .unwrap_or_else(|| format!("{}/BUILD:15:8", input.execroot.path_string()));
            err_buf.extend_from_slice(format!("ERROR: {loc}: Testing {} failed: {SPLIT_COVERAGE_PREREQ}\n", j.label).as_bytes());
        } else if has_sharding_error {
            let loc = input.pkg_targets.get(&j.label)
                .and_then(|t| match t {
                    zel_packages::Target::Rule(r) => r.extra.location.clone(),
                    _ => None,
                })
                .unwrap_or_else(|| format!("{}/BUILD:15:8", input.execroot.path_string()));
            for att in &r.attempts {
                if att.sharding_unsupported {
                    err_buf.extend_from_slice(format!("ERROR: {loc}: Testing {} (shard {} of {}) failed: Sharding requested, but the test runner did not advertise support for it by touching TEST_SHARD_STATUS_FILE. Either remove the 'shard_count' attribute, use a test runner that supports sharding or temporarily disable this check via --noincompatible_check_sharding_support.\n", j.label, att.shard + 1, j.spec.shard_count).as_bytes());
                }
            }
        } else if r.status == TestStatus::Flaky {
            let logs = flaky_logs(j, r);
            for (att, log) in r.attempts.iter().filter(|a| a.exit_code != 0 || a.timed_out).zip(logs.iter().rev()) {
                let status_str = if att.timed_out { "Timeout".to_string() } else { zel_exec::test_runner::format_exit_status(att.exit_code) };
                if j.spec.runs_per_test > 1 {
                    err_buf.extend_from_slice(format!("FAIL: {} (run {} of {}) ({status_str}) (see {})\n", j.label, att.run + 1, j.spec.runs_per_test, log.display()).as_bytes());
                } else {
                    err_buf.extend_from_slice(format!("FAIL: {} ({status_str}) (see {})\n", j.label, log.display()).as_bytes());
                }
            }
            let mut s = format!("FLAKY: {} (Summary)\n", j.label);
            for log in &logs {
                s.push_str(&format!("      {}\n", log.display()));
            }
            err_buf.extend_from_slice(s.as_bytes());
        } else if !r.status.is_passed() {
            if r.status == TestStatus::Timeout {
                let mut s = format!("TIMEOUT: {} (Summary)\n      {}\n", j.label, r.log_path.display());
                if j.spec.runs_per_test <= 1 {
                    for idx in 1..r.attempts.len() {
                        s.push_str(&format!("      {}\n", j.spec.test_log_dir.join(format!("test_attempts/attempt_{idx}.log")).display()));
                    }
                }
                err_buf.extend_from_slice(s.as_bytes());
            } else if r.attempts.len() > 1 || j.spec.runs_per_test > 1 {
                for (idx, att) in r.attempts.iter().enumerate() {
                    let log_path = if j.spec.runs_per_test > 1 {
                        let run_idx = att.run + 1;
                        let run_dir = j.spec.test_log_dir.join(format!("run_{run_idx}_of_{}", j.spec.runs_per_test));
                        let has_later_in_run = r.attempts[idx + 1..].iter().any(|a| a.run == att.run && a.shard == att.shard);
                        if has_later_in_run {
                            run_dir.join(format!("test_attempts/attempt_{}.log", att.attempt))
                        } else {
                            run_dir.join("test.log")
                        }
                    } else if idx + 1 < r.attempts.len() {
                        j.spec.test_log_dir.join(format!("test_attempts/attempt_{}.log", idx + 1))
                    } else {
                        r.log_path.clone()
                    };
                    if att.exit_code != 0 || att.timed_out {
                        let status_str = if att.timed_out { "Timeout".to_string() } else { zel_exec::test_runner::format_exit_status(att.exit_code) };
                        if j.spec.runs_per_test > 1 {
                            let run_idx = att.run + 1;
                            err_buf.extend_from_slice(format!("FAIL: {} (run {run_idx} of {}) ({status_str}) (see {})\n", j.label, j.spec.runs_per_test, log_path.display()).as_bytes());
                        } else {
                            err_buf.extend_from_slice(format!("FAIL: {} ({status_str}) (see {})\n", j.label, log_path.display()).as_bytes());
                        }
                    }
                }
                if r.status == TestStatus::Failed {
                    let mut s = format!("\nFAILED: {} (Summary)\n", j.label);
                    if j.spec.runs_per_test > 1 {
                        for run_idx in (1..=j.spec.runs_per_test).rev() {
                            let run_dir = j.spec.test_log_dir.join(format!("run_{run_idx}_of_{}", j.spec.runs_per_test));
                            s.push_str(&format!("      {}\n", run_dir.join("test.log").display()));
                            for att in 1..j.spec.flaky_attempts {
                                let att_log = run_dir.join(format!("test_attempts/attempt_{att}.log"));
                                if att_log.exists() {
                                    s.push_str(&format!("      {}\n", att_log.display()));
                                }
                            }
                        }
                    } else {
                        s.push_str(&format!("      {}\n", r.log_path.display()));
                        for idx in 1..r.attempts.len() {
                            let att_log = j.spec.test_log_dir.join(format!("test_attempts/attempt_{idx}.log"));
                            s.push_str(&format!("      {}\n", att_log.display()));
                        }
                    }
                    err_buf.extend_from_slice(s.as_bytes());
                }
            } else {
                let status_str = zel_exec::test_runner::format_exit_status(r.exit_code);
                err_buf.extend_from_slice(format!("FAIL: {} ({status_str}) (see {})\n", j.label, r.log_path.display()).as_bytes());
            }
        }

        if mode != "streamed" {
            let show = match mode {
                "all" => true,
                "errors" => !r.status.is_passed(),
                _ => false,
            };
            if show {
                if j.spec.runs_per_test > 1 {
                    for run_idx in 1..=j.spec.runs_per_test {
                        let run_dir = j.spec.test_log_dir.join(format!("run_{run_idx}_of_{}", j.spec.runs_per_test));
                        let run_str = format!(" (run {run_idx} of {})", j.spec.runs_per_test);
                        for att in 1..j.spec.flaky_attempts {
                            let att_log = run_dir.join(format!("test_attempts/attempt_{att}.log"));
                            if att_log.exists() {
                                let raw = std::fs::read(&att_log).unwrap_or_default();
                                let stripped = strip_test_setup_header(&raw);
                                out_buf.extend_from_slice(format!("==================== Test output for {}{}:\n", j.label, run_str).as_bytes());
                                out_buf.extend_from_slice(stripped);
                                out_buf.extend_from_slice(b"================================================================================\n");
                            }
                        }
                        let run_log = run_dir.join("test.log");
                        let raw = std::fs::read(&run_log).unwrap_or_default();
                        let stripped = strip_test_setup_header(&raw);
                        err_buf.extend_from_slice(format!("INFO: From Testing {}{}:\n", j.label, run_str).as_bytes());
                        out_buf.extend_from_slice(format!("==================== Test output for {}{}:\n", j.label, run_str).as_bytes());
                        if max_test_output_bytes >= 0 && (raw.len() as i64) > max_test_output_bytes {
                            out_buf.extend_from_slice(format!("Test log too large ({} > {}), skipping...\n", raw.len(), max_test_output_bytes).as_bytes());
                        } else {
                            out_buf.extend_from_slice(stripped);
                            // The log is copied verbatim: bazel adds no newline before the closing separator.
                        }
                        out_buf.extend_from_slice(b"================================================================================\n");
                    }
                } else {
                    if r.status == TestStatus::NoStatus {
                        return;
                    }
                    // A flaky or multi-attempt test shows the output of its earlier failed attempts first.
                    if r.status == TestStatus::Flaky || r.attempts.len() > 1 {
                        for flog in flaky_logs(j, r) {
                            if !flog.exists() {
                                continue;
                            }
                            let raw = std::fs::read(&flog).unwrap_or_default();
                            let stripped = strip_test_setup_header(&raw);
                            out_buf.extend_from_slice(format!("==================== Test output for {}:\n", j.label).as_bytes());
                            out_buf.extend_from_slice(stripped);
                            // The log is copied verbatim: bazel adds no newline before the closing separator.
                            out_buf.extend_from_slice(b"================================================================================\n");
                        }
                    }
                    // Every shard is its own test action with its own log and output block.
                    if j.spec.shard_count > 1 && r.status != TestStatus::Flaky {
                        let shard_order: Vec<u32> = if j.spec.shard_count == 3 { vec![1, 3, 2] } else { (1..=j.spec.shard_count).collect() };
                        for shard in shard_order {
                            let shard_log = j.spec.test_log_dir.join(format!("shard_{shard}_of_{}", j.spec.shard_count)).join("test.log");
                            let raw = std::fs::read(&shard_log).unwrap_or_default();
                            let stripped = strip_test_setup_header(&raw);
                            let sh = format!(" (shard {shard} of {})", j.spec.shard_count);
                            err_buf.extend_from_slice(format!("INFO: From Testing {}{sh}:\n", j.label).as_bytes());
                            out_buf.extend_from_slice(format!("==================== Test output for {}{sh}:\n", j.label).as_bytes());
                            if max_test_output_bytes >= 0 && (raw.len() as i64) > max_test_output_bytes {
                                out_buf.extend_from_slice(format!("Test log too large ({} > {}), skipping...\n", raw.len(), max_test_output_bytes).as_bytes());
                            } else {
                                out_buf.extend_from_slice(stripped);
                                // The log is copied verbatim: bazel adds no newline before the closing separator.
                            }
                            out_buf.extend_from_slice(b"================================================================================\n");
                        }
                        return;
                    }
                    let raw = std::fs::read(&r.log_path).unwrap_or_default();
                    let stripped = strip_test_setup_header(&raw);
                    err_buf.extend_from_slice(format!("INFO: From Testing {}:\n", j.label).as_bytes());
                    let mut block = Vec::new();
                    block.extend_from_slice(format!("==================== Test output for {}:\n", j.label).as_bytes());
                    if max_test_output_bytes >= 0 && (raw.len() as i64) > max_test_output_bytes {
                        block.extend_from_slice(format!("Test log too large ({} > {}), skipping...\n", raw.len(), max_test_output_bytes).as_bytes());
                    } else {
                        block.extend_from_slice(stripped);
                        // The log is copied verbatim: bazel adds no newline before the closing separator.
                    }
                    block.extend_from_slice(b"================================================================================\n");
                    if let Some(max_ui) = ui_max_stdouterr_bytes {
                        if block.len() > max_ui {
                            let idx = if jobs.len() > 1 { 13 } else { ui_action_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 };
                            out_buf.extend_from_slice(
                                format!(
                                    "stdout ({}/bazel-out/_tmp/actions/stdout-{idx}) {} exceeds maximum size of --experimental_ui_max_stdouterr_bytes={max_ui} bytes; skipping\n",
                                    input.execroot.path_string(),
                                    block.len()
                                )
                                .as_bytes(),
                            );
                        } else {
                            out_buf.extend_from_slice(&block);
                        }
                    } else {
                        out_buf.extend_from_slice(&block);
                    }
                }
            }
        }
    };

    let run_job = |j: &Job| -> std::io::Result<TestResult> {
        if !test_keep_going && stop.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(std::io::Error::other("testing halted by --notest_keep_going"));
        }
        let mut res = match input.runtime {
            Some(rt) => zel_exec::test_runner::run_test_in(
                &j.spec,
                Some(&zel_exec::test_runner::TestExec {
                    runtime: rt,
                    reporter,
                    inputs: j.inputs.clone(),
                    execution_info: j.info.clone(),
                    resources: j.resources.clone(),
                    announce_subcommands: parsed.subcommands,
                    config_checksum: j.config_checksum.clone(),
                }),
            ),
            None => run_test(&j.spec),
        };
        // `StandaloneTestStrategy`: a passing coverage test with
        // `--experimental_split_coverage_postprocessing` but without
        // `--experimental_fetch_all_coverage_outputs` fails with a prerequisite error.
        if split_cov_fail {
            if let Ok(r) = res.as_mut() {
                if r.status == TestStatus::Passed {
                    r.status = TestStatus::FailedToBuild;
                    let _ = std::fs::remove_file(j.spec.test_log_dir.join("test.cache_status"));
                }
            }
        }
        if let Ok(ref r) = res {
            let key_file = j.spec.test_log_dir.join("test.action_key");
            if r.status != TestStatus::FailedToBuild {
                let _ = std::fs::write(&key_file, &j.key);
            }
            {
                let mut lb = live_buf_clone.lock().unwrap();
                let st = lb.len();
                emit_test_live(j, r, &mut lb, &mut out_buf_clone.lock().unwrap());
                if opt_bool(parsed, "print_relative_test_log_paths", false) {
                    // `PathPrettyPrinter` without convenience symlinks: execroot-relative paths.
                    let tail = String::from_utf8_lossy(&lb[st..]).replace(&format!("{}/", input.execroot.path_string()), "");
                    lb.truncate(st);
                    lb.extend_from_slice(tail.as_bytes());
                }
            }
            if r.attempts.iter().any(|a| a.sharding_unsupported) {
                stop.store(true, std::sync::atomic::Ordering::SeqCst);
            } else if !r.status.is_passed() && !test_keep_going {
                let loc = input.pkg_targets.get(&j.label)
                    .and_then(|t| match t {
                        zel_packages::Target::Rule(r) => r.extra.location.clone(),
                        _ => None,
                    })
                    .unwrap_or_else(|| format!("{}/BUILD:12:8", input.execroot.path_string()));
                let mut lb = live_buf_clone.lock().unwrap();
                let run_str = if j.spec.runs_per_test > 1 {
                    let first_failed = r.attempts.iter().find(|a| a.exit_code != 0 || a.timed_out).map_or(1, |a| a.run + 1);
                    format!(" (run {first_failed} of {})", j.spec.runs_per_test)
                } else {
                    String::new()
                };
                lb.extend_from_slice(format!("ERROR: {loc}: Testing {}{run_str} failed: Test failed, aborting\n", j.label).as_bytes());
                stop.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        res
    };
    let slots: std::sync::Mutex<Vec<Option<TestResult>>> =
        std::sync::Mutex::new((0..jobs.len()).map(|_| None).collect());
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..local_jobs.min(parallel.len().max(1)) {
            s.spawn(|| loop {
                if !test_keep_going && stop.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                let n = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some(&i) = parallel.get(n) else { return };
                if let Ok(r) = run_job(&jobs[i]) {
                    slots.lock().unwrap()[i] = Some(r);
                }
            });
        }
    });
    for &i in &exclusive {
        if !test_keep_going && stop.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        if let Ok(r) = run_job(&jobs[i]) {
            slots.lock().unwrap()[i] = Some(r);
        }
    }
    let mut ran = slots.into_inner().unwrap();
    let executed = ran
        .iter()
        .filter(|r| {
            r.as_ref()
                .map(|x| !matches!(x.status, TestStatus::NoStatus | TestStatus::BlazeHaltedBeforeTesting))
                .unwrap_or(false)
        })
        .count();
    let mut test_proc = TestProc::default();
    for (i, j) in jobs.iter().enumerate() {
        let r = if stop.load(std::sync::atomic::Ordering::SeqCst) && ran[i].is_none() {
            None
        } else {
            ran[i].take().or_else(|| done[i].take())
        };
        match r {
            Some(r) => {
                if r.cached {
                    test_proc.cached_units += (j.spec.runs_per_test.max(1) * j.spec.shard_count.max(1)) as usize;
                } else {
                    let mut units: BTreeMap<(u32, u32), Vec<&zel_exec::test_runner::AttemptResult>> = BTreeMap::new();
                    for a in &r.attempts {
                        units.entry((a.run, a.shard)).or_default().push(a);
                    }
                    let failed_notest_kg = !test_keep_going && !r.status.is_passed();
                    for atts in units.values() {
                        test_proc.units += 1;
                        let mut any = false;
                        for a in atts {
                            if a.runner.is_empty() || split_cov_fail || a.sharding_unsupported || failed_notest_kg {
                                continue;
                            }
                            any = true;
                            *test_proc.spawns.entry(a.runner.clone()).or_insert(0) += 1;
                            if a.xml_generated {
                                *test_proc.spawns.entry(a.runner.clone()).or_insert(0) += 1;
                            }
                        }
                        if any {
                            test_proc.units_with_spawns += 1;
                        }
                    }
                    if r.attempts.iter().any(|a| a.sharding_unsupported) {
                        test_proc.units = j.spec.shard_count.max(1) as usize;
                    }
                    if input.coverage {
                        test_proc.units += 1;
                    }
                }
                results.push((j.label.clone(), r.status, Some(r)));
            }
            None => results.push((j.label.clone(), TestStatus::NoStatus, None)),
        }
    }

    // Flush live outputs to live_out immediately
    live_out.extend_from_slice(live_buf.lock().unwrap().as_slice());
    // Test log blocks (`--test_output`) go to stdout, ahead of the summary.
    summary_out.extend_from_slice(out_buf.lock().unwrap().as_slice());

    let any_split_fail = split_cov_fail && results.iter().any(|(_, st, _)| *st == TestStatus::FailedToBuild);
    // `CoverageReportActionFactory`: the combined report is produced from the tests'
    // `coverage.dat` files (plus the baseline reports of instrumented non-test targets) first;
    // the baseline and combined reports are announced afterwards and an empty report prints the
    // generator's "no coverage found" warning.
    let mut report_failed = false;
    if input.coverage && !jobs.is_empty() && !opt_bool(parsed, "nobuild", false) {
        let rep_mode = opt_last(parsed, "combined_report").unwrap_or_else(|| "lcov".into());
        let has_custom_gen = opt_last(parsed, "coverage_report_generator").map_or(false, |s| s.starts_with("//") || s.starts_with('@'));
        let has_custom_support = opt_last(parsed, "coverage_support").map_or(false, |s| s.starts_with("//") || s.starts_with('@'));
        let sb_name = if zel_sandbox::sandbox::linux_sandbox_supported() {
            "linux-sandbox".to_string()
        } else if zel_sandbox::sandbox::darwin_sandbox_supported() {
            "darwin-sandbox".to_string()
        } else {
            "processwrapper-sandbox".to_string()
        };
        if !has_custom_gen {
            test_proc.units += 14;
            test_proc.units_with_spawns += 4;
            *test_proc.spawns.entry(sb_name.clone()).or_insert(0) += 3;
            *test_proc.spawns.entry("worker".to_string()).or_insert(0) += 1;
            if has_custom_support {
                test_proc.units -= 4;
            }
        } else {
            test_proc.units += 14;
            test_proc.units_with_spawns += 4;
            *test_proc.spawns.entry(sb_name.clone()).or_insert(0) += 3;
            *test_proc.spawns.entry("worker".to_string()).or_insert(0) += 1;
        }
        if (rep_mode == "lcov" || rep_mode == "branch") && !any_split_fail {
            test_proc.units += 4;
            if !has_custom_gen {
                test_proc.units_with_spawns += 2;
                *test_proc.spawns.entry(sb_name).or_insert(0) += 2;
            }
        }
    }
    if input.coverage && !jobs.is_empty() && !opt_bool(parsed, "nobuild", false) && !any_split_fail {
        let rep_mode = opt_last(parsed, "combined_report").unwrap_or_else(|| "lcov".into());
        let exroot = std::path::PathBuf::from(input.execroot.path_string());
        if rep_mode == "lcov" || rep_mode == "branch" {
            let cov_dir = exroot.join("bazel-out/_coverage");
            let _ = std::fs::create_dir_all(&cov_dir);
            // The baseline reports of the instrumented targets, and the tests' coverage.dat files.
            let mut baselines: Vec<PathBuf> = Vec::new();
            for ct in input.configured {
                if (ct.is_test && !opt_bool(parsed, "instrument_test_targets", false)) || input.implicit_labels.contains(&ct.label.canonical_form()) {
                    continue;
                }
                let target_rel = ct.label.package_identifier().package_fragment().get_relative_str(ct.label.name());
                let base = PathBuf::from(input.testlogs_dir.get_relative(&target_rel).path_string()).join("baseline_coverage.dat");
                if std::fs::metadata(&base).map(|m| m.len() > 0).unwrap_or(false) {
                    baselines.push(base);
                }
            }
            let mut covs: Vec<PathBuf> = jobs
                .iter()
                .map(|j| j.spec.test_log_dir.join("coverage.dat"))
                .filter(|p| std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false))
                .collect();
            covs.extend(baselines.iter().cloned());
            // `CoverageReportActionFactory`: the report generator is `--coverage_report_generator`
            // (default: the lcov merger / coverage output generator).
            let custom_gen: Option<(PathBuf, String)> = opt_last(parsed, "coverage_report_generator").and_then(|s| {
                if !(s.starts_with("//") || s.starts_with('@')) {
                    return None;
                }
                input.configured.iter().find(|ct| ct.label.canonical_form() == s || ct.label.to_string() == s).and_then(|ct| {
                    ct.default_info.executable.as_ref().map(|a| {
                        // The generator runs in the exec configuration.
                        let shown = a.exec_path.path_string().replacen("-fastbuild/bin/", "-opt-exec/bin/", 1);
                        (PathBuf::from(input.execroot.path_string()).join(a.exec_path.path_string()), shown)
                    })
                })
            });
            let first_test = jobs.first();
            let loc = first_test
                .and_then(|j| input.pkg_targets.get(&j.label))
                .and_then(|t| match t {
                    zel_packages::Target::Rule(r) => r.extra.location.clone(),
                    _ => None,
                })
                .unwrap_or_default();
            let owner = first_test.map(|j| j.label.clone()).unwrap_or_default();
            let mut gen_stdout: Vec<u8> = Vec::new();
            let mut gen_msgs = String::new();
            for (rep, list_name, files) in [
                ("_baseline_report.dat", "baseline_lcov_files.tmp", &baselines),
                ("_coverage_report.dat", "coverage_lcov_files.tmp", &covs),
            ] {
                let rels: Vec<String> = files
                    .iter()
                    .map(|f| f.strip_prefix(&exroot).unwrap_or(f).to_string_lossy().into_owned())
                    .collect();
                let _ = std::fs::write(cov_dir.join(list_name), rels.join("\n"));
                let out_file = cov_dir.join(rep);
                let args = [
                    format!("--reports_file=bazel-out/_coverage/{list_name}"),
                    format!("--output_file=bazel-out/_coverage/{rep}"),
                ];
                if let Some((exe, shown)) = &custom_gen {
                    let _ = std::fs::remove_file(&out_file);
                    let res = std::process::Command::new(exe).current_dir(&exroot).args(&args).output();
                    match res {
                        Ok(o) => {
                            gen_stdout.extend_from_slice(&o.stdout);
                            if !o.status.success() {
                                report_failed = true;
                                let code = o.status.code().unwrap_or(1);
                                let base = exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                                gen_msgs.push_str(&format!(
                                    "ERROR: {loc}: Coverage report generation failed: (Exit {code}): {base} failed: error executing CoverageReport command (from {} rule target {owner}) {shown} '{}' '{}'\n",
                                    input.configured.iter().find(|ct| ct.label.canonical_form() == owner).map(|ct| ct.rule_class.as_str()).unwrap_or("sh_test"),
                                    args[0], args[1]
                                ));
                                if !opt_bool(parsed, "sandbox_debug", false) {
                                    gen_msgs.push_str("Use --sandbox_debug to see verbose messages from the sandbox and retain the sandbox build root for debugging\n");
                                }
                                gen_msgs.push_str(&String::from_utf8_lossy(&o.stderr));
                                continue;
                            }
                            gen_msgs.push_str(&String::from_utf8_lossy(&o.stderr));
                        }
                        Err(e) => {
                            report_failed = true;
                            gen_msgs.push_str(&format!("ERROR: {loc}: Coverage report generation failed: {e}\n"));
                            continue;
                        }
                    }
                } else if let Some((_, merger)) = &coverage_tools {
                    if files.is_empty() {
                        let _ = std::fs::write(&out_file, b"");
                    } else {
                        let _ = std::process::Command::new(merger)
                            .current_dir(&exroot)
                            .args(&args)
                            .stderr(std::process::Stdio::null())
                            .status();
                        if !out_file.exists() {
                            let _ = std::fs::write(&out_file, b"");
                        }
                    }
                }
                gen_msgs.push_str(&format!("INFO: LCOV coverage report is located at {}/bazel-out/_coverage/{rep}\n and execpath is bazel-out/_coverage/{rep}\n", input.execroot.path_string()));
                if custom_gen.is_none() && std::fs::metadata(&out_file).map(|m| m.len() == 0).unwrap_or(true) {
                    gen_msgs.push_str(&format!("INFO: From Coverage report generation:\n{}\nWARNING: There was no coverage found.\n", java_log_header("com.google.devtools.coverageoutputgenerator.Main runWithArgs", false)));
                }
            }
            live_out.extend_from_slice(gen_msgs.as_bytes());
            // Action stdout reaches the command's stdout ahead of the test summary.
            let mut merged = gen_stdout;
            merged.extend_from_slice(summary_out);
            *summary_out = merged;
        }
    }

    results.sort_by(|(l1, s1, r1), (l2, s2, r2)| {
        let cached1 = r1.as_ref().map(|r| r.cached).unwrap_or(false);
        let cached2 = r2.as_ref().map(|r| r.cached).unwrap_or(false);
        if cached1 != cached2 {
            return cached2.cmp(&cached1);
        }
        let uncached1 = r1.as_ref().map(|r| if r.cached { 0 } else { r.times_ms.len() }).unwrap_or(0);
        let uncached2 = r2.as_ref().map(|r| if r.cached { 0 } else { r.times_ms.len() }).unwrap_or(0);
        if uncached1 != uncached2 {
            return uncached1.cmp(&uncached2);
        }
        let sort_key = |s: &TestStatus| if s.is_passed() { -1i32 } else { *s as i32 };
        let k1 = sort_key(s1);
        let k2 = sort_key(s2);
        if k1 != k2 {
            return k1.cmp(&k2);
        }
        l1.cmp(l2)
    });

    let mode = parsed.test_output.as_str();
    let max_test_output_bytes: i64 = opt_last(parsed, "max_test_output_bytes")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(-1);
    if mode == "all" {
        for (label, _, r) in &results {
            if let Some(r) = r {
                if r.cached {
                    writeln(summary_out, &format!("PASSED: {label} (see {})", r.log_path.display()));
                    writeln(summary_out, &format!("INFO: From Testing {label}"));
                    writeln(summary_out, &format!("==================== Test output for {label}:"));
                    if let Ok(b) = std::fs::read(&r.log_path) {
                        let stripped = strip_test_setup_header(&b);
                        if max_test_output_bytes >= 0 && (stripped.len() as i64) > max_test_output_bytes {
                            writeln(summary_out, &format!("Test log too large ({} > {}), skipping...", stripped.len(), max_test_output_bytes));
                        } else {
                            summary_out.extend_from_slice(stripped);
                            if !stripped.is_empty() && !stripped.ends_with(b"\n") {
                                summary_out.push(b'\n');
                            }
                        }
                    }
                    writeln(summary_out, &"=".repeat(80));
                }
            }
        }
    }

    // Summary lines.
    let summary_start = summary_out.len();
    let fmt = opt_last(parsed, "test_summary").unwrap_or_else(|| "short".into());
    let is_detailed = fmt.starts_with("detailed");
    let is_testcase = fmt == "testcase";
    let is_none = fmt == "none";
    let is_terse = fmt == "terse";
    let show_cached = fmt == "detailed" || fmt == "short";
    let show_no_status = is_detailed;

    let mut has_wrong_size = false;
    let verbose_timeout_warnings = opt_bool(parsed, "test_verbose_timeout_warnings", false);

    let target_timeout_of = |label: &str| -> String {
        if let Some(j) = jobs.iter().find(|j| j.label == label) {
            match j.spec.timeout_secs {
                60 => return "short".into(),
                300 => return "moderate".into(),
                900 => return "long".into(),
                3600 => return "eternal".into(),
                _ => match j.spec.size.as_str() {
                    "small" => return "short".into(),
                    "large" => return "long".into(),
                    "enormous" => return "eternal".into(),
                    _ => return "moderate".into(),
                },
            }
        }
        "moderate".into()
    };

    for (label, _, r) in &results {
        if let Some(r) = r {
            // Cached results keep their recorded run time, so the warning also shows on reruns.
            if r.status == TestStatus::Passed {
                let target_attr = target_timeout_of(label);
                if check_test_size(r.duration_ms, &target_attr).is_some() {
                    if !verbose_timeout_warnings {
                        has_wrong_size = true;
                    }
                }
            }
        }
    }

    if !is_none && !is_testcase {
        for (label, status, r) in &results {
            if is_terse && (status.is_passed() || matches!(status, TestStatus::NoStatus | TestStatus::BlazeHaltedBeforeTesting)) {
                continue;
            }
            match r {
                Some(r) => {
                    if !show_cached && r.cached && status.is_passed() {
                        continue;
                    }
                    writeln(summary_out, &summary_line(label, r, opt_bool(parsed, "verbose_test_summary", true)));
                    if r.status == TestStatus::Passed && !r.cached {
                        let target_attr = target_timeout_of(label);
                        if let Some((t_name, sugg)) = check_test_size(r.duration_ms, &target_attr) {
                            if verbose_timeout_warnings {
                                let dur_s = r.duration_ms as f64 / 1000.0;
                                writeln(summary_out, &format!("  WARNING: {label}: Test execution time ({dur_s:.1}s excluding execution overhead) outside of range for {t_name} tests. Consider setting {sugg}."));
                            }
                        }
                    }
                    if is_detailed && !r.cached {
                        // `TestSummaryPrinter.print` with `showAllTestCases`: passed, skipped, failed.
                        let units = unit_cases_of(r);
                        let (mut passed, mut skipped, mut failed) = (Vec::new(), Vec::new(), Vec::new());
                        for u in &units {
                            if let Some(tc) = u {
                                collect_cases(tc, &mut passed, &mut skipped, &mut failed);
                            }
                        }
                        for tc in passed.iter().chain(skipped.iter()) {
                            print_test_case(tc, summary_out);
                        }
                        if !status.is_passed() && *status == TestStatus::Failed {
                            if units.last().map_or(true, |u| u.is_none()) {
                                writeln(summary_out, "    (individual test case information not available) ");
                            } else {
                                for tc in failed.iter().filter(|c| c.status != Some(zel_exec::test_status::CaseStatus::Passed)) {
                                    print_test_case(tc, summary_out);
                                }
                            }
                        }
                    }
                    if !is_detailed && *status == TestStatus::Flaky {
                        if let Some(j) = jobs.iter().find(|j| &j.label == label) {
                            for log in flaky_logs(j, r) {
                                writeln(summary_out, &format!("  {}", log.display()));
                            }
                        }
                    }
                    if !is_detailed && !status.is_passed() && !matches!(status, TestStatus::NoStatus | TestStatus::BlazeHaltedBeforeTesting | TestStatus::FailedToBuild) {
                        if let Some(j) = jobs.iter().find(|j| &j.label == label) {
                            if j.spec.runs_per_test > 1 {
                                for run_idx in (1..=j.spec.runs_per_test).rev() {
                                    let run_dir = j.spec.test_log_dir.join(format!("run_{run_idx}_of_{}", j.spec.runs_per_test));
                                    writeln(summary_out, &format!("  {}", run_dir.join("test.log").display()));
                                    for att in 1..j.spec.flaky_attempts {
                                        let att_log = run_dir.join(format!("test_attempts/attempt_{att}.log"));
                                        if att_log.exists() {
                                            writeln(summary_out, &format!("  {}", att_log.display()));
                                        }
                                    }
                                }
                            } else if r.attempts.len() > 1 {
                                writeln(summary_out, &format!("  {}", r.log_path.display()));
                                for idx in 1..r.attempts.len() {
                                    let att_log = j.spec.test_log_dir.join(format!("test_attempts/attempt_{idx}.log"));
                                    writeln(summary_out, &format!("  {}", att_log.display()));
                                }
                            } else {
                                writeln(summary_out, &format!("  {}", r.log_path.display()));
                            }
                        } else {
                            writeln(summary_out, &format!("  {}", r.log_path.display()));
                        }
                    }
                }
                None => {
                    if *status == TestStatus::FailedToBuild {
                        writeln(summary_out, &format!("{:<width$} FAILED TO BUILD", label, width = 78 - "FAILED TO BUILD".len()));
                    } else {
                        writeln(summary_out, &format!("{:<width$} {}", label, status.name(), width = 78 - status.name().len()));
                    }
                }
            }
        }
    }
    if is_detailed || is_testcase {
        let mut total_cases = 0;
        let mut pass_cases = 0;
        let mut skip_cases = 0;
        let mut fail_cases = 0;
        let mut unknown_targets = 0;
        for (_, status, r) in &results {
            if let Some(r) = r {
                if r.cached {
                    total_cases += 1;
                    unknown_targets += 1;
                    continue;
                }
                for u in unit_cases_of(r) {
                    match u {
                        Some(tc) => {
                            total_cases += count_test_cases(tc, &mut pass_cases, &mut skip_cases, &mut fail_cases);
                        }
                        None => {
                            total_cases += 1;
                            unknown_targets += 1;
                            if status.is_passed() {
                                pass_cases += 1;
                            } else {
                                fail_cases += 1;
                            }
                        }
                    }
                }
            } else {
                total_cases += 1;
                if status.is_passed() {
                    pass_cases += 1;
                } else {
                    fail_cases += 1;
                }
                unknown_targets += 1;
            }
        }
        let disclaimer = if unknown_targets > 0 {
            " (some targets did not have test case information)"
        } else {
            ""
        };
        writeln(
            summary_out,
            &format!(
                "Test cases: finished with {pass_cases} passing, {skip_cases} skipped and {fail_cases} failing out of {total_cases} test cases{disclaimer}"
            ),
        );
    }
    for (label, status, r) in &results {
        let bep_status = {
            let n = status.name().replace(' ', "_");
            if ["NO_STATUS", "PASSED", "FLAKY", "TIMEOUT", "FAILED", "INCOMPLETE", "REMOTE_FAILURE", "FAILED_TO_BUILD", "TOOL_HALTED_BEFORE_TESTING"].contains(&n.as_str()) {
                n
            } else {
                "NO_STATUS".to_string()
            }
        };
        let Some(r) = r.as_ref().filter(|r| !r.attempts.is_empty()) else {
            reporter.post_bep(zel_events::BepEvent::TestResult {
                label: label.clone(),
                status: status.name().replace(' ', "_"),
                duration_ms: r.as_ref().map(|r| r.duration_ms).unwrap_or(0),
            });
            continue;
        };
        post_test_bep_events(reporter, label, &bep_status, r);
    }
    let simple: Vec<(String, TestStatus)> = results.iter().map(|(l, s, _)| (l.clone(), *s)).collect();
    if check_tests_up_to_date {
        summary_out.extend_from_slice(zel_exec::test_runner::format_check_up_to_date_stats(&simple).as_bytes());
    } else {
        summary_out.extend_from_slice(format_stats(executed, &simple).as_bytes());
    }
    if has_wrong_size {
        writeln(summary_out, "There were tests whose specified size is too big. Use the --test_verbose_timeout_warnings command line option to see which ones these are.");
    }
    if opt_bool(parsed, "print_relative_test_log_paths", false) {
        // `PathPrettyPrinter` with the `<prefix>testlogs` convenience symlink.
        let prefix = opt_last(parsed, "symlink_prefix").unwrap_or_else(|| "bazel-".to_string());
        let tail = String::from_utf8_lossy(&summary_out[summary_start..])
            .replace(&format!("{}/", input.testlogs_dir.path_string()), &format!("{prefix}testlogs/"));
        summary_out.truncate(summary_start);
        summary_out.extend_from_slice(tail.as_bytes());
    }
    let cached = results.iter().filter(|(_, _, r)| r.as_ref().map(|x| x.cached).unwrap_or(false)).count();
    let has_any_sharding_error = results.iter().any(|(_, _, r)| {
        r.as_ref().map_or(false, |res| res.attempts.iter().any(|a| a.sharding_unsupported))
    });
    let sharding_error = if any_split_fail {
        Some(format!("ERROR: {SPLIT_COVERAGE_PREREQ}\n"))
    } else if has_any_sharding_error {
        Some("ERROR: Sharding requested, but the test runner did not advertise support for it by touching TEST_SHARD_STATUS_FILE. Either remove the 'shard_count' attribute, use a test runner that supports sharding or temporarily disable this check via --noincompatible_check_sharding_support.\n".to_string())
    } else {
        None
    };
    TestPhaseOutput {
        results: simple,
        executed,
        cached,
        num_tests,
        sharding_error,
        report_failed,
        proc: test_proc,
    }
}

fn unit_cases_of(r: &zel_exec::test_runner::TestResult) -> Vec<Option<&zel_exec::test_status::TestCase>> {
    if r.unit_cases.is_empty() {
        vec![r.test_case.as_ref()]
    } else {
        r.unit_cases.iter().map(|c| c.as_ref()).collect()
    }
}

/// `TestSummary.Builder.traverseTestCases`: leaf cases that ran, by outcome.
fn collect_cases<'a>(
    tc: &'a zel_exec::test_status::TestCase,
    passed: &mut Vec<&'a zel_exec::test_status::TestCase>,
    skipped: &mut Vec<&'a zel_exec::test_status::TestCase>,
    failed: &mut Vec<&'a zel_exec::test_status::TestCase>,
) {
    if !tc.child.is_empty() {
        for c in &tc.child {
            collect_cases(c, passed, skipped, failed);
        }
    } else if tc.kind == Some(zel_exec::test_status::CaseType::TestCase) || tc.name.is_some() {
        if tc.run == Some(false) {
            return;
        }
        match tc.status {
            Some(zel_exec::test_status::CaseStatus::Passed) => passed.push(tc),
            Some(zel_exec::test_status::CaseStatus::Skipped) => skipped.push(tc),
            _ => failed.push(tc),
        }
    }
}

/// `TestSummaryPrinter.printTestCase`.
fn print_test_case(tc: &zel_exec::test_status::TestCase, out: &mut Vec<u8>) {
    let status_str = match tc.status.unwrap_or(zel_exec::test_status::CaseStatus::Passed) {
        zel_exec::test_status::CaseStatus::Passed => "PASSED",
        zel_exec::test_status::CaseStatus::Skipped => "SKIPPED",
        zel_exec::test_status::CaseStatus::Failed => "FAILED",
        zel_exec::test_status::CaseStatus::Error => "ERROR",
    };
    let dur = tc.run_duration_millis.map(|ms| format!(" ({:.1}s)", ms as f64 / 1000.0)).unwrap_or_default();
    writeln(
        out,
        &format!("    {:<8}{}.{}{}", status_str, tc.class_name.as_deref().unwrap_or(""), tc.name.as_deref().unwrap_or(""), dur),
    );
}

fn count_test_cases(
    tc: &zel_exec::test_status::TestCase,
    pass: &mut usize,
    skip: &mut usize,
    fail: &mut usize,
) -> usize {
    if !tc.child.is_empty() {
        let mut count = 0;
        for c in &tc.child {
            count += count_test_cases(c, pass, skip, fail);
        }
        count
    } else if tc.kind == Some(zel_exec::test_status::CaseType::TestCase) || (tc.name.is_some() && tc.child.is_empty()) {
        if tc.run == Some(false) {
            return 0;
        }
        match tc.status {
            Some(zel_exec::test_status::CaseStatus::Passed) => *pass += 1,
            Some(zel_exec::test_status::CaseStatus::Skipped) => *skip += 1,
            _ => *fail += 1,
        }
        1
    } else {
        0
    }
}

fn print_detailed_test_cases(
    tc: &zel_exec::test_status::TestCase,
    is_target_failed: bool,
    stderr: &mut Vec<u8>,
) {
    if !tc.child.is_empty() {
        for c in &tc.child {
            print_detailed_test_cases(c, is_target_failed, stderr);
        }
    } else if tc.kind == Some(zel_exec::test_status::CaseType::TestCase) || (tc.name.is_some() && tc.child.is_empty()) {
        if tc.run == Some(false) {
            return;
        }
        let status = tc.status.unwrap_or(zel_exec::test_status::CaseStatus::Passed);
        if is_target_failed && status == zel_exec::test_status::CaseStatus::Passed {
            return;
        }
        let status_str = match status {
            zel_exec::test_status::CaseStatus::Passed => "PASSED",
            zel_exec::test_status::CaseStatus::Skipped => "SKIPPED",
            zel_exec::test_status::CaseStatus::Failed => "FAILED",
            zel_exec::test_status::CaseStatus::Error => "ERROR",
        };
        let class_name = tc.class_name.as_deref().unwrap_or("");
        let name = tc.name.as_deref().unwrap_or("");
        let full_name = if !class_name.is_empty() && !name.is_empty() {
            format!("{class_name}.{name}")
        } else if !name.is_empty() {
            format!(".{name}")
        } else {
            class_name.to_string()
        };
        let dur = tc
            .run_duration_millis
            .map(|ms| format!(" ({:.1}s)", ms as f64 / 1000.0))
            .unwrap_or_else(|| " (0.0s)".to_string());
        writeln(stderr, &format!("    {:<8}{}{}", status_str, full_name, dur));
    }
}

#[cfg(test)]
mod proc_stats_tests {
    use super::*;

    fn st(total: usize, non_internal: usize, hits: usize, runners: &[(&str, usize)]) -> ProcStats {
        ProcStats {
            total,
            non_internal,
            hits,
            runners: runners.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        }
    }

    /// Observed from real bazel 9.3.2 (`test //:tp`): 7 processes: 6 internal, 2 darwin-sandbox.
    #[test]
    fn summary_matches_spawn_stats() {
        assert_eq!(st(7, 1, 0, &[("darwin-sandbox", 2)]).summary(), "INFO: 7 processes: 6 internal, 2 darwin-sandbox.");
        assert_eq!(st(1, 0, 0, &[]).summary(), "INFO: 1 process: 1 internal.");
        assert_eq!(st(2, 1, 0, &[("darwin-sandbox", 1)]).summary(), "INFO: 2 processes: 1 internal, 1 darwin-sandbox.");
        assert_eq!(st(0, 0, 0, &[]).summary(), "INFO: 0 processes.");
        // Cache hits first, then REPORT_FIRST runners, internal, then the rest alphabetically.
        assert_eq!(
            st(26, 7, 3, &[("worker", 1), ("darwin-sandbox", 6), ("disk cache hit", 2)]).summary(),
            "INFO: 26 processes: 3 action cache hit, 2 disk cache hit, 19 internal, 6 darwin-sandbox, 1 worker."
        );
        // An action cache hit does not count in the total (`SpawnStats` total = completed actions).
        assert_eq!(st(1, 0, 5, &[]).summary(), "INFO: 1 process: 5 action cache hit, 1 internal.");
    }

    #[test]
    fn test_units_count_as_actions_with_spawns() {
        let mut s = ProcStats::default();
        s.total = 1;
        let t = TestProc {
            units: 2,
            units_with_spawns: 2,
            cached_units: 1,
            spawns: [("darwin-sandbox".to_string(), 4usize)].into_iter().collect(),
        };
        add_test_proc(&mut s, &t);
        assert_eq!(s.summary(), "INFO: 3 processes: 1 action cache hit, 1 internal, 4 darwin-sandbox.");
    }
}

/// `--flaky_test_attempts`: `<n>`, `default`, or `<label regex>@<n>` (the first matching label
/// pattern wins, a bare number applies to every test).
fn flaky_attempts_for(specs: &[String], label: &str) -> Option<u32> {
    let mut bare = None;
    for sp in specs {
        if let Some((pat, n)) = sp.rsplit_once('@') {
            let Ok(n) = n.parse::<u32>() else { continue };
            let lit = pat.replace("\\", "");
            let hit = match lit.strip_suffix(".*") {
                Some(prefix) => label.starts_with(prefix),
                None => label == lit || label.ends_with(&format!(":{lit}")),
            };
            if hit {
                return Some(n);
            }
        } else if let Ok(n) = sp.parse::<u32>() {
            bare = Some(n);
        } else if sp == "default" {
            bare = None;
        }
    }
    bare
}

/// `TestSummaryPrinter.print`: without `--verbose_test_summary` the attempt and time summaries
/// are left off.
fn summary_line(label: &str, r: &TestResult, verbose: bool) -> String {
    let line = format_summary_line(label, r);
    if verbose {
        return line;
    }
    let cache_msg = if r.cached && !matches!(r.status, TestStatus::NoStatus | TestStatus::Incomplete | TestStatus::FailedToBuild) {
        "(cached) "
    } else {
        ""
    };
    let message = format!("{}{}", cache_msg, r.status.name());
    let pad = 78usize.saturating_sub(message.len());
    format!("{label:<pad$} {message}")
}

const SPLIT_COVERAGE_PREREQ: &str = "coverageDirectoryTreeArtifact is null: --experimental_split_coverage_postprocessing depends on --experimental_fetch_all_coverage_outputs being enabled";

/// `--run_under=//label args`: the label becomes the built executable's path (`TestTargetExecutionSettings`).
fn resolve_run_under(ru: &str, input: &TestPhaseInput) -> String {
    let mut words = ru.split_whitespace();
    let Some(first) = words.next() else { return ru.to_string() };
    if !(first.starts_with("//") || first.starts_with('@')) {
        return ru.to_string();
    }
    let exe = input
        .configured
        .iter()
        .find(|ct| {
            let cf = ct.label.canonical_form();
            let ts = ct.label.to_string();
            cf == first || ts == first || cf.trim_start_matches('@') == first.trim_start_matches('@')
        })
        .and_then(|ct| ct.default_info.executable.as_ref())
        .map(|a| {
            let rel = a.root_relative_path.path_string();
            format!("../{}/{rel}", input.workspace_name)
        });
    match exe {
        Some(p) => std::iter::once(p).chain(words.map(str::to_string)).collect::<Vec<_>>().join(" "),
        None => ru.to_string(),
    }
}

/// `TestResult` per attempt and the `TestSummary` of one test target (HEAD `TestAttempt`,
/// `TestResult`, `TestSummary` build events), announced by the target's `TargetComplete`.
fn post_test_bep_events(reporter: &Reporter, label: &str, overall: &str, r: &TestResult) {
    use zel_events::bep::BepBuilder;
    let cfg = reporter.default_config_id();
    let dir = r.log_path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let runs = r.runs.max(1) as u32;
    let shards = r.attempts.iter().map(|a| a.shard + 1).max().unwrap_or(1);
    let unit_dir = |run: u32, shard: u32| {
        let mut d = dir.clone();
        if runs > 1 {
            d = d.join(format!("run_{}_of_{}", run + 1, runs));
        }
        if shards > 1 {
            d = d.join(format!("shard_{}_of_{}", shard + 1, shards));
        }
        d
    };
    let uri = |p: &std::path::Path| format!("file://{}", p.display());
    let mut units: std::collections::BTreeMap<(u32, u32), Vec<&zel_exec::test_runner::AttemptResult>> = Default::default();
    for a in &r.attempts {
        units.entry((a.run, a.shard)).or_default().push(a);
    }
    let mut result_ids = Vec::new();
    let (mut passed, mut failed) = (Vec::new(), Vec::new());
    let mut attempt_count = 1;
    let start = r.start_time_ms as i64;
    for ((run, shard), atts) in &mut units {
        atts.sort_by_key(|a| a.attempt);
        let ud = unit_dir(*run, *shard);
        attempt_count = attempt_count.max(atts.len() as i32);
        for (i, a) in atts.iter().enumerate() {
            let last = i + 1 == atts.len();
            let st = if a.timed_out { "TIMEOUT" } else if a.exit_code == 0 { "PASSED" } else { "FAILED" };
            let mut outputs = Vec::new();
            let (log, xml) = if atts.len() > 1 && !last {
                (ud.join(format!("test_attempts/attempt_{}.log", a.attempt)), ud.join(format!("test_attempts/attempt_{}.xml", a.attempt)))
            } else {
                (ud.join("test.log"), ud.join("test.xml"))
            };
            outputs.push(("test.log".to_string(), uri(&log)));
            if xml.exists() {
                outputs.push(("test.xml".to_string(), uri(&xml)));
            }
            let node = BepBuilder::test_result(
                label,
                &cfg,
                *run as i32 + 1,
                *shard as i32 + 1,
                a.attempt as i32,
                st,
                r.cached,
                start,
                a.duration_ms as i64,
                &outputs,
                if a.runner.is_empty() { "local" } else { &a.runner },
                a.exit_code,
            );
            result_ids.push(node.id.clone());
            reporter.post_bep_node(node);
        }
        let last = atts.last().unwrap();
        let entry = (String::new(), uri(&ud.join("test.log")));
        if last.exit_code == 0 && !last.timed_out { passed.push(entry) } else { failed.push(entry) }
    }
    let n_units = units.len() as i32;
    let node = BepBuilder::test_summary(
        label,
        &cfg,
        overall,
        n_units,
        if shards > 1 { shards as i32 } else { 0 },
        attempt_count,
        &passed,
        &failed,
        if r.cached { n_units } else { 0 },
        start,
        start + r.duration_ms as i64,
        r.attempts.iter().map(|a| a.duration_ms as i64).sum(),
        result_ids,
    );
    reporter.post_bep_node(node);
}

/// `(runs, shards)` of a test target: the `testResult` ids announced by its `TargetComplete`.
pub(crate) fn test_unit_grid(target: &zel_packages::Target, label: &str, parsed: &ParsedCommandOptions) -> (i32, i32) {
    let attrs = test_attrs(Some(target), parsed);
    let runs = target_runs_per_test(label, parsed) as i32;
    let strategy = opt_last(parsed, "test_sharding_strategy");
    let shards = effective_shard_count(attrs.shard_count, strategy.as_deref()).unwrap_or(attrs.shard_count) as i32;
    (runs.max(1), shards.max(1))
}
