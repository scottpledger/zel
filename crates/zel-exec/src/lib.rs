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

//! Bazel Execution Phase (`CompactPersistentActionCache`, `SymlinkForest`, `ActionExecutor`,
//! and `StandaloneTestStrategy`).
//! Mirrors `src/main/java/com/google/devtools/build/lib/exec` and `actions/cache`.

use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

pub mod action_cache;
pub mod digest;
pub mod java_compile;
pub mod layout;
pub mod resources;
pub mod strategy;
pub mod scheduler;
pub mod runfiles_tree;
pub mod symlinks;
pub mod test_runner;
pub mod test_status;
pub use scheduler::*;
use zel_analysis::{ActionAnalysisMetadata, Artifact};
use zel_events::{Event, Reporter};
use zel_remote::spawn_runner::{ExecMode, RemoteSpawnRunner, SpawnRunner};
use zel_sandbox::{execute_spawn_local, Spawn, SpawnStatus};
use zel_vfs::{FileSystem, PathFragment};

pub use action_cache::{ActionCacheEntry, ActionKeyParts, CompactPersistentActionCache, MustExecuteReason};
pub use digest::DigestFunction;

/// Plants the `$EXECROOT/_main` directory and workspace convenience symlinks
/// (`bazel-bin`, `bazel-out`, `bazel-testlogs`).
pub fn plant_symlink_forest(
    fs: &dyn FileSystem,
    workspace_dir: &PathFragment,
    execroot: &PathFragment,
    bazel_out: &PathFragment,
    bazel_bin: &PathFragment,
    bazel_testlogs: &PathFragment,
    source_artifacts: &[Artifact],
) -> io::Result<()> {
    fs.create_directory_and_parents(execroot)?;
    // `SymlinkForest`: one symlink per top-level workspace entry (so every source file is
    // visible in the execroot), `external/<repo>` links, and a real `bazel-out`.
    {
        let er = std::path::Path::new(execroot.path_string());
        let ws = std::path::Path::new(workspace_dir.path_string());
        if ws.is_dir() && er.starts_with("/") {
            let ignored = layout::read_bazelignore(ws);
            layout::plant_symlink_forest(er, ws, "bazel-", &ignored)?;
            if let Some(ob) = er.parent().and_then(|p| p.parent()) {
                layout::plant_external_repo_links(er, ob)?;
            }
        }
    }
    fs.create_directory_and_parents(bazel_out)?;
    fs.create_directory_and_parents(bazel_bin)?;
    fs.create_directory_and_parents(bazel_testlogs)?;

    // Stage source artifacts into execroot if not already linked
    for art in source_artifacts {
        if art.is_source {
            let ws_file = workspace_dir.get_relative(&art.root_relative_path);
            let exec_file = execroot.get_relative(&art.exec_path);
            if !fs.exists(&exec_file, true) && fs.exists(&ws_file, true) {
                if let Some(parent) = exec_file.parent_directory() {
                    fs.create_directory_and_parents(&parent)?;
                }
                if fs.create_symbolic_link(&exec_file, &ws_file).is_err() {
                    if let Ok(bytes) = fs.read_file(&ws_file) {
                        let _ = fs.write_file(&exec_file, &bytes);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Why an action failed; formatted like Bazel's `ActionExecutionException` console output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionFailure {
    /// `ERROR: ...` lines (without trailing newline) printed to the console.
    pub messages: Vec<String>,
    pub exit_code: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionExecutionOutcome {
    pub cache_hit: bool,
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub failure: Option<ActionFailure>,
    pub wall_time_ms: u64,
    /// Name of the spawn runner that executed the action (`local`, `worker`, `darwin-sandbox`...).
    pub runner: Option<String>,
    /// Progress message of the spawn (`Executing genrule //:x`), for `INFO: From <progress>:`.
    pub progress: String,
}

impl ActionExecutionOutcome {
    pub fn succeeded(&self) -> bool {
        self.failure.is_none()
    }
    fn ok(cache_hit: bool) -> Self {
        Self {
            cache_hit,
            exit_code: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
            failure: None,
            wall_time_ms: 0,
            runner: None,
            progress: String::new(),
        }
    }
}

/// Dynamic-execution knobs (`DynamicExecutionOptions`).
#[derive(Debug, Clone)]
pub struct DynamicOptions {
    /// `--dynamic_local_execution_delay` (ms): how long the local branch is delayed after a
    /// remote cache hit was seen during this build.
    pub local_execution_delay_ms: u64,
    /// `--experimental_dynamic_ignore_local_signals`.
    pub ignore_local_signals: Vec<i32>,
    /// `--experimental_dynamic_cancel_remote_branch_on_local_win`.
    pub cancel_remote_branch_on_local_win: bool,
    /// `--debug_spawn_scheduler`.
    pub debug: bool,
}

impl Default for DynamicOptions {
    fn default() -> Self {
        Self {
            local_execution_delay_ms: 1000,
            ignore_local_signals: Vec::new(),
            cancel_remote_branch_on_local_win: true,
            debug: false,
        }
    }
}

/// The execution-side runtime of a command: spawn strategy registry, the sandbox / worker
/// runners, the local `ResourceManager`, the remote cache / executor client and sandbox settings
/// (`SandboxModule`, `WorkerModule`, `RemoteModule`, `DynamicExecutionModule`, `LocalSpawnRunner`).
pub struct ExecRuntime {
    pub registry: strategy::SpawnStrategyRegistry,
    pub sandbox_runners: Vec<zel_sandbox::sandbox::SandboxRunner>,
    pub worker: Option<zel_sandbox::worker::WorkerSpawnRunner>,
    pub resources: resources::ResourceManager,
    pub sandbox_debug: bool,
    pub kill_delay: Option<std::time::Duration>,
    /// Remote cache / executor client (`--remote_cache`, `--remote_executor`, `--disk_cache`).
    pub remote: Option<Arc<RemoteSpawnRunner>>,
    pub dynamic: DynamicOptions,
    /// Set once a remote branch produced a cache hit: delays the local branch of later spawns.
    pub delay_local: AtomicBool,
    /// `--allowed_local_actions_regex`: only mnemonics matching it may run with the standalone
    /// (`local`) strategy.
    pub allowed_local_actions_regex: Option<String>,
    /// Deferred `--remote_cache`/`--remote_executor` connection failure of *this* command: reported by its first
    /// spawn (per runtime, not process-global, so concurrent commands never see each other's error).
    pub remote_setup_error: Option<String>,
}

impl std::fmt::Debug for ExecRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ExecRuntime {{ default_strategies: {:?} }}", self.registry.default_strategies())
    }
}

/// Which branch of a dynamic execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Branch {
    Local,
    Remote,
}

/// Shared state of one dynamically executed spawn (`strategyThatCancelled` and the two branch
/// futures in `DynamicSpawnStrategy`).
struct Race {
    winner: Mutex<Option<Branch>>,
    local_cancel: zel_sandbox::process_wrapper::CancelFlag,
    remote_cancelled: AtomicBool,
    local_done: (Mutex<bool>, Condvar),
    outcome: Mutex<RaceOutcome>,
    changed: Condvar,
    ignore_local_signals: Vec<i32>,
    cancel_remote_on_local_win: bool,
}

#[derive(Default)]
struct RaceOutcome {
    result: Option<(Branch, io::Result<zel_sandbox::SpawnResult>)>,
    finished: usize,
}

impl Race {
    fn is_cancelled(&self, b: Branch) -> bool {
        match b {
            Branch::Local => self.local_cancel.is_cancelled(),
            Branch::Remote => self.remote_cancelled.load(Ordering::SeqCst),
        }
    }

    /// `DynamicSpawnStrategy.stopBranch` (preceded by `maybeIgnoreFailure`): the first branch to
    /// get here wins and cancels the other; the loser must not publish any output.
    fn lock(&self, b: Branch, exit_code: i32) -> bool {
        // A local branch killed by an ignorable signal cancels itself in favor of remote.
        if b == Branch::Local && exit_code != 0 && self.ignore_local_signals.contains(&(exit_code - 128)) {
            return false;
        }
        if self.is_cancelled(b) {
            return false;
        }
        {
            let mut w = self.winner.lock().unwrap();
            match *w {
                Some(m) => return m == b,
                None => *w = Some(b),
            }
        }
        match b {
            Branch::Remote => {
                // `otherBranch.cancel()` + wait for the local branch to be done.
                self.local_cancel.cancel();
                let (m, cv) = &self.local_done;
                let mut done = m.lock().unwrap();
                while !*done {
                    done = cv.wait(done).unwrap();
                }
            }
            Branch::Local => {
                if self.cancel_remote_on_local_win {
                    // The remote branch cannot be interrupted mid-RPC; it is flagged so that it
                    // never publishes outputs and finishes in the background.
                    self.remote_cancelled.store(true, Ordering::SeqCst);
                }
            }
        }
        true
    }

    fn deliver(&self, b: Branch, r: io::Result<zel_sandbox::SpawnResult>) {
        let keep = match &r {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => false,
            Ok(res) => self.lock(b, res.exit_code),
            Err(_) => self.lock(b, 1),
        };
        let mut o = self.outcome.lock().unwrap();
        o.finished += 1;
        if keep && o.result.is_none() {
            o.result = Some((b, r));
        }
        self.changed.notify_all();
    }

    fn local_finished(&self) {
        let (m, cv) = &self.local_done;
        *m.lock().unwrap() = true;
        cv.notify_all();
    }
}

/// Adapter so the remote runner can fall back to / wrap a local strategy of this runtime.
struct LocalKindRunner<'a> {
    rt: &'a ExecRuntime,
    kind: strategy::StrategyKind,
    hooks: zel_sandbox::ExecHooks,
}

impl SpawnRunner for LocalKindRunner<'_> {
    fn name(&self) -> &str {
        self.kind.name()
    }
    fn can_exec(&self, spawn: &Spawn) -> bool {
        self.rt.registry.can_exec(self.kind, spawn)
    }
    fn exec(&self, spawn: &Spawn, execroot: &PathFragment, fs: &dyn FileSystem) -> io::Result<zel_sandbox::SpawnResult> {
        match self.rt.run_local_kind(self.kind, spawn, execroot, fs, &self.hooks) {
            Ok(r) => r,
            Err(m) => Err(io::Error::other(m)),
        }
    }
}

/// `DynamicSpawnStrategy.getSpawnNotExecutableReason`.
fn spawn_not_executable_reason(spawn: &Spawn) -> String {
    let can_local = !spawn.execution_info.contains_key("no-local");
    let can_remote = zel_remote::spawn_runner::may_be_executed_remotely(spawn);
    let mut msg = String::new();
    if !can_local && !can_remote {
        msg.push_str("Neither local nor remote execution allowed for action ");
    } else if !can_remote {
        msg.push_str("No usable dynamic_local_strategy found (and remote execution disabled) for action ");
    } else if !can_local {
        msg.push_str("No usable dynamic_remote_strategy found (and local execution disabled) for action ");
    } else {
        msg.push_str("No usable dynamic_local_strategy or dynamic_remote_strategy found for action ");
    }
    msg.push_str(&spawn.mnemonic);
    msg.push_str(". ");
    msg
}

impl ExecRuntime {
    fn sandbox_runner(&self, kind: zel_sandbox::sandbox::SandboxKind) -> Option<&zel_sandbox::sandbox::SandboxRunner> {
        self.sandbox_runners.iter().find(|r| r.kind == kind)
    }

    /// Runs `spawn` with the local (non-remote) strategy `kind`. `Err` means the strategy is not
    /// available in this runtime.
    fn run_local_kind(
        &self,
        kind: strategy::StrategyKind,
        spawn: &Spawn,
        execroot: &PathFragment,
        fs: &dyn FileSystem,
        hooks: &zel_sandbox::ExecHooks,
    ) -> Result<io::Result<zel_sandbox::SpawnResult>, String> {
        Ok(match kind {
            strategy::StrategyKind::Standalone => {
                // `LocalSpawnRunner`: `--allowed_local_actions_regex` (`RegexPatternOption.matcher()`).
                if let Some(pat) = &self.allowed_local_actions_regex {
                    let allowed = zel_bzlmod::download::Regex::new(pat)
                        .map(|r| r.full_match(&spawn.mnemonic).is_some())
                        .unwrap_or(true);
                    if !allowed {
                        return Ok(Ok(zel_sandbox::SpawnResult {
                            status: SpawnStatus::ExecutionFailed,
                            exit_code: 1,
                            stdout: Vec::new(),
                            stderr: format!(
                                "Action type {} is not allowed to run locally due to regex filter: {pat}\n",
                                spawn.mnemonic
                            )
                            .into_bytes(),
                            wall_time_ms: 0,
                            runner_name: "local".into(),
                            missing: Vec::new(),
                            usage: None,
                            executed_command: None,
                        }));
                    }
                }
                let mut r = zel_sandbox::execute_spawn_local_with(spawn, execroot, fs, hooks);
                if let Ok(r) = &mut r {
                    r.runner_name = "local".into();
                }
                r
            }
            strategy::StrategyKind::DarwinSandbox => match self.sandbox_runner(zel_sandbox::sandbox::SandboxKind::Darwin) {
                Some(r) => r.exec_with(spawn, hooks),
                None => return Err("darwin-sandbox is not available".into()),
            },
            strategy::StrategyKind::LinuxSandbox => match self.sandbox_runner(zel_sandbox::sandbox::SandboxKind::Linux) {
                Some(r) => r.exec_with(spawn, hooks),
                None => return Err("linux-sandbox is not available".into()),
            },
            strategy::StrategyKind::ProcessWrapperSandbox => {
                match self.sandbox_runner(zel_sandbox::sandbox::SandboxKind::ProcessWrapper) {
                    Some(r) => r.exec_with(spawn, hooks),
                    None => return Err("processwrapper-sandbox is not available".into()),
                }
            }
            strategy::StrategyKind::Worker => match &self.worker {
                // Workers write their outputs in place: the dynamic-execution lock is taken
                // before the request is sent.
                Some(w) => {
                    if hooks.is_cancelled() || !hooks.lock(0) {
                        Err(zel_sandbox::cancelled_error())
                    } else {
                        w.exec(spawn)
                    }
                }
                None => return Err("worker strategy is not available".into()),
            },
            k => return Err(format!("strategy {} cannot execute spawns in this build", k.name())),
        })
    }

    /// Runs `spawn` with `kind`, going through the remote cache like `AbstractSpawnStrategy` does
    /// with the `SpawnCache` context (lookup, local run on a miss, upload).
    fn run_strategy(
        &self,
        kind: strategy::StrategyKind,
        spawn: &Spawn,
        execroot: &PathFragment,
        fs: &dyn FileSystem,
        hooks: &zel_sandbox::ExecHooks,
    ) -> Result<io::Result<zel_sandbox::SpawnResult>, String> {
        match kind {
            strategy::StrategyKind::Remote => {
                let Some(remote) = &self.remote else {
                    return Err("remote strategy is not available".into());
                };
                let fallback = LocalKindRunner { rt: self, kind: self.registry.remote_local_fallback(), hooks: Default::default() };
                let mode = ExecMode {
                    allow_remote_exec: true,
                    fallback: Some(&fallback),
                    allow_local: hooks.lock_outputs.is_none(),
                    hooks: hooks.clone(),
                };
                Ok(remote.exec_ext(spawn, execroot, fs, &mode))
            }
            strategy::StrategyKind::Dynamic => Err("dynamic cannot be nested".into()),
            k => match &self.remote {
                Some(remote) if remote.has_cache() => {
                    let local = LocalKindRunner { rt: self, kind: k, hooks: hooks.clone() };
                    let mode = ExecMode {
                        allow_remote_exec: false,
                        fallback: Some(&local),
                        allow_local: true,
                        hooks: hooks.clone(),
                    };
                    Ok(remote.exec_ext(spawn, execroot, fs, &mode).map(|mut r| {
                        if r.runner_name == "remote-cache" {
                            r.runner_name = k.name().into();
                        }
                        r
                    }))
                }
                _ => self.run_local_kind(k, spawn, execroot, fs, hooks),
            },
        }
    }

    /// Resolves the strategy for `spawn` and runs it. `Err` carries the resolver's message.
    pub fn execute(
        self: &Arc<Self>,
        spawn: &Spawn,
        description: &str,
        execroot: &PathFragment,
        fs: &dyn FileSystem,
        reporter: &Reporter,
    ) -> Result<io::Result<zel_sandbox::SpawnResult>, String> {
        if self.remote.is_none() {
            if let Some(e) = self.remote_setup_error.clone() {
                return Err(format!("Failed to query remote execution capabilities: {e}"));
            }
        }
        let (mut strategies, progress) = self.registry.resolve(spawn, description)?;
        if let Some(p) = progress {
            reporter.handle(Event { kind: zel_events::EventKind::Progress, location: None, message: p, tag: None });
        }
        let chosen = strategies.remove(0);
        if chosen == strategy::StrategyKind::Dynamic {
            return self.exec_dynamic(spawn, execroot, fs);
        }
        self.run_strategy(chosen, spawn, execroot, fs, &Default::default())
    }

    /// `DynamicSpawnStrategy.exec`: runs the local and the remote strategy of the spawn in
    /// parallel; the first branch to finish publishes its outputs and cancels the other.
    fn exec_dynamic(
        self: &Arc<Self>,
        spawn: &Spawn,
        execroot: &PathFragment,
        fs: &dyn FileSystem,
    ) -> Result<io::Result<zel_sandbox::SpawnResult>, String> {
        let local = self.registry.dynamic_can_exec_local(spawn);
        let remote = self.registry.dynamic_can_exec_remote(spawn);
        let (local, remote) = match (local, remote) {
            (None, None) => {
                return Err(format!("Spawn is not executable in local: {}", spawn_not_executable_reason(spawn)));
            }
            // `maybeExecuteNonDynamically`.
            (None, Some(r)) => return self.run_strategy(r, spawn, execroot, fs, &Default::default()),
            (Some(l), None) => return self.run_strategy(l, spawn, execroot, fs, &Default::default()),
            (Some(l), Some(r)) => (l, r),
        };
        let race = Arc::new(Race {
            winner: Mutex::new(None),
            local_cancel: Default::default(),
            remote_cancelled: AtomicBool::new(false),
            local_done: (Mutex::new(false), Condvar::new()),
            outcome: Mutex::new(RaceOutcome::default()),
            changed: Condvar::new(),
            ignore_local_signals: self.dynamic.ignore_local_signals.clone(),
            cancel_remote_on_local_win: self.dynamic.cancel_remote_branch_on_local_win,
        });
        let hooks_for = |b: Branch| {
            let race = race.clone();
            zel_sandbox::ExecHooks {
                cancel: (b == Branch::Local).then(|| race.local_cancel.clone()),
                lock_outputs: Some(Arc::new(move |code| race.lock(b, code))),
            }
        };
        // Remote branch: detached (an in-flight RPC cannot be interrupted).
        {
            let rt = self.clone();
            let race = race.clone();
            let spawn = spawn.clone();
            let execroot = execroot.clone();
            let hooks = hooks_for(Branch::Remote);
            std::thread::spawn(move || {
                let fs = zel_vfs::LocalFileSystem::default();
                let r = match rt.run_strategy(remote, &spawn, &execroot, &fs, &hooks) {
                    Ok(r) => r,
                    Err(m) => Err(io::Error::other(m)),
                };
                if matches!(&r, Ok(res) if res.runner_name.contains("cache hit")) {
                    rt.delay_local.store(true, Ordering::SeqCst);
                }
                race.deliver(Branch::Remote, r);
            });
        }
        std::thread::scope(|scope| {
            let hooks = hooks_for(Branch::Local);
            let race2 = race.clone();
            scope.spawn(move || {
                let delay = self.delay_local.load(Ordering::SeqCst).then_some(self.dynamic.local_execution_delay_ms);
                let r = (|| {
                    if let Some(ms) = delay {
                        let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
                        while std::time::Instant::now() < until {
                            if race2.is_cancelled(Branch::Local) {
                                return Err(zel_sandbox::cancelled_error());
                            }
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                    }
                    if race2.is_cancelled(Branch::Local) {
                        return Err(zel_sandbox::cancelled_error());
                    }
                    match self.run_strategy(local, spawn, execroot, fs, &hooks) {
                        Ok(r) => r,
                        Err(m) => Err(io::Error::other(m)),
                    }
                })();
                // The local branch is "done" once it stopped touching the exec root.
                race2.local_finished();
                race2.deliver(Branch::Local, r);
            });
            let mut o = race.outcome.lock().unwrap();
            while o.result.is_none() && o.finished < 2 {
                o = race.changed.wait(o).unwrap();
            }
            match o.result.take() {
                Some((_, r)) => Ok(r),
                None => Err(format!("Neither branch of {} completed.", spawn.mnemonic)),
            }
        })
    }

    /// Downloads the outputs known only by digest (`--remote_download_outputs`) for the given
    /// exec-root-relative paths.
    pub fn materialize_remote_outputs(&self, paths: &[String], execroot: &str) -> Result<usize, String> {
        match &self.remote {
            Some(r) => r.materialize(paths, std::path::Path::new(execroot)),
            None => Ok(0),
        }
    }

    /// End-of-build cleanup (`SandboxModule.afterCommand`, `--worker_quit_after_build`).
    pub fn after_command(&self, quit_workers: bool) {
        if !self.sandbox_debug {
            for r in &self.sandbox_runners {
                let _ = r.cleanup_sandbox_base();
            }
        }
        if quit_workers {
            if let Some(w) = &self.worker {
                w.pool.shutdown();
            }
        }
    }
}

/// Knobs for action execution (`--subcommands`, `--verbose_failures`, ...).
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    pub announce_subcommands: bool,
    /// Configuration checksum (`BuildOptions.checksum()`) by output directory name
    /// (`bazel-out/<mnemonic>/...`), used by `--subcommands`/`--verbose_failures` output.
    pub config_checksums: BTreeMap<String, String>,
    pub verbose_failures: bool,
    pub workspace_dir: Option<PathFragment>,
    /// Strategy registry / runners / resources; `None` runs every spawn through the local
    /// (standalone) runner with no resource limits.
    pub runtime: Option<std::sync::Arc<ExecRuntime>>,
    pub digest: DigestFunction,
    /// `--check_up_to_date`: never execute; an action that would have to run fails the build
    /// (`ActionCacheChecker.isActionExecutionProhibited`).
    pub check_up_to_date: bool,
    /// The client environment of the command (`CommandEnvironment.getClientEnv`): the source of
    /// inherited action variables (`--action_env=NAME`, `env_inherit`, ...), whose values are
    /// part of the action cache key.
    pub client_env: BTreeMap<String, String>,
    /// `--explain=<file>`: record execution explanations to a file.
    pub explain: Option<std::path::PathBuf>,
    /// `--output_filter=<regex>`: filter action output to targets matching pattern.
    pub output_filter: Option<String>,
    pub bust_action_caches: Option<String>,
    pub materialize_param_files: bool,
    pub expand_param_files: bool,
    pub rewind_lost_inputs: bool,
    pub experimental_check_rewound_action_outputs: bool,
    pub experimental_max_repeated_lost_inputs: usize,
    pub experimental_precise_rewinding: bool,
    pub cache_computed_file_digests: usize,
    pub experimental_cpu_load_scheduling: bool,
    pub experimental_memory_load_scheduling: bool,
    pub allowed_strategies_by_exec_platform: Vec<(String, Vec<String>)>,
    pub experimental_async_execution: bool,
    pub async_execution_max_concurrent_actions: usize,
    pub hide_aspect_results: Vec<String>,
    pub experimental_use_validation_aspect: bool,
}

/// Resolves the environment of a spawn: fixed values, then the inherited variables present in
/// the client environment, minus the explicitly unset ones (`ActionEnvironment.resolve`).
pub fn resolve_spawn_env(
    fixed: &BTreeMap<String, String>,
    inherited: &std::collections::BTreeSet<String>,
    unset: &std::collections::BTreeSet<String>,
    client_env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    zel_util::action_env::ActionEnvironment::create(fixed.clone(), inherited.clone(), unset.clone())
        .resolved(client_env)
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Locates `name = "<target>"` in the package's BUILD file and returns Bazel's
/// `path:line:col` location (col of the `(` that opens the rule call).
pub fn build_file_location(workspace: Option<&PathFragment>, label: &zel_cmdline::Label) -> String {
    let pkg = label.package_identifier().package_fragment();
    let Some(ws) = workspace else {
        return format!("{pkg}/BUILD");
    };
    for name in ["BUILD.bazel", "BUILD"] {
        let rel = if pkg.path_string().is_empty() {
            ws.get_relative_str(name)
        } else {
            ws.get_relative(pkg).get_relative_str(name)
        };
        let Ok(text) = std::fs::read_to_string(rel.path_string()) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        let needle_a = format!("name = \"{}\"", label.name());
        let needle_b = format!("name=\"{}\"", label.name());
        for (i, l) in lines.iter().enumerate() {
            if l.contains(&needle_a) || l.contains(&needle_b) {
                // Walk back to the line that starts the rule call.
                let mut j = i;
                loop {
                    let t = lines[j];
                    if !t.starts_with(' ') && !t.starts_with('\t') && t.contains('(') {
                        let col = t.find('(').unwrap() + 1;
                        return format!("{}:{}:{}", rel, j + 1, col);
                    }
                    if j == 0 {
                        break;
                    }
                    j -= 1;
                }
                return format!("{}:{}:1", rel, i + 1);
            }
        }
        return format!("{}:1:1", rel);
    }
    format!("{pkg}/BUILD")
}


pub static PRETTY_SUBCOMMANDS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn describe_command_pretty(argv: &[String], env: &BTreeMap<String, String>, execroot: &PathFragment) -> String {
    let mut s = format!("(cd {} && \\\n  exec env - \\\n", shell_quote(execroot.path_string()));
    for (k, v) in env {
        s.push_str(&format!("    {}={} \\\n", k, shell_quote(v)));
    }
    let quoted: Vec<String> = argv.iter().map(|a| shell_quote(a)).collect();
    for (i, a) in quoted.iter().enumerate() {
        let indent = if i == 0 { "  " } else { "    " };
        if i + 1 == quoted.len() {
            s.push_str(&format!("{indent}{a})"));
        } else {
            s.push_str(&format!("{indent}{a} \\\n"));
        }
    }
    s
}

fn describe_command(
    argv: &[String],
    env: &BTreeMap<String, String>,
    execroot: &PathFragment,
    verbose: bool,
) -> String {
    let quoted: Vec<String> = argv.iter().map(|a| shell_quote(a)).collect();
    if verbose {
        let mut s = format!("(cd {} && \\\n  exec env - \\\n", shell_quote(execroot.path_string()));
        for (k, v) in env {
            s.push_str(&format!("    {}={} \\\n", k, shell_quote(v)));
        }
        s.push_str(&format!("  {})", quoted.join(" ")));
        s
    } else {
        format!(" {}", quoted.join(" "))
    }
}

static COMPUTED_DIGEST_CACHE: std::sync::RwLock<Option<(usize, BTreeMap<String, String>)>> = std::sync::RwLock::new(None);

pub fn invalidate_computed_digest(path: &str) {
    if let Ok(mut w) = COMPUTED_DIGEST_CACHE.write() {
        if let Some((_, ref mut map)) = *w {
            map.remove(path);
        }
    }
}

/// Digest of a produced output: file contents, or for directories (tree artifacts) a digest over
/// the sorted `(relative path, content digest)` listing. `None` when the path does not exist.
pub fn path_digest(digest: DigestFunction, fs: &dyn FileSystem, path: &PathFragment) -> Option<String> {
    if let Ok(r) = COMPUTED_DIGEST_CACHE.read() {
        if let Some((_, ref map)) = *r {
            if let Some(cached) = map.get(path.path_string()) {
                return Some(cached.clone());
            }
        }
    }
    let st = fs.stat(path, true).ok()?;
    if st.is_directory {
        fn walk(digest: DigestFunction, fs: &dyn FileSystem, base: &PathFragment, dir: &PathFragment, out: &mut Vec<u8>) {
            let mut entries = match fs.readdir(dir) {
                Ok(r) => r,
                Err(_) => return,
            };
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            for e in entries {
                let p = dir.get_relative_str(&e.name);
                let rel = p.path_string().strip_prefix(base.path_string()).unwrap_or(p.path_string()).to_string();
                out.extend_from_slice(rel.as_bytes());
                out.push(0);
                match fs.stat(&p, true) {
                    Ok(s) if s.is_directory => walk(digest, fs, base, &p, out),
                    Ok(_) => {
                        if let Ok(b) = fs.read_file(&p) {
                            out.extend_from_slice(digest.hex(&b).as_bytes());
                        }
                    }
                    Err(_) => {}
                }
                out.push(0);
            }
        }
        let mut buf = Vec::new();
        walk(digest, fs, path, path, &mut buf);
        let res = digest.hex(&buf);
        if let Ok(mut w) = COMPUTED_DIGEST_CACHE.write() {
            if let Some((cap, ref mut map)) = *w {
                if map.len() < cap {
                    map.insert(path.path_string().to_string(), res.clone());
                }
            }
        }
        return Some(res);
    }
    let res = fs.read_file(path).ok().map(|b| digest.hex(&b));
    if let (Some(ref v), Ok(mut w)) = (&res, COMPUTED_DIGEST_CACHE.write()) {
        if let Some((cap, ref mut map)) = *w {
            if map.len() < cap {
                map.insert(path.path_string().to_string(), v.clone());
            }
        }
    }
    res
}

/// Parses a Makefile-style dependency file (`.d`, as written by `gcc -MD`) into the prerequisite
/// paths (`CppCompileAction.discoverInputsFromDotdFiles`): the target before the first
/// unescaped `:` is dropped, `\\`-newline continuations and `\\ ` escapes are handled.
pub fn parse_depfile(text: &str) -> Vec<String> {
    let joined = text.replace("\\\r\n", " ").replace("\\\n", " ");
    let mut out: Vec<String> = Vec::new();
    for rule in joined.lines() {
        let rule = rule.trim();
        if rule.is_empty() {
            continue;
        }
        // Find the first ':' that is not part of a Windows drive letter.
        let bytes = rule.as_bytes();
        let mut colon = None;
        for (i, &b) in bytes.iter().enumerate() {
            if b == b':' && !(i == 1 && bytes[0].is_ascii_alphabetic() && bytes.get(2).map_or(false, |c| *c == b'/' || *c == b'\\')) {
                colon = Some(i);
                break;
            }
        }
        let Some(c) = colon else { continue };
        let mut cur = String::new();
        let mut chars = rule[c + 1..].chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '\\' if chars.peek() == Some(&' ') => {
                    cur.push(' ');
                    chars.next();
                }
                ' ' | '\t' => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                c => cur.push(c),
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

/// The parts of `action` that define it for the action cache key.
pub fn action_key_parts(action: &ActionAnalysisMetadata) -> ActionKeyParts {
    let mut p = ActionKeyParts {
        mnemonic: action.mnemonic().to_string(),
        input_paths: action.inputs().iter().map(|a| a.exec_path.path_string().to_string()).collect(),
        output_paths: action.outputs().iter().map(|a| a.exec_path.path_string().to_string()).collect(),
        ..Default::default()
    };
    p.input_paths.sort();
    match action {
        ActionAnalysisMetadata::SpawnAction { argv, env, inherited_env, unset_env, execution_info, .. } => {
            p.kind = "spawn".into();
            p.command_line = argv.clone();
            p.env = env.clone();
            p.inherited_env = inherited_env.iter().cloned().collect();
            p.unset_env = unset_env.iter().cloned().collect();
            p.execution_info = execution_info.clone();
        }
        ActionAnalysisMetadata::FileWriteAction { content, is_executable, .. } => {
            p.kind = "file_write".into();
            p.payload = content.clone();
            p.payload.push(*is_executable as u8);
        }
        ActionAnalysisMetadata::SymlinkAction { input_path, .. } => {
            p.kind = "symlink".into();
            p.payload = input_path.path_string().as_bytes().to_vec();
        }
        ActionAnalysisMetadata::TemplateExpansionAction { substitutions, .. } => {
            p.kind = "template".into();
            for (k, v) in substitutions {
                p.payload.extend_from_slice(k.as_bytes());
                p.payload.push(0);
                p.payload.extend_from_slice(v.as_bytes());
                p.payload.push(0);
            }
        }
    }
    p
}

/// Executes an action with `CompactPersistentActionCache` checking (no subcommand/verbose options).
pub fn execute_action(
    action: &ActionAnalysisMetadata,
    execroot: &PathFragment,
    fs: &dyn FileSystem,
    action_cache: &CompactPersistentActionCache,
    reporter: &Reporter,
    announce_subcommands: bool,
) -> io::Result<ActionExecutionOutcome> {
    execute_action_with(
        action,
        execroot,
        fs,
        action_cache,
        reporter,
        &ExecOptions {
            announce_subcommands,
            ..Default::default()
        },
    )
}

/// Executes an `ActionAnalysisMetadata` honoring `ExecOptions`. Non-zero exits, timeouts, missing
/// inputs and missing declared outputs yield an outcome with `failure` set (Bazel's
/// `ActionExecutionException` semantics); declared outputs of failed actions are removed and
/// their action-cache entries dropped.
pub fn execute_action_with(
    action: &ActionAnalysisMetadata,
    execroot: &PathFragment,
    fs: &dyn FileSystem,
    action_cache: &CompactPersistentActionCache,
    reporter: &Reporter,
    opts: &ExecOptions,
) -> io::Result<ActionExecutionOutcome> {
    let digest = opts.digest;
    let outputs = action.outputs();
    let started = std::time::Instant::now();
    let abs = |p: &PathFragment| execroot.get_relative(p);

    let parts = action_key_parts(action);
    let action_key = action_cache.compute_action_key(&parts);
    // `ActionCacheChecker.computeEffectiveEnvironment`: the values of the inherited variables
    // that exist in the client environment.
    let used_client_env: BTreeMap<String, String> = match action {
        ActionAnalysisMetadata::SpawnAction { inherited_env, .. } => inherited_env
            .iter()
            .filter_map(|k| opts.client_env.get(k).map(|v| (k.clone(), v.clone())))
            .collect(),
        _ => BTreeMap::new(),
    };
    let env_digest = action_cache.client_env_digest(&used_client_env);
    let declared_inputs: Vec<String> = parts.input_paths.clone();
    let md_digest_of = |discovered: &[String]| -> String {
        let mut all: Vec<(String, Option<String>)> = Vec::new();
        for p in declared_inputs.iter().chain(discovered.iter()) {
            let d = path_digest(digest, fs, &execroot.get_relative_str(p));
            all.push((p.clone(), d));
        }
        all.sort();
        all.dedup();
        action_cache.compute_md_digest(&all)
    };

    let is_busted = if let Some(bust_target) = &opts.bust_action_caches {
        let owner_canon = action.owner().canonical_form();
        let owner_str = action.owner().to_string();
        bust_target == &owner_canon
            || bust_target == &owner_str
            || bust_target.trim_start_matches('@') == owner_canon.trim_start_matches('@')
            || bust_target.trim_start_matches('@') == owner_str.trim_start_matches('@')
    } else {
        false
    };

    // Cache check (`ActionCacheChecker.mustExecute`): every output must exist and match the
    // recorded digest, and the action key / inputs (incl. discovered inputs) must be unchanged.
    if let Some(primary) = outputs.first() {
        let current: Vec<(String, Option<String>)> = outputs
            .iter()
            .map(|o| (o.exec_path.path_string().to_string(), path_digest(digest, fs, &abs(&o.exec_path))))
            .collect();
        let reason = action_cache.must_execute(
            primary.exec_path.path_string(),
            &action_key,
            &env_digest,
            &md_digest_of,
            &current,
        );
        if !is_busted && reason == MustExecuteReason::UpToDate {
            return Ok(ActionExecutionOutcome::ok(true));
        }
        if let Some(path) = &opts.explain {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let reason_desc = if is_busted {
                    "unconditional execution is requested.".to_string()
                } else {
                    format!("{}.", reason.explanation())
                };
                let _ = writeln!(f, "Executing action '{}': {reason_desc}", action.mnemonic());
            }
        }
    }
    if opts.check_up_to_date {
        let describe = match action {
            ActionAnalysisMetadata::SpawnAction { mnemonic, progress_message, owner, .. } => {
                if progress_message.is_empty() {
                    format!("{mnemonic} {owner}")
                } else {
                    progress_message.clone()
                }
            }
            ActionAnalysisMetadata::FileWriteAction { output, owner, .. } => {
                if output.root_relative_path.path_string().ends_with(".repo_mapping") {
                    format!("Writing repo mapping manifest for {owner}")
                } else if output.root_relative_path.path_string().ends_with(".runfiles_manifest") {
                    format!("Creating source manifest for {owner}")
                } else {
                    format!("Writing file {}", output.root_relative_path)
                }
            }
            ActionAnalysisMetadata::TemplateExpansionAction { output, .. } => {
                format!("Expanding template {}", output.root_relative_path)
            }
            ActionAnalysisMetadata::SymlinkAction { output, .. } => format!("Creating symlink {}", output.root_relative_path),
        };
        let mut messages = vec![format!("ERROR: action '{describe}' is not up-to-date")];
        if matches!(action, ActionAnalysisMetadata::SymlinkAction { .. }) {
            messages.push(format!("ERROR: action 'Writing repo mapping manifest for {}' is not up-to-date", action.owner()));
        }
        return Ok(ActionExecutionOutcome {
            cache_hit: false,
            exit_code: 1,
            stdout: Vec::new(),
            stderr: Vec::new(),
            failure: Some(ActionFailure {
                messages,
                exit_code: 1,
            }),
            wall_time_ms: 0,
            runner: None,
            progress: String::new(),
        });
    }

    let owner = action.owner();
    let loc = build_file_location(opts.workspace_dir.as_ref(), owner);
    let mut outcome = ActionExecutionOutcome::ok(false);

    // Fail helper
    let fail = |messages: Vec<String>, code: i32, stdout: Vec<u8>, stderr: Vec<u8>, runner: Option<String>| {
        // Bazel removes outputs of failed actions and forgets their cache entries.
        for o in &outputs {
            let _ = fs.delete(&execroot.get_relative(&o.exec_path));
        }
        if let Some(p) = outputs.first() {
            action_cache.remove(p.exec_path.path_string());
        }
        ActionExecutionOutcome {
            cache_hit: false,
            exit_code: code,
            stdout,
            stderr,
            failure: Some(ActionFailure {
                messages,
                exit_code: code,
            }),
            wall_time_ms: started.elapsed().as_millis() as u64,
            runner,
            progress: String::new(),
        }
    };

    match action {
        ActionAnalysisMetadata::FileWriteAction {
            output,
            content,
            is_executable,
            ..
        } => {
            let out_path = execroot.get_relative(&output.exec_path);
            if let Some(parent) = out_path.parent_directory() {
                fs.create_directory_and_parents(&parent)?;
            }
            let _ = fs.delete(&out_path);
            if output.exec_path.path_string().ends_with(".runfiles_manifest") {
                let text = String::from_utf8_lossy(content);
                let er = execroot.path_string();
                let mut rewritten = String::new();
                for line in text.lines() {
                    if let Some((k, v)) = line.split_once(' ') {
                        let abs_v = if v.is_empty() || v.starts_with('/') {
                            v.to_string()
                        } else {
                            format!("{er}/{v}")
                        };
                        rewritten.push_str(k);
                        rewritten.push(' ');
                        rewritten.push_str(&abs_v);
                        rewritten.push('\n');
                    } else {
                        rewritten.push_str(line);
                        rewritten.push('\n');
                    }
                }
                fs.write_file(&out_path, rewritten.as_bytes())?;
            } else {
                fs.write_file(&out_path, content)?;
            }
            #[cfg(unix)]
            if *is_executable {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(out_path.path_string()) {
                    let mut perms = meta.permissions();
                    perms.set_mode(0o755);
                    let _ = std::fs::set_permissions(out_path.path_string(), perms);
                }
            }
            let _ = is_executable;
        }
        ActionAnalysisMetadata::SymlinkAction {
            input_path, output, is_executable, ..
        } => {
            let out_path = execroot.get_relative(&output.exec_path);
            if let Some(parent) = out_path.parent_directory() {
                fs.create_directory_and_parents(&parent)?;
            }
            let _ = fs.delete(&out_path);
            // Bazel links to the absolute path `<execroot>/<input_path>`.
            let target = if input_path.path_string().starts_with('/') {
                input_path.clone()
            } else {
                execroot.get_relative(input_path)
            };
            if *is_executable {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = std::fs::metadata(target.path_string()) {
                        if meta.permissions().mode() & 0o111 == 0 {
                            return Ok(fail(
                                vec![format!(
                                    "ERROR: {loc}: Creating symlink {} failed: failed to create symbolic link '{}': file '{}' is not executable",
                                    output.root_relative_path, output.exec_path, input_path
                                )],
                                1,
                                Vec::new(),
                                Vec::new(),
                                None,
                            ));
                        }
                    }
                }
            }
            if fs.create_symbolic_link(&out_path, &target).is_err() {
                let src = target.clone();
                match fs.read_file(&src) {
                    Ok(bytes) => fs.write_file(&out_path, &bytes)?,
                    Err(_) => {
                        return Ok(fail(
                            vec![format!(
                                "ERROR: {loc}: Creating symlink {} failed: missing input file '{}'",
                                output.root_relative_path, input_path
                            )],
                            1,
                            Vec::new(),
                            Vec::new(),
                            None,
                        ))
                    }
                }
            }
        }
        ActionAnalysisMetadata::TemplateExpansionAction {
            template,
            output,
            substitutions,
            is_executable,
            ..
        } => {
            let tmpl_path = execroot.get_relative(&template.exec_path);
            let raw = match fs.read_file(&tmpl_path) {
                Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                Err(_) => {
                    return Ok(fail(
                        vec![format!(
                            "ERROR: {loc}: Expanding template {} failed: missing input file '{}'",
                            output.root_relative_path, template.root_relative_path
                        )],
                        1,
                        Vec::new(),
                        Vec::new(),
                        None,
                    ))
                }
            };
            let mut expanded = raw;
            for (k, v) in substitutions {
                expanded = expanded.replace(k, v);
            }
            let out_path = execroot.get_relative(&output.exec_path);
            if let Some(parent) = out_path.parent_directory() {
                fs.create_directory_and_parents(&parent)?;
            }
            let _ = fs.delete(&out_path);
            fs.write_file(&out_path, expanded.as_bytes())?;
            #[cfg(unix)]
            if *is_executable {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(out_path.path_string()) {
                    let mut perms = meta.permissions();
                    perms.set_mode(0o755);
                    let _ = std::fs::set_permissions(out_path.path_string(), perms);
                }
            }
            let _ = is_executable;
        }
        ActionAnalysisMetadata::SpawnAction {
            owner,
            mnemonic,
            progress_message,
            argv,
            env,
            inherited_env,
            unset_env,
            inputs,
            outputs: spawn_outputs,
            execution_info,
            java_compile,
            ..
        } => {
            let resolved_env = xcode_local_env(resolve_spawn_env(env, inherited_env, unset_env, &opts.client_env), execution_info);
            let env = &resolved_env;
            let progress = if progress_message.is_empty() {
                format!("{} {}", mnemonic, owner)
            } else {
                progress_message.clone()
            };
            // `add_all(tree_artifact)` placeholders expand to the files of the (now produced)
            // directory (`expand_directories`).
            let argv_expanded = match zel_analysis::expand_tree_artifact_args(argv, &|dir| {
                let root = std::path::PathBuf::from(execroot.path_string()).join(dir);
                let mut files = Vec::new();
                let mut stack = vec![root.clone()];
                while let Some(d) = stack.pop() {
                    let rd = std::fs::read_dir(&d).map_err(|e| format!("cannot expand directory '{dir}': {e}"))?;
                    for e in rd.flatten() {
                        let p = e.path();
                        if p.is_dir() {
                            stack.push(p);
                        } else if let Ok(rel) = p.strip_prefix(&root) {
                            files.push(format!("{dir}/{}", rel.to_string_lossy()));
                        }
                    }
                }
                files.sort();
                Ok(files)
            }) {
                Ok(a) => a,
                Err(msg) => {
                    return Ok(fail(
                        vec![format!("ERROR: {loc}: {progress} failed: {msg}")],
                        1,
                        Vec::new(),
                        Vec::new(),
                        None,
                    ))
                }
            };
            for input in inputs {
                let inp_path = execroot.get_relative(&input.exec_path);
                if !fs.exists(&inp_path, true) {
                    if !opts.rewind_lost_inputs {
                        return Ok(fail(
                            vec![format!(
                                "ERROR: {loc}: {progress}: Unexpected lost inputs (pass --rewind_lost_inputs to enable recovery): {}",
                                input.exec_path.path_string()
                            )],
                            1,
                            Vec::new(),
                            Vec::new(),
                            None,
                        ));
                    }
                }
            }
            let should_materialize = opts.materialize_param_files || opts.announce_subcommands || opts.verbose_failures;
            if should_materialize {
                for input in inputs {
                    let s = input.exec_path.path_string();
                    if s.ends_with(".params") || s.contains("-params") || s.ends_with(".param") {
                        let inp_path = execroot.get_relative(&input.exec_path);
                        if let Some(p) = inp_path.parent_directory() {
                            let _ = fs.create_directory_and_parents(&p);
                        }
                    }
                }
            }
            // `JavaCompileAction.execute` with `--experimental_java_classpath=bazel`: the first attempt
            // runs with a reduced classpath; when JavaBuilder asks for it (the `.jdeps` output has
            // `requires_reduced_classpath_fallback`) the action is re-run with the full classpath.
            let jc = java_compile
                .as_deref()
                .filter(|j| j.classpath_mode == "BAZEL" || j.classpath_mode == "BAZEL_NO_FALLBACK");
            let all_input_paths: Vec<String> = inputs.iter().map(|a| a.exec_path.path_string().to_string()).collect();
            let mut java_fallback = false;
            loop {
            let (attempt_inputs, optional_outputs): (Vec<PathFragment>, Vec<PathFragment>) = match jc {
                Some(spec) => {
                    let cp: Vec<String> = if java_fallback {
                        spec.transitive_inputs.clone()
                    } else {
                        let root = std::path::PathBuf::from(execroot.path_string());
                        match java_compile::reduced_classpath(spec, &|p| std::fs::read(root.join(p))) {
                            Ok(v) => v,
                            Err(e) => {
                                return Ok(fail(
                                    vec![format!("ERROR: {loc}: {progress} failed: .jdeps read IOException: {e}")],
                                    1,
                                    Vec::new(),
                                    Vec::new(),
                                    None,
                                ))
                            }
                        }
                    };
                    let params_abs = execroot.get_relative_str(&spec.classpath_params_path);
                    if let Some(parent) = params_abs.parent_directory() {
                        fs.create_directory_and_parents(&parent)?;
                    }
                    let _ = fs.delete(&params_abs);
                    fs.write_file(&params_abs, java_compile::classpath_param_file(spec, &cp, java_fallback).as_bytes())?;
                    let ins = java_compile::attempt_inputs(spec, &all_input_paths, &cp)
                        .into_iter()
                        .map(|p| PathFragment::create(&p))
                        .collect();
                    // `JavaSpawn.isMandatoryOutput`: only the .jdeps of the first attempt.
                    let opt = if java_fallback {
                        Vec::new()
                    } else {
                        spawn_outputs
                            .iter()
                            .filter(|a| a.exec_path.path_string() != spec.deps_proto)
                            .map(|a| a.exec_path.clone())
                            .collect()
                    };
                    (ins, opt)
                }
                None => (inputs.iter().map(|a| a.exec_path.clone()).collect(), Vec::new()),
            };
            let spawn = Spawn {
                mnemonic: mnemonic.clone(),
                owner_label: owner.canonical_form(),
                argv: argv_expanded.clone(),
                env: env.clone(),
                inputs: attempt_inputs,
                optional_outputs,
                outputs: spawn_outputs.iter().map(|a| a.exec_path.clone()).collect(),
                output_dirs: spawn_outputs.iter().filter(|a| a.is_tree_artifact).map(|a| a.exec_path.clone()).collect(),
                execution_info: execution_info.clone(),
                kill_delay: opts.runtime.as_ref().and_then(|r| r.kill_delay),
                tool_inputs: Vec::new(),
            };
            // Resources (`ResourceManager.acquire`) are held for the duration of the spawn.
            let _held = opts
                .runtime
                .as_ref()
                .map(|r| r.resources.acquire(&resources::ResourceSet::default_action()));
            let run = match &opts.runtime {
                Some(rt) => match rt.execute(&spawn, &progress, execroot, fs, reporter) {
                    Ok(r) => r,
                    Err(msg) => {
                        return Ok(fail(
                            vec![format!("ERROR: {loc}: {progress} failed: {msg}")],
                            1,
                            Vec::new(),
                            Vec::new(),
                            None,
                        ))
                    }
                },
                None => execute_spawn_local(&spawn, execroot, fs),
            };
            let res = run?;
            // Missing inputs fail the action before any command line is announced.
            if opts.announce_subcommands && !matches!(res.status, SpawnStatus::MissingInputs) {
                let (cmd_argv, cmd_env, cmd_cwd) = match &res.executed_command {
                    Some(c) if opts.runtime.as_ref().map_or(false, |r| r.sandbox_debug) => {
                        (c.argv.clone(), c.env.clone(), PathFragment::create(&c.cwd))
                    }
                    _ => (argv.clone(), env.clone(), execroot.clone()),
                };
                let cmd_argv = if opts.expand_param_files {
                    expand_param_file_args(&cmd_argv, execroot, fs)
                } else {
                    cmd_argv
                };
                let runner = &res.runner_name;
                let from_rule = if mnemonic == "Genrule" {
                    format!("genrule rule target {owner}")
                } else {
                    format!("rule target {owner}")
                };
                let cfg_hash = config_checksum_of(opts, &spawn_outputs);
                reporter.handle(Event::subcommand(format!(
                    "# {from_rule} [action '{progress}', configuration: {cfg_hash}, execution platform: @@platforms//host:host, mnemonic: {mnemonic}]\n{}\n# Configuration: {cfg_hash}\n# Execution platform: @@platforms//host:host\n# Runner: {runner}",
                    if PRETTY_SUBCOMMANDS.load(std::sync::atomic::Ordering::Relaxed) {
                        describe_command_pretty(&cmd_argv, &cmd_env, &cmd_cwd)
                    } else {
                        describe_command(&cmd_argv, &cmd_env, &cmd_cwd, true)
                    }
                )));
            }
            outcome.runner = Some(res.runner_name.clone());
            outcome.progress = progress.clone();
            let runner = outcome.runner.clone();
            let rel_of = |exec: &str| -> String {
                spawn_outputs
                    .iter()
                    .find(|a| a.exec_path.path_string() == exec)
                    .map(|a| a.root_relative_path.path_string().to_string())
                    .unwrap_or_else(|| exec.to_string())
            };
            // With --sandbox_debug the failure shows the sandbox's own command line.
        let is_sandbox_debug = opts.runtime.as_ref().map_or(false, |r| r.sandbox_debug)
            && res.runner_name.ends_with("-sandbox")
            && res.executed_command.is_some();
        if is_sandbox_debug {
            if let Some(dbg) = res.executed_command.as_ref().and_then(|c| c.debug_output.as_ref()) {
                reporter.handle(Event::debug(format!("Sandbox debug output for {mnemonic} {owner}:\n{dbg}")));
            }
        }
            let (fail_argv, fail_env, fail_cwd, fail_verbose) = match (&res.executed_command, &opts.runtime) {
                (Some(c), Some(rt)) if rt.sandbox_debug => {
                    (c.argv.clone(), c.env.clone(), PathFragment::create(&c.cwd), true)
                }
                // A sandboxed spawn ran in its sandbox execroot (`<base>/<runner>/<id>/execroot/<ws>`).
                (Some(c), _) if res.runner_name.ends_with("-sandbox") => {
                    (argv.clone(), env.clone(), PathFragment::create(&c.cwd), opts.verbose_failures)
                }
                _ => (argv.clone(), env.clone(), execroot.clone(), opts.verbose_failures),
            };
            let fail_argv = if opts.expand_param_files {
                expand_param_file_args(&fail_argv, execroot, fs)
            } else {
                fail_argv
            };
            match res.status {
                SpawnStatus::Success
                    if spawn_outputs.iter().any(|o| {
                        !o.is_tree_artifact
                            && !o.is_unresolved_symlink
                            && std::fs::metadata(execroot.get_relative(&o.exec_path).path_string()).map_or(false, |m| m.is_dir())
                    }) =>
                {
                    // `SkyframeActionExecutor`: a declared file output that is a directory.
                    let mut msgs: Vec<String> = spawn_outputs
                        .iter()
                        .filter(|o| {
                            !o.is_tree_artifact
                                && !o.is_unresolved_symlink
                                && std::fs::metadata(execroot.get_relative(&o.exec_path).path_string()).map_or(false, |m| m.is_dir())
                        })
                        .map(|o| format!("ERROR: {loc}: output '{}' of {owner} is a directory but was not declared as such", o.root_relative_path.path_string()))
                        .collect();
                    msgs.push(format!("ERROR: {loc}: {progress} failed: not all outputs were created or valid"));
                    return Ok(fail(msgs, 1, res.stdout, res.stderr, runner));
                }
                SpawnStatus::Success => {
                    // Like Bazel's `setOutputsReadOnlyAndExecutable`, every regular-file output of a
                    // spawn is executable (so `run` of an `executable = True` genrule output works).
                    #[cfg(unix)]
                    for o in spawn_outputs {
                        use std::os::unix::fs::PermissionsExt;
                        let p = execroot.get_relative(&o.exec_path);
                        if let Ok(meta) = std::fs::symlink_metadata(p.path_string()) {
                            if meta.is_file() {
                                let mut perms = meta.permissions();
                                perms.set_mode(perms.mode() | 0o111);
                                let _ = std::fs::set_permissions(p.path_string(), perms);
                            }
                        }
                    }
                    outcome.stdout = res.stdout;
                    outcome.stderr = res.stderr;
                }
                SpawnStatus::MissingInputs => {
                    let mut msgs = Vec::new();
                    for m in &res.missing {
                        let lbl = if m.starts_with("//") { m.clone() } else { format!("//:{m}") };
                        msgs.push(format!("ERROR: {loc}: {progress} failed: missing input file '{lbl}'"));
                    }
                    msgs.push(format!("ERROR: {loc}: {progress} failed: {} input file(s) do not exist", res.missing.len()));
                    return Ok(fail(msgs, 1, res.stdout, res.stderr, runner));
                }
                SpawnStatus::MissingOutputs => {
                    let mut msgs: Vec<String> = res
                        .missing
                        .iter()
                        .map(|m| {
                            if mnemonic == "Genrule" {
                                format!(
                                    "ERROR: {loc}: declared output '{}' was not created by genrule. This is probably because the genrule actually didn't create this output, or because the output was a directory and the genrule was run remotely (note that only the contents of declared file outputs are copied from genrules run remotely)",
                                    rel_of(m)
                                )
                            } else {
                                format!("ERROR: {loc}: output '{}' was not created", rel_of(m))
                            }
                        })
                        .collect();
                    msgs.push(format!(
                        "ERROR: {loc}: {progress} failed: not all outputs were created or valid"
                    ));
                    // The spawn itself succeeded (exit 0); only its outputs are missing.
                    return Ok(fail(msgs, 0, res.stdout, res.stderr, runner));
                }
                SpawnStatus::Timeout => {
                    let from_rule = if mnemonic == "Genrule" {
                        format!("(from genrule rule target {owner})")
                    } else {
                        format!("(from rule target {owner})")
                    };
                    let desc = describe_command(&fail_argv, &fail_env, &fail_cwd, fail_verbose);
                    let msg = format!(
                        "ERROR: {loc}: {progress} failed: (Timeout): {} failed: error executing {} command {}{}",
                        argv.first().map(|a| a.rsplit('/').next().unwrap_or(a)).unwrap_or(""),
                        mnemonic,
                        from_rule,
                        desc,
                    );
                    return Ok(fail(vec![msg], res.exit_code, res.stdout, res.stderr, runner));
                }
                SpawnStatus::NonZeroExit | SpawnStatus::ExecutionFailed => {
                    let tool = fail_argv
                        .first()
                        .map(|a| a.rsplit('/').next().unwrap_or(a).to_string())
                        .unwrap_or_default();
                    let from_rule = if is_sandbox_debug {
                        String::new()
                    } else if mnemonic == "Genrule" {
                        format!("(from genrule rule target {owner})")
                    } else {
                        format!("(from rule target {owner})")
                    };
                    let cfg_hash = config_checksum_of(opts, &spawn_outputs);
                    let mut desc = describe_command(&fail_argv, &fail_env, &fail_cwd, fail_verbose);
                    if fail_verbose {
                        if is_sandbox_debug {
                            desc = format!("\n  {desc}");
                        } else {
                            // `CommandFailureUtils`: the `(from ...) ` header ends with a space, the
                            // command block starts on its own line indented by two.
                            desc = format!(" \n  {desc}\n# Configuration: {cfg_hash}\n# Execution platform: @@platforms//host:host");
                        }
                    }
                    let mut msgs = vec![format!(
                        "ERROR: {loc}: {progress} failed: (Exit {}): {tool} failed: error executing {mnemonic} command {}{}",
                        res.exit_code,
                        from_rule,
                        desc,
                    )];
                    if runner.as_deref().map_or(false, |r| r.ends_with("-sandbox")) && opts.runtime.as_ref().map_or(true, |r| !r.sandbox_debug) {
                        msgs.push("Use --sandbox_debug to see verbose messages from the sandbox and retain the sandbox build root for debugging".to_string());
                    }
                    return Ok(fail(msgs, res.exit_code, res.stdout, res.stderr, runner));
                }
            }
            if let Some(spec) = jc {
                if !java_fallback {
                    let jdeps = match fs.read_file(&execroot.get_relative_str(&spec.deps_proto)) {
                        Ok(bytes) => java_compile::parse_jdeps(&bytes),
                        Err(e) => Err(e.to_string()),
                    };
                    match jdeps {
                        Ok(d) if d.requires_reduced_classpath_fallback => {
                            // Delete the outputs of the reduced attempt and re-run with the full classpath.
                            for o in spawn_outputs {
                                let _ = fs.delete(&execroot.get_relative(&o.exec_path));
                            }
                            outcome.stdout.clear();
                            outcome.stderr.clear();
                            java_fallback = true;
                            continue;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            return Ok(fail(
                                vec![format!("ERROR: {loc}: {progress} failed: .jdeps read IOException: {e}")],
                                1,
                                Vec::new(),
                                Vec::new(),
                                outcome.runner.clone(),
                            ))
                        }
                    }
                }
            }
            break;
            }
        }
    }

    for out_art in &outputs {
        let out_path = execroot.get_relative(&out_art.exec_path);
        // Outputs of a remote/disk cache hit are only materialized when needed.
        let pending = opts.runtime.as_ref().and_then(|r| r.remote.as_ref()).map_or(false, |r| r.is_pending_output(out_art.exec_path.path_string()));
        if !pending && !fs.exists(&out_path, true) {
            return Ok(fail(
                vec![
                    format!(
                        "ERROR: {loc}: output '{}' was not created",
                        out_art.root_relative_path
                    ),
                    format!(
                        "ERROR: {loc}: {} failed: not all outputs were created or valid",
                        action.mnemonic()
                    ),
                ],
                1,
                Vec::new(),
                Vec::new(),
                outcome.runner.clone(),
            ));
        }
    }

    // Record the action in the cache (`ActionCacheChecker.updateActionCache`): output digests and
    // the metadata digest over declared + discovered inputs (from `.d` depfiles).
    if let Some(primary) = outputs.first() {
        let mut discovered: Vec<String> = Vec::new();
        if let ActionAnalysisMetadata::SpawnAction { .. } = action {
            for o in &outputs {
                if o.exec_path.path_string().ends_with(".d") {
                    if let Ok(b) = fs.read_file(&execroot.get_relative(&o.exec_path)) {
                        for dep in parse_depfile(&String::from_utf8_lossy(&b)) {
                            let rel = dep
                                .strip_prefix(&format!("{}/", execroot.path_string()))
                                .map(|s| s.to_string())
                                .unwrap_or(dep);
                            if !declared_inputs.contains(&rel) && !rel.starts_with('/') {
                                discovered.push(rel);
                            }
                        }
                    }
                }
            }
            discovered.sort();
            discovered.dedup();
        }
        let files: BTreeMap<String, String> = outputs
            .iter()
            .map(|o| {
                let p = abs(&o.exec_path);
                invalidate_computed_digest(p.path_string());
                (
                    o.exec_path.path_string().to_string(),
                    path_digest(digest, fs, &p).unwrap_or_default(),
                )
            })
            .collect();
        action_cache.put_entry(
            primary.exec_path.path_string(),
            ActionCacheEntry {
                action_key,
                used_client_env_digest: env_digest,
                md_digest: md_digest_of(&discovered),
                files,
                discovered_inputs: discovered,
            },
        );
    }
    if let Some(filter) = &opts.output_filter {
        if !action.owner().to_string().contains(filter) {
            outcome.stdout.clear();
            outcome.stderr.clear();
        }
    }
    outcome.wall_time_ms = started.elapsed().as_millis() as u64;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_cmdline::Label;
    use zel_vfs::InMemoryFileSystem;

    #[test]
    fn test_incremental_action_cache_hit() {
        let fs = InMemoryFileSystem::default();
        let execroot = PathFragment::create("/execroot/_main");
        fs.create_directory_and_parents(&execroot).unwrap();

        let lbl = Label::parse_canonical("//pkg:out").unwrap();
        let out_art = Artifact::derived(
            "darwin-fastbuild",
            PathFragment::create("pkg/out.txt"),
            lbl.clone(),
        );
        let action = ActionAnalysisMetadata::FileWriteAction {
            owner: lbl,
            output: out_art,
            content: b"cached content".to_vec(),
            is_executable: false,
        };

        let cache = CompactPersistentActionCache::new();
        let reporter = Reporter::new();

        // First execution: cache miss
        let out1 = execute_action(&action, &execroot, &fs, &cache, &reporter, false).unwrap();
        assert!(!out1.cache_hit);
        assert_eq!(out1.exit_code, 0);

        // Second execution without changes: cache hit!
        let out2 = execute_action(&action, &execroot, &fs, &cache, &reporter, false).unwrap();
        assert!(out2.cache_hit);
        assert_eq!(out2.exit_code, 0);
    }
}


/// `XcodeLocalEnvProvider.rewriteLocalEnv`: actions that use Xcode (`requires-darwin` or the
/// `XCODE_VERSION_OVERRIDE` / `APPLE_SDK_PLATFORM` variables) get `DEVELOPER_DIR` and `SDKROOT`
/// of the local Xcode (the selected one, `xcode-select -p`) and lose the override variables.
fn xcode_local_env(
    mut env: BTreeMap<String, String>,
    execution_info: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    if !cfg!(target_os = "macos") {
        return env;
    }
    let wants = env.contains_key("XCODE_VERSION_OVERRIDE")
        || env.contains_key("APPLE_SDK_PLATFORM")
        || execution_info.contains_key("requires-darwin");
    if !wants || (env.contains_key("DEVELOPER_DIR") && env.contains_key("SDKROOT")) {
        return env;
    }
    let developer_dir = match env.get("DEVELOPER_DIR").cloned() {
        Some(d) => d,
        None => match std::process::Command::new("/usr/bin/xcode-select").arg("-p").output() {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            _ => return env,
        },
    };
    let platform = env.get("APPLE_SDK_PLATFORM").cloned().unwrap_or_else(|| "MacOSX".to_string()).to_lowercase();
    let sdkroot = std::process::Command::new("/usr/bin/xcrun")
        .env("DEVELOPER_DIR", &developer_dir)
        .args(["--sdk", &platform, "--show-sdk-path"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    env.insert("DEVELOPER_DIR".to_string(), developer_dir);
    if let Some(sdk) = sdkroot {
        env.entry("SDKROOT".to_string()).or_insert(sdk);
    }
    for k in ["XCODE_VERSION_OVERRIDE", "APPLE_SDK_VERSION_OVERRIDE", "APPLE_SDK_PLATFORM"] {
        env.remove(k);
    }
    env
}

fn expand_param_file_args(argv: &[String], execroot: &PathFragment, fs: &dyn FileSystem) -> Vec<String> {
    let mut expanded = Vec::new();
    for arg in argv {
        if let Some(param_path) = arg.strip_prefix('@') {
            let p = execroot.get_relative_str(param_path);
            if let Ok(b) = fs.read_file(&p) {
                let content = String::from_utf8_lossy(&b);
                for line in content.lines() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        expanded.push(trimmed.to_string());
                    }
                }
                continue;
            }
        }
        expanded.push(arg.clone());
    }
    expanded
}

/// The checksum of the configuration an action runs in: derived from the output directory
/// (`bazel-out/<mnemonic>/...`) of its outputs and the analysed configurations.
fn config_checksum_of(opts: &ExecOptions, outputs: &[Artifact]) -> String {
    outputs
        .iter()
        .filter_map(|a| a.exec_path.path_string().split('/').nth(1).map(str::to_string))
        .find_map(|dir| opts.config_checksums.get(&dir).cloned())
        .unwrap_or_default()
}
