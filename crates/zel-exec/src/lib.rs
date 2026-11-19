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
pub mod bin_tools;
pub mod buildjar;
pub mod digest;
pub mod file_cache;
pub mod input_expander;
pub mod java_compile;
pub mod junit_runner;
pub mod layout;
pub mod resources;
pub mod runfiles_tree;
pub mod scheduler;
pub mod strategy;
pub mod symlinks;
pub mod test_runner;
pub mod test_status;
pub use scheduler::*;
use zel_analysis::{ActionAnalysisMetadata, Artifact};
use zel_events::{Event, Reporter};
use zel_remote::spawn_runner::{ExecMode, RemoteSpawnRunner, SpawnRunner};
use zel_sandbox::{execute_spawn_local, Spawn, SpawnStatus};
use zel_vfs::{FileSystem, PathFragment};

pub use action_cache::{
    ActionCacheEntry, ActionKeyParts, CompactPersistentActionCache, MustExecuteReason,
};
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
        if ws.is_dir() && er.is_absolute() {
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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActionFailure {
    /// `ERROR: ...` lines (without trailing newline) printed to the console.
    pub messages: Vec<String>,
    pub exit_code: i32,
    pub catastrophic: bool,
}

/// Builder for execution configuration (`ExecutorBuilder.java`).
#[derive(Debug, Clone, Default)]
pub struct ExecutorBuilder {
    action_execution_salt: Option<String>,
}

impl ExecutorBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_action_execution_salt(
        &mut self,
        salt: impl Into<String>,
    ) -> Result<&mut Self, String> {
        if self.action_execution_salt.is_some() {
            return Err("actionExecutionSalt was already set".to_string());
        }
        self.action_execution_salt = Some(salt.into());
        Ok(self)
    }

    pub fn action_execution_salt(&self) -> &str {
        self.action_execution_salt.as_deref().unwrap_or("")
    }
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
    /// `--experimental_local_lockfree_output`.
    pub local_lockfree_output: bool,
    /// `--debug_spawn_scheduler`.
    pub debug: bool,
}

impl Default for DynamicOptions {
    fn default() -> Self {
        Self {
            local_execution_delay_ms: 1000,
            ignore_local_signals: Vec::new(),
            cancel_remote_branch_on_local_win: true,
            local_lockfree_output: false,
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
        write!(
            f,
            "ExecRuntime {{ default_strategies: {:?} }}",
            self.registry.default_strategies()
        )
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
    outer_cancel: Option<zel_sandbox::process_wrapper::CancelFlag>,
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
    remote_publishing: bool,
}

impl Race {
    fn is_outer_cancelled(&self) -> bool {
        self.outer_cancel.as_ref().is_some_and(|c| c.is_cancelled())
    }

    fn cancel_all(&self) {
        self.local_cancel.cancel();
        self.remote_cancelled.store(true, Ordering::SeqCst);
        self.local_done.1.notify_all();
        self.changed.notify_all();
    }

    fn is_cancelled(&self, b: Branch) -> bool {
        if self.is_outer_cancelled() {
            self.cancel_all();
            return true;
        }
        match b {
            Branch::Local => self.local_cancel.is_cancelled(),
            Branch::Remote => self.remote_cancelled.load(Ordering::SeqCst),
        }
    }

    /// `DynamicSpawnStrategy.stopBranch` (preceded by `maybeIgnoreFailure`): the first branch to
    /// get here wins and cancels the other; the loser must not publish any output.
    fn lock_with_outer(
        &self,
        b: Branch,
        exit_code: i32,
        outer_lock: Option<&(dyn Fn(i32) -> bool + Send + Sync)>,
        from_deliver: bool,
    ) -> bool {
        // A local branch killed by an ignorable signal cancels itself in favor of remote.
        if b == Branch::Local
            && exit_code != 0
            && self.ignore_local_signals.contains(&(exit_code - 128))
        {
            return false;
        }
        if self.is_cancelled(b) {
            return false;
        }
        let first_claim = {
            let mut w = self.winner.lock().unwrap();
            match *w {
                Some(m) if m != b => return false,
                Some(_) => false,
                None => {
                    *w = Some(b);
                    true
                }
            }
        };
        if first_claim {
            match b {
                Branch::Remote => {
                    // `otherBranch.cancel()` + `otherBranch.getDoneSemaphore().acquire()`,
                    // aborting if outer cancellation arrives while waiting.
                    self.local_cancel.cancel();
                    let (m, cv) = &self.local_done;
                    let mut done = m.lock().unwrap();
                    while !*done && !self.is_cancelled(Branch::Remote) {
                        done = cv.wait(done).unwrap();
                    }
                    if self.is_cancelled(Branch::Remote) {
                        return false;
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
            if let Some(f) = outer_lock {
                if !f(exit_code) {
                    return false;
                }
            }
        }
        if b == Branch::Remote {
            let mut o = self.outcome.lock().unwrap();
            if self.is_cancelled(Branch::Remote) {
                return false;
            }
            if !from_deliver {
                o.remote_publishing = true;
            }
        }
        !self.is_cancelled(b)
    }

    fn deliver(&self, b: Branch, r: io::Result<zel_sandbox::SpawnResult>) {
        let keep = match &r {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => false,
            Ok(res) => self.lock_with_outer(b, res.exit_code, None, true),
            Err(_) => self.lock_with_outer(b, 1, None, true),
        };
        let mut o = self.outcome.lock().unwrap();
        if b == Branch::Remote {
            o.remote_publishing = false;
        }
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
    fn exec(
        &self,
        spawn: &Spawn,
        execroot: &PathFragment,
        fs: &dyn FileSystem,
    ) -> io::Result<zel_sandbox::SpawnResult> {
        match self
            .rt
            .run_local_kind(self.kind, spawn, execroot, fs, &self.hooks)
        {
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
        msg.push_str(
            "No usable dynamic_local_strategy found (and remote execution disabled) for action ",
        );
    } else if !can_local {
        msg.push_str(
            "No usable dynamic_remote_strategy found (and local execution disabled) for action ",
        );
    } else {
        msg.push_str(
            "No usable dynamic_local_strategy or dynamic_remote_strategy found for action ",
        );
    }
    msg.push_str(&spawn.mnemonic);
    msg.push_str(". ");
    msg
}

impl ExecRuntime {
    fn sandbox_runner(
        &self,
        kind: zel_sandbox::sandbox::SandboxKind,
    ) -> Option<&zel_sandbox::sandbox::SandboxRunner> {
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
                    let allowed = zel_util::java_regex::Regex::new(pat)
                        .map(|r| r.full_match(&spawn.mnemonic))
                        .unwrap_or(true);
                    if !allowed {
                        let msg = format!(
                            "Action type {} is not allowed to run locally due to regex filter: {pat}",
                            spawn.mnemonic
                        );
                        return Ok(Ok(zel_sandbox::SpawnResult {
                            status: SpawnStatus::ExecutionDenied,
                            exit_code: -1,
                            stdout: Vec::new(),
                            stderr: format!("{msg}\n").into_bytes(),
                            wall_time_ms: 0,
                            runner_name: "local".into(),
                            failure_message: Some(msg),
                            ..Default::default()
                        }));
                    }
                }
                let mut r = zel_sandbox::execute_spawn_local_with(spawn, execroot, fs, hooks);
                if let Ok(r) = &mut r {
                    r.runner_name = "local".into();
                }
                r
            }
            strategy::StrategyKind::DarwinSandbox => {
                match self.sandbox_runner(zel_sandbox::sandbox::SandboxKind::Darwin) {
                    Some(r) => r.exec_with(spawn, hooks),
                    None => return Err("darwin-sandbox is not available".into()),
                }
            }
            strategy::StrategyKind::LinuxSandbox => {
                match self.sandbox_runner(zel_sandbox::sandbox::SandboxKind::Linux) {
                    Some(r) => r.exec_with(spawn, hooks),
                    None => return Err("linux-sandbox is not available".into()),
                }
            }
            strategy::StrategyKind::ProcessWrapperSandbox => {
                match self.sandbox_runner(zel_sandbox::sandbox::SandboxKind::ProcessWrapper) {
                    Some(r) => r.exec_with(spawn, hooks),
                    None => return Err("processwrapper-sandbox is not available".into()),
                }
            }
            strategy::StrategyKind::Worker => match &self.worker {
                Some(w) => w.exec_with(spawn, hooks),
                None => return Err("worker strategy is not available".into()),
            },
            k => {
                return Err(format!(
                    "strategy {} cannot execute spawns in this build",
                    k.name()
                ))
            }
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
                let fallback = LocalKindRunner {
                    rt: self,
                    kind: self.registry.remote_local_fallback(),
                    hooks: Default::default(),
                };
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
                    let local = LocalKindRunner {
                        rt: self,
                        kind: k,
                        hooks: hooks.clone(),
                    };
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
        self.execute_opt(
            spawn,
            Some(description),
            Some(description),
            execroot,
            fs,
            reporter,
        )
    }

    pub fn execute_opt(
        self: &Arc<Self>,
        spawn: &Spawn,
        strategy_description: Option<&str>,
        progress_event: Option<&str>,
        execroot: &PathFragment,
        fs: &dyn FileSystem,
        reporter: &Reporter,
    ) -> Result<io::Result<zel_sandbox::SpawnResult>, String> {
        self.execute_opt_with_hooks(
            spawn,
            strategy_description,
            progress_event,
            execroot,
            fs,
            reporter,
            &Default::default(),
        )
    }

    pub fn execute_with_hooks(
        self: &Arc<Self>,
        spawn: &Spawn,
        description: &str,
        execroot: &PathFragment,
        fs: &dyn FileSystem,
        reporter: &Reporter,
        hooks: &zel_sandbox::ExecHooks,
    ) -> Result<io::Result<zel_sandbox::SpawnResult>, String> {
        self.execute_opt_with_hooks(
            spawn,
            Some(description),
            Some(description),
            execroot,
            fs,
            reporter,
            hooks,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn execute_opt_with_hooks(
        self: &Arc<Self>,
        spawn: &Spawn,
        strategy_description: Option<&str>,
        progress_event: Option<&str>,
        execroot: &PathFragment,
        fs: &dyn FileSystem,
        reporter: &Reporter,
        hooks: &zel_sandbox::ExecHooks,
    ) -> Result<io::Result<zel_sandbox::SpawnResult>, String> {
        if self.remote.is_none() {
            if let Some(e) = self.remote_setup_error.clone() {
                return Err(format!(
                    "Failed to query remote execution capabilities: {e}"
                ));
            }
        }
        let (mut strategies, resolved_progress) =
            self.registry.resolve_opt(spawn, strategy_description)?;
        let progress = resolved_progress.or_else(|| {
            progress_event
                .filter(|p| !p.is_empty())
                .map(|p| p.to_string())
        });
        if let Some(p) = progress {
            reporter.handle(Event::progress(p));
        }
        let chosen = strategies.remove(0);
        if chosen == strategy::StrategyKind::Dynamic {
            return self.exec_dynamic_with_hooks(spawn, execroot, fs, hooks);
        }
        self.run_strategy(chosen, spawn, execroot, fs, hooks)
    }

    /// `DynamicSpawnStrategy.exec`: runs the local and the remote strategy of the spawn in
    /// parallel; the first branch to finish publishes its outputs and cancels the other.
    fn exec_dynamic_with_hooks(
        self: &Arc<Self>,
        spawn: &Spawn,
        execroot: &PathFragment,
        fs: &dyn FileSystem,
        outer_hooks: &zel_sandbox::ExecHooks,
    ) -> Result<io::Result<zel_sandbox::SpawnResult>, String> {
        if outer_hooks.is_cancelled() {
            return Ok(Err(zel_sandbox::cancelled_error()));
        }
        let local = self.registry.dynamic_can_exec_local(spawn);
        let remote = self.registry.dynamic_can_exec_remote(spawn);
        let (local, remote) = match (local, remote) {
            (None, None) => {
                return Err(format!(
                    "Spawn is not executable in local: {}",
                    spawn_not_executable_reason(spawn)
                ));
            }
            // `maybeExecuteNonDynamically`.
            (None, Some(r)) => return self.run_strategy(r, spawn, execroot, fs, outer_hooks),
            (Some(l), None) => return self.run_strategy(l, spawn, execroot, fs, outer_hooks),
            (Some(l), Some(r)) => (l, r),
        };
        let race = Arc::new(Race {
            winner: Mutex::new(None),
            local_cancel: Default::default(),
            outer_cancel: outer_hooks.cancel.clone(),
            remote_cancelled: AtomicBool::new(false),
            local_done: (Mutex::new(false), Condvar::new()),
            outcome: Mutex::new(RaceOutcome::default()),
            changed: Condvar::new(),
            ignore_local_signals: self.dynamic.ignore_local_signals.clone(),
            cancel_remote_on_local_win: self.dynamic.cancel_remote_branch_on_local_win,
        });
        let local_lockfree =
            self.dynamic.local_lockfree_output || outer_hooks.local_lockfree_output;
        let hooks_for = |b: Branch| {
            let race_lock = race.clone();
            let outer_lock = outer_hooks.lock_outputs.clone();
            let stream_out = if b == Branch::Local {
                outer_hooks.stream_out.as_ref().map(|cb| {
                    let cb = cb.clone();
                    let race_stream = race.clone();
                    let wrapped: zel_sandbox::StreamOutHook = Arc::new(move |chunk: &[u8]| {
                        if !race_stream.is_cancelled(Branch::Local) {
                            cb(chunk);
                        }
                    });
                    wrapped
                })
            } else {
                None
            };
            zel_sandbox::ExecHooks {
                cancel: (b == Branch::Local).then(|| race.local_cancel.clone()),
                lock_outputs: Some(Arc::new(move |code| {
                    race_lock.lock_with_outer(b, code, outer_lock.as_deref(), false)
                })),
                runfiles_updater: outer_hooks.runfiles_updater.clone(),
                local_lockfree_output: b == Branch::Local && local_lockfree,
                stream_out,
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
            let scope_done = Arc::new(AtomicBool::new(false));
            if let Some(outer_c) = outer_hooks.cancel.clone() {
                let race_watch = race.clone();
                let done_watch = scope_done.clone();
                scope.spawn(move || {
                    let mut guard = race_watch.outcome.lock().unwrap();
                    while !done_watch.load(Ordering::SeqCst)
                        && guard.result.is_none()
                        && guard.finished < 2
                    {
                        if outer_c.is_cancelled() {
                            drop(guard);
                            race_watch.cancel_all();
                            break;
                        }
                        let (next_guard, _) = race_watch
                            .changed
                            .wait_timeout(guard, std::time::Duration::from_millis(5))
                            .unwrap();
                        guard = next_guard;
                    }
                });
            }
            let hooks = hooks_for(Branch::Local);
            let race2 = race.clone();
            scope.spawn(move || {
                let delay = self
                    .delay_local
                    .load(Ordering::SeqCst)
                    .then_some(self.dynamic.local_execution_delay_ms);
                let r = (|| {
                    if let Some(ms) = delay {
                        let until =
                            std::time::Instant::now() + std::time::Duration::from_millis(ms);
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
            while o.result.is_none()
                && o.finished < 2
                && (!outer_hooks.is_cancelled() || o.remote_publishing)
            {
                o = race.changed.wait(o).unwrap();
            }
            if outer_hooks.is_cancelled() {
                drop(o);
                race.cancel_all();
                let mut o = race.outcome.lock().unwrap();
                while o.remote_publishing {
                    o = race.changed.wait(o).unwrap();
                }
                drop(o);
                scope_done.store(true, Ordering::SeqCst);
                race.changed.notify_all();
                return Ok(Err(zel_sandbox::cancelled_error()));
            }
            let res = match o.result.take() {
                Some((_, r)) => Ok(r),
                None => Err(format!("Neither branch of {} completed.", spawn.mnemonic)),
            };
            drop(o);
            scope_done.store(true, Ordering::SeqCst);
            race.changed.notify_all();
            res
        })
    }

    /// Downloads the outputs known only by digest (`--remote_download_outputs`) for the given
    /// exec-root-relative paths.
    pub fn materialize_remote_outputs(
        &self,
        paths: &[String],
        execroot: &str,
    ) -> Result<usize, String> {
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
    /// Pre-compiled / lazily cached `--output_filter` shared across actions in a build.
    pub compiled_output_filter: CompiledOutputFilter,
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
    /// Pre-configured tool runfiles trees, keyed by tool executable exec path.
    pub tool_runfiles: BTreeMap<String, ToolRunfilesSpec>,
    /// Salt mixed into action cache keys (`ExecutorBuilder.setActionExecutionSalt`).
    pub action_execution_salt: String,
    /// Per-build source / non-derived input metadata cache (`SingleBuildFileCache`).
    pub file_cache: Option<Arc<file_cache::SingleBuildFileCache<'static>>>,
}

/// Shared compiled `--output_filter` cache (`OutputFilter.RegexOutputFilter`).
///
/// Compiles the Java regex pattern once per build / `ExecOptions` (and shares it across clones via
/// `Arc`) rather than recompiling on every non-`TestRunner` action.
#[derive(Debug, Clone, Default)]
pub struct CompiledOutputFilter(
    Arc<std::sync::OnceLock<(String, Option<zel_events::OutputFilter>)>>,
);

impl CompiledOutputFilter {
    /// Eagerly compiles `pattern` once for reuse across all actions in an execution phase.
    pub fn from_pattern(pattern: &str) -> Self {
        let lock = std::sync::OnceLock::new();
        let _ = lock.set((
            pattern.to_string(),
            zel_events::OutputFilter::from_regex(pattern).ok(),
        ));
        Self(Arc::new(lock))
    }

    /// Evaluates `tag` against `pattern`, compiling `pattern` at most once when `pattern`
    /// matches the cached pattern string.
    pub fn show_output(&self, pattern: &str, tag: &str) -> bool {
        let (cached_pat, compiled) = self.0.get_or_init(|| {
            (
                pattern.to_string(),
                zel_events::OutputFilter::from_regex(pattern).ok(),
            )
        });
        if cached_pat == pattern {
            match compiled {
                Some(of) => of.show_output(tag),
                None => tag.contains(pattern),
            }
        } else {
            match zel_events::OutputFilter::from_regex(pattern) {
                Ok(of) => of.show_output(tag),
                Err(_) => tag.contains(pattern),
            }
        }
    }

    /// Returns `true` once the underlying pattern cache has been populated.
    pub fn is_compiled(&self) -> bool {
        self.0.get().is_some()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ToolRunfilesSpec {
    pub workspace_name: String,
    pub entries: BTreeMap<String, String>,
    pub repo_mapping: Vec<(String, String, String)>,
    pub enable_runfiles: bool,
    pub legacy_external_runfiles: bool,
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
        && s.chars()
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

pub static PRETTY_SUBCOMMANDS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn describe_command_pretty(
    argv: &[String],
    env: &BTreeMap<String, String>,
    execroot: &PathFragment,
) -> String {
    let mut s = format!(
        "(cd {} && \\\n  exec env - \\\n",
        shell_quote(execroot.path_string())
    );
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
        let mut s = format!(
            "(cd {} && \\\n  exec env - \\\n",
            shell_quote(execroot.path_string())
        );
        for (k, v) in env {
            s.push_str(&format!("    {}={} \\\n", k, shell_quote(v)));
        }
        s.push_str(&format!("  {})", quoted.join(" ")));
        s
    } else {
        format!(" {}", quoted.join(" "))
    }
}

#[allow(clippy::type_complexity)]
static COMPUTED_DIGEST_CACHE: std::sync::LazyLock<
    std::sync::RwLock<std::collections::HashMap<(DigestFunction, String), (u64, u64, String)>>,
> = std::sync::LazyLock::new(|| std::sync::RwLock::new(std::collections::HashMap::new()));

pub fn invalidate_computed_digest(path: &str) {
    if let Ok(mut w) = COMPUTED_DIGEST_CACHE.write() {
        for df in DigestFunction::ALL {
            w.remove(&(df, path.to_string()));
        }
    }
}

/// Digest of a produced output: file contents, or for directories (tree artifacts) a digest over
/// the sorted `(relative path, content digest)` listing. `None` when the path does not exist.
pub fn path_digest(
    digest: DigestFunction,
    fs: &dyn FileSystem,
    path: &PathFragment,
) -> Option<String> {
    let st = fs.stat(path, true).ok()?;
    if st.is_directory {
        fn walk(
            digest: DigestFunction,
            fs: &dyn FileSystem,
            base: &PathFragment,
            dir: &PathFragment,
            out: &mut Vec<u8>,
        ) {
            let mut entries = match fs.readdir(dir) {
                Ok(r) => r,
                Err(_) => return,
            };
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            for e in entries {
                let p = dir.get_relative_str(&e.name);
                let rel = p
                    .path_string()
                    .strip_prefix(base.path_string())
                    .unwrap_or(p.path_string())
                    .to_string();
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
        return Some(digest.hex(&buf));
    }
    if !st.is_file {
        return None;
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let cacheable = st.last_modified_time_ms != 0
        && (st.size > 65536 || now_ms.saturating_sub(st.last_modified_time_ms) >= 2000);
    if cacheable {
        if let Ok(r) = COMPUTED_DIGEST_CACHE.read() {
            if let Some((sz, mt, cached)) = r.get(&(digest, path.path_string().to_string())) {
                if *sz == st.size && *mt == st.last_modified_time_ms {
                    return Some(cached.clone());
                }
            }
        }
    }
    let res = fs.read_file(path).ok().map(|b| digest.hex(&b));
    if cacheable {
        if let (Some(ref v), Ok(mut w)) = (&res, COMPUTED_DIGEST_CACHE.write()) {
            if w.len() < 100_000 {
                w.insert(
                    (digest, path.path_string().to_string()),
                    (st.size, st.last_modified_time_ms, v.clone()),
                );
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
            if b == b':'
                && !(i == 1
                    && bytes[0].is_ascii_alphabetic()
                    && bytes.get(2).is_some_and(|c| *c == b'/' || *c == b'\\'))
            {
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
        input_paths: action
            .inputs()
            .iter()
            .map(|a| a.exec_path.path_string().to_string())
            .collect(),
        output_paths: action
            .outputs()
            .iter()
            .map(|a| a.exec_path.path_string().to_string())
            .collect(),
        ..Default::default()
    };
    p.input_paths.sort();
    match action {
        ActionAnalysisMetadata::SpawnAction {
            argv,
            env,
            inherited_env,
            unset_env,
            execution_info,
            ..
        } => {
            p.kind = "spawn".into();
            p.command_line = argv.clone();
            p.env = env.clone();
            p.inherited_env = inherited_env.iter().cloned().collect();
            p.unset_env = unset_env.iter().cloned().collect();
            p.execution_info = execution_info.clone();
        }
        ActionAnalysisMetadata::FileWriteAction {
            content,
            is_executable,
            ..
        } => {
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
        ActionAnalysisMetadata::BuildInfoTranslateAction {
            template,
            status_file,
            is_volatile,
            translator,
            ..
        } => {
            p.kind = "build_info_translate".into();
            p.payload
                .extend_from_slice(template.exec_path.path_string().as_bytes());
            p.payload.push(0);
            p.payload
                .extend_from_slice(status_file.exec_path.path_string().as_bytes());
            p.payload.push(*is_volatile as u8);
            p.payload.extend_from_slice(translator.key.as_bytes());
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

    let mut parts = action_key_parts(action);
    parts.action_execution_salt = opts.action_execution_salt.clone();
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
    let mut tool_rf_entries: Vec<(String, Option<String>)> = Vec::new();
    if let ActionAnalysisMetadata::SpawnAction { tools, .. } = action {
        for tp in tools {
            if let Some(spec) = opts.tool_runfiles.get(tp.exec_path.path_string()) {
                for (rf_path, target) in &spec.entries {
                    tool_rf_entries.push((
                        format!("runfiles:{}:{}", tp.exec_path.path_string(), rf_path),
                        Some(target.clone()),
                    ));
                }
            }
        }
    }
    let action_inputs = action.inputs();
    let constant_metadata_paths: std::collections::HashSet<&str> = action_inputs
        .iter()
        .filter(|a| a.is_constant_metadata)
        .map(|a| a.exec_path.path_string())
        .collect();
    let source_by_exec: std::collections::HashMap<&str, &Artifact> = action_inputs
        .iter()
        .filter(|a| a.is_source && !a.is_tree_artifact && !a.is_unresolved_symlink)
        .map(|a| (a.exec_path.path_string(), a))
        .collect();
    let md_digest_of = |discovered: &[String]| -> String {
        let mut all: Vec<(String, Option<String>)> = Vec::new();
        for p in declared_inputs.iter().chain(discovered.iter()) {
            let d = if constant_metadata_paths.contains(p.as_str()) {
                let target = execroot.get_relative_str(p);
                fs.exists(&target, true)
                    .then(|| "CONSTANT_METADATA".to_string())
            } else if let Some(fc) = &opts.file_cache {
                if let Some(src_art) = source_by_exec.get(p.as_str()) {
                    match fc.get_artifact_metadata(src_art) {
                        Ok(m) => Some(zel_util::to_hex(&m.digest)),
                        Err(file_cache::FileCacheError::Directory(_)) => {
                            path_digest(digest, fs, &execroot.get_relative_str(p))
                        }
                        Err(_) => None,
                    }
                } else if !p.starts_with("bazel-out/") {
                    match fc.get_input_metadata(&PathFragment::create(p)) {
                        Ok(m) => Some(zel_util::to_hex(&m.digest)),
                        Err(file_cache::FileCacheError::Directory(_)) => {
                            path_digest(digest, fs, &execroot.get_relative_str(p))
                        }
                        Err(_) => None,
                    }
                } else {
                    path_digest(digest, fs, &execroot.get_relative_str(p))
                }
            } else {
                path_digest(digest, fs, &execroot.get_relative_str(p))
            };
            all.push((p.clone(), d));
        }
        all.extend(tool_rf_entries.iter().cloned());
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
    let is_unconditional = is_busted
        || matches!(
            action,
            ActionAnalysisMetadata::BuildInfoTranslateAction {
                is_volatile: true,
                ..
            }
        );

    let out_digest = |o: &Artifact| -> Option<String> {
        let p = abs(&o.exec_path);
        if o.is_unresolved_symlink {
            fs.read_symbolic_link(&p)
                .ok()
                .map(|t| digest.hex(t.path_string().as_bytes()))
        } else {
            path_digest(digest, fs, &p)
        }
    };
    // Cache check (`ActionCacheChecker.mustExecute`): every output must exist and match the
    // recorded digest, and the action key / inputs (incl. discovered inputs) must be unchanged.
    if let Some(primary) = outputs.first() {
        let current: Vec<(String, Option<String>)> = outputs
            .iter()
            .map(|o| (o.exec_path.path_string().to_string(), out_digest(o)))
            .collect();
        let reason = action_cache.must_execute(
            primary.exec_path.path_string(),
            &action_key,
            &env_digest,
            &md_digest_of,
            &current,
        );
        if !is_unconditional && reason == MustExecuteReason::UpToDate {
            return Ok(ActionExecutionOutcome::ok(true));
        }
        if let Some(path) = &opts.explain {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let reason_desc = if is_unconditional {
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
            ActionAnalysisMetadata::SpawnAction {
                mnemonic,
                progress_message,
                owner,
                ..
            } => {
                if progress_message.is_empty() {
                    format!("{mnemonic} {owner}")
                } else {
                    progress_message.clone()
                }
            }
            ActionAnalysisMetadata::FileWriteAction { output, owner, .. } => {
                if output
                    .root_relative_path
                    .path_string()
                    .ends_with(".repo_mapping")
                {
                    format!("Writing repo mapping manifest for {owner}")
                } else if output
                    .root_relative_path
                    .path_string()
                    .ends_with(".runfiles_manifest")
                {
                    format!("Creating source manifest for {owner}")
                } else {
                    format!("Writing file {}", output.root_relative_path)
                }
            }
            ActionAnalysisMetadata::TemplateExpansionAction { output, .. } => {
                format!("Expanding template {}", output.root_relative_path)
            }
            ActionAnalysisMetadata::BuildInfoTranslateAction { is_volatile, .. } => {
                if *is_volatile {
                    "Translating volatile BuildInfo file".to_string()
                } else {
                    "Translating stable BuildInfo file".to_string()
                }
            }
            ActionAnalysisMetadata::SymlinkAction { output, .. } => {
                format!("Creating symlink {}", output.root_relative_path)
            }
        };
        let mut messages = vec![format!("ERROR: action '{describe}' is not up-to-date")];
        if matches!(action, ActionAnalysisMetadata::SymlinkAction { .. }) {
            messages.push(format!(
                "ERROR: action 'Writing repo mapping manifest for {}' is not up-to-date",
                action.owner()
            ));
        }
        return Ok(ActionExecutionOutcome {
            cache_hit: false,
            exit_code: 1,
            stdout: Vec::new(),
            stderr: Vec::new(),
            failure: Some(ActionFailure {
                messages,
                exit_code: 1,
                catastrophic: false,
            }),
            wall_time_ms: 0,
            runner: None,
            progress: String::new(),
        });
    }

    let owner = action.owner();
    let loc = build_file_location(opts.workspace_dir.as_ref(), owner);
    let mut outcome = ActionExecutionOutcome::ok(false);

    let fail_ext = |messages: Vec<String>,
                    code: i32,
                    stdout: Vec<u8>,
                    stderr: Vec<u8>,
                    runner: Option<String>,
                    catastrophic: bool| {
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
                catastrophic,
            }),
            wall_time_ms: started.elapsed().as_millis() as u64,
            runner,
            progress: String::new(),
        }
    };
    let fail = |messages: Vec<String>,
                code: i32,
                stdout: Vec<u8>,
                stderr: Vec<u8>,
                runner: Option<String>| {
        fail_ext(messages, code, stdout, stderr, runner, false)
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
                if let Err(e) = fs.create_directory_and_parents(&parent) {
                    return Ok(fail_ext(
                        vec![format!(
                            "ERROR: {loc}: Writing file {} failed: {e}",
                            output.root_relative_path
                        )],
                        36,
                        Vec::new(),
                        Vec::new(),
                        None,
                        true,
                    ));
                }
            }
            let host_execroot = std::path::Path::new(execroot.path_string());
            if host_execroot.is_dir() {
                let p = std::path::Path::new(out_path.path_string());
                if let Err(e) = file_write_strategy_write(p, content, *is_executable) {
                    return Ok(fail_ext(
                        vec![format!(
                            "ERROR: {loc}: Writing file {} failed: {e}",
                            output.root_relative_path
                        )],
                        36,
                        Vec::new(),
                        Vec::new(),
                        None,
                        true,
                    ));
                }
            } else if let Err(e) = fs.write_file(&out_path, content) {
                return Ok(fail_ext(
                    vec![format!(
                        "ERROR: {loc}: Writing file {} failed: {e}",
                        output.root_relative_path
                    )],
                    36,
                    Vec::new(),
                    Vec::new(),
                    None,
                    true,
                ));
            }
        }
        ActionAnalysisMetadata::SymlinkAction {
            input_path,
            output,
            is_executable,
            ..
        } => {
            let out_path = execroot.get_relative(&output.exec_path);
            if let Some(parent) = out_path.parent_directory() {
                fs.create_directory_and_parents(&parent)?;
            }
            let _ = fs.delete(&out_path);
            let target = if output.is_unresolved_symlink || input_path.is_absolute() {
                input_path.clone()
            } else {
                execroot.get_relative(input_path)
            };
            if *is_executable && !output.is_unresolved_symlink {
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
            if output.is_unresolved_symlink {
                fs.create_symbolic_link(&out_path, &target)?;
            } else if fs.create_symbolic_link(&out_path, &target).is_err() {
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
                if !k.is_empty() {
                    expanded = expanded.replace(k, v);
                }
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
        ActionAnalysisMetadata::BuildInfoTranslateAction {
            template,
            status_file,
            output,
            is_volatile,
            translator,
            ..
        } => {
            let tmpl_path = execroot.get_relative(&template.exec_path);
            let raw = match fs.read_file(&tmpl_path) {
                Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                Err(_) => {
                    return Ok(fail(
                        vec![format!(
                            "ERROR: {loc}: Translating {} BuildInfo file {} failed: missing input file '{}'",
                            if *is_volatile { "volatile" } else { "stable" },
                            output.root_relative_path,
                            template.root_relative_path
                        )],
                        1,
                        Vec::new(),
                        Vec::new(),
                        None,
                    ))
                }
            };
            let mut status_map: BTreeMap<String, String> = if *is_volatile {
                BTreeMap::from([
                    ("BUILD_TIMESTAMP".to_string(), "0".to_string()),
                    (
                        "FORMATTED_DATE".to_string(),
                        "1970 Jan 1 00 00 00 Thu".to_string(),
                    ),
                ])
            } else {
                BTreeMap::from([
                    ("BUILD_EMBED_LABEL".to_string(), String::new()),
                    ("BUILD_HOST".to_string(), "hostname".to_string()),
                    ("BUILD_USER".to_string(), "username".to_string()),
                ])
            };
            let status_path = execroot.get_relative(&status_file.exec_path);
            if fs.exists(&status_path, true) {
                status_map.clear();
                if let Err(e) =
                    zel_analysis::read_workspace_status_file(fs, &status_path, &mut status_map)
                {
                    return Ok(fail(
                        vec![format!(
                            "ERROR: {loc}: Translating {} BuildInfo file {} failed: {e}",
                            if *is_volatile { "volatile" } else { "stable" },
                            output.root_relative_path,
                        )],
                        1,
                        Vec::new(),
                        Vec::new(),
                        None,
                    ));
                }
            }
            let subs = match translator.translate(&status_map) {
                Ok(s) => s,
                Err(e) => {
                    return Ok(fail(
                        vec![format!(
                            "ERROR: {loc}: Translating {} BuildInfo file {} failed: {e}",
                            if *is_volatile { "volatile" } else { "stable" },
                            output.root_relative_path,
                        )],
                        1,
                        Vec::new(),
                        Vec::new(),
                        None,
                    ))
                }
            };
            let mut expanded = raw;
            for (k, v) in &subs {
                if !k.is_empty() {
                    expanded = expanded.replace(k, v);
                }
            }
            let out_path = execroot.get_relative(&output.exec_path);
            if let Some(parent) = out_path.parent_directory() {
                fs.create_directory_and_parents(&parent)?;
            }
            let _ = fs.delete(&out_path);
            fs.write_file(&out_path, expanded.as_bytes())?;
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
            tools,
            java_compile,
        } => {
            let resolved_env = resolve_spawn_env(env, inherited_env, unset_env, &opts.client_env);
            let env = &resolved_env;
            let strategy_desc = (!progress_message.is_empty()).then_some(progress_message.as_str());
            let progress = if progress_message.is_empty() {
                format!("{} {}", mnemonic, owner)
            } else {
                progress_message.clone()
            };
            // `SkyframeActionExecutor.deleteOutputs`: remove any stale output paths before the action executes.
            for o in spawn_outputs {
                let _ = fs.delete(&execroot.get_relative(&o.exec_path));
            }
            // `add_all(tree_artifact)` placeholders expand to the files of the (now produced)
            // directory (`expand_directories`).
            let argv_expanded = match zel_analysis::expand_tree_artifact_args(argv, &|dir| {
                let root = std::path::PathBuf::from(execroot.path_string()).join(dir);
                let mut files = Vec::new();
                let mut stack = vec![root.clone()];
                while let Some(d) = stack.pop() {
                    let rd = std::fs::read_dir(&d)
                        .map_err(|e| format!("cannot expand directory '{dir}': {e}"))?;
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
            let builtin_ijar = should_run_builtin_ijar(mnemonic, &argv_expanded, execroot, fs);
            let exe_first = argv_expanded.first().map(String::as_str).unwrap_or("");
            let mut missing_sources = Vec::new();
            for input in inputs {
                if builtin_ijar && input.exec_path.path_string() == exe_first {
                    continue;
                }
                let inp_path = execroot.get_relative(&input.exec_path);
                if !fs.exists(&inp_path, true) {
                    if input.is_source {
                        let lbl = input.owner_label.canonical_form();
                        missing_sources.push(format!(
                            "ERROR: {loc}: {progress} failed: missing input file '{lbl}'"
                        ));
                    } else if !opts.rewind_lost_inputs {
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
            if !missing_sources.is_empty() {
                missing_sources.push(format!(
                    "ERROR: {loc}: {progress} failed: {} input file(s) do not exist",
                    missing_sources.len()
                ));
                return Ok(fail(missing_sources, 1, Vec::new(), Vec::new(), None));
            }
            let should_materialize =
                opts.materialize_param_files || opts.announce_subcommands || opts.verbose_failures;
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
            let all_input_paths: Vec<String> = inputs
                .iter()
                .map(|a| a.exec_path.path_string().to_string())
                .collect();
            let mut java_fallback = false;
            loop {
                let (attempt_inputs, optional_outputs): (Vec<PathFragment>, Vec<PathFragment>) =
                    match jc {
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
                            fs.write_file(
                                &params_abs,
                                java_compile::classpath_param_file(spec, &cp, java_fallback)
                                    .as_bytes(),
                            )?;
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
                        None => (
                            inputs.iter().map(|a| a.exec_path.clone()).collect(),
                            Vec::new(),
                        ),
                    };
                let tool_paths: Vec<PathFragment> =
                    tools.iter().map(|a| a.exec_path.clone()).collect();
                for tp in &tool_paths {
                    if let Some(spec) = opts.tool_runfiles.get(tp.path_string()) {
                        let er_path = std::path::Path::new(execroot.path_string());
                        let exe_abs = er_path.join(tp.path_string());
                        let rf_opts = crate::runfiles_tree::RunfilesTreeOptions {
                            enable_runfiles: spec.enable_runfiles,
                            legacy_external_runfiles: spec.legacy_external_runfiles,
                            repo_mapping: spec.repo_mapping.clone(),
                        };
                        let _ = crate::runfiles_tree::build_runfiles_tree(
                            er_path,
                            &exe_abs,
                            &spec.workspace_name,
                            &spec.entries,
                            &rf_opts,
                        );
                    }
                }
                let exe_arg = argv_expanded.first().map(String::as_str).unwrap_or("");
                let rf_prefix = format!("{exe_arg}.runfiles/");
                let mut tool_inputs = tool_paths;
                if !exe_arg.is_empty() {
                    for p in &attempt_inputs {
                        let s = p.path_string();
                        if (s == exe_arg || s.starts_with(&rf_prefix)) && !tool_inputs.contains(p) {
                            tool_inputs.push(p.clone());
                        }
                    }
                }
                let symlink_inputs: Vec<PathFragment> = inputs
                    .iter()
                    .filter(|a| a.is_unresolved_symlink)
                    .map(|a| a.exec_path.clone())
                    .collect();
                let spawn = Spawn {
                    mnemonic: mnemonic.clone(),
                    owner_label: owner.canonical_form(),
                    argv: argv_expanded.clone(),
                    env: env.clone(),
                    inputs: attempt_inputs,
                    optional_outputs,
                    outputs: spawn_outputs.iter().map(|a| a.exec_path.clone()).collect(),
                    output_dirs: spawn_outputs
                        .iter()
                        .filter(|a| a.is_tree_artifact)
                        .map(|a| a.exec_path.clone())
                        .collect(),
                    execution_info: execution_info.clone(),
                    kill_delay: opts.runtime.as_ref().and_then(|r| r.kill_delay),
                    tool_inputs,
                    symlink_inputs,
                    client_env: Some(opts.client_env.clone()),
                    ..Default::default()
                };
                if let Err(err_msg) = spawn.parse_timeout() {
                    return Ok(fail(
                        vec![format!("ERROR: {loc}: {progress} failed: {err_msg}")],
                        1,
                        Vec::new(),
                        Vec::new(),
                        None,
                    ));
                }
                let sandbox_debug = opts.runtime.as_ref().is_some_and(|r| r.sandbox_debug);
                if opts.announce_subcommands && !sandbox_debug {
                    let runner_name = match &opts.runtime {
                        Some(rt) => match rt.registry.resolve_opt(&spawn, strategy_desc) {
                            Ok((strats, _)) => strats
                                .first()
                                .map(|s| s.name().to_string())
                                .unwrap_or_else(|| "local".to_string()),
                            Err(_) => "local".to_string(),
                        },
                        None => "local".to_string(),
                    };
                    let cmd_argv = if opts.expand_param_files {
                        expand_param_file_args(argv, execroot, fs)
                    } else {
                        argv.clone()
                    };
                    let from_rule = if mnemonic == "Genrule" {
                        format!("genrule rule target {owner}")
                    } else {
                        format!("rule target {owner}")
                    };
                    let cfg_hash = config_checksum_of(opts, spawn_outputs);
                    reporter.handle(Event::subcommand(format!(
                        "# {from_rule} [action '{progress}', configuration: {cfg_hash}, execution platform: @@platforms//host:host, mnemonic: {mnemonic}]\n{}\n# Configuration: {cfg_hash}\n# Execution platform: @@platforms//host:host\n# Runner: {runner_name}",
                        if PRETTY_SUBCOMMANDS.load(std::sync::atomic::Ordering::Relaxed) {
                            describe_command_pretty(&cmd_argv, env, execroot)
                        } else {
                            describe_command(&cmd_argv, env, execroot, true)
                        }
                    )));
                }
                // Resources (`ResourceManager.acquire`) are held for the duration of the spawn.
                let _held = opts.runtime.as_ref().map(|r| {
                    r.resources
                        .acquire(&resources::ResourceSet::default_action())
                });
                let run = if builtin_ijar {
                    execute_builtin_ijar(&spawn, execroot, fs)
                } else {
                    match &opts.runtime {
                        Some(rt) => match rt.execute_opt(
                            &spawn,
                            strategy_desc,
                            Some(&progress),
                            execroot,
                            fs,
                            reporter,
                        ) {
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
                    }
                };
                let res = run?;
                if opts.announce_subcommands
                    && sandbox_debug
                    && !matches!(res.status, SpawnStatus::MissingInputs)
                {
                    let (cmd_argv, cmd_env, cmd_cwd) = match &res.executed_command {
                        Some(c) => (c.argv.clone(), c.env.clone(), PathFragment::create(&c.cwd)),
                        None => (argv.clone(), env.clone(), execroot.clone()),
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
                    let cfg_hash = config_checksum_of(opts, spawn_outputs);
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
                let is_sandbox_debug = opts.runtime.as_ref().is_some_and(|r| r.sandbox_debug)
                    && res.runner_name.ends_with("-sandbox")
                    && res.executed_command.is_some();
                if is_sandbox_debug {
                    if let Some(dbg) = res
                        .executed_command
                        .as_ref()
                        .and_then(|c| c.debug_output.as_ref())
                    {
                        reporter.handle(Event::debug(format!(
                            "Sandbox debug output for {mnemonic} {owner}:\n{dbg}"
                        )));
                    }
                }
                let (fail_argv, fail_env, fail_cwd, fail_verbose) =
                    match (&res.executed_command, &opts.runtime) {
                        (Some(c), Some(rt)) if rt.sandbox_debug => (
                            c.argv.clone(),
                            c.env.clone(),
                            PathFragment::create(&c.cwd),
                            true,
                        ),
                        _ => (
                            argv.clone(),
                            env.clone(),
                            execroot.clone(),
                            opts.verbose_failures,
                        ),
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
                                && std::fs::metadata(
                                    execroot.get_relative(&o.exec_path).path_string(),
                                )
                                .is_ok_and(|m| m.is_dir())
                        }) =>
                    {
                        // `SkyframeActionExecutor`: a declared file output that is a directory.
                        let mut msgs: Vec<String> = spawn_outputs
                        .iter()
                        .filter(|o| {
                            !o.is_tree_artifact
                                && !o.is_unresolved_symlink
                                && std::fs::metadata(execroot.get_relative(&o.exec_path).path_string()).is_ok_and(|m| m.is_dir())
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
                            let lbl = if m.starts_with("//") {
                                m.clone()
                            } else {
                                format!("//:{m}")
                            };
                            msgs.push(format!(
                                "ERROR: {loc}: {progress} failed: missing input file '{lbl}'"
                            ));
                        }
                        msgs.push(format!(
                            "ERROR: {loc}: {progress} failed: {} input file(s) do not exist",
                            res.missing.len()
                        ));
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
                    SpawnStatus::InvalidTimeout => {
                        let msg = res
                            .failure_message
                            .unwrap_or_else(|| "Invalid timeout".to_string());
                        return Ok(fail(
                            vec![format!("ERROR: {loc}: {progress} failed: {msg}")],
                            1,
                            res.stdout,
                            res.stderr,
                            runner,
                        ));
                    }
                    SpawnStatus::ExecutionDenied => {
                        let msg = res.failure_message.unwrap_or_else(|| {
                            format!("Action type {} is not allowed to run locally", mnemonic)
                        });
                        return Ok(fail_ext(
                            vec![format!("ERROR: {loc}: {progress} failed: {msg}")],
                            1,
                            res.stdout,
                            res.stderr,
                            runner,
                            res.catastrophic,
                        ));
                    }
                    SpawnStatus::RemoteCacheEvicted => {
                        let msg = res.failure_message.unwrap_or_else(|| {
                            "Lost inputs no longer available remotely".to_string()
                        });
                        return Ok(fail_ext(
                            vec![format!("ERROR: {loc}: {progress} failed: {msg}")],
                            39,
                            res.stdout,
                            res.stderr,
                            runner,
                            true,
                        ));
                    }
                    SpawnStatus::OutOfMemory => {
                        let msg = if let Some(fm) = &res.failure_message {
                            format!("ERROR: {loc}: {progress} failed: (Out of memory): {fm}")
                        } else {
                            let from_rule = if mnemonic == "Genrule" {
                                format!("(from genrule rule target {owner})")
                            } else {
                                format!("(from rule target {owner})")
                            };
                            let desc =
                                describe_command(&fail_argv, &fail_env, &fail_cwd, fail_verbose);
                            format!(
                                "ERROR: {loc}: {progress} failed: (Out of memory): {} failed: error executing {mnemonic} command {from_rule}{desc}\nAction was terminated because it exceeded its memory limit.",
                                argv.first()
                                    .map(|a| a.rsplit('/').next().unwrap_or(a))
                                    .unwrap_or("")
                            )
                        };
                        return Ok(fail_ext(
                            vec![msg],
                            res.exit_code.max(1),
                            res.stdout,
                            res.stderr,
                            runner,
                            res.catastrophic,
                        ));
                    }
                    SpawnStatus::Timeout => {
                        let from_rule = if mnemonic == "Genrule" {
                            format!("(from genrule rule target {owner})")
                        } else {
                            format!("(from rule target {owner})")
                        };
                        let desc = describe_command(&fail_argv, &fail_env, &fail_cwd, fail_verbose);
                        let wall_secs = res.wall_time_ms / 1000;
                        let msg = format!(
                            "ERROR: {loc}: {progress} failed: (Timeout): {} failed: error executing {} command {}{} (timed out after {} seconds)",
                            argv.first().map(|a| a.rsplit('/').next().unwrap_or(a)).unwrap_or(""),
                            mnemonic,
                            from_rule,
                            desc,
                            wall_secs,
                        );
                        return Ok(fail_ext(
                            vec![msg],
                            res.exit_code,
                            res.stdout,
                            res.stderr,
                            runner,
                            res.catastrophic,
                        ));
                    }
                    SpawnStatus::NonZeroExit | SpawnStatus::ExecutionFailed => {
                        if res.status == SpawnStatus::ExecutionFailed {
                            if let Some(fm) =
                                res.failure_message.as_deref().filter(|s| !s.is_empty())
                            {
                                return Ok(fail_ext(
                                    vec![format!("ERROR: {loc}: {progress} failed: {fm}")],
                                    res.exit_code,
                                    res.stdout,
                                    res.stderr,
                                    runner,
                                    res.catastrophic,
                                ));
                            }
                        }
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
                        let cfg_hash = config_checksum_of(opts, spawn_outputs);
                        let mut desc =
                            describe_command(&fail_argv, &fail_env, &fail_cwd, fail_verbose);
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
                        if runner.as_deref().is_some_and(|r| r.ends_with("-sandbox"))
                            && opts.runtime.as_ref().is_none_or(|r| !r.sandbox_debug)
                        {
                            msgs.push("Use --sandbox_debug to see verbose messages from the sandbox and retain the sandbox build root for debugging".to_string());
                        }
                        return Ok(fail_ext(
                            msgs,
                            res.exit_code,
                            res.stdout,
                            res.stderr,
                            runner,
                            res.catastrophic,
                        ));
                    }
                }
                if let Some(spec) = jc {
                    if !java_fallback {
                        let jdeps = match fs.read_file(&execroot.get_relative_str(&spec.deps_proto))
                        {
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
                                    vec![format!(
                                    "ERROR: {loc}: {progress} failed: .jdeps read IOException: {e}"
                                )],
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
        let pending = opts
            .runtime
            .as_ref()
            .and_then(|r| r.remote.as_ref())
            .is_some_and(|r| r.is_pending_output(out_art.exec_path.path_string()));
        if !pending && !fs.exists(&out_path, !out_art.is_unresolved_symlink) {
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
                    out_digest(o).unwrap_or_default(),
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
    if action.mnemonic() != "TestRunner" {
        let owner_str = action.owner().to_string();
        let shown = if let Some(filter) = &opts.output_filter {
            opts.compiled_output_filter.show_output(filter, &owner_str)
        } else {
            reporter.show_output(&owner_str)
        };
        if !shown {
            outcome.stdout.clear();
            outcome.stderr.clear();
        }
    }
    outcome.wall_time_ms = started.elapsed().as_millis() as u64;
    Ok(outcome)
}

/// `FileWriteStrategy.beginWrite`: opens `out_path` with truncate (without creating parent
/// directories or unlinking first), writes all bytes, flushes, and ORs executable bits when
/// `is_executable` is set.
pub fn file_write_strategy_write(
    out_path: &std::path::Path,
    content: &[u8],
    is_executable: bool,
) -> io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(out_path)?;
    f.write_all(content)?;
    f.flush()?;
    drop(f);
    if is_executable {
        zel_sandbox::set_executable_mode(out_path)?;
    }
    Ok(())
}

fn expand_param_file_args(
    argv: &[String],
    execroot: &PathFragment,
    fs: &dyn FileSystem,
) -> Vec<String> {
    let mut expanded = Vec::new();
    for arg in argv {
        expand_param_file_arg_recursive(arg, execroot, fs, &mut expanded, 0);
    }
    expanded
}

fn expand_param_file_arg_recursive(
    arg: &str,
    execroot: &PathFragment,
    fs: &dyn FileSystem,
    expanded: &mut Vec<String>,
    depth: usize,
) {
    if depth <= 64 {
        if let Some(escaped) = arg.strip_prefix("@@") {
            expanded.push(format!("@{escaped}"));
            return;
        }
        if let Some(param_path) = arg.strip_prefix('@') {
            let p = execroot.get_relative_str(param_path);
            if let Ok(b) = fs.read_file(&p) {
                let content = String::from_utf8_lossy(&b);
                for line in zel_util::jar::split_java_lines(&content) {
                    if !line.is_empty() {
                        expand_param_file_arg_recursive(line, execroot, fs, expanded, depth + 1);
                    }
                }
                return;
            }
        }
    }
    expanded.push(arg.to_string());
}

/// The checksum of the configuration an action runs in: derived from the output directory
/// (`bazel-out/<mnemonic>/...`) of its outputs and the analysed configurations.
fn config_checksum_of(opts: &ExecOptions, outputs: &[Artifact]) -> String {
    outputs
        .iter()
        .filter_map(|a| {
            a.exec_path
                .path_string()
                .split('/')
                .nth(1)
                .map(str::to_string)
        })
        .find_map(|dir| opts.config_checksums.get(&dir).cloned())
        .unwrap_or_default()
}

fn is_ijar_executable(exe: &str) -> bool {
    let base = exe.rsplit(['/', '\\']).next().unwrap_or(exe);
    base == "ijar" || base == "ijar.exe"
}

fn should_run_builtin_ijar(
    mnemonic: &str,
    argv: &[String],
    execroot: &PathFragment,
    fs: &dyn FileSystem,
) -> bool {
    let Some(exe) = argv.first().map(String::as_str) else {
        return false;
    };
    if !is_ijar_executable(exe) {
        return false;
    }
    if exe == "_bin/ijar" || exe.ends_with("/_bin/ijar") {
        return true;
    }
    let _ = mnemonic;
    let exe_pf = if std::path::Path::new(exe).is_absolute() {
        PathFragment::create(exe)
    } else {
        execroot.get_relative_str(exe)
    };
    !fs.exists(&exe_pf, true)
}

fn execute_builtin_ijar(
    spawn: &Spawn,
    execroot: &PathFragment,
    fs: &dyn FileSystem,
) -> io::Result<zel_sandbox::SpawnResult> {
    let t0 = std::time::Instant::now();
    let base_dir = std::path::Path::new(execroot.path_string());
    let raw_args: &[String] = if spawn.argv.len() > 1 {
        &spawn.argv[1..]
    } else {
        &[]
    };
    let read_pf = |p: &std::path::Path| -> Result<Vec<u8>, String> {
        let pf = PathFragment::create(&p.to_string_lossy());
        fs.read_file(&pf)
            .map_err(|e| format!("{}: {e}", p.display()))
    };
    let opts = match zel_util::ijar::parse_ijar_args_with_reader(raw_args, base_dir, &|p| {
        let bytes = read_pf(p)?;
        String::from_utf8(bytes).map_err(|e| format!("{}: {e}", p.display()))
    }) {
        Ok(o) => o,
        Err(msg) => {
            return Ok(zel_sandbox::SpawnResult {
                status: SpawnStatus::NonZeroExit,
                exit_code: 1,
                stderr: format!("ijar: {msg}\n").into_bytes(),
                wall_time_ms: t0.elapsed().as_millis() as u64,
                runner_name: "in-process".to_string(),
                ..Default::default()
            })
        }
    };
    let in_bytes = match read_pf(&opts.input_jar) {
        Ok(b) => b,
        Err(msg) => {
            return Ok(zel_sandbox::SpawnResult {
                status: SpawnStatus::NonZeroExit,
                exit_code: 1,
                stderr: format!("Opening zip \"{}\": {msg}\n", opts.input_jar.display())
                    .into_bytes(),
                wall_time_ms: t0.elapsed().as_millis() as u64,
                runner_name: "in-process".to_string(),
                ..Default::default()
            })
        }
    };
    match zel_util::ijar::process_jar_bytes(
        &in_bytes,
        opts.strip_jar,
        opts.target_label.as_deref(),
        opts.injecting_rule_kind.as_deref(),
        opts.verbose,
    ) {
        Ok((out_bytes, log)) => {
            let out_pf = PathFragment::create(&opts.output_jar.to_string_lossy());
            if let Some(parent) = out_pf.parent_directory() {
                fs.create_directory_and_parents(&parent)?;
            }
            fs.write_file(&out_pf, &out_bytes)?;
            let mut missing = Vec::new();
            for out in &spawn.outputs {
                if !fs.exists(&execroot.get_relative(out), true) {
                    missing.push(out.path_string().to_string());
                }
            }
            let status = if missing.is_empty() {
                SpawnStatus::Success
            } else {
                SpawnStatus::MissingOutputs
            };
            Ok(zel_sandbox::SpawnResult {
                status,
                exit_code: 0,
                stderr: log,
                wall_time_ms: t0.elapsed().as_millis() as u64,
                runner_name: "in-process".to_string(),
                missing,
                ..Default::default()
            })
        }
        Err(msg) => Ok(zel_sandbox::SpawnResult {
            status: SpawnStatus::NonZeroExit,
            exit_code: 1,
            stderr: format!("ijar: {msg}\n").into_bytes(),
            wall_time_ms: t0.elapsed().as_millis() as u64,
            runner_name: "in-process".to_string(),
            ..Default::default()
        }),
    }
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

    #[test]
    fn file_write_preserves_raw_bytes_and_ors_executable_bits() {
        let d = std::env::temp_dir().join(format!("zel-fw-raw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let execroot = PathFragment::create(d.to_str().unwrap());
        let fs = zel_vfs::LocalFileSystem::default();
        let cache = CompactPersistentActionCache::new();
        let reporter = Reporter::new();

        // Even with .runfiles_manifest suffix, bytes (incl. CRLF and invalid UTF-8) are written verbatim.
        let raw = b"k rel/path\r\n\xff\xfe".to_vec();
        let lbl = Label::parse_canonical("//pkg:m").unwrap();
        let out_art = Artifact::derived(
            "k8-fastbuild",
            PathFragment::create("pkg/t.runfiles_manifest"),
            lbl.clone(),
        );
        let act = ActionAnalysisMetadata::FileWriteAction {
            owner: lbl.clone(),
            output: out_art.clone(),
            content: raw.clone(),
            is_executable: false,
        };
        let res = execute_action(&act, &execroot, &fs, &cache, &reporter, false).unwrap();
        assert!(res.succeeded());
        assert_eq!(
            std::fs::read(d.join(out_art.exec_path.path_string())).unwrap(),
            raw
        );

        // Make-executable ORs 0111 onto existing mode (preserving group-write / umask bits).
        let exe_art = Artifact::derived(
            "k8-fastbuild",
            PathFragment::create("pkg/script.sh"),
            lbl.clone(),
        );
        let act_exe = ActionAnalysisMetadata::FileWriteAction {
            owner: lbl,
            output: exe_art.clone(),
            content: b"#!/bin/sh\n".to_vec(),
            is_executable: true,
        };
        let res = execute_action(&act_exe, &execroot, &fs, &cache, &reporter, false).unwrap();
        assert!(res.succeeded());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(d.join(exe_art.exec_path.path_string()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_ne!(mode & 0o111, 0);
            assert_eq!(mode & 0o600, 0o600);
        }
    }

    #[test]
    fn java_ijar_spawn_runs_in_process_when_ijar_binary_absent() {
        let fs = InMemoryFileSystem::default();
        let execroot = PathFragment::create("/execroot/_main");
        fs.create_directory_and_parents(&execroot).unwrap();

        let lbl = Label::parse_canonical("//pkg:lib").unwrap();
        let in_art = Artifact::source(PathFragment::create("pkg/in.jar"), lbl.clone());
        let tool_art = Artifact::source(
            PathFragment::create("external/remote_java_tools/java_tools/ijar/ijar"),
            lbl.clone(),
        );
        let out_art = Artifact::derived(
            "k8-fastbuild",
            PathFragment::create("pkg/in-ijar.jar"),
            lbl.clone(),
        );

        let jar_bytes = zel_util::ijar::write_stored_jar(&[zel_util::ijar::JarEntry {
            name: "META-INF/my.kotlin_module".to_string(),
            external_attr: 0o100644 << 16,
            data: b"kt".to_vec(),
        }])
        .unwrap();
        let in_path = execroot.get_relative(&in_art.exec_path);
        fs.create_directory_and_parents(&in_path.parent_directory().unwrap())
            .unwrap();
        fs.write_file(&in_path, &jar_bytes).unwrap();

        let act = ActionAnalysisMetadata::SpawnAction {
            owner: lbl,
            mnemonic: "JavaIjar".to_string(),
            progress_message: "Extracting interface //pkg:lib".to_string(),
            argv: vec![
                tool_art.exec_path.path_string().to_string(),
                in_art.exec_path.path_string().to_string(),
                out_art.exec_path.path_string().to_string(),
                "--target_label".to_string(),
                "//pkg:lib".to_string(),
            ],
            env: BTreeMap::new(),
            inherited_env: Default::default(),
            unset_env: Default::default(),
            inputs: vec![in_art, tool_art.clone()],
            outputs: vec![out_art.clone()],
            execution_info: BTreeMap::new(),
            tools: vec![tool_art],
            java_compile: None,
        };

        let cache = CompactPersistentActionCache::new();
        let reporter = Reporter::new();
        let res = execute_action(&act, &execroot, &fs, &cache, &reporter, false).unwrap();
        assert!(res.succeeded(), "JavaIjar failed: {:?}", res.failure);

        let out_bytes = fs
            .read_file(&execroot.get_relative(&out_art.exec_path))
            .unwrap();
        let entries = zel_util::ijar::read_jar_entries(&out_bytes).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"META-INF/MANIFEST.MF"));
        assert!(names.contains(&"META-INF/my.kotlin_module"));
    }
}
