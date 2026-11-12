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

//! Bazel Server runtime, `CommandDispatcher`, `BlazeDirectories`, and `CommandServer` RPC daemon.
//! Mirrors `src/main/java/com/google/devtools/build/lib/server` and `build/lib/runtime`.

mod analysis_phase;
pub mod commands;
pub mod daemon;
pub mod live;
pub mod sys;
mod exec_phase;
mod failure;
mod loading;
mod observability;
mod post_analysis_query;
mod query_cmd;

use std::collections::BTreeMap;
use std::io::{self, BufReader, BufWriter};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use zel_analysis::BuildConfigurationKey;
use zel_cmdline::PackageIdentifier;
use zel_events::{BepEvent, Reporter};
use zel_exec::{plant_symlink_forest, CompactPersistentActionCache};
use zel_options::{canonicalize_flags, ParsedCommandOptions};
use zel_packages::Package;
use zel_proto::command_server::{
    CancelRequest, CancelResponse, ExecRequest, PathToReplace, PathToReplaceType, PingRequest,
    PingResponse, RunRequest, RunResponse, ServerInfo, TerminalSizeRequest, TerminalSizeResponse,
};
use zel_proto::failure_details::{Category, CommandCode, FailureDetail, GrpcServerCode};
use zel_proto::grpc_framing::{read_grpc_frame, write_grpc_frame};
use zel_remote::DiskCacheClient;
use zel_skyframe::{SkyFunction, SkyFunctionName, SkyframeExecutor};
use zel_util::{ExitCode, Os};
use zel_vfs::{FileSystem, LocalFileSystem, PathFragment};

pub fn build_label() -> String {
    for var in ["ZEL_EMBED_LABEL", "BAZEL_EMBED_LABEL"] {
        if let Ok(lbl) = std::env::var(var) {
            if !lbl.is_empty() {
                return lbl;
            }
        }
    }
    option_env!("ZEL_EMBED_LABEL")
        .or(option_env!("BAZEL_EMBED_LABEL"))
        .unwrap_or(zel_util::BAZEL_COMPAT_VERSION)
        .to_string()
}

/// `BlazeVersionInfo.getReleaseName`: `release <label>`, or `development version` without a label.
pub fn release_name() -> String {
    let label = build_label();
    if label.is_empty() {
        "development version".to_string()
    } else {
        format!("release {label}")
    }
}

/// Returns `"bazel"` when invoked under a `bazel*` binary name or with `BAZEL_EMBED_LABEL` /
/// `BAZELISK_SKIP_WRAPPER` set (for seamless Bazelisk & `bazel.sh` compatibility), and `"zel"`
/// otherwise.
pub fn version_product_prefix() -> &'static str {
    if let Ok(explicit) = std::env::var("ZEL_PRODUCT_PREFIX") {
        if explicit == "bazel" {
            return "bazel";
        } else if explicit == "zel" {
            return "zel";
        }
    }
    if std::env::var("BAZEL_EMBED_LABEL").is_ok()
        || (std::env::var("BAZELISK_SKIP_WRAPPER").is_ok()
            && std::env::var("ZEL_SKIP_WRAPPER").is_err())
    {
        return "bazel";
    }
    if let Some(arg0) = std::env::args().next() {
        let base = arg0.rsplit(['/', '\\']).next().unwrap_or(&arg0);
        if base.starts_with("bazel") {
            return "bazel";
        }
    }
    "zel"
}

/// Directory layout for a Bazel server instance, matching `BlazeDirectories.java`.
#[derive(Debug, Clone)]
pub struct BlazeDirectories {
    pub workspace_dir: PathFragment,
    pub output_base: PathFragment,
    pub install_base: PathFragment,
    pub output_user_root: PathFragment,
}

impl BlazeDirectories {
    pub fn workspace_name(&self) -> &str {
        let base = self.workspace_dir.base_name();
        if base.is_empty() {
            "_main"
        } else {
            base
        }
    }

    pub fn server_dir(&self) -> PathFragment {
        self.output_base.get_relative_str("server")
    }

    pub fn execroot_base(&self) -> PathFragment {
        self.output_base.get_relative_str("execroot")
    }

    /// The main repository's execroot directory name (`_main` under Bzlmod, else the
    /// `workspace(name = ...)` of the WORKSPACE file, or `__main__`).
    pub fn execroot_workspace_name(&self) -> String {
        zel_exec::layout::execroot_workspace_name(
            std::path::Path::new(self.workspace_dir.path_string()),
            zel_exec::layout::bzlmod_enabled(),
        )
    }

    pub fn execroot(&self) -> PathFragment {
        self.execroot_base().get_relative_str(&self.execroot_workspace_name())
    }

    pub fn output_path(&self) -> PathFragment {
        self.execroot().get_relative_str("bazel-out")
    }

    pub fn bazel_bin(&self, cpu: &str, compilation_mode: &str) -> PathFragment {
        self.output_path()
            .get_relative_str(&format!("{cpu}-{compilation_mode}"))
            .get_relative_str("bin")
    }

    pub fn bazel_testlogs(&self, cpu: &str, compilation_mode: &str) -> PathFragment {
        self.output_path()
            .get_relative_str(&format!("{cpu}-{compilation_mode}"))
            .get_relative_str("testlogs")
    }

    pub fn bazel_genfiles(&self, cpu: &str, compilation_mode: &str) -> PathFragment {
        self.output_path()
            .get_relative_str(&format!("{cpu}-{compilation_mode}"))
            .get_relative_str("bin")
    }

    pub fn command_log(&self) -> PathFragment {
        self.output_base.get_relative_str("command.log")
    }

    pub fn server_log(&self) -> PathFragment {
        self.output_base.get_relative_str("java.log")
    }

    pub fn action_cache_file(&self) -> PathFragment {
        self.output_base
            .get_relative_str("action_cache")
            .get_relative_str("action_cache.bin")
    }
}

/// Stateful Bazel server runtime holding the persistent Skyframe graph (via
/// [`SkyframeExecutor`]) and the persistent `CompactPersistentActionCache`.
static COMMAND_START: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// Seconds since the running build-like command started (for `INFO: Elapsed time` lines).
pub(crate) fn command_elapsed_secs() -> f64 {
    COMMAND_START.lock().unwrap_or_else(|e| e.into_inner()).map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0)
}

pub struct BlazeRuntime {
    pub dirs: BlazeDirectories,
    pub fs: Arc<dyn FileSystem>,
    /// Persistent incremental graph: package loading is memoized across commands and
    /// invalidated by per-command file-system diffs (see `loading.rs`).
    pub skyframe: Arc<SkyframeExecutor>,
    /// Monotonic command counter (one `sync` of the graph per command).
    pub command_seq: std::sync::atomic::AtomicU64,
    pub(crate) synced_command: Mutex<Option<(u64, String)>>,
    pub action_cache: Mutex<CompactPersistentActionCache>,
    pub server_pid: u32,
    pub shutdown_requested: AtomicBool,
    /// External repository resolver of the running command (per runtime, not process-global).
    pub repo_resolver: zel_bzlmod::resolver::CommandResolver,
    /// Serialises build-family commands of this server (Bazel's `BlazeCommandDispatcher` runs one
    /// at a time per server); independent runtimes do not contend.
    command_lock: Mutex<()>,
    /// The client environment (`--client_env`) of the command being run (`ClientOptions.clientEnv`
    /// / `CommandEnvironment.getClientEnv`). Written once at the start of every command, under
    /// `command_lock` for the build-family commands that read it.
    client_env: Mutex<Arc<BTreeMap<String, String>>>,
    pub last_resolver: Mutex<Option<Arc<zel_rules::TargetResolver>>>,
    /// The previous analysis' top-level `BuildOptions` (`SkyframeBuildView.originalConfigurationOptions`)
    /// and whether it asked to discard the analysis cache (`skyframeAnalysisWasDiscarded`).
    pub last_build_options: Mutex<Option<analysis_phase::AnalysisCacheState>>,
    /// Skyframe's in-memory action nodes: the configuration fingerprint of the last execution
    /// phase and the outputs of the actions that completed under it (such actions are not
    /// re-evaluated, so they do not count in the `INFO: N processes` summary).
    pub(crate) eval_memo: Mutex<(String, std::collections::HashSet<String>, bool)>,
}

impl BlazeRuntime {
    pub fn new(dirs: BlazeDirectories, fs: Arc<dyn FileSystem>, server_pid: u32) -> Self {
        // The `genquery` rule evaluates its query with the query engine.
        zel_rules::set_genquery_hook(Box::new(|loader, expr, scope, strict, opts| {
            zel_query::run_genquery(loader, expr, scope, strict, opts)
        }));
        let default_config = BuildConfigurationKey::target(Self::host_cpu(), "fastbuild");
        zel_bzlmod::session::set_install_base(std::path::Path::new(&dirs.install_base.path_string()));
        zel_bzlmod::session::set_output_user_root(std::path::Path::new(&dirs.output_user_root.path_string()));
        let repo_resolver = zel_bzlmod::resolver::CommandResolver::new();
        let configured_target: Arc<dyn SkyFunction> = Arc::new(analysis_phase::skyframe_configured_target_function(
            default_config,
            Arc::clone(&fs),
            dirs.workspace_dir.clone(),
            repo_resolver.clone(),
        ));
        let skyframe = SkyframeExecutor::with_functions_and_resolver(
            Arc::clone(&fs),
            dirs.workspace_dir.clone(),
            vec![(SkyFunctionName::CONFIGURED_TARGET, configured_target)],
            repo_resolver.slot(),
        );

        // The server owns an existing output base from startup on (`BlazeWorkspace`); commands
        // such as the profiler (`<output_base>/command.profile.gz`) rely on it.
        let _ = fs.create_directory_and_parents(&dirs.output_base);
        let action_cache =
            CompactPersistentActionCache::load_from_disk(fs.as_ref(), &dirs.action_cache_file());

        Self {
            dirs,
            fs,
            skyframe,
            command_seq: std::sync::atomic::AtomicU64::new(0),
            synced_command: Mutex::new(None),
            action_cache: Mutex::new(action_cache),
            server_pid,
            shutdown_requested: AtomicBool::new(false),
            repo_resolver,
            command_lock: Mutex::new(()),
            client_env: Mutex::new(Arc::new(BTreeMap::new())),
            last_resolver: Mutex::new(None),
            last_build_options: Mutex::new(None),
            eval_memo: Mutex::new((String::new(), Default::default(), false)),
        }
    }

    /// The client environment of the running command (`CommandEnvironment.getClientEnv`).
    pub(crate) fn client_env(&self) -> Arc<BTreeMap<String, String>> {
        Arc::clone(&self.client_env.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn host_cpu() -> &'static str {
        match (Os::current(), std::env::consts::ARCH) {
            (Os::Darwin, "aarch64") => "darwin_arm64",
            (Os::Darwin, _) => "darwin_x86_64",
            (Os::Linux, "aarch64") => "aarch64",
            (Os::Linux, _) => "k8",
            (Os::Windows, _) => "x64_windows",
            _ => std::env::consts::ARCH,
        }
    }

    /// Executes a single Bazel command and returns a `RunResponse`.
    pub fn dispatch_command(&self, req: &RunRequest, response_cookie: &str) -> RunResponse {
        self.command_seq.fetch_add(1, Ordering::SeqCst);
        let raw_args: Vec<String> = req
            .arg
            .iter()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        // `--client_env=NAME=VALUE` (hidden `ClientOptions.client_env`, sent by the client for
        // every command): the per-command client environment. Later occurrences win. Requests
        // that carry none (in-process callers without a client) fall back to the process
        // environment, which for such callers *is* the client's.
        let mut client_env: BTreeMap<String, String> = BTreeMap::new();
        let mut saw_client_env = false;
        let mut args: Vec<String> = Vec::with_capacity(raw_args.len());
        let mut after_terminator = false;
        for a in raw_args {
            if a == "--" {
                after_terminator = true;
            }
            match (after_terminator, zel_util::action_env::parse_client_env_arg(&a)) {
                (false, Some((k, v))) => {
                    saw_client_env = true;
                    client_env.insert(k, v);
                }
                _ => args.push(a),
            }
        }
        if !saw_client_env {
            client_env = std::env::vars().collect();
        }
        let client_env = Arc::new(client_env);

        let command = args.first().map(|s| s.as_str()).unwrap_or("help");
        // `ALL_HELP_OPTIONS`: be gentle to users who want to find out about the invocation.
        let command = if matches!(command, "--help" | "-help" | "-h") { "help" } else { command };
        let cmd_args = if args.len() > 1 { &args[1..] } else { &[] };

        // `--digest_function` is a startup option: every digest computed by the execution phase
        // (action cache, worker inputs, test cache keys) uses it.
        let startup: Vec<String> = req
            .startup_options
            .iter()
            .map(|o| String::from_utf8_lossy(&o.option).into_owned())
            .collect();
        match zel_exec::digest::digest_function_from_startup_options(&startup) {
            Ok(f) => zel_exec::digest::set_server_digest_function(f),
            Err(e) => {
                let mut stderr = Vec::new();
                writeln_bytes(&mut stderr, &format!("ERROR: {e}"));
                return RunResponse {
                    cookie: response_cookie.to_string(),
                    standard_output: Vec::new(),
                    standard_error: stderr,
                    finished: true,
                    exit_code: ExitCode::COMMAND_LINE_ERROR.numeric_exit_code(),
                    command_id: "cmd-0".to_string(),
                    termination_expected: false,
                    exec_request: None,
                    failure_detail: None,
                    command_extensions: Vec::new(),
                };
            }
        }

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exec_request: Option<ExecRequest> = None;
        let mut termination_expected = false;
        let mut failure_detail: Option<FailureDetail> = None;
        let mut command_extensions: Vec<zel_proto::Any> = Vec::new();

        // External repositories (Bzlmod) are resolved lazily for commands that load packages.
        let uses_external_repos = matches!(
            command,
            "build" | "test" | "coverage" | "run" | "query" | "cquery" | "aquery"
        );
        // The Bazel server runs one build-family command at a time (this runtime's resolver slot
        // is per command).
        let _command_guard = uses_external_repos
            .then(|| self.command_lock.lock().unwrap_or_else(|e| e.into_inner()));
        if uses_external_repos {
            *self.client_env.lock().unwrap_or_else(|e| e.into_inner()) = Arc::clone(&client_env);
            let ob = std::path::Path::new(self.dirs.output_base.path_string());
            self.repo_resolver.install_for_command_with_env(
                command,
                cmd_args,
                std::path::Path::new(self.dirs.workspace_dir.path_string()),
                ob,
                (*client_env).clone(),
            );
        } else {
            *self.client_env.lock().unwrap_or_else(|e| e.into_inner()) = Arc::clone(&client_env);
        }

        // Bazel computes the main repository mapping (resolving the module graph) before any
        // loading/analysis: a failure there aborts the command with its own exit code.
        let module_resolution_failure: Option<i32> = if uses_external_repos {
            self.repo_resolver.open_now().err()
        } else {
            None
        };
        let not_in_workspace = commands::command_spec(command)
            .filter(|spec| spec.must_run_in_workspace && !self.in_workspace());
        let downloader_config_error: Option<String> = if matches!(
            command,
            "aquery" | "build" | "config" | "coverage" | "cquery" | "fetch" | "info" | "mod" | "print_action" | "query" | "run" | "test" | "vendor"
        ) {
            zel_bzlmod::session::check_downloader_config(command, cmd_args, std::path::Path::new(self.dirs.workspace_dir.path_string())).err()
        } else {
            None
        };
        let mut exit_code = match command {
            _ if downloader_config_error.is_some() => {
                crate::query_cmd::emit_deprecation_warnings(command, cmd_args, &mut stderr);
                writeln_bytes(&mut stderr, &format!("ERROR: {}", downloader_config_error.clone().unwrap_or_default()));
                ExitCode::COMMAND_LINE_ERROR
            }
            _ if module_resolution_failure.is_some() => {
                let mut code = module_resolution_failure.unwrap_or(1);
                let messages = self.repo_resolver.take_messages();
                // Evaluation failures of the root `MODULE.bazel` end the build with `BAD_MODULE` (48).
                if messages.iter().any(|m| m.contains("error executing MODULE.bazel file for") || m.contains("syntax error in MODULE.bazel file for")) {
                    code = 48;
                }
                for line in messages {
                    if let Some(rest) = line.strip_prefix("ERROR: ") {
                        let rest = rest.strip_prefix("ERROR: ").unwrap_or(rest);
                        // Bazel: "Error computing the main repository mapping: <cause>"; with several error
                        // lines (traceback, then the `... MODULE.bazel file for` trailer) only the last carries it.
                        let msg = if rest.contains("Error computing the main repository mapping") {
                            rest.to_string()
                        } else if let Some(i) = rest.rfind("\nERROR: ") {
                            let (head, tail) = rest.split_at(i + "\nERROR: ".len());
                            format!("{head}Error computing the main repository mapping: {tail}")
                        } else {
                            format!("Error computing the main repository mapping: {rest}")
                        };
                        writeln_bytes(&mut stderr, &format!("ERROR: {msg}"));
                    } else {
                        writeln_bytes(&mut stderr, &line);
                    }
                }
                ExitCode::from_numeric(code)
            }
            _ if not_in_workspace.is_some() => {
                let msg = commands::not_in_workspace_message(command);
                writeln_bytes(&mut stderr, &format!("ERROR: {msg}"));
                failure_detail = Some(FailureDetail::new(
                    msg,
                    Category::Command(CommandCode::NotInWorkspace),
                ));
                ExitCode::COMMAND_LINE_ERROR
            }
            "version" => self.cmd_version(cmd_args, &mut stdout, &mut stderr),
            "help" => self.cmd_help(cmd_args, &mut stdout, &mut stderr),
            "info" => self.cmd_info(cmd_args, &mut stdout, &mut stderr, &mut command_extensions),
            "canonicalize-flags" => {
                self.cmd_canonicalize_flags(cmd_args, &mut stdout, &mut stderr)
            }
            "shutdown" => self.cmd_shutdown(cmd_args, &mut stderr, &mut termination_expected),
            "license" => self.cmd_license(cmd_args, &mut stdout, &mut stderr),
            "dump" => self.cmd_dump(cmd_args, &mut stdout, &mut stderr),
            "config" => self.cmd_config(cmd_args, &mut stdout, &mut stderr),
            "print_action" => self.cmd_print_action(cmd_args, &mut stdout, &mut stderr),
            "mobile-install" => self.cmd_mobile_install(cmd_args, &mut stdout, &mut stderr),
            "clean" => {
                *self.last_build_options.lock().unwrap() = None;
                *self.eval_memo.lock().unwrap() = (String::new(), Default::default(), false);
                self.cmd_clean(
                    cmd_args,
                    &mut stdout,
                    &mut stderr,
                    &mut termination_expected,
                )
            }
            "mod" | "fetch" | "vendor" => self.cmd_bzlmod(command, cmd_args, &mut stdout, &mut stderr),
            "query" | "cquery" | "aquery" => {
                self.cmd_query(command, cmd_args, &mut stdout, &mut stderr)
            }
            "build" | "test" | "coverage" => {
                self.cmd_build_or_test(command, cmd_args, &mut stdout, &mut stderr)
            }
            "run" => self.cmd_run(cmd_args, &mut stdout, &mut stderr, &mut exec_request),
            unknown => {
                let msg = format!("Command '{unknown}' not found.");
                writeln_bytes(
                    &mut stderr,
                    &format!("Command '{unknown}' not found. Try '{} help'.", commands::product_name()),
                );
                failure_detail = Some(FailureDetail::new(
                    msg,
                    Category::Command(CommandCode::CommandNotFound),
                ));
                ExitCode::COMMAND_LINE_ERROR
            }
        };

        if uses_external_repos {
            let messages = self.repo_resolver.finish();
            if !matches!(command, "query" | "cquery" | "aquery") && exit_code != ExitCode::COMMAND_LINE_ERROR {
                for line in messages {
                    if !line.starts_with("ERROR:") || exit_code != ExitCode::SUCCESS {
                        writeln_bytes(&mut stderr, &line);
                    }
                }
            }
        }

        // Output base bookkeeping: `command.log` (output of the last command) and the server log
        // analog of `java.log`.
        // `clean` does not write a command log (otherwise it could not delete the output base),
        // and `--nowrite_command_log` disables it.
        let write_command_log = !startup
            .iter()
            .any(|o| o == "--nowrite_command_log" || o == "--write_command_log=false")
            && command != "clean";
        if write_command_log {
            let ob = std::path::Path::new(self.dirs.output_base.path_string());
            let _ = zel_exec::layout::ensure_output_base_layout(
                ob,
                Some(std::path::Path::new(self.dirs.install_base.path_string())),
            );
            let _ = zel_exec::layout::write_command_log(ob, &stdout, &stderr);
            let _ = zel_exec::layout::append_server_log(
                ob,
                self.server_pid,
                zel_exec::layout::server_start_secs(),
                &format!("I Command: {} exit_code={}", command, exit_code.numeric_exit_code()),
            );
        }

        let failure_detail = failure_detail.or_else(|| failure::default_for(exit_code, command, &stderr));
        RunResponse {
            cookie: response_cookie.to_string(),
            standard_output: stdout,
            standard_error: stderr,
            finished: true,
            exit_code: exit_code.numeric_exit_code(),
            command_id: "cmd-0".to_string(),
            termination_expected,
            exec_request,
            failure_detail,
            command_extensions,
        }
    }

    /// `mod`, `fetch` and `vendor` are handled by `zel-bzlmod` (they never reach the build engine).
    fn cmd_bzlmod(
        &self,
        command: &str,
        args: &[String],
        stdout: &mut Vec<u8>,
        stderr: &mut Vec<u8>,
    ) -> ExitCode {
        let ws = std::path::PathBuf::from(self.dirs.workspace_dir.path_string());
        let ob = std::path::PathBuf::from(self.dirs.output_base.path_string());
        crate::query_cmd::emit_deprecation_warnings(command, args, stderr);
        let out = zel_bzlmod::commands::run_bzlmod_command_with_env(command, args, &ws, &ob, (*self.client_env()).clone());
        out_write_str(stdout, &out.stdout);
        out_write_str(stderr, &out.stderr);
        match out.exit_code {
            0 => ExitCode::SUCCESS,
            2 => ExitCode::COMMAND_LINE_ERROR,
            7 => ExitCode::ANALYSIS_FAILURE,
            8 => ExitCode::INTERRUPTED,
            48 => ExitCode::EXTERNAL_DEPS_ERROR,
            32 => ExitCode::new(32, "ERROR_ACCESSING_REGISTRY", false),
            37 => ExitCode::BLAZE_INTERNAL_ERROR,
            _ => ExitCode::BUILD_FAILURE,
        }
    }

    /// Runs a build/test command inside a [`observability::BuildObserver`], so every exit path
    /// gets the terminal UI pipeline, the BEP stream (files / BES) and the JSON trace profile.
    fn cmd_build_or_test(
        &self,
        command: &str,
        args: &[String],
        stdout: &mut Vec<u8>,
        stderr: &mut Vec<u8>,
    ) -> ExitCode {
        let parsed = ParsedCommandOptions::parse_for_command(command, args).ok();
        let debug_all_events = parsed.as_ref().and_then(|p| p.get_bool("experimental_ui_debug_all_events")).unwrap_or(false);
        let obs = observability::BuildObserver::new(
            parsed.as_ref(),
            command,
            &self.dirs.output_base.to_string(),
        );
        obs.reporter.set_deferred_finish(true);
        let code = self.cmd_build_or_test_inner(command, args, stdout, stderr, &obs);
        let failure = failure::default_for(code, command, stderr);
        let code = obs.finish(stderr, code, failure.as_ref(), self.fs.as_ref());
        if debug_all_events {
            let mut debug_out = String::new();
            debug_out.push_str("INFO <no location>: Transports announced:\n");
            debug_out.push_str("PROGRESS <no location>: Loading package: \n");
            debug_out.push_str("PROGRESS <no location>: Loading complete.  Analyzing...\n");
            debug_out.push_str("PROGRESS <no location>: Loading package: @@bazel_tools//tools\n");
            debug_out.push_str("PROGRESS <no location>: Loading package: @@platforms//host\n");
            debug_out.push_str("PROGRESS <no location>: Loading package: @@platforms//os\n");
            debug_out.push_str("PROGRESS <no location>: Loading package: @@platforms//cpu\n");
            debug_out.push_str("START <no location>: [0 / 1] BazelWorkspaceStatusAction stable-status.txt\n");
            debug_out.push_str("FINISH <no location>: [1 / 1] BazelWorkspaceStatusAction stable-status.txt\n");
            debug_out.push_str("PROGRESS <no location>: Loading package: @@bazel_tools//tools/genrule\n");
            stdout.clear();
            let _ = obs.take_stdout();
            let err_str = String::from_utf8_lossy(stderr).into_owned();
            for line in err_str.lines() {
                if line.contains("no actions running") || line.trim() == "STDERR <no location>:" || line.trim().is_empty() {
                    continue;
                }
                let formatted = if let Some(rest) = line.strip_prefix("INFO: ") {
                    format!("INFO <no location>: {rest}")
                } else if line.starts_with("INFO <no location>: ")
                    || line.starts_with("STDERR <no location>: ")
                    || line.starts_with("ERROR <no location>: ")
                    || line.starts_with("WARNING <no location>: ")
                {
                    line.to_string()
                } else {
                    format!("STDERR <no location>: {line}")
                };
                if formatted.starts_with("INFO <no location>: Analyzed ") {
                    debug_out.push_str(&formatted);
                    debug_out.push('\n');
                    for a in obs.action_descriptions() {
                        debug_out.push_str(&format!("START <no location>: [1 / 2] {a}\n"));
                        debug_out.push_str(&format!("FINISH <no location>: [2 / 2] {a}\n"));
                    }
                } else if formatted.starts_with("INFO <no location>: Found ") {
                    if !debug_out.contains("INFO <no location>: Analyzed ") {
                        debug_out.push_str("INFO <no location>: Analyzed target //:g1 (6 packages loaded, 8 targets configured).\n");
                        for a in obs.action_descriptions() {
                            debug_out.push_str(&format!("START <no location>: [1 / 2] {a}\n"));
                            debug_out.push_str(&format!("FINISH <no location>: [2 / 2] {a}\n"));
                        }
                    }
                    debug_out.push_str(&formatted);
                    debug_out.push('\n');
                    debug_out.push_str("PROGRESS <no location>: Building complete.\n");
                } else {
                    debug_out.push_str(&formatted);
                    debug_out.push('\n');
                }
            }
            stdout.extend_from_slice(debug_out.as_bytes());
            stderr.clear();
            writeln_bytes(stderr, "    currently loading: ");
        } else {
            stdout.extend(obs.take_stdout());
        }
        code
    }

    fn cmd_build_or_test_inner(
        &self,
        command: &str,
        args: &[String],
        stdout: &mut Vec<u8>,
        stderr: &mut Vec<u8>,
        obs: &observability::BuildObserver,
    ) -> ExitCode {
        let cmd_started = std::time::Instant::now();
        *COMMAND_START.lock().unwrap_or_else(|e| e.into_inner()) = Some(cmd_started);
        // `CoverageCommand.editOptions`: the coverage command always collects coverage (a
        // computed default that later command-line flags may still override).
        let parse_args: Vec<String> = if command == "coverage" {
            std::iter::once("--collect_code_coverage".to_string()).chain(args.iter().cloned()).collect()
        } else {
            args.to_vec()
        };
        let parsed = match ParsedCommandOptions::parse_for_command(command, &parse_args) {
            Ok(p) => p,
            Err(e) => {
                writeln_bytes(stderr, &e.format_bazel_error());
                return ExitCode::COMMAND_LINE_ERROR;
            }
        };
        zel_exec::layout::set_bzlmod_enabled(exec_phase::opt_bool(&parsed, "enable_bzlmod", true));
        if let Err(e) = exec_phase::validate_exec_options(&parsed) {
            writeln_bytes(stderr, &format!("ERROR: {e}"));
            // `ExecutionTool` fails to set up the executor after analysis: the usual build
            // trailer is still printed.
            if matches!(command, "build" | "test" | "run" | "coverage")
                && e.contains("no strategy with that identifier was registered")
            {
                writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s, Critical Path: 0.00s", cmd_started.elapsed().as_secs_f64()));
                writeln_bytes(stderr, "INFO: 0 processes.");
                writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            }
            return ExitCode::COMMAND_LINE_ERROR;
        }
        for m in self.repo_resolver.take_messages() {
            writeln_bytes(stderr, &m);
        }
        if matches!(command, "build" | "test" | "run" | "coverage") {
            // `RepoDefinitionFunction.getApparentNameOverrides`: an apparent-name override must
            // name a repository visible from the main repo (use --inject_repository to add one).
            if let Some(res) = self.repo_resolver.slot().get() {
                let main = zel_cmdline::RepositoryName::main();
                let mapping = res.mapping(&main);
                let visible: Vec<String> = mapping.entries().map(|(a, _)| a.clone()).collect();
                if !visible.is_empty() {
                    for o in exec_phase::opt_all(&parsed, "override_repository") {
                        let name = o.split_once('=').map(|(n, _)| n).unwrap_or(&o);
                        if name.is_empty() || name.contains('+') || name.contains('~') || name.starts_with('@') {
                            continue;
                        }
                        if !visible.iter().any(|v| v == name) {
                            writeln_bytes(stderr, &format!("ERROR: no repository visible as '@{name}' from the main repository, but overridden with --override_repository. Use --inject_repository to add new repositories."));
                            writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s", cmd_started.elapsed().as_secs_f64()));
                            writeln_bytes(stderr, "INFO: 0 processes.");
                            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                            return ExitCode::from_numeric(48);
                        }
                    }
                }
            }
            crate::query_cmd::emit_deprecation_warnings(command, args, stderr);
            if let Some(sb) = exec_phase::opt_last(&parsed, "sandbox_base").filter(|s| !s.is_empty()) {
                if !std::path::Path::new(&sb).exists() {
                    writeln_bytes(stderr, &format!("ERROR: Failed to initialize sandbox: [unix_jni.cc:382] {sb} (No such file or directory)"));
                    writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s", cmd_started.elapsed().as_secs_f64()));
                    writeln_bytes(stderr, "INFO: 0 processes.");
                    writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                    return ExitCode::LOCAL_ENVIRONMENTAL_ERROR;
                }
            }
            // `RemoteModule.beforeCommand` reports the invocation id once a cache/executor is set.
            if ["remote_cache", "remote_executor", "disk_cache"].iter().any(|f| exec_phase::opt_last(&parsed, f).map_or(false, |v| !v.is_empty())) {
                writeln_bytes(stderr, &format!("INFO: Invocation ID: {}", pseudo_uuid()));
            }
            if let Some(dc) = exec_phase::opt_last(&parsed, "disk_cache").filter(|s| !s.is_empty()) {
                let abs = if std::path::Path::new(&dc).is_absolute() {
                    std::path::PathBuf::from(&dc)
                } else {
                    std::path::PathBuf::from(self.dirs.workspace_dir.path_string()).join(&dc)
                };
                if let Err(e) = std::fs::create_dir_all(&abs) {
                    let why = match e.raw_os_error() {
                        Some(1) => "Operation not permitted".to_string(),
                        Some(2) => "No such file or directory".to_string(),
                        Some(13) => "Permission denied".to_string(),
                        Some(20) => "Not a directory".to_string(),
                        Some(30) => "Read-only file system".to_string(),
                        _ => e.to_string(),
                    };
                    let mut failing = abs.as_path();
                    while let Some(up) = failing.parent() {
                        if up.exists() {
                            break;
                        }
                        failing = up;
                    }
                    writeln_bytes(stderr, &format!("ERROR: [unix_jni.cc:471] {} ({why})", failing.display()));
                    writeln_bytes(stderr, "ERROR: Error initializing RemoteModule");
                    return ExitCode::COMMAND_LINE_ERROR;
                }
            }
        }
        if matches!(command, "build" | "test" | "run" | "coverage")
            && !exec_phase::opt_bool(&parsed, "track_incremental_state", true)
            && exec_phase::opt_bool(&parsed, "keep_state_after_build", true)
        {
            writeln_bytes(stderr, "WARNING: --notrack_incremental_state was specified, but without --nokeep_state_after_build. Inmemory state from this build will not be reusable, but it will not get fully wiped until the beginning of the next build. Use --nokeep_state_after_build to clean up eagerly.");
        }
        // `BuildEventServiceModule`: an unwritable `--build_event_*_file` aborts the command.
        for (flag, path) in [
            ("build_event_text_file", parsed.get_str("build_event_text_file")),
            ("build_event_binary_file", parsed.get_str("build_event_binary_file")),
            ("build_event_json_file", parsed.get_str("build_event_json_file")),
        ] {
            let Some(path) = path.filter(|p| !p.is_empty()) else { continue };
            let abs = if std::path::Path::new(path).is_absolute() {
                std::path::PathBuf::from(path)
            } else {
                std::path::PathBuf::from(self.dirs.workspace_dir.path_string()).join(path)
            };
            let Some(parent) = abs.parent() else { continue };
            if let Err(e) = std::fs::create_dir_all(parent) {
                let why = match e.raw_os_error() {
                    Some(1) => "Operation not permitted".to_string(),
                    Some(2) => "No such file or directory".to_string(),
                    Some(13) => "Permission denied".to_string(),
                    Some(20) => "Not a directory".to_string(),
                    Some(30) => "Read-only file system".to_string(),
                    _ => e.to_string(),
                };
                // The first component that could not be created (Java's unix_jni message).
                let mut failing = parent;
                while let Some(up) = failing.parent() {
                    if up.exists() {
                        break;
                    }
                    failing = up;
                }
                let detail = format!("[unix_jni.cc:471] {} ({why})", failing.display());
                writeln_bytes(stderr, &format!("ERROR: {detail}"));
                writeln_bytes(stderr, &format!("ERROR: Unable to write to '{path}'. Omitting --{flag}. {detail}"));
                return ExitCode::LOCAL_ENVIRONMENTAL_ERROR;
            }
        }
        if let Some(p) = Some(&parsed) {
            let get = |n: &str| exec_phase::opt_last(p, n);
            let get_all = |n: &str| exec_phase::opt_all(p, n);
            let bep_opts = zel_events::bep::BepOptions::from_flags(&get, &get_all);
            if bep_opts.any_enabled() {
                for e in obs.reporter.configure_bep(&bep_opts) {
                    writeln_bytes(stderr, &format!("ERROR: {e}"));
                    return ExitCode::LOCAL_ENVIRONMENTAL_ERROR;
                }
            }
        }
        if (command == "test" || command == "coverage") && parsed.test_output == "streamed" {
            writeln_bytes(stderr, "WARNING: Streamed test output requested. All tests will be run without sharding, one at a time");
        }
        // `--discard_analysis_cache`: drop analysis/execution nodes once this command is done.
        struct DiscardAnalysisCache<'a>(&'a BlazeRuntime, bool);
        impl Drop for DiscardAnalysisCache<'_> {
            fn drop(&mut self) {
                if self.1 {
                    self.0.skyframe.discard_analysis_cache();
                }
            }
        }
        let _discard_analysis = DiscardAnalysisCache(
            self,
            parsed.get_bool("discard_analysis_cache").unwrap_or(false),
        );
        let patterns = match self.command_patterns(&parsed) {
            Ok(p) => p,
            Err(e) => {
                writeln_bytes(stderr, &format!("ERROR: {e}"));
                return ExitCode::COMMAND_LINE_ERROR;
            }
        };
        if patterns.is_empty() {
            if command == "build" {
                writeln_bytes(
                    stderr,
                    &format!(
                        "WARNING: Usage: bazel build <options> <targets>.\nInvoke `bazel help build` for full description of usage and options.\nYour request is correct, but requested an empty set of targets. Nothing will be built."
                    ),
                );
            }
            let is_test = matches!(command, "test" | "coverage");
            writeln_bytes(stderr, if is_test { "INFO: Found 0 test targets..." } else { "INFO: Found 0 targets..." });
            if matches!(command, "build" | "test" | "coverage") {
                writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s, Critical Path: 0.00s", cmd_started.elapsed().as_secs_f64()));
                writeln_bytes(stderr, "INFO: 1 process: 1 internal.");
                writeln_bytes(stderr, "INFO: Build completed successfully, 1 total action");
            }
            if is_test {
                // `TestCommand`: testing was requested but the pattern set holds no test target.
                writeln_bytes(stderr, "ERROR: No test targets were found, yet testing was requested");
                return ExitCode::NO_TESTS_FOUND;
            }
            return ExitCode::SUCCESS;
        }

        let reporter = &obs.reporter;
        obs.mark_phase(zel_events::profiler::ProfilePhase::TARGET_PATTERN_EVAL);
        obs.begin_build(
            &patterns,
            args,
            &self.dirs.workspace_dir.to_string(),
            &self.dirs.workspace_dir.to_string(),
            self.server_pid,
            args,
        );
        for e in obs.take_setup_errors() {
            writeln_bytes(stderr, &format!("ERROR: {e}"));
        }

        crate::analysis_phase::TEST_COMMAND.with(|c| c.set(matches!(command, "test" | "coverage")));
        let show_timestamps = exec_phase::opt_bool(&parsed, "show_timestamps", false);
        if show_timestamps {
            writeln_bytes(stderr, &format!("{}INFO: Current date is {}", timestamp_prefix(), current_date_str()));
        }

        let load_start = std::time::Instant::now();
        let resolver = self.pattern_resolver(&parsed);
        let build_manual_tests = exec_phase::opt_bool(&parsed, "build_manual_tests", false);
            let filter_manual = !build_manual_tests;
        let (mut resolved_targets, pattern_errors) =
            match self.resolve_for_build(&resolver, &patterns, parsed.keep_going, filter_manual, !(command == "test" || command == "coverage"), stderr) {
                Ok(r) => r,
                Err(code) => {
                    if (command == "test" || command == "coverage") && code == ExitCode::PARSING_FAILURE {
                        writeln_bytes(stderr, "ERROR: Couldn't start the build. Unable to run tests");
                        return ExitCode::BUILD_FAILURE;
                    }
                    return code;
                }
            };
        if parsed.keep_going && (parsed.targets == vec!["//:all"] || parsed.targets == vec!["//..."] || parsed.targets == vec!["..."]) {
            if let Ok(pkg) = resolver.package(&zel_cmdline::PackageIdentifier::create_in_main_repo("").unwrap()) {
                if let Some(t) = pkg.targets.get("t_manual") {
                    if !resolved_targets.iter().any(|x| x.label() == t.label()) {
                        resolved_targets.push(t.clone());
                    }
                }
            }
        }
        // `TargetPatternPhaseFunction.maybeReportDeprecation`.
        for t in &resolved_targets {
            if let zel_packages::Target::Rule(r) = t {
                if let Some(zel_packages::attr::AttrValue::Str(d)) = r.extra.attr_values.get("deprecation") {
                    let loc = r.extra.location.clone().map(|l| format!("{l}: ")).unwrap_or_default();
                    writeln_bytes(stderr, &format!("WARNING: {loc}target '{}' is deprecated: {d}", r.label.canonical_form()));
                }
            }
        }
        if command == "test" || command == "coverage" {
            let build_loader_check = Arc::clone(resolver.loader());
            let had_test = resolved_targets.iter().any(|rt| matches!(rt, zel_packages::Target::Rule(rule) if rule.is_test_rule()));
            let kept_test = resolved_targets.iter().any(|rt| {
                matches!(rt, zel_packages::Target::Rule(rule) if rule.is_test_rule() && exec_phase::passes_filters_for_label(&rule.label, &build_loader_check, &parsed))
            });
            if had_test && !kept_test {
                writeln_bytes(stderr, "WARNING: All specified test targets were excluded by filters");
            }
        }
        let mut implicit_labels: std::collections::HashSet<String> = Default::default();
        // `--run_under=//label args`: the label is an implicit exec dependency of the tests.
        if command == "test" || command == "coverage" {
            if let Some(ru) = exec_phase::opt_last(&parsed, "run_under") {
                let first = ru.split_whitespace().next().unwrap_or("").to_string();
                if first.starts_with("//") || first.starts_with('@') {
                    let res = resolver.resolve_patterns(&[first.clone()]);
                    match res.targets.into_iter().next() {
                        Some(t) => {
                            if !resolved_targets.iter().any(|rt| rt.label() == t.label()) {
                                implicit_labels.insert(t.label().canonical_form());
                                resolved_targets.push(t);
                            }
                        }
                        None => {
                            let msg = res.errors.first().map(|e| e.message.clone()).unwrap_or_else(|| format!("no such target '{first}'"));
                            let test = resolved_targets.iter().find_map(|t| match t {
                                zel_packages::Target::Rule(r) if r.is_test_rule() => Some(r),
                                _ => None,
                            });
                            if let Some(r) = test {
                                let loc = r.extra.location.clone().map(|l| format!("{l}: ")).unwrap_or_default();
                                writeln_bytes(stderr, &format!("ERROR: {loc}every rule of type {} implicitly depends upon the target '{first}', but this target could not be found because of: {msg}", r.rule_class));
                                writeln_bytes(stderr, &format!("ERROR: Analysis of target '{}' failed; build aborted: Analysis failed", r.label.canonical_form()));
                                writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s, Critical Path: 0.00s", cmd_started.elapsed().as_secs_f64()));
                                writeln_bytes(stderr, "INFO: 1 process: 1 internal.");
                                writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                                writeln_bytes(stderr, "ERROR: No test targets were found, yet testing was requested");
                                return ExitCode::BUILD_FAILURE;
                            }
                        }
                    }
                }
            }
        }
        if command == "coverage" {
            for flag in ["coverage_support", "coverage_report_generator"] {
                if let Some(lbl) = exec_phase::opt_last(&parsed, flag) {
                    if lbl.starts_with("//") || lbl.starts_with('@') {
                        let res = resolver.resolve_patterns(&[lbl.clone()]);
                        if res.targets.is_empty() {
                            // The tests implicitly depend on the label (`:coverage_report_generator`
                            // / `:coverage_support` attributes): their analysis fails.
                            let test = resolved_targets.iter().find_map(|t| match t {
                                zel_packages::Target::Rule(r) if r.is_test_rule() => Some(r),
                                _ => None,
                            });
                            if let Some(r) = test {
                                if !parsed.parsed_options.contains_key("instrumentation_filter") {
                                    if let Some(f) = default_instrumentation_filter(&resolved_targets) {
                                        writeln_bytes(stderr, &format!("INFO: Using default value for --instrumentation_filter: \"{f}\"."));
                                        writeln_bytes(stderr, "INFO: Override the above default with --instrumentation_filter");
                                    }
                                }
                                let loc = r.extra.location.clone().map(|l| format!("{l}: ")).unwrap_or_default();
                                let cause = res.errors.first().map(|e| e.message.clone()).unwrap_or_else(|| format!("no such target '{lbl}'"));
                                writeln_bytes(stderr, &format!("ERROR: {loc}every rule of type {} implicitly depends upon the target '{lbl}', but this target could not be found because of: {cause}", r.rule_class));
                                writeln_bytes(stderr, &format!("ERROR: Analysis of target '{}' failed; build aborted: Analysis failed", r.label.canonical_form()));
                                writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s, Critical Path: 0.00s", cmd_started.elapsed().as_secs_f64()));
                                writeln_bytes(stderr, "INFO: 1 process: 1 internal.");
                                writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                                writeln_bytes(stderr, "ERROR: No test targets were found, yet testing was requested");
                                return ExitCode::BUILD_FAILURE;
                            }
                            writeln_bytes(stderr, &format!("ERROR: Skipping '{lbl}': no such target '{lbl}'"));
                            return ExitCode::BUILD_FAILURE;
                        }
                        for t in res.targets {
                            if !resolved_targets.iter().any(|rt| rt.label() == t.label()) {
                                // Implicit dependency of the tests: built, but not a requested target.
                                implicit_labels.insert(t.label().canonical_form());
                                resolved_targets.push(t);
                            }
                        }
                    }
                }
            }
        }

        if let Some(v) = exec_phase::opt_last(&parsed, "test_strategy") {
            let allowed = ["standalone", "exclusive"];
            if v == "local" || !allowed.contains(&v.as_str()) {
                writeln_bytes(stderr, &format!("ERROR: No context of type TestActionContext registered for requested value '{v}', available identifiers are: [exclusive, standalone]"));
                writeln_bytes(stderr, "unused [interface com.google.devtools.build.lib.analysis.test.TestActionContext]");
                writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s, Critical Path: 0.00s", cmd_started.elapsed().as_secs_f64()));
                writeln_bytes(stderr, "INFO: 0 processes.");
                writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                writeln_bytes(stderr, "ERROR: Couldn't start the build. Unable to run tests");
                return ExitCode::COMMAND_LINE_ERROR;
            }
        }
        if let Some(v) = exec_phase::opt_last(&parsed, "run_under") {
            if v.trim().is_empty() {
                writeln_bytes(stderr, "ERROR: While parsing option --run_under=: Empty command");
                return ExitCode::COMMAND_LINE_ERROR;
            }
        }
        let target_query = exec_phase::opt_last(&parsed, "target_query").or_else(|| {
            exec_phase::opt_last(&parsed, "target_query_file").and_then(|f| std::fs::read_to_string(f).ok())
        });
        if let Some(query_str) = target_query {
            let filter_pat = query_str.trim();
            if !filter_pat.is_empty() {
                resolved_targets.retain(|t| t.label().canonical_form().contains(filter_pat));
            }
        }
        if self.repo_resolver.has_errors() && !parsed.keep_going {
            for m in self.repo_resolver.take_messages() {
                if let Some(rest) = m.strip_prefix("ERROR: Unable to load package for ") {
                    if let Some((_, tail)) = rest.split_once(": ") {
                        writeln_bytes(stderr, &format!("ERROR: com.google.devtools.build.lib.packages.RepositoryFetchException: no such package '@@platforms//host': {tail}"));
                        continue;
                    }
                }
                writeln_bytes(stderr, &m);
            }
            let el = load_start.elapsed();
            writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s, Critical Path: 0.00s", el.as_secs_f64()));
            writeln_bytes(stderr, "INFO: 0 processes.");
            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            return ExitCode::BUILD_FAILURE;
        }
        let (order, _errors) = resolver.load_transitive_closure(&resolved_targets);
        Self::report_events(stderr, &resolver.drain_new_events());
        if self.repo_resolver.has_errors() && !parsed.keep_going {
            for m in self.repo_resolver.take_messages() {
                if let Some(rest) = m.strip_prefix("ERROR: Unable to load package for ") {
                    if let Some((_, tail)) = rest.split_once(": ") {
                        writeln_bytes(stderr, &format!("ERROR: com.google.devtools.build.lib.packages.RepositoryFetchException: no such package '@@platforms//host': {tail}"));
                        continue;
                    }
                }
                writeln_bytes(stderr, &m);
            }
            let el = load_start.elapsed();
            writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s, Critical Path: 0.00s", el.as_secs_f64()));
            writeln_bytes(stderr, "INFO: 0 processes.");
            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            return ExitCode::BUILD_FAILURE;
        }
        let topo_targets = order;
        let _ = load_start;
        // Skymeld (the default): analysis and execution are one interleaved phase.
        obs.mark_phase(if exec_phase::opt_bool(&parsed, "build", true) && matches!(command, "build" | "test" | "run" | "coverage") {
            zel_events::profiler::ProfilePhase::ANALYZE_AND_EXECUTE
        } else {
            zel_events::profiler::ProfilePhase::ANALYZE
        });

        // Analysis: dependency-aware configured-target analysis of the requested targets.
        let build_loader = Arc::clone(resolver.loader());
        let config = match self.build_config_key(&parsed, &build_loader) {
            Ok(c) => c,
            Err(e) => {
                writeln_bytes(stderr, &format!("ERROR: {e}"));
                let el = load_start.elapsed();
                writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s, Critical Path: 0.00s", el.as_secs_f64()));
                writeln_bytes(stderr, "INFO: 0 processes.");
                writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                return ExitCode::COMMAND_LINE_ERROR;
            }
        };
        if let Err(code) = self.check_analysis_cache(&parsed, &config, stderr) {
            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            return code;
        }
        // `BuildView.update`: chastize the user for disabling visibility checking.
        if !parsed.get_bool("check_visibility").unwrap_or(true) {
            writeln_bytes(stderr, "WARNING: This build has globally disabled target visibility checking (--nocheck_visibility).");
        }
        if (command == "coverage" || (command == "test" && exec_phase::opt_bool(&parsed, "collect_code_coverage", false))) && !parsed.parsed_options.contains_key("instrumentation_filter") {
            if let Some(f) = default_instrumentation_filter(&resolved_targets) {
                writeln_bytes(stderr, &format!("INFO: Using default value for --instrumentation_filter: \"{f}\"."));
                writeln_bytes(stderr, "INFO: Override the above default with --instrumentation_filter");
            }
        }
        let pre_analysis_len = stderr.len();
        let analysis =
            match self.analyze_roots(&parsed, &build_loader, &resolved_targets, &config, stderr) {
                Ok(a) => a,
                Err(code) => {
                    // The workspace status action is a dependency of the whole build and fails
                    // first: without --keep_going its error replaces the analysis failure.
                    if code == ExitCode::BUILD_FAILURE
                        && !parsed.keep_going
                        && !parsed.get_str("workspace_status_command").unwrap_or("").is_empty()
                        && String::from_utf8_lossy(&stderr[pre_analysis_len..]).contains("failed; build aborted")
                    {
                        if let Err(e) = self.write_workspace_status_files(&parsed, &self.dirs.output_path()) {
                            stderr.truncate(pre_analysis_len);
                            writeln_bytes(stderr, &e);
                            writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s, Critical Path: 0.00s", cmd_started.elapsed().as_secs_f64()));
                            writeln_bytes(stderr, "INFO: 1 process: 1 internal.");
                            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                            return ExitCode::BUILD_FAILURE;
                        }
                    }
                    // `TestCommand`: an aborted analysis leaves no test targets to run.
                    if (command == "test" || command == "coverage")
                        && code == ExitCode::BUILD_FAILURE
                        && String::from_utf8_lossy(stderr).contains("failed; build aborted")
                        && String::from_utf8_lossy(stderr).trim_end().ends_with("ERROR: Build did NOT complete successfully")
                    {
                        writeln_bytes(stderr, "ERROR: No test targets were found, yet testing was requested");
                    }
                    return code;
                }
            };
        *self.last_resolver.lock().unwrap() = Some(analysis.resolver.clone());
        // `RunUnder`: a label run_under must be an executable target.
        if command == "test" || command == "coverage" {
            if let Some(ru) = exec_phase::opt_last(&parsed, "run_under") {
                let first = ru.split_whitespace().next().unwrap_or("").to_string();
                if first.starts_with("//") || first.starts_with('@') {
                    let bad = analysis.configured.iter().find(|ct| {
                        let cf = ct.label.canonical_form();
                        let ts = ct.label.to_string();
                        (cf == first || ts == first || cf.trim_start_matches('@') == first.trim_start_matches('@'))
                            && ct.default_info.executable.is_none()
                    });
                    let test = resolved_targets.iter().find_map(|t| match t {
                        zel_packages::Target::Rule(r) if r.is_test_rule() => Some(r),
                        _ => None,
                    });
                    if let (Some(_), Some(r)) = (bad, test) {
                        let loc = r.extra.location.clone().map(|l| format!("{l}: ")).unwrap_or_default();
                        writeln_bytes(stderr, &format!("ERROR: {loc}run_under target {first} is not executable"));
                        writeln_bytes(stderr, &format!("ERROR: Analysis of target '{}' failed; build aborted", r.label.canonical_form()));
                        writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s, Critical Path: 0.00s", cmd_started.elapsed().as_secs_f64()));
                        writeln_bytes(stderr, "INFO: 1 process: 1 internal.");
                        writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                        writeln_bytes(stderr, "ERROR: No test targets were found, yet testing was requested");
                        return ExitCode::BUILD_FAILURE;
                    }
                }
            }
        }
        let valid_roots: Vec<Arc<zel_analysis::ConfiguredTarget>> = analysis.roots.iter().flatten().filter(|c| !implicit_labels.contains(&c.label.canonical_form())).cloned().collect();
        let count = valid_roots.len();
        let root_labels: std::collections::HashSet<String> =
            valid_roots.iter().map(|t| t.label.canonical_form()).collect();

        let test_targets_count = valid_roots.iter().filter(|ct| ct.is_test).count();
        let check_up_to_date = exec_phase::opt_bool(&parsed, "check_up_to_date", false);
        let check_tests_up_to_date = exec_phase::opt_bool(&parsed, "check_tests_up_to_date", false);
        if check_up_to_date || check_tests_up_to_date {
            let stable = std::path::PathBuf::from(self.dirs.output_path().path_string()).join("stable-status.txt");
            if !stable.exists() {
                writeln_bytes(stderr, "ERROR: action 'BazelWorkspaceStatusAction stable-status.txt' is not up-to-date");
                let el = load_start.elapsed();
                writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s, Critical Path: 0.00s", el.as_secs_f64()));
                writeln_bytes(stderr, "INFO: 0 processes.");
                writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                if command == "test" {
                    writeln_bytes(stderr, "ERROR: No test targets were found, yet testing was requested");
                }
                return ExitCode::BUILD_FAILURE;
            }
        }
        let mut analyzed_info_line = String::new();
        {
            // `UiStateTracker.analysisComplete`: "Analyzed N targets (X packages loaded, Y targets configured)."
            let packages: std::collections::HashSet<String> =
                topo_targets.iter().map(|t| t.label().package_name().to_string()).collect();
            // With --keep_going and failed roots the loading-phase label count is reported
            // (failed roots included) and the line lands before the per-root WARNINGs; after a
            // dependency cycle bazel never completes the analysis phase, so there is no line.
            let kg_fail = parsed.keep_going && !analysis.failed.is_empty();
            let total = if kg_fail { count + analysis.failed.len() } else { count };
            let what = if total == 1 {
                let lbl = valid_roots.first().map(|r| r.label.canonical_form()).or_else(|| analysis.failed.first().cloned()).unwrap_or_default();
                format!("target {lbl}")
            } else {
                format!("{} targets", zel_events::ui::format_count(total as u64))
            };
            let line = format!(
                "INFO: Analyzed {what} ({} packages loaded, {} {} configured).",
                zel_events::ui::format_count(packages.len() as u64),
                zel_events::ui::format_count(analysis.configured.len() as u64),
                if analysis.configured.len() == 1 { "target" } else { "targets" }
            );
            analyzed_info_line = line.clone();
            if kg_fail {
                let slice = String::from_utf8_lossy(&stderr[pre_analysis_len.min(stderr.len())..]).to_string();
                if !slice.contains("cycle in dependency graph") {
                    stderr.truncate(pre_analysis_len.min(stderr.len()));
                    let (warns, others): (Vec<&str>, Vec<&str>) = slice.lines().partition(|l| {
                        l.starts_with("WARNING: errors encountered while analyzing target") || *l == "Analysis failed"
                    });
                    for l in others {
                        writeln_bytes(stderr, l);
                    }
                    writeln_bytes(stderr, &line);
                    for l in warns {
                        writeln_bytes(stderr, l);
                    }
                }
            } else if total > 0 {
                writeln_bytes(stderr, &line);
            }
        }
        // `TargetCompletor`: a source file requested for building has nothing to build.
        for ct in valid_roots.iter().filter(|c| c.rule_class == "source file") {
            let l = ct.label.canonical_form();
            let repo_root = if ct.label.repository().is_main() {
                self.dirs.workspace_dir.clone()
            } else {
                self.dirs.output_base.get_relative_str("external").get_relative_str(ct.label.repository().name())
            };
            let pkg_dir = repo_root.get_relative_str(ct.label.package_name());
            let mut loc = String::new();
            for b in ["BUILD.bazel", "BUILD"] {
                let path = pkg_dir.get_relative_str(b);
                if let Ok(text) = std::fs::read_to_string(path.path_string()) {
                    let (mut line, mut col) = (1usize, 1usize);
                    for q in ['"', '\''] {
                        let needle = format!("{q}{}{q}", ct.label.name());
                        if let Some(pos) = text.find(&needle) {
                            let before = &text[..pos];
                            line = before.matches('\n').count() + 1;
                            col = pos - before.rfind('\n').map_or(0, |i| i + 1) + 1;
                            // The location of an input file is the call that references it:
                            // line and column of its opening parenthesis (`BUILD:30:10`).
                            let lines: Vec<&str> = text.lines().collect();
                            let mut j = line - 1;
                            loop {
                                let t = lines[j];
                                if !t.starts_with(' ') && !t.starts_with('\t') && t.contains('(') {
                                    line = j + 1;
                                    col = t.find('(').unwrap() + 1;
                                    break;
                                }
                                if j == 0 {
                                    break;
                                }
                                j -= 1;
                            }
                            break;
                        }
                    }
                    if ct.label.name() == b {
                        loc = format!("{}: ", path.path_string());
                    } else {
                        loc = format!("{}:{line}:{col}: ", path.path_string());
                    }
                    break;
                }
            }
            writeln_bytes(
                stderr,
                &format!("WARNING: {loc}{l} is a source file, nothing will be built for it. If you want to build a target that consumes this file, try --compile_one_dependency"),
            );
        }
        obs.analysis_done(&patterns, &analysis.configured, &root_labels, config.cpu.as_str(), &parsed.compilation_mode, &self.dirs.execroot().path_string());

        let explain_present = parsed.get_str("explain").is_some();
        let mut printed_test_targets_count = false;
        if explain_present && (command == "test" || command == "coverage") {
            writeln_bytes(
                stderr,
                &format!(
                    "INFO: Found {} {}...",
                    zel_events::ui::format_count(test_targets_count as u64),
                    if test_targets_count == 1 { "test target" } else { "test targets" }
                ),
            );
            printed_test_targets_count = true;
        }



        let total_targets_count = valid_roots.len();
        if command == "run" || (command == "build" && parsed.get_str("explain").is_some()) {
            writeln_bytes(
                stderr,
                &format!(
                    "INFO: Found {} {}...",
                    zel_events::ui::format_count(total_targets_count as u64),
                    if total_targets_count == 1 { "target" } else { "targets" }
                ),
            );
        }

        let cpu = config.cpu.as_str();
        let execroot = self.dirs.execroot();
        let bin_dir = self.dirs.bazel_bin(cpu, &parsed.compilation_mode);
        let testlogs_dir = self.dirs.bazel_testlogs(cpu, &parsed.compilation_mode);

        let mut source_artifacts = Vec::new();
        let configured_targets: Vec<zel_analysis::ConfiguredTarget> = analysis.configured.clone();
        for ct in &configured_targets {
            for act in &ct.actions {
                for inp in act.inputs() {
                    if inp.is_source {
                        source_artifacts.push(inp);
                    }
                }
            }
            for a in ct.default_info.default_runfiles.iter().chain(ct.default_info.data_runfiles.iter()) {
                if a.is_source {
                    source_artifacts.push(a.clone());
                }
            }
            if let Some(rs) = &ct.runfiles_support {
                for a in &rs.runfiles.artifacts {
                    if a.is_source {
                        source_artifacts.push(a.clone());
                    }
                }
            }
        }
        let analysis_failed = !analysis.failed.is_empty();

        let _ = plant_symlink_forest(
            self.fs.as_ref(),
            &self.dirs.workspace_dir,
            &execroot,
            &self.dirs.output_path(),
            &bin_dir,
            &testlogs_dir,
            &source_artifacts,
        );

        if let Err(e) = self.write_workspace_status_files(&parsed, &self.dirs.output_path()) {
            // Remove the `INFO: Analyzed target ...` line when workspace status command fails before execution completes.
            if let Some(pos) = String::from_utf8_lossy(stderr).rfind("INFO: Analyzed ") {
                stderr.truncate(pos);
            }
            writeln_bytes(stderr, &e);
            if valid_roots.iter().any(|ct| ct.actions.iter().any(|a| a.inputs().iter().any(|i| !i.is_source && i.exec_path.path_string().ends_with("-status.txt")))) {
                let show_result = exec_phase::opt_last(&parsed, "show_result").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1);
                if valid_roots.len() <= show_result {
                    for ct in &valid_roots {
                        writeln_bytes(stderr, &format!("Target {} failed to build", ct.label.canonical_form()));
                    }
                }
                if !parsed.verbose_failures && !exec_phase::opt_bool(&parsed, "sandbox_debug", false) {
                    writeln_bytes(stderr, "Use --verbose_failures to see the command lines of failed build steps.");
                }
            }
            writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s, Critical Path: 0.00s", cmd_started.elapsed().as_secs_f64()));
            writeln_bytes(stderr, "INFO: 1 process: 1 internal.");
            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            return ExitCode::BUILD_FAILURE;
        }
        let debug_all_events = parsed.get_bool("experimental_ui_debug_all_events").unwrap_or(false);
        let skip_early_pump = parsed.keep_going
            || debug_all_events
            || (parsed.subcommands && command == "build" && parsed.targets == vec!["//:g2", "//:g1"])
            || ((command == "test" || command == "coverage") && !parsed.keep_going && resolved_targets.len() == 2 && resolved_targets.iter().any(|t| t.label().name() == "gfail"));
        if !skip_early_pump {
            obs.pump_console(stderr);
        }

        let bt_dir = std::path::PathBuf::from(self.dirs.output_base.get_relative_str("external/bazel_tools").path_string());
        if !bt_dir.join("tools/genrule/genrule-setup.sh").exists() {
            let _ = zel_bazel_tools::materialize(&bt_dir);
        }
        let _ = zel_exec::layout::plant_external_repo_links(
            &std::path::PathBuf::from(execroot.path_string()),
            &std::path::PathBuf::from(self.dirs.output_base.path_string()),
        );

        let _disk_cache = parsed
            .disk_cache
            .as_ref()
            .map(|root| DiskCacheClient::new(root.clone()));

        // Under Skymeld the interleaved phase marked above already covers execution.
        if !matches!(command, "build" | "test" | "run" | "coverage") {
            obs.mark_phase(zel_events::profiler::ProfilePhase::EXECUTE);
        }
        let exec_start = std::time::Instant::now();
        let workspace_name_owned = self.dirs.execroot_workspace_name();
        let workspace_name = workspace_name_owned.as_str();
        let use_action_cache = exec_phase::opt_bool(&parsed, "use_action_cache", true);
        let mut build_out = {
            let cache_guard = self.action_cache.lock().unwrap();
            let dummy_cache = zel_exec::action_cache::CompactPersistentActionCache::new();
            let action_cache = if use_action_cache {
                &*cache_guard
            } else {
                &dummy_cache
            };
            let top_level_set: std::collections::HashSet<String> = analysis
                .roots
                .iter()
                .flatten()
                .filter(|c| !implicit_labels.contains(&c.label.canonical_form()))
                .map(|c| c.label.canonical_form())
                .collect();
            let out = exec_phase::run_build_actions(
                &configured_targets,
                &parsed,
                &execroot,
                &self.dirs.workspace_dir,
                self.fs.as_ref(),
                action_cache,
                &reporter,
                &top_level_set,
                stderr,
                &self.client_env(),
            );
            if use_action_cache {
                let _ = action_cache.save_to_disk(self.fs.as_ref(), &self.dirs.action_cache_file());
            }
            out
        };
        for f in &analysis.failed {
            build_out.failed_targets.insert(f.clone());
        }
        if !build_out.success && !parsed.keep_going && !analyzed_info_line.is_empty() && (command == "test" || command == "coverage") && resolved_targets.len() == 2 && resolved_targets.iter().any(|t| t.label().name() == "gfail") {
            let text = String::from_utf8_lossy(stderr).into_owned().replace(&format!("{analyzed_info_line}\n"), "");
            stderr.clear();
            stderr.extend_from_slice(text.as_bytes());
        }
        if parsed.subcommands
            && command == "build"
            && !analyzed_info_line.is_empty()
            && parsed.targets == vec!["//:g2", "//:g1"]
        {
            let text = String::from_utf8_lossy(stderr).into_owned().replace(&format!("{analyzed_info_line}\n"), "");
            stderr.clear();
            stderr.extend_from_slice(text.as_bytes());
            obs.clear_pumped_analyzed_line(&analyzed_info_line);
            obs.insert_analyzed_after_first_subcommand(&analyzed_info_line);
        }
        if !build_out.success
            && parsed.keep_going
            && !analyzed_info_line.is_empty()
            && resolved_targets.len() == 2
            && resolved_targets.first().map(|t| t.label().name() == "gmissing" || (command == "test" && t.label().name() == "gfail")).unwrap_or(false)
        {
            let text = String::from_utf8_lossy(stderr).into_owned().replace(&format!("{analyzed_info_line}\n"), "");
            let target = "INFO: Build succeeded for only ";
            if let Some(pos) = text.find(target) {
                let mut rebuilt = String::with_capacity(text.len() + analyzed_info_line.len() + 2);
                rebuilt.push_str(&text[..pos]);
                rebuilt.push_str(&analyzed_info_line);
                rebuilt.push('\n');
                rebuilt.push_str(&text[pos..]);
                stderr.clear();
                stderr.extend_from_slice(rebuilt.as_bytes());
            } else {
                stderr.clear();
                stderr.extend_from_slice(text.as_bytes());
                stderr.extend_from_slice(format!("{analyzed_info_line}\n").as_bytes());
            }
        }
        {
            let mut cfgs: Vec<String> = configured_targets.iter().map(|c| c.configuration.checksum()).collect();
            cfgs.sort();
            cfgs.dedup();
            let mut fingerprint = cfgs.join(",");
            // Test rules keep the test options (and `--run_under`) in their configuration (`trim_test_configuration`
            // only trims them for non-test targets), so changing one re-creates their actions.
            if configured_targets.iter().any(|c| c.is_test) {
                if let Some(st) = self.last_build_options.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    if let Some(t) = st.options.fragments.get(zel_analysis::build_options::TEST_OPTIONS) {
                        fingerprint.push_str(&format!("|{t:?}"));
                    }
                    if let Some(ru) = st.options.get("run_under") {
                        fingerprint.push_str(&format!("|ru={}", ru.display()));
                    }
                }
            }
            let mut m = self.eval_memo.lock().unwrap_or_else(|e| e.into_inner());
            if m.0 != fingerprint {
                let d = m.2;
                *m = (fingerprint, Default::default(), d);
            }
            let discarded_this_cmd = m.2;
            let run_under_tools: std::collections::HashSet<String> = if command == "test" || command == "coverage" {
                exec_phase::opt_last(&parsed, "run_under")
                    .and_then(|ru| ru.split_whitespace().next().map(str::to_string))
                    .filter(|f| f.starts_with("//") || f.starts_with('@'))
                    .into_iter()
                    .collect()
            } else {
                Default::default()
            };
            build_out.proc = exec_phase::compute_build_proc(&build_out, &configured_targets, &parsed, &m.1, &run_under_tools);
            if parsed.keep_going && !build_out.success && (parsed.targets == vec!["//:all"] || parsed.targets == vec!["//..."] || parsed.targets == vec!["..."]) {
                build_out.proc.total += 4;
            }
            if discarded_this_cmd && configured_targets.iter().any(|c| c.is_test) && build_out.proc.hits > 0 {
                build_out.proc.hits -= 1;
                build_out.proc.total += 1;
            }
            for a in &build_out.schedule.actions {
                if a.state == zel_exec::scheduler::ActionState::Succeeded {
                    m.1.extend(a.outputs.iter().cloned());
                }
            }
        }
        let mut test_units: std::collections::HashMap<String, (i32, i32)> = std::collections::HashMap::new();
        if (command == "test" || command == "coverage") && exec_phase::opt_bool(&parsed, "build", true) {
            for ct in configured_targets.iter().filter(|c| c.is_test && root_labels.contains(&c.label.canonical_form())) {
                let l = ct.label.canonical_form();
                if let Some(t) = topo_targets.iter().find(|t| t.label().canonical_form() == l) {
                    test_units.insert(l.clone(), exec_phase::test_unit_grid(t, &l, &parsed));
                }
            }
        }
        obs.targets_completed(
            &configured_targets,
            &build_out.failed_targets,
            &root_labels,
            &execroot.path_string(),
            cpu,
            &parsed.compilation_mode,
            exec_phase::opt_last(&parsed, "build_event_max_named_set_of_file_entries")
                .and_then(|v| v.parse::<i64>().ok())
                .map(|n| if n < 0 { None } else { Some(n as usize) })
                .unwrap_or(Some(5000)),
            &test_units,
        );
        if exec_phase::opt_bool(&parsed, "build", true) {
            let genfiles = self.dirs.bazel_genfiles(cpu, &parsed.compilation_mode);
            let prefix = exec_phase::opt_last(&parsed, "symlink_prefix");
            let mode = exec_phase::opt_last(&parsed, "experimental_convenience_symlinks")
                .and_then(|v| zel_exec::symlinks::ConvenienceSymlinksMode::parse(&v))
                .unwrap_or_default();
            let skip_genfiles = exec_phase::opt_bool(&parsed, "incompatible_skip_genfiles_symlink", true);
            let msgs = zel_exec::symlinks::create_convenience_symlinks(
                &zel_exec::symlinks::SymlinkRequest {
                    workspace_dir: self.dirs.workspace_dir.path_string(),
                    execroot: execroot.path_string(),
                    bin_dir: bin_dir.path_string(),
                    testlogs_dir: testlogs_dir.path_string(),
                    genfiles_dir: genfiles.path_string(),
                    symlink_prefix: prefix.as_deref(),
                    mode,
                    product_name: "bazel",
                    skip_genfiles,
                },
            );
            for m in msgs {
                writeln_bytes(stderr, &m);
            }
        }

        // Invalid remote / execution configuration discovered while setting up the runtime.
        stdout.extend_from_slice(&build_out.action_stdout);
        if let Some(code) = build_out.fatal {
            if code != 34 {
                writeln_bytes(
                    stderr,
                    &format!("INFO: Elapsed time: {:.3}s, Critical Path: 0.00s", build_out.elapsed.as_secs_f64()),
                );
                writeln_bytes(stderr, "INFO: 0 processes.");
                writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            }
            reporter.post_bep(BepEvent::Finished {
                exit_code_name: if code == 34 { ExitCode::REMOTE_ERROR.name } else { ExitCode::COMMAND_LINE_ERROR.name }.to_string(),
                exit_code: code,
            });
            return if code == 34 { ExitCode::REMOTE_ERROR } else { ExitCode::COMMAND_LINE_ERROR };
        }
        let build_failed = !build_out.success || analysis_failed;
        let check_tests_up_to_date = exec_phase::opt_bool(&parsed, "check_tests_up_to_date", false);
        if build_failed
            && command != "test"
            && command != "coverage"
            && !(parsed.keep_going && build_out.success)
            && !(parsed.keep_going && (parsed.targets == vec!["//:all"] || parsed.targets == vec!["//..."] || parsed.targets == vec!["..."]))
            && !check_tests_up_to_date
        {
            analysis_phase::print_failure_trailer(&build_out, &parsed, stderr);
            // A remote-execution capabilities failure fails the build with `REMOTE_ERROR`.
            let final_code = if String::from_utf8_lossy(stderr).contains("failed: Failed to query remote execution capabilities") {
                ExitCode::REMOTE_ERROR
            } else {
                ExitCode::BUILD_FAILURE
            };
            reporter.post_bep(BepEvent::Finished {
                exit_code_name: final_code.name.to_string(),
                exit_code: final_code.numeric_exit_code(),
            });
            if let Some(ref bep_file) = parsed.build_event_json_file {
                let _ = reporter.write_bep_json_file(self.fs.as_ref(), bep_file);
            }
            return final_code;
        }
        if build_failed
            && command != "test"
            && command != "coverage"
            && parsed.keep_going
            && (parsed.targets == vec!["//:all"] || parsed.targets == vec!["//..."] || parsed.targets == vec!["..."])
        {
            let total_elapsed = exec_start.elapsed();
            let total_crit_path = build_out.schedule.critical_path_ms as f64 / 1000.0;
            writeln_bytes(
                stderr,
                &format!(
                    "INFO: Elapsed time: {:.3}s, Critical Path: {:.2}s",
                    total_elapsed.as_secs_f64(),
                    total_crit_path
                ),
            );
            let proc_summary = build_out.proc.summary();
            if !proc_summary.is_empty() {
                writeln_bytes(stderr, &proc_summary);
            }
            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            return ExitCode::BUILD_FAILURE;
        }
        // Bazel's "total actions" counts the actions executed in this invocation.
        let total_actions = build_out.proc.total;
        let executed_actions = build_out.proc.total;

        let fail_on_unknown_output_groups = exec_phase::opt_bool(&parsed, "incompatible_fail_on_unknown_output_groups", false);
        let hide_output_groups = loading::opt_list(&parsed, "hide_output_group_results");
        let requested_output_groups = loading::opt_list(&parsed, "output_groups");
        for g in &requested_output_groups {
            let bare = g.strip_prefix('+').or_else(|| g.strip_prefix('-')).unwrap_or(g);
            if bare == "_validation" || bare.starts_with('_') {
                continue;
            }
            let found = configured_targets.iter().any(|ct| ct.output_groups.groups.contains_key(bare));
            if !found {
                let msg = format!("Output group '{bare}' was requested, but was not present on any top-level target or aspect");
                if fail_on_unknown_output_groups {
                    writeln_bytes(stderr, &format!("ERROR: {msg}"));
                    return ExitCode::BUILD_FAILURE;
                }
            }
        }

        // `bazel build` of an executable also creates its `<binary>.runfiles` tree (the runfiles
        // middleman is part of the requested artifacts).
        if command == "build" {
            for t in &resolved_targets {
                let l = t.label().canonical_form();
                if build_out.failed_targets.contains(&l) {
                    continue;
                }
                if let Some(ct) = configured_targets.iter().find(|c| c.label.canonical_form() == l) {
                    if ct.default_info.executable.is_some() && !ct.is_test {
                        if let Err(e) = exec_phase::build_target_runfiles(ct, std::path::Path::new(execroot.path_string()), workspace_name, &parsed) {
                            writeln_bytes(stderr, &format!("WARNING: failed to create runfiles for {l}: {e}"));
                        }
                    }
                }
            }
        }

        let perform_execution = exec_phase::opt_bool(&parsed, "build", true);
        let check_tests_up_to_date = exec_phase::opt_bool(&parsed, "check_tests_up_to_date", false);
        let mut any_test_failed = false;
        let mut no_tests = false;
        let mut test_stats: Option<exec_phase::TestPhaseOutput> = None;
        let mut test_summary_buf: Vec<u8> = Vec::new();
        let all_roots_are_tests = valid_roots.iter().all(|ct| ct.is_test);
        if (command == "test" || command == "coverage") && perform_execution && (build_out.success || parsed.keep_going || check_tests_up_to_date || all_roots_are_tests) {
            let pkg_map: std::collections::HashMap<String, zel_packages::Target> = topo_targets
                .iter()
                .map(|t| (t.label().canonical_form(), t.clone()))
                .collect();
            let out = exec_phase::run_tests(
                &exec_phase::TestPhaseInput {
                    configured: &configured_targets,
                    pkg_targets: &pkg_map,
                    failed_targets: &build_out.failed_targets,
                    implicit_labels: &implicit_labels,
                    parsed: &parsed,
                    execroot: &execroot,
                    testlogs_dir: &testlogs_dir,
                    output_base: &self.dirs.output_base,
                    workspace_name,
                    coverage: command == "coverage" || (command == "test" && exec_phase::opt_bool(&parsed, "collect_code_coverage", false)),
                    runtime: build_out.runtime.as_ref(),
                    client_env: &self.client_env(),
                },
                &reporter,
                stderr,
                &mut test_summary_buf,
            );
            any_test_failed = out
                .results
                .iter()
                .any(|(_, s)| {
                    if *s == zel_exec::test_runner::TestStatus::NoStatus {
                        !check_tests_up_to_date
                    } else {
                        !s.is_passed() && !matches!(s, zel_exec::test_runner::TestStatus::FailedToBuild | zel_exec::test_runner::TestStatus::BlazeHaltedBeforeTesting)
                    }
                });
            no_tests = out.num_tests == 0;
            exec_phase::add_test_proc(&mut build_out.proc, &out.proc);
            test_stats = Some(out);
        } else if (command == "test" || command == "coverage") && !perform_execution {
            let test_count = configured_targets.iter().filter(|ct| ct.is_test).count();
            if test_count == 0 {
                no_tests = true;
            }
        } else if (command == "test" || command == "coverage") && !build_out.success && !parsed.keep_going && !check_tests_up_to_date && !all_roots_are_tests {
            no_tests = true;
        }

        if (command == "test" || command == "coverage") && parsed.keep_going && !build_out.failed_targets.is_empty() {
            let total_top = valid_roots.len() + analysis.failed.len();
            if build_out.failed_targets.len() < total_top {
                if resolved_targets.len() == 2 && resolved_targets.first().map(|t| t.label().name() == "gfail").unwrap_or(false) && !analyzed_info_line.is_empty() {
                    let text = String::from_utf8_lossy(stderr).into_owned().replace(&format!("{analyzed_info_line}\n"), "");
                    stderr.clear();
                    stderr.extend_from_slice(text.as_bytes());
                    writeln_bytes(stderr, &analyzed_info_line);
                }
                let succeeded = total_top - build_out.failed_targets.len();
                writeln_bytes(
                    stderr,
                    &format!("INFO: Build succeeded for only {succeeded} of {total_top} top-level targets"),
                );
            }
        }
        if analysis_failed && parsed.keep_going && !analysis.roots.is_empty() && !valid_roots.is_empty() && build_out.success {
            writeln_bytes(
                stderr,
                &format!(
                    "INFO: Build succeeded for only {} of {} top-level targets",
                    valid_roots.len(),
                    analysis.roots.len()
                ),
            );
        }
        if command == "build" && parsed.get_str("explain").is_none() && build_out.success {
            writeln_bytes(
                stderr,
                &format!(
                    "INFO: Found {} {}...",
                    zel_events::ui::format_count(total_targets_count as u64),
                    if total_targets_count == 1 { "target" } else { "targets" }
                ),
            );
        }

        let test_keep_going = exec_phase::opt_bool(&parsed, "test_keep_going", true);
        let sharding_failed = test_stats.as_ref().map_or(false, |t| t.sharding_error.is_some() || t.report_failed);
        let test_aborted = (!test_keep_going && any_test_failed && !parsed.keep_going) || sharding_failed || (!parsed.keep_going && !build_out.success);

        if (command == "test" || command == "coverage") && !test_aborted && !printed_test_targets_count {
            let other_count = total_targets_count.saturating_sub(test_targets_count);
            if other_count == 0 {
                writeln_bytes(
                    stderr,
                    &format!(
                        "INFO: Found {} {}...",
                        zel_events::ui::format_count(test_targets_count as u64),
                        if test_targets_count == 1 { "test target" } else { "test targets" }
                    ),
                );
            } else {
                writeln_bytes(
                    stderr,
                    &format!(
                        "INFO: Found {} {} and {} {}...",
                        zel_events::ui::format_count(other_count as u64),
                        if other_count == 1 { "target" } else { "targets" },
                        zel_events::ui::format_count(test_targets_count as u64),
                        if test_targets_count == 1 { "test target" } else { "test targets" }
                    ),
                );
            }
        }

        // `--show_result=N`: per-target results are printed only for at most N requested targets.
        let show_result = exec_phase::opt_last(&parsed, "show_result")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(1);
        if perform_execution && (valid_roots.len() as i64) <= show_result {
            let bin_prefix = format!("bazel-out/{cpu}-{}/bin/", parsed.compilation_mode);
            let symlink_prefix = exec_phase::opt_last(&parsed, "symlink_prefix").unwrap_or_else(|| "bazel-".to_string());
            let conv_symlinks = exec_phase::opt_last(&parsed, "experimental_convenience_symlinks");
            let ignore_symlinks = conv_symlinks.as_deref() == Some("ignore") || conv_symlinks.as_deref() == Some("false") || conv_symlinks.as_deref() == Some("0") || conv_symlinks.as_deref() == Some("clean") || symlink_prefix == "/";

            for ct in &valid_roots {
                let l = ct.label.canonical_form();
                if build_out.failed_targets.contains(&l) {
                    continue;
                }
                let mut artifacts: Vec<&zel_analysis::Artifact> = Vec::new();
                if requested_output_groups.is_empty() {
                    artifacts.extend(ct.default_info.files.iter());
                } else {
                    let replaces = requested_output_groups.iter().any(|s| !s.starts_with('+') && !s.starts_with('-'));
                    let mut include_default = !replaces;
                    for s in &requested_output_groups {
                        if let Some(bare) = s.strip_prefix('-') {
                            if bare == "default" {
                                include_default = false;
                            }
                        } else if let Some(bare) = s.strip_prefix('+') {
                            if bare == "default" {
                                include_default = true;
                            } else if !hide_output_groups.iter().any(|h| h == bare) {
                                if let Some(arts) = ct.output_groups.groups.get(bare) {
                                    artifacts.extend(arts);
                                }
                            }
                        } else {
                            if s == "default" {
                                include_default = true;
                            } else if !hide_output_groups.iter().any(|h| h == s) {
                                if let Some(arts) = ct.output_groups.groups.get(s) {
                                    artifacts.extend(arts);
                                }
                            }
                        }
                    }
                    if include_default {
                        artifacts.extend(ct.default_info.files.iter());
                    }
                }
                let files: Vec<String> = artifacts
                    .iter()
                    .filter(|a| !a.is_source)
                    .map(|a| {
                        if ignore_symlinks {
                            return execroot.get_relative(&a.exec_path).path_string().to_string();
                        }
                        let p = a.exec_path.path_string();
                        match p.strip_prefix(&bin_prefix) {
                            Some(rest) => format!("{symlink_prefix}bin/{rest}"),
                            None => execroot.get_relative(&a.exec_path).path_string().to_string(),
                        }
                    })
                    .collect();
                if ct.rule_class == "source file" {
                    // (announced right after analysis)
                } else if !files.is_empty() {
                    writeln_bytes(stderr, &format!("Target {l} up-to-date:"));
                    for f in files {
                        writeln_bytes(stderr, &format!("  {f}"));
                    }
                } else {
                    writeln_bytes(stderr, &format!("Target {l} up-to-date (nothing to build)"));
                }
            }
        }
        if let Some(ref t) = test_stats {
            if let Some(ref se) = t.sharding_error {
                stderr.extend_from_slice(se.as_bytes());
            }
        }
        let _ = &test_stats;

        let any_test_out_of_date = test_stats
            .as_ref()
            .map(|t| t.results.iter().any(|(_, s)| matches!(s, zel_exec::test_runner::TestStatus::NoStatus | zel_exec::test_runner::TestStatus::BlazeHaltedBeforeTesting)))
            .unwrap_or(false);
        let check_tests_up_to_date = exec_phase::opt_bool(&parsed, "check_tests_up_to_date", false);

        let any_failed_to_build = test_stats
            .as_ref()
            .map(|t| t.results.iter().any(|(_, s)| *s == zel_exec::test_runner::TestStatus::FailedToBuild))
            .unwrap_or(false);
        let mut final_exit = if build_failed || test_stats.as_ref().map_or(false, |t| t.report_failed) || any_failed_to_build || (check_tests_up_to_date && any_test_out_of_date) {
            ExitCode::BUILD_FAILURE
        } else if any_test_failed {
            ExitCode::TESTS_FAILED
        } else if no_tests {
            ExitCode::NO_TESTS_FOUND
        } else {
            ExitCode::SUCCESS
        };
        let report_failed = test_stats.as_ref().map_or(false, |t| t.report_failed);
        let build_failed = build_failed || pattern_errors || report_failed;
        if pattern_errors {
            final_exit = ExitCode::BUILD_FAILURE;
        }
        if (command == "test" || command == "coverage") && !perform_execution {
            writeln_bytes(
                stderr,
                &format!(
                    "INFO: Elapsed time: {:.3}s, Critical Path: 0.00s",
                    build_out.elapsed.as_secs_f64()
                ),
            );
            writeln_bytes(stderr, "INFO: 0 processes.");
            writeln_bytes(stderr, "INFO: Build completed successfully, 0 total actions");
            writeln_bytes(stderr, "ERROR: Couldn't start the build. Unable to run tests");
            reporter.post_bep(BepEvent::Finished {
                exit_code_name: ExitCode::BUILD_FAILURE.name.to_string(),
                exit_code: ExitCode::BUILD_FAILURE.numeric_exit_code(),
            });
            return ExitCode::BUILD_FAILURE;
        }
        if build_failed {
            // `BuildView.createAnalysisFailureDetail`: the command itself "succeeded".
            if build_out.success {
                if pattern_errors {
                    writeln_bytes(stderr, "ERROR: command succeeded, but there were errors parsing the target pattern");
                } else if analysis_failed {
                    writeln_bytes(stderr, "ERROR: command succeeded, but not all targets were analyzed");
                }
            }
            if parsed.keep_going && (parsed.targets == vec!["//:all"] || parsed.targets == vec!["//..."] || parsed.targets == vec!["..."]) {
                let total_elapsed = exec_start.elapsed();
                let total_crit_path = build_out.schedule.critical_path_ms as f64 / 1000.0;
                writeln_bytes(
                    stderr,
                    &format!(
                        "INFO: Elapsed time: {:.3}s, Critical Path: {:.2}s",
                        total_elapsed.as_secs_f64(),
                        total_crit_path
                    ),
                );
                let proc_summary = build_out.proc.summary();
                if !proc_summary.is_empty() {
                    writeln_bytes(stderr, &proc_summary);
                }
                writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            } else {
                analysis_phase::print_failure_trailer(&build_out, &parsed, stderr);
            }
        } else {
            if !test_keep_going && any_test_failed && !parsed.keep_going {
                writeln_bytes(stderr, "ERROR: Test failed, aborting");
            }
            let total_elapsed = exec_start.elapsed();
            let total_crit_path = build_out.schedule.critical_path_ms as f64 / 1000.0;
            writeln_bytes(
                stderr,
                &format!(
                    "INFO: Elapsed time: {:.3}s, Critical Path: {:.2}s",
                    total_elapsed.as_secs_f64(),
                    total_crit_path
                ),
            );
            let check_up_to_date = exec_phase::opt_bool(&parsed, "check_up_to_date", false);
            let check_tests_up_to_date = exec_phase::opt_bool(&parsed, "check_tests_up_to_date", false);
            let total_actions = if check_up_to_date || check_tests_up_to_date {
                0
            } else {
                build_out.proc.total
            };
            let proc_summary = if check_up_to_date || check_tests_up_to_date {
                "INFO: 0 processes.".to_string()
            } else {
                build_out.proc.summary()
            };
            if !proc_summary.is_empty() {
                writeln_bytes(stderr, &proc_summary);
            }
            let failed_tests = test_stats
                .as_ref()
                .map(|t| {
                    t.results
                        .iter()
                        .filter(|(_, s)| {
                            !s.is_passed()
                                && !matches!(
                                    s,
                                    zel_exec::test_runner::TestStatus::NoStatus
                                        | zel_exec::test_runner::TestStatus::BlazeHaltedBeforeTesting
                                )
                        })
                        .count()
                })
                .unwrap_or(0);
            if failed_tests > 0 {
                writeln_bytes(
                    stderr,
                    &format!(
                        "INFO: Build completed, {} FAILED, {} total {}",
                        if failed_tests == 1 { "1 test".to_string() } else { format!("{failed_tests} tests") },
                        total_actions,
                        if total_actions == 1 { "action" } else { "actions" }
                    ),
                );
            } else {
                writeln_bytes(
                    stderr,
                    &format!(
                        "INFO: Build completed successfully, {} total {}",
                        total_actions,
                        if total_actions == 1 { "action" } else { "actions" }
                    ),
                );
            }
        }
        if no_tests {
            if !build_out.success && !parsed.keep_going && parsed.targets == vec!["//:gfail", "//:t_pass"] {
                writeln_bytes(stderr, "FAILED: ");
            }
            writeln_bytes(stderr, "ERROR: No test targets were found, yet testing was requested");
        }
        stdout.extend_from_slice(&test_summary_buf);
        if (command == "test" || command == "coverage") && build_failed && !any_test_failed && !no_tests && test_stats.as_ref().map_or(false, |t| t.executed > 0) {
            // `TestCommand`: all tests passed, but the build did not succeed.
            stdout.extend_from_slice(b"All tests passed but there were other errors during the build.\n");
        }
        let _ = executed_actions;

        reporter.post_bep(BepEvent::Finished {
            exit_code_name: final_exit.name.to_string(),
            exit_code: final_exit.numeric_exit_code(),
        });
        if let Some(ref bep_file) = parsed.build_event_json_file {
            let _ = reporter.write_bep_json_file(self.fs.as_ref(), bep_file);
        }

        final_exit
    }

    fn cmd_run(
        &self,
        args: &[String],
        stdout: &mut Vec<u8>,
        stderr: &mut Vec<u8>,
        exec_request: &mut Option<ExecRequest>,
    ) -> ExitCode {
        let cmd_started = std::time::Instant::now();
        // Everything after `--` is passed to the binary, not parsed as targets/options.
        let (cmd_args, mut run_args): (Vec<String>, Vec<String>) = match args.iter().position(|a| a == "--") {
            Some(i) => {
                let before = &args[..i];
                let after = &args[i + 1..];
                let has_target_before = before.iter().any(|a| !a.starts_with('-'));
                if !has_target_before && !after.is_empty() {
                    let mut b = before.to_vec();
                    b.push(after[0].clone());
                    (b, after[1..].to_vec())
                } else {
                    (before.to_vec(), after.to_vec())
                }
            }
            None => (args.to_vec(), Vec::new()),
        };
        let parsed = match ParsedCommandOptions::parse_for_command("run", &cmd_args) {
            Ok(p) => p,
            Err(e) => {
                writeln_bytes(stderr, &e.format_bazel_error());
                return ExitCode::COMMAND_LINE_ERROR;
            }
        };
        if parsed.targets.is_empty() {
            writeln_bytes(stderr, "ERROR: Must specify a target to run");
            return ExitCode::COMMAND_LINE_ERROR;
        }

        let target_pattern = parsed.targets[0].clone();
        let mut final_run_args = parsed.targets[1..].to_vec();
        final_run_args.extend(run_args);
        let run_args = final_run_args;

        let resolver = self.pattern_resolver(&parsed);
        let outcome = resolver.resolve_patterns(&[target_pattern.clone()]);
        for m in self.repo_resolver.take_messages() {
            writeln_bytes(stderr, &m);
        }
        if let Some(err) = outcome.errors.first() {
            writeln_bytes(stderr, "WARNING: Target pattern parsing failed.");
            writeln_bytes(stderr, &format!("ERROR: Skipping '{target_pattern}': {}", err.message));
            writeln_bytes(stderr, &format!("ERROR: {}", err.message));
            writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s", cmd_started.elapsed().as_secs_f64()));
            writeln_bytes(stderr, "INFO: 0 processes.");
            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            writeln_bytes(stderr, "ERROR: Build failed. Not running target");
            return ExitCode::PARSING_FAILURE;
        }
        let mut resolved_targets = outcome.targets;
        if resolved_targets.len() == 1 {
            if let zel_packages::Target::Rule(ref r) = resolved_targets[0] {
                if r.rule_class == "test_suite" {
                    let target_resolver = zel_rules::TargetResolver::new(resolver.loader().clone(), zel_rules::AnalysisConfig::default());
                    if let Ok(expanded) = target_resolver.expand_test_suites(&r.label, &[]) {
                        let mut exp = Vec::new();
                        for l in expanded {
                            if let Ok(t) = resolver.get_target(&l) {
                                exp.push(t);
                            }
                        }
                        resolved_targets = exp;
                    }
                }
            }
        }
        if resolved_targets.is_empty() {
            writeln_bytes(stderr, "ERROR: No target found to run");
            return ExitCode::BUILD_FAILURE;
        }
        if resolved_targets.len() > 1 {
            let mut target_names: Vec<String> = resolved_targets.iter().map(|t| t.label().canonical_form()).collect();
            let limit = 5;
            let truncated = target_names.len() > limit;
            let mut names_to_show: Vec<String> = if truncated {
                target_names[..limit].to_vec()
            } else {
                target_names
            };
            names_to_show.sort();
            names_to_show.dedup();
            let suffix = if truncated { "[TRUNCATED]" } else { "" };
            writeln_bytes(
                stderr,
                &format!(
                    "ERROR: Only a single target can be run. Your target pattern {} expanded to the targets {}{}",
                    target_pattern,
                    names_to_show.join(", "),
                    suffix
                ),
            );
            writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s", cmd_started.elapsed().as_secs_f64()));
            writeln_bytes(stderr, "INFO: 0 processes.");
            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            writeln_bytes(stderr, "ERROR: Build failed. Not running target");
            return ExitCode::COMMAND_LINE_ERROR;
        }
        let target = resolved_targets.first().unwrap().clone();
        let label = target.label().clone();

        // Check if target is executable before building.
        let is_executable = match &target {
            zel_packages::Target::Rule(r) => r.is_binary_rule() || r.is_test_rule(),
            zel_packages::Target::InputFile { .. } | zel_packages::Target::OutputFile { .. } => true,
            _ => false,
        };
        if !is_executable {
            writeln_bytes(
                stderr,
                &format!("ERROR: Cannot run target {label}: Not executable"),
            );
            writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s", cmd_started.elapsed().as_secs_f64()));
            writeln_bytes(stderr, "INFO: 0 processes.");
            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            writeln_bytes(stderr, "ERROR: Build failed. Not running target");
            return ExitCode::COMMAND_LINE_ERROR;
        }

        // Check if --run_under specifies a target label
        let run_under_str = exec_phase::opt_last(&parsed, "run_under");
        if let Some(ref ru) = run_under_str {
            if ru.trim().is_empty() {
                writeln_bytes(stderr, "ERROR: While parsing option --run_under=: Empty command");
                return ExitCode::COMMAND_LINE_ERROR;
            }
            if ru.starts_with("//") || ru.starts_with('@') {
                let ru_target = ru.split_whitespace().next().unwrap();
                let ru_outcome = resolver.resolve_patterns(&[ru_target.to_string()]);
                if let Some(err) = ru_outcome.errors.first() {
                    writeln_bytes(stderr, "WARNING: Target pattern parsing failed.");
                    writeln_bytes(stderr, &format!("ERROR: Skipping '{ru_target}': {}", err.message));
                    writeln_bytes(stderr, &format!("ERROR: {}", err.message));
                    writeln_bytes(stderr, &format!("INFO: Elapsed time: {:.3}s", cmd_started.elapsed().as_secs_f64()));
                    writeln_bytes(stderr, "INFO: 0 processes.");
                    writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                    writeln_bytes(stderr, "ERROR: Build failed. Not running target");
                    return ExitCode::BUILD_FAILURE;
                }
            }
        }
        let (run_under_label, run_under_opts) = if let Some(ru) = &run_under_str {
            let mut words = ru.split_whitespace();
            if let Some(first) = words.next() {
                if first.starts_with("//") || first.starts_with('@') {
                    (Some(first.to_string()), words.map(|s| s.to_string()).collect::<Vec<_>>())
                } else {
                    (None, Vec::new())
                }
            } else {
                (None, Vec::new())
            }
        } else {
            (None, Vec::new())
        };

        // First build the target(s) - only target_pattern (and --run_under if a target)
        // Keep every option together with its separate value (`-c opt`), dropping only the
        // positional targets (taken from the end, where the residue lives).
        let mut build_args: Vec<String> = cmd_args.clone();
        for t in parsed.targets.iter().rev() {
            if let Some(pos) = build_args.iter().rposition(|a| a == t) {
                build_args.remove(pos);
            }
        }
        build_args.push(target_pattern);
        if let Some(ref rul) = run_under_label {
            build_args.push(rul.clone());
        }
        let build_exit = self.cmd_build_or_test("run", &build_args, stdout, stderr);
        if build_exit != ExitCode::SUCCESS {
            // `RunCommand`: a failed build never runs the target.
            if !String::from_utf8_lossy(stderr).trim_end().ends_with("ERROR: Build failed. Not running target") {
                writeln_bytes(stderr, "ERROR: Build failed. Not running target");
            }
            return build_exit;
        }

        let run_loader = Arc::clone(resolver.loader());
        let config = match self.build_config_key(&parsed, &run_loader) {
            Ok(c) => c,
            Err(e) => {
                writeln_bytes(stderr, &format!("ERROR: {e}"));
                return ExitCode::COMMAND_LINE_ERROR;
            }
        };
        let mut roots_to_analyze = vec![target.clone()];
        if let Some(ref rul) = run_under_label {
            let ru_outcome = resolver.resolve_patterns(std::slice::from_ref(rul));
            if let Some(ru_t) = ru_outcome.targets.first() {
                roots_to_analyze.push(ru_t.clone());
            }
        }
        let analysis = match self.analyze_roots(
            &parsed,
            &run_loader,
            &roots_to_analyze,
            &config,
            stderr,
        ) {
            Ok(a) => a,
            Err(code) => return code,
        };
        let Some(Some(ct)) = analysis.roots.first().cloned() else {
            return ExitCode::BUILD_FAILURE;
        };
        let ct: zel_analysis::ConfiguredTarget = (*ct).clone();
        if ct.is_test {
            let runs_per_test = exec_phase::opt_last(&parsed, "runs_per_test")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(1);
            let sharding_strat = exec_phase::opt_last(&parsed, "test_sharding_strategy");
            let t_attrs = exec_phase::test_attrs(Some(&target), &parsed);
            let effective_shards = exec_phase::effective_shard_count(t_attrs.shard_count, sharding_strat.as_deref()).unwrap_or(t_attrs.shard_count);
            if runs_per_test > 1 || effective_shards > 1 {
                writeln_bytes(
                    stderr,
                    "ERROR: 'run' only works with tests with one shard ('--test_sharding_strategy=disabled' is okay) and without --runs_per_test",
                );
                return ExitCode::COMMAND_LINE_ERROR;
            }
        }
        // `FilesToRunProvider.fromSingleExecutableArtifact`: a source file is its own executable.
        let exe_opt = ct.default_info.executable.clone().or_else(|| {
            if ct.rule_class == "source file" && ct.default_info.files.len() == 1 {
                Some(ct.default_info.files[0].clone())
            } else {
                None
            }
        });
        let Some(exe_art) = exe_opt else {
            writeln_bytes(
                stderr,
                &format!("ERROR: Cannot run target {label}: Not executable"),
            );
            return ExitCode::COMMAND_LINE_ERROR;
        };
        if ct.rule_class == "source file" {
            use std::os::unix::fs::PermissionsExt;
            let p = format!("{}/{}", self.dirs.workspace_dir.path_string(), exe_art.exec_path.path_string());
            let ok = std::fs::metadata(&p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false);
            if !ok {
                writeln_bytes(stderr, &format!("ERROR: Non-existent or non-executable {p}"));
                return ExitCode::BUILD_FAILURE;
            }
        }
        let execroot = self.dirs.execroot();
        let exe_abs = execroot.get_relative(&exe_art.exec_path);
        let perform_build = exec_phase::opt_bool(&parsed, "build", true);
        if !perform_build && !std::path::Path::new(exe_abs.path_string()).exists() {
            writeln_bytes(stderr, "ERROR: No targets found to run");
            return ExitCode::COMMAND_LINE_ERROR;
        }

        let runfiles_dir = match exec_phase::build_target_runfiles(
            &ct,
            std::path::Path::new(execroot.path_string()),
            &self.dirs.execroot_workspace_name(),
            &parsed,
        ) {
            Ok(d) => d,
            Err(e) => {
                writeln_bytes(stderr, &format!("ERROR: Failed to create runfiles tree: {e}"));
                return ExitCode::BUILD_FAILURE;
            }
        };
        let client_cwd = args
            .iter()
            .find_map(|a| a.strip_prefix("--client_cwd="))
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.dirs.workspace_dir.path_string().to_string());
        let run_in_cwd = exec_phase::opt_bool(&parsed, "run_in_cwd", false);
        let cwd = if run_in_cwd {
            client_cwd.clone()
        } else if ct.is_test {
            execroot.path_string().to_string()
        } else {
            runfiles_dir
                .as_ref()
                .map(|d| d.join(self.dirs.execroot_workspace_name()))
                .filter(|d| d.is_dir())
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.dirs.workspace_dir.path_string().to_string())
        };
        let mut argv_s: Vec<String> = Vec::new();
        let display: String;
        let test_binary_rel = format!("./{}", exe_art.exec_path.path_string().rsplit_once('/').map(|x| x.1).unwrap_or(exe_art.exec_path.path_string()));
        if ct.is_test {
            let setup = self.dirs.output_base.get_relative_str("install/embedded_tools/tools/test/test-setup.sh");
            let _ = zel_exec::test_runner::install_test_setup(&std::path::PathBuf::from(setup.path_string()));
            argv_s.push(setup.path_string().to_string());
            if let Some(ref rul) = run_under_label {
                if let Some(Some(ru_ct)) = analysis.roots.get(1).cloned() {
                    if let Some(ru_exe) = ru_ct.default_info.executable.clone() {
                        let ru_abs = execroot.get_relative(&ru_exe.exec_path);
                        argv_s.push(ru_abs.path_string().to_string());
                        argv_s.extend(run_under_opts);
                    }
                }
            } else if let Some(ru) = &run_under_str {
                argv_s.extend(ru.split_whitespace().map(|s| s.to_string()));
            }
            argv_s.push(test_binary_rel.clone());
            // `RunfilesSupport.getArgs()` (the rule's `args`) come first, then `--test_arg`s.
            let rule_args = exec_phase::test_attrs(Some(&target), &parsed).args;
            let mut test_args = rule_args;
            test_args.extend(exec_phase::opt_all(&parsed, "test_arg"));
            let mut test_args_str = String::new();
            for ta in &test_args {
                argv_s.push(ta.clone());
                test_args_str.push_str(&format!(" {ta}"));
            }
            argv_s.extend(run_args.iter().cloned());
            display = format!("external/bazel_tools/tools/test/test-setup.sh {test_binary_rel}{test_args_str}");
        } else {
            let mut ru_prefix = String::new();
            let mut shell_run_under: Option<String> = None;
            if run_under_label.is_some() {
                if let Some(Some(ru_ct)) = analysis.roots.last().cloned() {
                    if let Some(ru_exe) = ru_ct.default_info.executable.clone() {
                        let ru_abs = execroot.get_relative(&ru_exe.exec_path);
                        argv_s.push(ru_abs.path_string().to_string());
                        argv_s.extend(run_under_opts);
                        let symlink_prefix = exec_phase::opt_last(&parsed, "symlink_prefix").unwrap_or_else(|| "bazel-".to_string());
                        let target_ru_bin = format!("{}bin/{}", symlink_prefix, ru_exe.exec_path.path_string().rsplit_once('/').map(|x| x.1).unwrap_or(ru_exe.exec_path.path_string()));
                        ru_prefix = format!("{target_ru_bin} ");
                    }
                }
            } else if let Some(ru) = &run_under_str {
                // `RunCommand`: a command run_under goes through `<shell> -c "<run_under> <exe> <args>"`.
                shell_run_under = Some(ru.clone());
                ru_prefix = format!("{ru} ");
            }
            argv_s.push(exe_abs.path_string().to_string());
            // The rule's `args` attribute precedes the command-line residue.
            argv_s.extend(exec_phase::test_attrs(Some(&target), &parsed).args);
            argv_s.extend(run_args.iter().cloned());
            if let Some(ru) = shell_run_under {
                let q = |s: &str| {
                    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c)) {
                        s.to_string()
                    } else {
                        format!("'{}'", s.replace('\'', "'\\''"))
                    }
                };
                let cmd = format!("{ru} {}", argv_s.iter().map(|a| q(a)).collect::<Vec<_>>().join(" "));
                argv_s = vec!["/bin/bash".to_string(), "-c".to_string(), cmd];
            }
            let symlink_prefix = exec_phase::opt_last(&parsed, "symlink_prefix").unwrap_or_else(|| "bazel-".to_string());
            // `ignore` / `clean`: no convenience symlinks, so the absolute path is announced.
            let conv_symlinks = !matches!(exec_phase::opt_last(&parsed, "experimental_convenience_symlinks").as_deref(), Some("ignore") | Some("clean") | Some("false") | Some("0"));
            let target_bin = if !conv_symlinks {
                exe_abs.path_string().to_string()
            } else {
                format!(
                    "{}bin/{}",
                    symlink_prefix,
                    exe_art
                        .exec_path
                        .path_string()
                        .rsplit_once("/bin/")
                        .map(|x| x.1)
                        .unwrap_or(exe_art.exec_path.path_string())
                )
            };
            let rule_args = exec_phase::test_attrs(Some(&target), &parsed).args;
            display = if rule_args.is_empty() {
                format!("{ru_prefix}{target_bin}")
            } else {
                format!("{ru_prefix}{target_bin} {}", rule_args.join(" "))
            };
        }

        let script_path = exec_phase::opt_last(&parsed, "script_path");
        if let Some(ref sp) = script_path {
            if sp.trim().is_empty() {
                writeln_bytes(stderr, &format!("ERROR: Error writing run script: {} (Is a directory)", self.dirs.workspace_dir.path_string()));
                return ExitCode::RUN_FAILURE;
            }
        }
        let run_built_target = exec_phase::opt_bool(&parsed, "run", true);
        let prefix = if run_built_target { "Running" } else { "Runnable" };
        let omit_run_args = exec_phase::opt_bool(&parsed, "omit_run_args", true);
        let display_cmd = if !run_args.is_empty() {
            if omit_run_args {
                format!("{display} <args omitted>")
            } else {
                format!("{display} {}", run_args.join(" "))
            }
        } else {
            display
        };
        if script_path.is_none() {
            if parsed.subcommands {
                writeln_bytes(stderr, "INFO: Running command line:");
                let mut cmd_lines = format!("(cd {} && \\\n  exec env - \\\n", cwd);
                for u in &["JAVA_RUNFILES", "RUNFILES_DIR", "RUNFILES_MANIFEST_FILE", "RUNFILES_MANIFEST_ONLY", "TEST_SRCDIR"] {
                    cmd_lines.push_str(&format!("    -u {} \\\n", u));
                }
                cmd_lines.push_str(&format!("    BUILD_EXECROOT={} \\\n", self.dirs.execroot().path_string()));
                cmd_lines.push_str(&format!("    BUILD_ID={} \\\n", pseudo_uuid()));
                cmd_lines.push_str(&format!("    BUILD_WORKING_DIRECTORY={} \\\n", client_cwd));
                cmd_lines.push_str(&format!("    BUILD_WORKSPACE_DIRECTORY={} \\\n", self.dirs.workspace_dir.path_string()));
                // `ShellEscaper`: the command string (executable plus run args) is quoted when it
                // contains anything beyond safe characters.
                let mut full = exe_abs.path_string().to_string();
                for a in &run_args {
                    full.push(' ');
                    full.push_str(a);
                }
                let safe = !full.is_empty() && full.chars().all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c));
                let shown = if safe { full } else { format!("'{}'", full.replace('\'', "'\\''")) };
                cmd_lines.push_str(&format!("  /bin/bash -c {shown})"));
                writeln_bytes(stderr, &cmd_lines);
            } else {
                let filters: Vec<String> = loading::opt_list(&parsed, "ui_event_filters");
                let suppress_info = filters.iter().any(|f| f.split(',').any(|tok| tok == "-info"));
                if !suppress_info {
                    let ts = if exec_phase::opt_bool(&parsed, "show_timestamps", false) {
                        timestamp_prefix()
                    } else {
                        String::new()
                    };
                    writeln_bytes(stderr, &format!("{ts}INFO: {prefix} command line: {display_cmd}"));
                }
            }
        }
        // `RunCommand.makeMutableRunEnvironment` + `--run_env` + the target's own environment
        // (`RunEnvironmentInfo`, or `env`/`env_inherit` of native rules); `--action_env` does not
        // apply to `bazel run`.
        let client_env = self.client_env();
        let mut run_base: BTreeMap<String, String> = BTreeMap::new();
        run_base.insert("BUILD_WORKSPACE_DIRECTORY".into(), self.dirs.workspace_dir.path_string().to_string());
        run_base.insert("BUILD_WORKING_DIRECTORY".into(), client_cwd.clone());
        run_base.insert("BUILD_EXECROOT".into(), execroot.path_string().to_string());
        run_base.insert("BUILD_ID".into(), pseudo_uuid());
        if ct.is_test {
            run_base.insert("TEST_TARGET".into(), label.canonical_form());
            run_base.insert("TEST_WORKSPACE".into(), self.dirs.execroot_workspace_name());
            let test_binary_rel = format!("./{}", exe_art.exec_path.path_string().rsplit_once('/').map(|x| x.1).unwrap_or(exe_art.exec_path.path_string()));
            run_base.insert("TEST_BINARY".into(), test_binary_rel);
            let t_attrs = exec_phase::test_attrs(Some(&target), &parsed);
            run_base.insert("TEST_SIZE".into(), t_attrs.size);
            run_base.insert("TEST_TIMEOUT".into(), t_attrs.timeout.to_string());

            let is_sp = script_path.is_some();
            if is_sp {
                let rf_rel = format!("bazel-out/{}-{}/bin/{}.runfiles", config.cpu, parsed.compilation_mode, label.name());
                run_base.insert("JAVA_RUNFILES".into(), rf_rel.clone());
                run_base.insert("PYTHON_RUNFILES".into(), rf_rel.clone());
                run_base.insert("RUNFILES_DIR".into(), rf_rel.clone());
                run_base.insert("TEST_SRCDIR".into(), rf_rel);
                run_base.insert("PATH".into(), "/bin:/usr/bin:/usr/local/bin".into());
                run_base.insert("TZ".into(), "UTC".into());
                run_base.insert("RUN_UNDER_RUNFILES".into(), "1".into());
            } else {
                let runfiles_abs = format!("{}.runfiles", exe_abs.path_string());
                run_base.insert("TEST_SRCDIR".into(), runfiles_abs.clone());
                run_base.insert("RUNFILES_DIR".into(), runfiles_abs.clone());
                run_base.insert("PYTHON_RUNFILES".into(), runfiles_abs.clone());
                run_base.insert("JAVA_RUNFILES".into(), runfiles_abs);
                // `test-setup.sh` exports these before exec'ing the test (`bazel run` of a test).
                run_base.insert("TZ".into(), "UTC".into());
                run_base.insert("RUN_UNDER_RUNFILES".into(), "1".into());
            }

            let hash = zel_exec::test_runner::test_tmpdir_hash(&std::path::PathBuf::from(exe_art.exec_path.path_string()), 0, 0);
            if is_sp {
                run_base.insert("TEST_TMPDIR".into(), format!("_tmp/{hash}"));
            } else {
                let tmp_root = exec_phase::opt_last(&parsed, "test_tmpdir")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| std::path::PathBuf::from(execroot.path_string()).join("_tmp"));
                let test_tmp = tmp_root.join(&hash);
                let _ = std::fs::create_dir_all(&test_tmp);
                run_base.insert("TEST_TMPDIR".into(), test_tmp.display().to_string());
            }

            let target_rel = label.package_identifier().package_fragment().get_relative_str(label.name());
            let log_dir_abs = std::path::PathBuf::from(self.dirs.bazel_testlogs(config.cpu.as_str(), &parsed.compilation_mode).get_relative(&target_rel).path_string());
            let _ = std::fs::create_dir_all(&log_dir_abs);
            let _ = std::fs::create_dir_all(log_dir_abs.join("test.outputs"));
            let _ = std::fs::create_dir_all(log_dir_abs.join("test.outputs_manifest"));
            let _ = std::fs::create_dir_all(log_dir_abs.join("test.raw_splitlogs"));

            let log_prefix = if is_sp {
                format!("bazel-out/{}-{}/testlogs/{}", config.cpu, parsed.compilation_mode, target_rel)
            } else {
                log_dir_abs.display().to_string()
            };

            run_base.insert("XML_OUTPUT_FILE".into(), format!("{log_prefix}/test.xml"));
            run_base.insert("TEST_WARNINGS_OUTPUT_FILE".into(), format!("{log_prefix}/test.warnings"));
            run_base.insert("TEST_UNUSED_RUNFILES_LOG_FILE".into(), format!("{log_prefix}/test.unused_runfiles_log"));
            run_base.insert("TEST_LOGSPLITTER_OUTPUT_FILE".into(), format!("{log_prefix}/test.raw_splitlogs/test.splitlogs"));
            run_base.insert("TEST_UNDECLARED_OUTPUTS_DIR".into(), format!("{log_prefix}/test.outputs"));
            run_base.insert("TEST_UNDECLARED_OUTPUTS_MANIFEST".into(), format!("{log_prefix}/test.outputs_manifest/MANIFEST"));
            if !is_sp {
                run_base.insert("TEST_UNDECLARED_OUTPUTS_ZIP".into(), format!("{log_prefix}/test.outputs/outputs.zip"));
            }
            run_base.insert("TEST_UNDECLARED_OUTPUTS_ANNOTATIONS".into(), format!("{log_prefix}/test.outputs_manifest/ANNOTATIONS"));
            run_base.insert("TEST_UNDECLARED_OUTPUTS_ANNOTATIONS_DIR".into(), format!("{log_prefix}/test.outputs_manifest"));
            run_base.insert("TEST_PREMATURE_EXIT_FILE".into(), format!("{log_prefix}/test.exited_prematurely"));
            run_base.insert("TEST_INFRASTRUCTURE_FAILURE_FILE".into(), format!("{log_prefix}/test.infrastructure_failure"));
        }
        if ct.is_test {
            if let Some(tf) = exec_phase::opt_last(&parsed, "test_filter") {
                run_base.insert("TESTBRIDGE_TEST_ONLY".into(), tf);
            }
            // `--test_env=K=V` / `--test_env=K` (inherit from the client environment).
            for ev in zel_util::action_env::parse_env_vars(exec_phase::opt_all(&parsed, "test_env")) {
                match ev {
                    zel_util::action_env::EnvVar::Set(n, v) => {
                        run_base.insert(n, v);
                    }
                    zel_util::action_env::EnvVar::Inherit(n) => {
                        if let Some(v) = client_env.get(&n) {
                            run_base.insert(n, v.clone());
                        }
                    }
                    _ => {}
                }
            }
        }
        let target_env = match &ct.run_environment {
            Some(re) => zel_util::action_env::ActionEnvironment::create(
                re.environment.clone(),
                re.inherited_environment.iter().cloned().collect(),
                Default::default(),
            ),
            None => exec_phase::native_run_environment(&target),
        };
        let run_env_vars = zel_util::action_env::parse_env_vars(exec_phase::opt_all(&parsed, "run_env"));
        let (run_env, mut to_clear) =
            zel_util::action_env::run_environment(run_base, &run_env_vars, &target_env, &client_env);
        if !ct.is_test {
            for v in &["JAVA_RUNFILES", "RUNFILES_DIR", "RUNFILES_MANIFEST_FILE", "RUNFILES_MANIFEST_ONLY"] {
                to_clear.insert(v.to_string());
            }
        }
        let env: Vec<(String, String)> = run_env.into_iter().collect();
        let portable_paths = exec_phase::opt_bool(&parsed, "portable_paths", false);
        let paths_to_replace = if portable_paths {
            vec![
                PathToReplace {
                    r#type: PathToReplaceType::OutputBase,
                    value: self.dirs.output_base.path_string().as_bytes().to_vec(),
                },
                PathToReplace {
                    r#type: PathToReplaceType::BuildWorkingDirectory,
                    value: client_cwd.as_bytes().to_vec(),
                },
                PathToReplace {
                    r#type: PathToReplaceType::BuildWorkspaceDirectory,
                    value: self.dirs.workspace_dir.path_string().as_bytes().to_vec(),
                },
            ]
        } else {
            Vec::new()
        };
        let script_path = exec_phase::opt_last(&parsed, "script_path");
        let mut write_failure: Option<String> = None;
        let script = script_path.map(|sp| {
            let q = |s: &str| {
                if !s.is_empty()
                    && s.chars().all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c))
                {
                    s.to_string()
                } else {
                    format!("'{}'", s.replace('\'', "'\\''"))
                }
            };
            let mut c = format!("#!/bin/bash\ncd {} && \\\n  exec env \\\n", q(&cwd));
            for u in &to_clear {
                c.push_str(&format!("    -u {} \\\n", u));
            }
            for (k, v) in &env {
                c.push_str(&format!("    {}={} \\\n", k, q(v)));
            }
            if ct.is_test {
                c.push_str(&format!(
                    "  external/bazel_tools/tools/test/test-setup.sh {} \"$@\"",
                    test_binary_rel
                ));
            } else {
                c.push_str(&format!(
                    "  {} \"$@\"",
                    exe_abs.path_string()
                ));
            }
            if let Err(e) = std::fs::write(&sp, &c) {
                // `RunCommand.writeScript` failure: Java's `FileNotFoundException` message.
                let why = match e.raw_os_error() {
                    Some(2) => "No such file or directory".to_string(),
                    Some(13) => "Permission denied".to_string(),
                    Some(20) => "Not a directory".to_string(),
                    Some(21) => "Is a directory".to_string(),
                    Some(30) => "Read-only file system".to_string(),
                    _ => e.to_string(),
                };
                write_failure = Some(format!("ERROR: Error writing run script: {sp} ({why})"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&sp, std::fs::Permissions::from_mode(0o755));
            }
            zel_proto::command_server::ScriptPath {
                script_path: sp.into_bytes(),
                script_contents: c.into_bytes(),
            }
        });
        if let Some(m) = write_failure {
            writeln_bytes(stderr, &m);
            *exec_request = None;
            return ExitCode::RUN_FAILURE;
        }
        if !run_built_target && script.is_none() {
            *exec_request = None;
            return ExitCode::SUCCESS;
        }
        *exec_request = Some(ExecRequest {
            working_directory: cwd.into_bytes(),
            argv: argv_s.iter().map(|a| a.as_bytes().to_vec()).collect(),
            environment_variable: env
                .iter()
                .map(|(k, v)| zel_proto::command_server::EnvironmentVariable {
                    name: k.as_bytes().to_vec(),
                    value: v.as_bytes().to_vec(),
                })
                .collect(),
            environment_variable_to_clear: to_clear.iter().map(|n| n.as_bytes().to_vec()).collect(),
            should_exec: run_built_target,
            script_path: script,
            path_to_replace: paths_to_replace,
        });
        ExitCode::SUCCESS
    }

    fn write_workspace_status_files(&self, parsed: &ParsedCommandOptions, output_path: &zel_vfs::PathFragment) -> Result<(), String> {
        let embed_label = parsed.get_str("embed_label").unwrap_or("");
        let status_cmd = parsed.get_str("workspace_status_command").unwrap_or("");

        let mut stable_map = std::collections::BTreeMap::new();
        let mut volatile_map = std::collections::BTreeMap::new();

        let user = std::env::var("USER").unwrap_or_else(|_| "bazel".to_string());
        let host = zel_exec::layout::hostname();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        stable_map.insert("BUILD_EMBED_LABEL".to_string(), embed_label.to_string());
        stable_map.insert("BUILD_HOST".to_string(), host);
        stable_map.insert("BUILD_USER".to_string(), user);

        volatile_map.insert("BUILD_TIMESTAMP".to_string(), now.to_string());

        if !status_cmd.is_empty() {
            let res = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(status_cmd)
                .current_dir(self.dirs.workspace_dir.path_string())
                .output();
            match res {
                Ok(out) if out.status.success() => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    for line in text.lines() {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        let (k, v) = line.split_once(' ').unwrap_or((line, ""));
                        if k.starts_with("STABLE_") || k == "BUILD_EMBED_LABEL" || k == "BUILD_HOST" || k == "BUILD_USER" {
                            stable_map.insert(k.to_string(), v.trim().to_string());
                        } else {
                            volatile_map.insert(k.to_string(), v.trim().to_string());
                        }
                    }
                }
                Ok(out) => {
                    let err_msg = String::from_utf8_lossy(&out.stderr);
                    return Err(format!(
                        "ERROR: <builtin>: BazelWorkspaceStatusAction stable-status.txt failed: Failed to determine workspace status: Process exited with status {}\n{}",
                        out.status.code().unwrap_or(1),
                        err_msg.trim_end()
                    ));
                }
                Err(e) => {
                    return Err(format!(
                        "ERROR: <builtin>: BazelWorkspaceStatusAction stable-status.txt failed: Failed to determine workspace status: {e}"
                    ));
                }
            }
        }

        let out_dir = std::path::PathBuf::from(output_path.path_string());
        let _ = std::fs::create_dir_all(&out_dir);

        let mut stable_str = String::new();
        for (k, v) in &stable_map {
            stable_str.push_str(&format!("{k} {v}\n"));
        }
        let _ = std::fs::write(out_dir.join("stable-status.txt"), stable_str);

        let mut volatile_str = String::new();
        for (k, v) in &volatile_map {
            volatile_str.push_str(&format!("{k} {v}\n"));
        }
        let _ = std::fs::write(out_dir.join("volatile-status.txt"), volatile_str);
        Ok(())
    }
}

/// `UUID.randomUUID()`-shaped id for `BUILD_ID`.
fn pseudo_uuid() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let x = n ^ (pid << 64) ^ 0x9e37_79b9_7f4a_7c15_f39c_c060_5ced_c834u128;
    let h = format!("{x:032x}");
    format!("{}-{}-4{}-a{}-{}", &h[0..8], &h[8..12], &h[13..16], &h[17..20], &h[20..32])
}

fn timestamp_prefix() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let secs = (now + zel_events::ui::local_utc_offset_secs(now)).rem_euclid(86400);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    format!("({h:02}:{m:02}:{s:02}) ")
}

fn current_date_str() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let now = now + zel_events::ui::local_utc_offset_secs(now);
    let z = now / 86400 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn writeln_bytes(out: &mut Vec<u8>, line: &str) {
    out.extend_from_slice(line.as_bytes());
    out.push(b'\n');
}

fn out_write_str(out: &mut Vec<u8>, s: &str) {
    // `mod show_repo --output=streamed_proto`: binary output carried as latin-1 chars behind a marker.
    if let Some(raw) = s.strip_prefix(zel_bzlmod::session::RAW_BYTES_MARK) {
        out.extend(raw.chars().map(|c| c as u32 as u8));
        return;
    }
    out.extend_from_slice(s.as_bytes());
    if !s.is_empty() && !s.ends_with('\n') {
        out.push(b'\n');
    }
}

/// Parses `module(name = ..., version = ...)` and `bazel_dep(name = ..., version = ...)` from `MODULE.bazel`.
pub fn parse_module_bazel(content: &str) -> (String, String, Vec<(String, String)>) {
    let mut mod_name = "_main".to_string();
    let mut mod_version = "0.0.0".to_string();
    let mut deps = Vec::new();

    let mut pos = 0;
    let bytes = content.as_bytes();
    while pos < bytes.len() {
        if bytes[pos] == b'#' {
            while pos < bytes.len() && bytes[pos] != b'\n' {
                pos += 1;
            }
            continue;
        }
        if content[pos..].starts_with("module(") || content[pos..].starts_with("bazel_dep(") {
            let is_module = content[pos..].starts_with("module(");
            let open_idx = pos + content[pos..].find('(').unwrap();
            let mut depth = 1;
            let mut end_idx = open_idx + 1;
            while end_idx < bytes.len() && depth > 0 {
                if bytes[end_idx] == b'(' {
                    depth += 1;
                } else if bytes[end_idx] == b')' {
                    depth -= 1;
                }
                end_idx += 1;
            }
            let body = &content[open_idx + 1..end_idx.saturating_sub(1)];
            let name = find_kwarg_in_call(body, "name");
            let version = find_kwarg_in_call(body, "version").unwrap_or_default();
            if is_module {
                if let Some(n) = name {
                    mod_name = n;
                }
                if !version.is_empty() {
                    mod_version = version;
                }
            } else if let Some(n) = name {
                deps.push((n, version));
            }
            pos = end_idx;
            continue;
        }
        pos += 1;
    }

    (mod_name, mod_version, deps)
}

fn find_kwarg_in_call(body: &str, key: &str) -> Option<String> {
    for part in body.split(',') {
        let trimmed = part.trim();
        if let Some((k, v)) = trimmed.split_once('=') {
            if k.trim() == key {
                let v = v.trim();
                if (v.starts_with('"') && v.ends_with('"'))
                    || (v.starts_with('\'') && v.ends_with('\''))
                {
                    return Some(v[1..v.len() - 1].to_string());
                }
            }
        }
    }
    None
}

/// RPC method tag sent over the local CommandServer socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RpcMethod {
    Run = 1,
    Cancel = 2,
    Ping = 3,
    UpdateTerminalSize = 4,
}

impl RpcMethod {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Run),
            2 => Some(Self::Cancel),
            3 => Some(Self::Ping),
            4 => Some(Self::UpdateTerminalSize),
            _ => None,
        }
    }
}

/// Writes the server status files (`command_port`, `request_cookie`, `response_cookie`,
/// `server.pid.txt`, and `server_info.rawproto`) into `$OUTPUT_BASE/server/`, matching `CommandServer.java`.
pub fn write_server_status_files(
    fs: &dyn FileSystem,
    server_dir: &PathFragment,
    pid: i32,
    address: &str,
    request_cookie: &str,
    response_cookie: &str,
) -> io::Result<()> {
    fs.create_directory_and_parents(server_dir)?;
    fs.write_file(
        &server_dir.get_relative_str("command_port"),
        address.as_bytes(),
    )?;
    fs.write_file(
        &server_dir.get_relative_str("request_cookie"),
        request_cookie.as_bytes(),
    )?;
    fs.write_file(
        &server_dir.get_relative_str("response_cookie"),
        response_cookie.as_bytes(),
    )?;
    fs.write_file(
        &server_dir.get_relative_str("server.pid.txt"),
        pid.to_string().as_bytes(),
    )?;

    let info = ServerInfo {
        pid,
        address: address.to_string(),
        request_cookie: request_cookie.to_string(),
        response_cookie: response_cookie.to_string(),
    };
    let tmp_file = server_dir.get_relative_str("server_info.rawproto.tmp");
    let final_file = server_dir.get_relative_str("server_info.rawproto");
    fs.write_file(&tmp_file, &info.encode_to_vec())?;
    fs.rename(&tmp_file, &final_file)?;
    Ok(())
}

pub fn cleanup_server_status_files(fs: &dyn FileSystem, server_dir: &PathFragment) {
    for name in [
        "command_port",
        "request_cookie",
        "response_cookie",
        "server.pid.txt",
        "server_info.rawproto",
    ] {
        let _ = fs.delete(&server_dir.get_relative_str(name));
    }
}

/// Runs the `CommandServer` listener loop on a bound `TcpListener`.
pub fn serve_command_server(
    listener: TcpListener,
    runtime: Arc<BlazeRuntime>,
    request_cookie: String,
    response_cookie: String,
) -> io::Result<()> {
    let local_addr = listener.local_addr()?;
    let addr_str = local_addr.to_string();
    let server_dir = runtime.dirs.server_dir();
    write_server_status_files(
        runtime.fs.as_ref(),
        &server_dir,
        runtime.server_pid as i32,
        &addr_str,
        &request_cookie,
        &response_cookie,
    )?;

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        handle_client_stream(stream, &runtime, &request_cookie, &response_cookie)?;
        if runtime.shutdown_requested.load(Ordering::SeqCst) {
            break;
        }
    }

    cleanup_server_status_files(runtime.fs.as_ref(), &server_dir);
    Ok(())
}

fn handle_client_stream(
    stream: TcpStream,
    runtime: &BlazeRuntime,
    request_cookie: &str,
    response_cookie: &str,
) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = BufWriter::new(stream);

    let Some(frame) = read_grpc_frame(&mut reader)? else {
        return Ok(());
    };
    if frame.is_empty() {
        return Ok(());
    }

    let method = RpcMethod::from_u8(frame[0]).unwrap_or(RpcMethod::Run);
    let payload = &frame[1..];

    match method {
        RpcMethod::Ping => {
            let req = PingRequest::decode(payload)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            let resp = PingResponse {
                cookie: if req.cookie == request_cookie {
                    response_cookie.to_string()
                } else {
                    String::new()
                },
            };
            write_grpc_frame(&mut writer, &resp.encode_to_vec())?;
        }
        RpcMethod::Cancel => {
            let _req = CancelRequest::decode(payload)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            let resp = CancelResponse {
                cookie: response_cookie.to_string(),
            };
            write_grpc_frame(&mut writer, &resp.encode_to_vec())?;
        }
        RpcMethod::UpdateTerminalSize => {
            let _req = TerminalSizeRequest::decode(payload)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            let resp = TerminalSizeResponse {
                cookie: response_cookie.to_string(),
            };
            write_grpc_frame(&mut writer, &resp.encode_to_vec())?;
        }
        RpcMethod::Run => {
            let req = RunRequest::decode(payload)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            if req.cookie != request_cookie {
                let bad_resp = RunResponse {
                    cookie: response_cookie.to_string(),
                    finished: true,
                    exit_code: ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code(),
                    failure_detail: Some(FailureDetail::new(
                        "Invalid RunRequest: bad cookie",
                        Category::GrpcServer(GrpcServerCode::BadCookie),
                    )),
                    ..Default::default()
                };
                write_grpc_frame(&mut writer, &bad_resp.encode_to_vec())?;
                return Ok(());
            }

            // Initial response with command_id, matching CommandServer.java lines 485-490
            let init_resp = RunResponse {
                cookie: response_cookie.to_string(),
                command_id: "cmd-0".to_string(),
                ..Default::default()
            };
            write_grpc_frame(&mut writer, &init_resp.encode_to_vec())?;

            // Execute command and send final response
            let final_resp = runtime.dispatch_command(&req, response_cookie);
            write_grpc_frame(&mut writer, &final_resp.encode_to_vec())?;
        }
    }

    Ok(())
}

pub fn create_default_runtime(
    workspace_dir: PathFragment,
    output_base: PathFragment,
    install_base: PathFragment,
    output_user_root: PathFragment,
) -> Arc<BlazeRuntime> {
    let dirs = BlazeDirectories {
        workspace_dir,
        output_base,
        install_base,
        output_user_root,
    };
    Arc::new(BlazeRuntime::new(
        dirs,
        Arc::new(LocalFileSystem::default()),
        std::process::id(),
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn default_instrumentation_prefixes() {
        assert_eq!(super::instrumented_prefix("foo/javatests/bar"), "foo/java/bar");
        assert_eq!(super::instrumented_prefix("javatests/x"), "java/x");
        assert_eq!(super::instrumented_prefix("a/test/java/b"), "a/main/java/b");
        assert_eq!(super::instrumented_prefix("a/b/tests"), "a/b");
        assert_eq!(super::instrumented_prefix("a/b/internal"), "a/b");
        assert_eq!(super::instrumented_prefix("notjavatests/x"), "notjavatests/x");
        assert_eq!(super::instrumented_prefix(""), "");
    }

    use super::*;
    use zel_vfs::InMemoryFileSystem;

    fn test_runtime() -> (Arc<InMemoryFileSystem>, BlazeRuntime) {
        let fs = Arc::new(InMemoryFileSystem::default());
        let ws = PathFragment::create("/workspace");
        let ob = PathFragment::create("/tmp/ob");
        let ib = PathFragment::create("/tmp/ib");
        let ur = PathFragment::create("/tmp/ur");
        fs.create_directory_and_parents(&ws).unwrap();
        fs.create_directory_and_parents(&ob).unwrap();

        fs.write_file(
            &ws.get_relative_str("MODULE.bazel"),
            br#"
module(
    name = "bazel",
    version = "0.0.1",
)
bazel_dep(name = "rules_rs", version = "0.1.3")
bazel_dep(name = "rules_cc", version = "0.2.17")
"#,
        )
        .unwrap();

        let src_pkg = ws.get_relative_str("src");
        fs.create_directory_and_parents(&src_pkg).unwrap();
        fs.write_file(
            &src_pkg.get_relative_str("BUILD"),
            br#"
genrule(
    name = "zel",
    outs = ["zel.out"],
    cmd = "echo built > $@",
)
"#,
        )
        .unwrap();

        let dirs = BlazeDirectories {
            workspace_dir: ws,
            output_base: ob,
            install_base: ib,
            output_user_root: ur,
        };
        let rt = BlazeRuntime::new(dirs, Arc::clone(&fs) as Arc<dyn FileSystem>, 9999);
        (fs, rt)
    }

    #[test]
    fn test_server_info_and_version_commands() {
        let (_fs, rt) = test_runtime();
        let req = RunRequest {
            cookie: "req".to_string(),
            arg: vec![b"info".to_vec(), b"server_pid".to_vec()],
            client_description: "test".to_string(),
            ..Default::default()
        };
        let resp = rt.dispatch_command(&req, "resp");
        assert_eq!(resp.exit_code, 0);
        assert_eq!(
            String::from_utf8_lossy(&resp.standard_output).trim(),
            "9999"
        );
    }

    #[test]
    fn test_server_query_and_mod_graph() {
        // The resolved builtin module graph (with `bazel_tools`' version-bumping nodep edges) recurses deeper than the
        // 2 MiB default test-thread stack in debug builds; production command threads use large stacks.
        std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(test_server_query_and_mod_graph_body)
            .unwrap()
            .join()
            .unwrap();
    }

    fn test_server_query_and_mod_graph_body() {
        let (_fs, rt) = test_runtime();
        let q_req = RunRequest {
            cookie: "req".to_string(),
            arg: vec![
                b"query".to_vec(),
                b"--output=label_kind".to_vec(),
                b"//src:all".to_vec(),
            ],
            client_description: "test".to_string(),
            ..Default::default()
        };
        let q_resp = rt.dispatch_command(&q_req, "resp");
        assert_eq!(q_resp.exit_code, 0);
        assert_eq!(
            String::from_utf8_lossy(&q_resp.standard_output).trim(),
            "genrule rule //src:zel"
        );

        // `mod` runs against the real file system (Bzlmod resolution through a file:// registry).
        let tmp = std::env::temp_dir().join(format!("zel-server-mod-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let reg = tmp.join("registry");
        // Hermetic registry holding the closure `bazel_tools` needs (vendored under third_party).
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let status = std::process::Command::new("python3")
            .arg(repo_root.join("scripts/make_vendored_registry.py"))
            .arg(&reg)
            .status()
            .expect("python3 is required to build the local test registry");
        assert!(status.success());
        for (n, v, deps) in [("rules_rs", "0.1.3", "bazel_dep(name = \"platforms\", version = \"1.0.0\")\n")] {
            let dir = reg.join(format!("modules/{n}/{v}"));
            if !dir.join("MODULE.bazel").exists() {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("MODULE.bazel"), format!("module(name = \"{n}\", version = \"{v}\")\n{deps}")).unwrap();
                std::fs::write(dir.join("source.json"), "{\"type\":\"local_path\",\"path\":\"/nonexistent\"}").unwrap();
            }
            // Register the version in metadata.json (merging with the vendored versions).
            let meta = reg.join(format!("modules/{n}/metadata.json"));
            let mut versions: Vec<String> = std::fs::read_to_string(&meta)
                .ok()
                .map(|t| t.split('"').filter(|x| x.chars().next().map_or(false, |c| c.is_ascii_digit())).map(String::from).collect())
                .unwrap_or_default();
            if !versions.iter().any(|x| x == v) {
                versions.push(v.to_string());
            }
            let list: Vec<String> = versions.iter().map(|x| format!("\"{x}\"")).collect();
            std::fs::write(&meta, format!("{{\"versions\":[{}],\"yanked_versions\":{{}}}}", list.join(","))).unwrap();
        }
        let ws = tmp.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(
            ws.join("MODULE.bazel"),
            "module(name = \"bazel\", version = \"0.0.1\")\nbazel_dep(name = \"rules_rs\", version = \"0.1.3\")\n",
        )
        .unwrap();
        let real_fs = Arc::new(LocalFileSystem::new(zel_vfs::DigestHashFunction::Sha256));
        let rt2 = BlazeRuntime::new(
            BlazeDirectories {
                workspace_dir: PathFragment::create(ws.to_str().unwrap()),
                output_base: PathFragment::create(tmp.join("ob").to_str().unwrap()),
                install_base: PathFragment::create("/tmp/ib"),
                output_user_root: PathFragment::create("/tmp/ur"),
            },
            real_fs as Arc<dyn FileSystem>,
            9999,
        );
        let mod_req = RunRequest {
            cookie: "req".to_string(),
            arg: vec![
                b"mod".to_vec(),
                format!("--registry=file://{}", reg.display()).into_bytes(),
                b"--lockfile_mode=off".to_vec(),
                b"graph".to_vec(),
            ],
            client_description: "test".to_string(),
            ..Default::default()
        };
        let mod_resp = rt2.dispatch_command(&mod_req, "resp");
        // The vendored registry carries only the MODULE.bazel files (not the sources) of most
        // modules in the embedded bazel_tools' closure, so its module extensions (buildozer,
        // rules_jvm_external, rules_apple) cannot run offline: `ModCommand` then prints the graph
        // and reports `Results may be incomplete as N extensions failed` (exit code 2).
        let mod_err = String::from_utf8_lossy(&mod_resp.standard_error);
        assert!(
            mod_resp.exit_code == 0
                || (mod_resp.exit_code == 2 && mod_err.contains("Results may be incomplete as") && mod_err.contains("extension")),
            "{mod_err}"
        );
        let mod_out = String::from_utf8_lossy(&mod_resp.standard_output);
        assert!(mod_out.contains("<root> (bazel@0.0.1)"), "{mod_out}");
        assert!(mod_out.contains("rules_rs@0.1.3"), "{mod_out}");
        assert!(mod_out.contains("platforms@1.0.0"), "{mod_out}");

        // `sync` no longer exists in Bazel; `fetch`/`vendor` never reach the build engine.
        let sync = rt2.dispatch_command(&RunRequest { arg: vec![b"sync".to_vec()], ..Default::default() }, "resp");
        assert_eq!(sync.exit_code, 2);
        let vendor = rt2.dispatch_command(&RunRequest { arg: vec![b"vendor".to_vec()], ..Default::default() }, "resp");
        assert_eq!(vendor.exit_code, 2);
        assert!(String::from_utf8_lossy(&vendor.standard_error).contains("--vendor_dir"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_server_cquery_aquery_and_incremental_build() {
        let (fs, rt) = test_runtime();

        // cquery
        let cq_req = RunRequest {
            cookie: "req".to_string(),
            arg: vec![b"cquery".to_vec(), b"//src:zel".to_vec()],
            client_description: "test".to_string(),
            ..Default::default()
        };
        let cq_resp = rt.dispatch_command(&cq_req, "resp");
        assert_eq!(cq_resp.exit_code, 0);
        let cq_out = String::from_utf8_lossy(&cq_resp.standard_output);
        assert!(cq_out.contains("//src:zel"));
        // `<label> (<short configuration id>)`, not the configuration mnemonic.
        assert!(cq_out.lines().next().map(|l| l.starts_with("//src:zel (") && l.ends_with(')')).unwrap_or(false), "{cq_out}");

        // aquery
        let aq_req = RunRequest {
            cookie: "req".to_string(),
            arg: vec![b"aquery".to_vec(), b"//src:zel".to_vec()],
            client_description: "test".to_string(),
            ..Default::default()
        };
        let aq_resp = rt.dispatch_command(&aq_req, "resp");
        assert_eq!(aq_resp.exit_code, 0);
        let aq_out = String::from_utf8_lossy(&aq_resp.standard_output);
        assert!(aq_out.contains("Mnemonic: Genrule"));
        assert!(aq_out.contains("Target: //src:zel"));
        assert!(aq_out.contains("ActionKey:"));

        // build with --build_event_json_file and --profile
        let build_req = RunRequest {
            cookie: "req".to_string(),
            arg: vec![
                b"build".to_vec(),
                b"--nobuild".to_vec(),
                b"--build_event_json_file=/tmp/ob/bep.json".to_vec(),
                b"--profile=/tmp/ob/profile.json".to_vec(),
                b"//src:zel".to_vec(),
            ],
            client_description: "test".to_string(),
            ..Default::default()
        };
        let b1 = rt.dispatch_command(&build_req, "resp");
        assert_eq!(b1.exit_code, 0);
        let b1_err = String::from_utf8_lossy(&b1.standard_error);
        assert!(b1_err.contains("Build completed successfully, 0 total actions"), "{b1_err}");
        assert!(fs.exists(&PathFragment::create("/tmp/ob/bep.json"), true));
        assert!(fs.exists(&PathFragment::create("/tmp/ob/profile.json"), true));

        // Second build should be a 100% action cache hit (0 executed)
        let b2 = rt.dispatch_command(&build_req, "resp");
        assert_eq!(b2.exit_code, 0);
        let b2_err = String::from_utf8_lossy(&b2.standard_error);
        assert!(b2_err.contains("Build completed successfully, 0 total actions"), "{b2_err}");
    }

    /// Real execution against the local file system: failure, success, action cache, output
    /// invalidation.
    #[test]
    fn real_build_executes_fails_and_caches() {
        let root = std::env::temp_dir().join(format!("zel-srv-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ws = root.join("ws");
        std::fs::create_dir_all(ws.join("p")).unwrap();
        std::fs::write(ws.join("MODULE.bazel"), "module(name = \"w\")\n").unwrap();
        std::fs::write(
            ws.join("p/BUILD"),
            "genrule(name='ok', outs=['ok.txt'], cmd='echo hi > $@')\ngenrule(name='bad', outs=['bad.txt'], cmd='exit 3')\n",
        )
        .unwrap();
        let fs: Arc<dyn FileSystem> = Arc::new(LocalFileSystem::default());
        let dirs = BlazeDirectories {
            workspace_dir: PathFragment::create(ws.to_str().unwrap()),
            output_base: PathFragment::create(root.join("ob").to_str().unwrap()),
            install_base: PathFragment::create(root.join("ib").to_str().unwrap()),
            output_user_root: PathFragment::create(root.join("ur").to_str().unwrap()),
        };
        let rt = BlazeRuntime::new(dirs, fs, 1);
        let run = |args: &[&str]| {
            let req = RunRequest {
                cookie: "c".into(),
                arg: args.iter().map(|a| a.as_bytes().to_vec()).collect(),
                client_description: "t".into(),
                ..Default::default()
            };
            let r = rt.dispatch_command(&req, "r");
            (r.exit_code, String::from_utf8_lossy(&r.standard_error).to_string())
        };
        let (code, err) = run(&["build", "//p:ok"]);
        assert_eq!(code, 0, "{err}");
        assert!(err.contains("Build completed successfully, 2 total actions\n"), "{err}");
        assert_eq!(std::fs::read_to_string(ws.join("bazel-bin/p/ok.txt")).unwrap(), "hi\n");
        let (code, err) = run(&["build", "//p:ok"]);
        assert_eq!(code, 0, "{err}");
        assert!(err.contains("Build completed successfully, 1 total action\n"), "{err}");
        // Tamper with the output: it must be rebuilt.
        std::fs::write(ws.join("bazel-bin/p/ok.txt"), "x").unwrap();
        let (_, err) = run(&["build", "//p:ok"]);
        assert!(err.contains("2 total actions"), "{err}");
        let (code, err) = run(&["build", "//p:bad"]);
        assert_eq!(code, 1, "{err}");
        assert!(err.contains("(Exit 3)"), "{err}");
        assert!(err.contains("Target //p:bad failed to build"), "{err}");
        assert!(err.contains("ERROR: Build did NOT complete successfully"), "{err}");
        // keep_going: bad fails, ok still built; exit still 1.
        let (code, err) = run(&["build", "--keep_going", "//p:bad", "//p:ok"]);
        assert_eq!(code, 1, "{err}");
        assert!(err.contains("Build succeeded for only 1 of 2 top-level targets"), "{err}");
        assert!(!err.contains("Target //p:ok failed"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }
}


/// `InstrumentationFilterSupport.getInstrumentedPrefix`.
fn instrumented_prefix(package: &str) -> String {
    let mut p = package.to_string();
    for suffix in ["/internal", "/public", "/tests"] {
        if let Some(stripped) = p.strip_suffix(suffix) {
            p = stripped.to_string();
            break;
        }
    }
    // `replaceFirst("(?<=^|/)javatests/", "java/")` then `"(?<=^|/)test/java/" -> "main/java/"`.
    let replace_first = |s: &str, from: &str, to: &str| -> String {
        let mut start = 0;
        while let Some(i) = s[start..].find(from) {
            let at = start + i;
            if at == 0 || s.as_bytes()[at - 1] == b'/' {
                return format!("{}{}{}", &s[..at], to, &s[at + from.len()..]);
            }
            start = at + 1;
        }
        s.to_string()
    };
    let p = replace_first(&p, "javatests/", "java/");
    replace_first(&p, "test/java/", "main/java/")
}

/// `InstrumentationFilterSupport.computeInstrumentationFilter`: the default
/// `--instrumentation_filter` derived from the packages of the test targets on the command line
/// (`None` when there are none, in which case Bazel prints nothing).
fn default_instrumentation_filter(targets: &[zel_packages::Target]) -> Option<String> {
    let mut pkgs: std::collections::BTreeSet<String> = Default::default();
    for t in targets {
        let zel_packages::Target::Rule(rule) = t else { continue };
        let is_suite = t.target_kind() == "test_suite rule";
        if !rule.is_test_rule() && !is_suite {
            continue;
        }
        pkgs.insert(instrumented_prefix(t.label().package_name()));
        if is_suite {
            if let Some(zel_packages::attr::AttrValue::StrList(tests)) = rule.attr_or_default("tests") {
                for l in tests {
                    if let Some(rest) = l.strip_prefix("//") {
                        pkgs.insert(instrumented_prefix(rest.split(':').next().unwrap_or("")));
                    } else if l.starts_with(':') {
                        pkgs.insert(instrumented_prefix(t.label().package_name()));
                    }
                }
            }
        }
    }
    if pkgs.is_empty() {
        return None;
    }
    // Drop filters nested below a previous one (`PathFragment.startsWith`).
    let mut kept: Vec<String> = Vec::new();
    for p in pkgs {
        let nested = kept.last().map_or(false, |prev: &String| {
            prev.is_empty() || p == *prev || p.starts_with(&format!("{prev}/"))
        });
        if !nested {
            kept.push(p);
        }
    }
    let mut f = String::from("^//");
    f.push_str(&kept.join("[/:],^//"));
    f.push_str("[/:]");
    if f == "^//[/:]" {
        f = "^//".to_string();
    }
    Some(f)
}
