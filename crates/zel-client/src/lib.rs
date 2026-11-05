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

//! Bazel CLI Client (`zel`).
//! Mirrors `src/main/cpp/blaze.cc` (workspace discovery, `.bazelrc` loading, server lifecycle,
//! gRPC `CommandServer` connection, streaming `RunResponse`, and `ExecRequest` execution).

pub mod bazelisk;
pub mod daemon_client;

pub use bazelisk::{plan_invocation, BazeliskEnv, LauncherAction, ResolvedVersionSpec, VersionSource};

use std::io::{self, BufReader, BufWriter, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;
use zel_options::{discover_and_load_rc_files, split_command_line, StartupOptions};
use zel_proto::command_server::{PingRequest, PingResponse, RunRequest, RunResponse, ServerInfo};
use zel_proto::grpc_framing::{read_grpc_frame, write_grpc_frame};
use zel_server::{
    create_default_runtime, serve_command_server, BlazeDirectories, BlazeRuntime, RpcMethod,
};
use zel_util::ExitCode;
use zel_vfs::{FileSystem, PathFragment};

const WORKSPACE_BOUNDARY_FILES: &[&str] = &[
    "MODULE.zel",
    "MODULE.bazel",
    "REPO.bazel",
    "WORKSPACE.zel",
    "WORKSPACE.bazel",
    "WORKSPACE",
];

/// Walks up from `cwd` looking for a Bazel workspace boundary marker file
/// (`WorkspaceLayout::GetWorkspace`); `None` when the client is outside any workspace.
pub fn find_workspace_root_opt(fs: &dyn FileSystem, cwd: &PathFragment) -> Option<PathFragment> {
    let mut curr = Some(cwd.clone());
    while let Some(dir) = curr {
        for marker in WORKSPACE_BOUNDARY_FILES {
            if fs.exists(&dir.get_relative_str(marker), true) {
                return Some(dir);
            }
        }
        curr = dir.parent_directory();
    }
    None
}

/// Like [`find_workspace_root_opt`] but falls back to `cwd` outside a workspace.
pub fn find_workspace_root(fs: &dyn FileSystem, cwd: &PathFragment) -> PathFragment {
    find_workspace_root_opt(fs, cwd).unwrap_or_else(|| cwd.clone())
}

/// Sends a `PingRequest` to verify if an existing server is alive and matches `request_cookie`.
pub fn ping_server(address: &str, request_cookie: &str, expected_response_cookie: &str) -> bool {
    let Ok(stream) = TcpStream::connect(address) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return false,
    });
    let mut writer = BufWriter::new(stream);

    let req = PingRequest {
        cookie: request_cookie.to_string(),
    };
    let mut payload = vec![RpcMethod::Ping as u8];
    payload.extend_from_slice(&req.encode_to_vec());
    if write_grpc_frame(&mut writer, &payload).is_err() {
        return false;
    }
    let Ok(Some(resp_bytes)) = read_grpc_frame(&mut reader) else {
        return false;
    };
    let Ok(resp) = PingResponse::decode(&resp_bytes) else {
        return false;
    };
    resp.cookie == expected_response_cookie
}

/// Sends a `RunRequest` over the `CommandServer` stream and forwards `standard_output` and
/// `standard_error` chunks to the provided writers.
pub fn send_run_request<O: Write, E: Write>(
    address: &str,
    req: &RunRequest,
    expected_response_cookie: &str,
    stdout_writer: &mut O,
    stderr_writer: &mut E,
) -> io::Result<RunResponse> {
    let stream = TcpStream::connect(address)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = BufWriter::new(stream);

    let mut frame = vec![RpcMethod::Run as u8];
    frame.extend_from_slice(&req.encode_to_vec());
    write_grpc_frame(&mut writer, &frame)?;

    let mut final_response = RunResponse {
        exit_code: ExitCode::BLAZE_INTERNAL_ERROR.numeric_exit_code(),
        finished: true,
        ..Default::default()
    };

    while let Some(resp_bytes) = read_grpc_frame(&mut reader)? {
        let resp = RunResponse::decode(&resp_bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        if !expected_response_cookie.is_empty() && resp.cookie != expected_response_cookie {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Server response cookie mismatch",
            ));
        }
        if !resp.standard_output.is_empty() {
            stdout_writer.write_all(&resp.standard_output)?;
            stdout_writer.flush()?;
        }
        if !resp.standard_error.is_empty() {
            stderr_writer.write_all(&resp.standard_error)?;
            stderr_writer.flush()?;
        }
        if resp.finished {
            final_response = resp;
            break;
        }
    }

    Ok(final_response)
}

/// Reads `$OUTPUT_BASE/server/server_info.rawproto` if present.
pub fn read_existing_server_info(
    fs: &dyn FileSystem,
    output_base: &PathFragment,
) -> Option<ServerInfo> {
    let info_path = output_base
        .get_relative_str("server")
        .get_relative_str("server_info.rawproto");
    let bytes = fs.read_file(&info_path).ok()?;
    ServerInfo::decode(&bytes).ok()
}

/// Entrypoint for running a CLI invocation and returning the numeric exit code.
pub fn execute_cli<O: Write + Send, E: Write + Send>(
    raw_args: &[String],
    cwd: &PathFragment,
    fs: Arc<dyn FileSystem>,
    stdout_writer: &mut O,
    stderr_writer: &mut E,
) -> i32 {
    let mut exec_request = None;
    execute_cli_with_exec(raw_args, cwd, fs, stdout_writer, stderr_writer, &mut exec_request)
}

/// Directories the client substitutes for `PathToReplace` placeholders in an `ExecRequest`.
#[derive(Debug, Clone, Default)]
pub struct ExecPathContext {
    pub output_base: String,
    pub workspace_dir: String,
    pub working_dir: String,
}

/// An `ExecRequest` together with the directories needed to apply it.
#[derive(Debug, Clone)]
pub struct PendingExec {
    pub request: zel_proto::command_server::ExecRequest,
    pub paths: ExecPathContext,
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Applies a server `ExecRequest` exactly like the Bazel client: writes `--script_path` scripts
/// (and returns 0 without running), otherwise substitutes `PathToReplace` placeholders, sets up
/// the environment, `chdir`s and `exec`s the command (never returns on success on Unix).
/// Returns an exit code only if the program could not be exec'd or a script was written.
pub fn perform_exec_request(pending: &PendingExec) -> i32 {
    use zel_proto::command_server::PathToReplaceType;
    let req = &pending.request;
    let replace = |s: &[u8]| -> String {
        let mut out = lossy(s);
        for p in &req.path_to_replace {
            let actual = match p.r#type {
                PathToReplaceType::OutputBase => &pending.paths.output_base,
                PathToReplaceType::BuildWorkingDirectory => &pending.paths.working_dir,
                PathToReplaceType::BuildWorkspaceDirectory => &pending.paths.workspace_dir,
                _ => continue,
            };
            let ph = lossy(&p.value);
            if !ph.is_empty() {
                out = out.replace(&ph, actual);
            }
        }
        out
    };
    if let Some(sp) = &req.script_path {
        let path = lossy(&sp.script_path);
        if let Err(e) = std::fs::write(&path, &sp.script_contents) {
            eprintln!("ERROR: Could not write script '{path}': {e}");
            return ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
        }
        return 0;
    }
    if req.argv.is_empty() {
        eprintln!("ERROR: empty exec request");
        return ExitCode::BLAZE_INTERNAL_ERROR.numeric_exit_code();
    }
    let argv: Vec<String> = req.argv.iter().map(|a| replace(a)).collect();
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    for name in &req.environment_variable_to_clear {
        cmd.env_remove(lossy(name));
    }
    for ev in &req.environment_variable {
        cmd.env(lossy(&ev.name), replace(&ev.value));
    }
    if !req.working_directory.is_empty() {
        cmd.current_dir(replace(&req.working_directory));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = cmd.exec();
        eprintln!("ERROR: Cannot execute {}: {}", argv[0], err);
        return ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code();
    }
    #[cfg(not(unix))]
    match cmd.status() {
        Ok(st) => st.code().unwrap_or(1),
        Err(e) => {
            eprintln!("ERROR: Cannot execute {}: {}", argv[0], e);
            ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code()
        }
    }
}

#[cfg(unix)]
extern "C" {
    fn localtime_r(timep: *const i64, result: *mut Tm) -> *mut Tm;
}

#[cfg(unix)]
#[repr(C)]
struct Tm {
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

fn client_timestamp() -> String {
    let now = std::time::SystemTime::now();
    let since = now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs();
    let millis = since.subsec_millis();
    #[cfg(unix)]
    unsafe {
        let t = secs as i64;
        let mut tm: Tm = std::mem::zeroed();
        if !localtime_r(&t, &mut tm).is_null() {
            return format!("{:02}:{:02}:{:02}.{:03}", tm.tm_hour, tm.tm_min, tm.tm_sec, millis);
        }
    }
    let s = secs % 60;
    let mi = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    format!("{h:02}:{mi:02}:{s:02}.{millis:03}")
}

fn fatal_blaze_error(msg: &str) -> String {
    let clean = if let Some(idx) = msg.find(" :: ") {
        &msg[idx + 4..]
    } else {
        msg.strip_prefix("ERROR: ").unwrap_or(msg)
    };
    format!("[FATAL {} src/main/cpp/blaze.cc:1275] {}", client_timestamp(), clean)
}

/// `OptionProcessor::PrintStartupOptionsProvenanceMessage` /
/// `BlazeCommandDispatcher`'s "Reading 'startup' options from <rc>: a, b" notes: one per run of
/// consecutive startup options with the same rc source (command-line ones, whose source is empty
/// in blaze.cc, are skipped).
fn startup_provenance_lines(startup: &StartupOptions, sep: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut last_rc = String::new();
    let mut accumulated: Vec<String> = Vec::new();
    for o in &startup.raw_tagged_options {
        let src = String::from_utf8_lossy(&o.source).to_string();
        if src.is_empty() || src == "command line" {
            continue;
        }
        if !last_rc.is_empty() && src != last_rc {
            lines.push(format!("INFO: Reading 'startup' options from {last_rc}: {}", accumulated.join(sep)));
            accumulated.clear();
        }
        last_rc = src;
        accumulated.push(String::from_utf8_lossy(&o.option).to_string());
    }
    if !last_rc.is_empty() {
        lines.push(format!("INFO: Reading 'startup' options from {last_rc}: {}", accumulated.join(sep)));
    }
    lines
}

/// A fatal client error after rc discovery started (`ParseOptionsOrDie`): the startup option
/// provenance goes to stderr first, then the buffered `BAZEL_LOG(INFO)` lines, then the FATAL line.
fn fatal_after_rc_parsing(startup: &StartupOptions, msg: &str) -> String {
    let mut out = String::new();
    for l in startup_provenance_lines(startup, " ") {
        out.push_str(&l);
        out.push('\n');
    }
    for (loc, m) in zel_options::rc::take_client_log() {
        out.push_str(&format!("[INFO {} {loc}] {m}\n", client_timestamp()));
    }
    out.push_str(&fatal_blaze_error(msg));
    out
}

fn error_option_processor(msg: &str) -> String {
    format!("[ERROR {} src/main/cpp/option_processor.cc:362] {}", client_timestamp(), msg)
}

/// Like [`execute_cli`] but surfaces a server `ExecRequest` (as used by `run`) to the caller.
pub fn execute_cli_with_exec<O: Write + Send, E: Write + Send>(
    raw_args: &[String],
    cwd: &PathFragment,
    fs: Arc<dyn FileSystem>,
    stdout_writer: &mut O,
    stderr_writer: &mut E,
    exec_request: &mut Option<PendingExec>,
) -> i32 {
    // Internal flag used when spawning the background server process
    if raw_args.first().map(|s| s.as_str()) == Some("--internal_server_daemon") {
        return run_background_daemon(&raw_args[1..]);
    }

    // Built-in Bazelisk behavior: version resolution (`USE_BAZEL_VERSION`, `.bazelversion`),
    // enforcement and `tools/bazel` wrapper delegation. Only against the host file system.
    let proc_env = BazeliskEnv::from_process_env();
    let is_host_fs = proc_env.current_exe.as_ref().is_some_and(|exe| fs.exists(exe, true));
    let bazelisk_env = if is_host_fs { proc_env } else { BazeliskEnv::default() };
    {
        let ws = find_workspace_root(fs.as_ref(), cwd);
        match bazelisk::plan_invocation(fs.as_ref(), cwd, &ws, raw_args, &bazelisk_env) {
            Ok(LauncherAction::RunInProcess { spec }) => {
                if let Some(ref warn) = spec.warn_message {
                    let _ = writeln!(stderr_writer, "{warn}");
                }
            }
            Ok(LauncherAction::ExecExternal { program, args, env_vars, spec }) => {
                if let Some(ref warn) = spec.warn_message {
                    let _ = writeln!(stderr_writer, "{warn}");
                }
                return match bazelisk::execute_external_action(
                    &program,
                    &args,
                    &env_vars,
                    cwd,
                    bazelisk_env.inherit_stdio,
                    stdout_writer,
                    stderr_writer,
                ) {
                    Ok(code) => code,
                    Err(e) => {
                        let _ = writeln!(stderr_writer, "ERROR: {e}");
                        ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code()
                    }
                };
            }
            Err(e) => {
                let _ = writeln!(stderr_writer, "ERROR: {e}");
                return ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code();
            }
        }
    }

    let (mut startup, command, raw_cmd_args) = match split_command_line(raw_args) {
        Ok(res) => res,
        Err(e) => {
            let _ = writeln!(stderr_writer, "{}", fatal_blaze_error(&e.format_bazel_error()));
            return ExitCode::COMMAND_LINE_ERROR.numeric_exit_code();
        }
    };

    // Fast path for `zel --version` / `bazel --version` (does not start server or load rc files)
    if command == "version" && raw_cmd_args.iter().any(|a| a == "--startup_version") {
        let prefix = zel_server::version_product_prefix();
        let _ = writeln!(stdout_writer, "{prefix} {}", bazelisk_env.current_version);
        return ExitCode::SUCCESS.numeric_exit_code();
    }

    // Outside a workspace the workspace is empty (output base = md5(""), like blaze.cc).
    let workspace_found = find_workspace_root_opt(fs.as_ref(), cwd);
    let workspace_dir = workspace_found.clone().unwrap_or_else(|| PathFragment::create(""));

    let home = std::env::var("HOME").ok();
    let user = std::env::var("USER").unwrap_or_else(|_| "bazel".to_string());

    // Discover and load rc files in exact Bazel precedence order
    let rc_config = match discover_and_load_rc_files(
        &mut startup,
        workspace_found.as_ref(),
        home.as_deref(),
        cwd,
        fs.as_ref(),
        &zel_server::build_label(),
    ) {
        Ok(cfg) => cfg,
        Err(e) => {
            // `ParseErrorToExitCode`: unreadable rc files are INTERNAL_ERROR, everything else BAD_ARGV.
            let code = if e.message.starts_with("Unexpected error reading config file") {
                ExitCode::BLAZE_INTERNAL_ERROR
            } else {
                ExitCode::COMMAND_LINE_ERROR
            };
            let msg = e.message.strip_prefix("FATAL: ").unwrap_or(&e.message).to_string();
            let _ = writeln!(stderr_writer, "{}", fatal_after_rc_parsing(&startup, &msg));
            return code.numeric_exit_code();
        }
    };
    if let Some(e) = startup.deferred_error.take() {
        let _ = writeln!(stderr_writer, "{}", fatal_after_rc_parsing(&startup, &e.message));
        return ExitCode::COMMAND_LINE_ERROR.numeric_exit_code();
    }

    if !startup.workspace_rc {
        if let Some(ref ws) = workspace_found {
            let rc_file = ws.get_relative_str(".bazelrc");
            if fs.exists(&rc_file, false) {
                let real_rc = std::fs::canonicalize(rc_file.path_string()).unwrap_or_else(|_| std::path::PathBuf::from(rc_file.path_string()));
                let _ = writeln!(
                    stderr_writer,
                    "WARNING: The following rc files are no longer being read, please transfer their contents or import their path into one of the standard rc files:\n{}",
                    real_rc.display()
                );
            }
        }
    }

    // Check for explicit empty command string: `zel ''`
    if command.is_empty() && raw_args.iter().any(|a| a.is_empty()) {
        let prefix = if startup.product_name.is_empty() { "bazel" } else { &startup.product_name };
        let _ = writeln!(
            stderr_writer,
            "{}",
            fatal_blaze_error(&format!("Command cannot be the empty string.\n  For more info, run '{prefix} help'."))
        );
        return ExitCode::COMMAND_LINE_ERROR.numeric_exit_code();
    }

    // Bare `zel` without workspace: Bazel prints error that it must be invoked from within a workspace
    if command.is_empty() && workspace_found.is_none() {
        let prefix = if startup.product_name == "bazel" || startup.product_name.is_empty() {
            "Bazel"
        } else {
            &startup.product_name
        };
        let _ = writeln!(
            stderr_writer,
            "ERROR: {prefix} must be invoked from within a workspace (below a directory having a MODULE.bazel file).\nSee documentation at https://bazel.build/concepts/build-ref#workspace"
        );
        return ExitCode::COMMAND_LINE_ERROR.numeric_exit_code();
    }

    let effective_cmd = if command.is_empty() {
        "help".to_string()
    } else {
        command
    };

    if workspace_found.is_none() {
        if let Some(spec) = zel_server::commands::command_spec(&effective_cmd) {
            if spec.must_run_in_workspace {
                let prefix = if startup.product_name == "bazel" || startup.product_name.is_empty() {
                    "Bazel"
                } else {
                    &startup.product_name
                };
                let _ = writeln!(
                    stderr_writer,
                    "ERROR: {prefix} '{effective_cmd}' must be invoked from within a workspace (below a directory having a MODULE.bazel file).\nSee documentation at https://bazel.build/concepts/build-ref#workspace"
                );
                return ExitCode::COMMAND_LINE_ERROR.numeric_exit_code();
            }
        }
    }

    // rc options are structured for the invoked command itself (BlazeOptionHandler), also for
    // `canonicalize-flags` (whose `--for_command` only affects how the command line is parsed).
    let rc_command = effective_cmd.as_str();

    let (expanded_cmd_args, rc_diagnostics) = match rc_config.expand_command_args_with_diagnostics(rc_command, &raw_cmd_args) {
        Ok(res) => res,
        Err(e) => {
            let _ = writeln!(stderr_writer, "ERROR: {e}");
            return ExitCode::COMMAND_LINE_ERROR.numeric_exit_code();
        }
    };
    let output_user_root = startup.resolve_output_user_root(&user, home.as_deref());
    let output_base = startup.resolve_output_base(&workspace_dir, &user, home.as_deref());
    let install_md5 = binary_install_md5();
    let install_base = startup.resolve_install_base(&install_md5, &user, home.as_deref());

    // `StartupOptions::SanityCheckJavabase` (`--server_javabase`), then `PrepareDirectories`
    // (`output_user_root`, `output_base`), as the C++ client does before it talks to a server.
    if let Some(desc) = startup.parsed_options.get("server_javabase") {
        let jb = desc.raw_values.last().cloned().unwrap_or_default();
        if !jb.is_empty() {
            let java = std::path::Path::new(&jb).join("bin/java");
            let problem = match std::fs::metadata(&java) {
                Err(_) => Some(format!("Couldn't find java at '{}'.", java.display())),
                Ok(m) => {
                    use std::os::unix::fs::PermissionsExt;
                    if m.is_dir() || m.permissions().mode() & 0o111 == 0 {
                        Some(format!(
                            "Java at '{}' exists but is not executable: {}",
                            java.display(),
                            daemon_client::errno_string(&std::io::Error::from_raw_os_error(13))
                        ))
                    } else {
                        None
                    }
                }
            };
            if let Some(p) = problem {
                let src = if desc.source.is_empty() || desc.source == "command line" {
                    "on the command line".to_string()
                } else {
                    format!("in {}", desc.source)
                };
                let _ = writeln!(stderr_writer, "ERROR: {p}");
                let _ = writeln!(stderr_writer, "ERROR:   The java path was specified by a '--server_javabase' option {src}");
                return ExitCode::COMMAND_LINE_ERROR.numeric_exit_code();
            }
        }
    }
    if let Err(code) = daemon_client::prepare_directories(
        std::path::Path::new(output_user_root.path_string()),
        std::path::Path::new(output_base.path_string()),
        stderr_writer,
    ) {
        return code;
    }

    // `RepositoryOptions`/`BazelRepositoryModule`: the default repo contents cache
    // (`<output_user_root>/cache/repos/v1/contents`) must not live inside the main repository.
    if !workspace_dir.path_string().is_empty()
        && !expanded_cmd_args
            .iter()
            .take_while(|a| a.as_str() != "--")
            .any(|a| a == "--repo_contents_cache" || a.starts_with("--repo_contents_cache="))
        && !matches!(effective_cmd.as_str(), "help" | "version" | "shutdown" | "clean")
    {
        let real = |p: &str| std::fs::canonicalize(p).map(|c| c.display().to_string()).unwrap_or_else(|_| p.to_string());
        let cache = format!("{}/cache/repos/v1/contents", real(output_user_root.path_string()));
        let ws = real(workspace_dir.path_string());
        if cache.starts_with(&format!("{}/", ws.trim_end_matches('/'))) {
            let _ = writeln!(
                stderr_writer,
                "ERROR: The repo contents cache [{cache}] is inside the main repo [{ws}]. This can cause spurious failures. Disable the repo contents cache with `--repo_contents_cache=`, or specify `--repo_contents_cache=<path outside the main repo>`."
            );
            return ExitCode::COMMAND_LINE_ERROR.numeric_exit_code();
        }
    }

    // `shutdown` without a live server returns in the client before any option provenance is printed.
    let shutdown_without_server = effective_cmd == "shutdown"
        && daemon_client::find_running_server(&std::path::Path::new(output_base.path_string()).join("server")).is_none();
    // `--announce_rc` is an option of the invoked command: arguments after a `--` terminator are residue
    // (e.g. the flags `canonicalize-flags` is asked to canonicalize) and never set it.
    let announce_rc = !shutdown_without_server
        && expanded_cmd_args
            .iter()
            .take_while(|a| a.as_str() != "--")
            .any(|a| a == "--announce_rc" || a == "--announce_rc=true");
    if announce_rc {
        for l in startup_provenance_lines(&startup, ", ") {
            let _ = writeln!(stderr_writer, "{l}");
        }
        // The launcher's own `common` chunk (blaze.cc passes --isatty/--terminal_columns through
        // `--default_override=0:common=...`, source "client").
        let _ = writeln!(
            stderr_writer,
            "INFO: Options provided by the client:\n  Inherited 'common' options: --isatty={} --terminal_columns={}",
            if zel_server::sys::is_tty(2) { 1 } else { 0 },
            zel_server::sys::terminal_columns(2)
        );
        for diag in &rc_diagnostics {
            let _ = writeln!(stderr_writer, "{diag}");
        }
    }

    let mut full_argv_bytes = Vec::with_capacity(2 + expanded_cmd_args.len());
    full_argv_bytes.push(effective_cmd.as_bytes().to_vec());
    // Like Bazel's C++ client, tell the server the directory the client was started in so that
    // relative target patterns (`:foo`, `...`, `sub/pkg:all`) resolve against it. The flag goes
    // right after the command name, i.e. before any `--` terminator.
    if matches!(
        effective_cmd.as_str(),
        "build" | "test" | "run" | "coverage" | "query" | "cquery" | "aquery"
    ) {
        full_argv_bytes.push(format!("--client_cwd={}", cwd.path_string()).into_bytes());
    }
    // Like `OptionProcessor::GetBlazercAndEnvCommandArgs`, pass the client's environment to the
    // server as `--client_env=NAME=VALUE` arguments (the server needs the *client's* variables for
    // `--action_env=NAME`, `--test_env=NAME`, `--repo_env=NAME`, `bazel run`, ...; its own
    // environment is whatever the daemon was started with).
    if !matches!(effective_cmd.as_str(), "help" | "version" | "shutdown") {
        let mut env: Vec<(String, String)> = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect();
        env.sort();
        for (k, v) in env {
            full_argv_bytes.push(format!("--client_env={k}={v}").into_bytes());
        }
    }
    // Terminal facts for the server-side UI (`--isatty`, `--terminal_columns`), like blaze.cc.
    // User-supplied values (appended below) win because they come later.
    if matches!(effective_cmd.as_str(), "build" | "test" | "run" | "coverage") {
        let tty = zel_server::sys::is_tty(2);
        full_argv_bytes.push(format!("--isatty={}", if tty { 1 } else { 0 }).into_bytes());
        full_argv_bytes.push(
            format!("--terminal_columns={}", zel_server::sys::terminal_columns(2)).into_bytes(),
        );
    }
    for a in &expanded_cmd_args {
        full_argv_bytes.push(a.as_bytes().to_vec());
    }

    let mut run_req = RunRequest {
        cookie: "batch-cookie".to_string(),
        arg: full_argv_bytes,
        block_for_lock: startup.block_for_lock,
        client_description: format!("pid={}", std::process::id()),
        invocation_policy: startup.invocation_policy.clone(),
        startup_options: startup.raw_tagged_options.clone(),
        preemptible: startup_flag(&startup, "preemptible", false),
        command_extensions: Vec::new(),
        quiet: startup.quiet,
    };

    if effective_cmd == "shutdown" {
        let in_ws = find_workspace_root_opt(fs.as_ref(), &cwd).is_some();
        if !in_ws {
            let prod = if startup.product_name.is_empty() { "Bazel" } else { &startup.product_name };
            let _ = writeln!(
                stderr_writer,
                "WARNING: Running command \"shutdown\" in batch mode.  Batch mode is triggered\n\
                 when not running {prod} within a workspace. If you intend to shutdown an\n\
                 existing {prod} server, run \"{} shutdown\" from the directory where\n\
                 it was started.",
                prod.to_ascii_lowercase()
            );
        }
        // `blaze.cc`: with no live server the client returns before the command is ever parsed (exit 0, no
        // message about unrecognized arguments); otherwise the *server* parses the options.
        let server_dir = std::path::Path::new(output_base.path_string()).join("server");
        if daemon_client::find_running_server(&server_dir).is_none() {
            return 0;
        }
        let parsed = match zel_options::ParsedCommandOptions::parse_for_command("shutdown", &expanded_cmd_args) {
            Ok(p) => p,
            Err(e) => {
                let _ = writeln!(stderr_writer, "{}", e.format_bazel_error());
                return ExitCode::COMMAND_LINE_ERROR.numeric_exit_code();
            }
        };
        if !parsed.targets.is_empty() {
            let _ = writeln!(stderr_writer, "ERROR: Unrecognized arguments: {}", parsed.targets.join(" "));
            return ExitCode::COMMAND_LINE_ERROR.numeric_exit_code();
        }
    }

    // Persistent server mode (default): lock, find / (re)start the daemon, stream the command.
    if !startup.batch && std::path::Path::new(cwd.path_string()).is_dir() {
        let paths = ExecPathContext {
            output_base: output_base.path_string().to_string(),
            workspace_dir: workspace_dir.path_string().to_string(),
            working_dir: cwd.path_string().to_string(),
        };
        return run_with_daemon(
            &startup,
            run_req,
            paths,
            output_user_root.path_string(),
            stdout_writer,
            stderr_writer,
            exec_request,
        );
    }

    // Fallback / `--batch` mode execution
    let paths = ExecPathContext {
        output_base: output_base.path_string().to_string(),
        workspace_dir: workspace_dir.path_string().to_string(),
        working_dir: cwd.path_string().to_string(),
    };
    let dirs = BlazeDirectories {
        workspace_dir,
        output_base,
        install_base,
        output_user_root,
    };
    let runtime = BlazeRuntime::new(dirs, fs, std::process::id());
    let resp = runtime.dispatch_command(&run_req, "batch-cookie");
    let _ = stdout_writer.write_all(&resp.standard_output);
    let _ = stderr_writer.write_all(&resp.standard_error);
    *exec_request = resp.exec_request.clone().map(|request| PendingExec { request, paths });
    resp.exit_code
}

/// Digest of the running binary (`install_md5`): the install base is derived from it so that a
/// new binary never reuses (or talks to a server of) an older installation.
pub fn binary_install_md5() -> String {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            if let Ok(v) = std::env::var("ZEL_INSTALL_MD5_OVERRIDE") {
                return v;
            }
            let Ok(exe) = std::env::current_exe() else {
                return zel_util::md5::hex_digest(zel_server::build_label().as_bytes());
            };
            // Memoize the (slow-in-debug) content digest keyed by path, size and mtime.
            let meta = std::fs::metadata(&exe).ok();
            let key = meta.as_ref().map(|m| {
                let mtime = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                zel_util::md5::hex_digest(
                    format!("{}|{}|{}", exe.display(), m.len(), mtime).as_bytes(),
                )
            });
            let cache_dir = std::env::temp_dir()
                .join(format!("zel-install-md5-{}", std::env::var("USER").unwrap_or_default()));
            if let Some(k) = &key {
                if let Ok(v) = std::fs::read_to_string(cache_dir.join(k)) {
                    if v.len() == 32 {
                        return v;
                    }
                }
            }
            let digest = match std::fs::read(&exe) {
                Ok(bytes) => zel_util::md5::hex_digest(&bytes),
                Err(_) => zel_util::md5::hex_digest(zel_server::build_label().as_bytes()),
            };
            if let Some(k) = &key {
                let _ = std::fs::create_dir_all(&cache_dir);
                let _ = std::fs::write(cache_dir.join(k), &digest);
            }
            digest
        })
        .clone()
}

#[allow(clippy::too_many_arguments)]
/// The effective boolean value of a startup option (`--flag`, `--noflag`, `--flag=false`).
pub fn startup_flag(startup: &StartupOptions, name: &str, default: bool) -> bool {
    startup
        .parsed_options
        .get(name)
        .and_then(|d| d.raw_values.last())
        .map(|v| !matches!(v.as_str(), "false" | "0" | "no"))
        .unwrap_or(default)
}

/// `--failure_detail_out`: writes the serialized `FailureDetail` of a failed command.
fn write_failure_detail(startup: &StartupOptions, detail: Option<&zel_proto::failure_details::FailureDetail>) {
    let Some(d) = startup.parsed_options.get("failure_detail_out").and_then(|d| d.raw_values.last()) else {
        return;
    };
    if d.is_empty() {
        return;
    }
    if let Some(detail) = detail {
        let _ = std::fs::write(d, detail.encode_to_vec());
    }
}

fn run_with_daemon<O: Write, E: Write>(
    startup: &StartupOptions,
    run_req: RunRequest,
    paths: ExecPathContext,
    output_user_root: &str,
    stdout_writer: &mut O,
    stderr_writer: &mut E,
    exec_request: &mut Option<PendingExec>,
) -> i32 {
    let install_md5 = binary_install_md5();
    let user = std::env::var("USER").unwrap_or_else(|_| "bazel".to_string());
    let home = std::env::var("HOME").ok();
    let install_base = startup.resolve_install_base(&install_md5, &user, home.as_deref());
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            let _ = writeln!(stderr_writer, "ERROR: cannot determine own path: {e}");
            return ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code();
        }
    };
    let spec = daemon_client::ServerSpec {
        exe,
        startup,
        workspace_dir: &paths.workspace_dir,
        output_base: &paths.output_base,
        install_base: install_base.path_string(),
        output_user_root,
        install_md5: &install_md5,
    };
    let debug = startup_flag(startup, "client_debug", false);
    // `--lock_install_base`: a shared lock on `<install_base>.lock` while the install is in use.
    let _install_lock = if startup_flag(startup, "lock_install_base", true) {
        match daemon_client::acquire_install_base_lock(install_base.path_string()) {
            Ok(l) => l,
            Err(e) => {
                let _ = writeln!(stderr_writer, "FATAL: open failed for install base lock: {}", daemon_client::errno_string(&e));
                return ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code();
            }
        }
    } else {
        None
    };
    let lock = match daemon_client::acquire_output_base_lock(
        std::path::Path::new(&paths.output_base),
        startup.block_for_lock,
        stderr_writer,
    ) {
        Ok(l) => l,
        Err(code) => return code,
    };
    let server = match daemon_client::connect_or_start(&spec, stderr_writer) {
        Ok(s) => s,
        Err((code, msg)) => {
            let _ = writeln!(stderr_writer, "FATAL: {msg}");
            return code;
        }
    };
    drop(lock);
    if debug {
        let _ = writeln!(
            stderr_writer,
            "INFO: connected to server pid={} at {}",
            server.info.pid, server.info.address
        );
    }
    daemon_client::set_product_name(&{
        let mut n = startup.product_name.clone();
        if let Some(c) = n.get_mut(0..1) {
            c.make_ascii_uppercase();
        }
        n
    });
    let outcome = daemon_client::run_command(
        &server.info,
        &run_req,
        stdout_writer,
        stderr_writer,
        spec.connect_timeout(),
    );
    if let Some(status) = outcome.abrupt {
        let _ = writeln!(
            stderr_writer,
            "Server terminated abruptly (error code: {}, error message: '{}', log file: '{}')\n",
            status.code,
            status.message,
            spec.jvm_out().display()
        );
        return ExitCode::BLAZE_INTERNAL_ERROR.numeric_exit_code();
    }
    let resp = outcome.response;
    write_failure_detail(startup, resp.failure_detail.as_ref());
    if resp.termination_expected {
        daemon_client::await_server_exit(&spec.server_dir(), server.info.pid);
    }
    *exec_request = resp.exec_request.clone().map(|request| PendingExec { request, paths });
    resp.exit_code
}

fn run_background_daemon(args: &[String]) -> i32 {
    let mut workspace_dir = PathFragment::create("/");
    let mut output_base = PathFragment::create("/tmp/zel-ob");
    let mut install_base = PathFragment::create("/tmp/zel-ib");
    let mut output_user_root = PathFragment::create("/tmp/zel-root");
    let mut cfg = zel_server::daemon::DaemonConfig {
        request_cookie: "cookie-req".to_string(),
        response_cookie: "cookie-resp".to_string(),
        max_idle_secs: 3 * 3600,
        shutdown_on_low_sys_mem: false,
        idle_server_tasks: true,
    };
    let mut command_port: u16 = 0;
    let truthy = |v: &str| !matches!(v, "false" | "0" | "no");
    for arg in args {
        if let Some(v) = arg.strip_prefix("--workspace_directory=") {
            workspace_dir = PathFragment::create(v);
        } else if let Some(v) = arg.strip_prefix("--output_base=") {
            output_base = PathFragment::create(v);
        } else if let Some(v) = arg.strip_prefix("--install_base=") {
            install_base = PathFragment::create(v);
        } else if let Some(v) = arg.strip_prefix("--output_user_root=") {
            output_user_root = PathFragment::create(v);
        } else if let Some(v) = arg.strip_prefix("--request_cookie=") {
            cfg.request_cookie = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--response_cookie=") {
            cfg.response_cookie = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--max_idle_secs=") {
            cfg.max_idle_secs = v.parse().unwrap_or(cfg.max_idle_secs);
        } else if let Some(v) = arg.strip_prefix("--command_port=") {
            command_port = v.parse().unwrap_or(0);
        } else if let Some(v) = arg.strip_prefix("--shutdown_on_low_sys_mem=") {
            cfg.shutdown_on_low_sys_mem = truthy(v);
        } else if let Some(v) = arg.strip_prefix("--idle_server_tasks=") {
            cfg.idle_server_tasks = truthy(v);
        } else if let Some(v) = arg.strip_prefix("--unlimit_coredumps=") {
            if truthy(v) {
                zel_server::sys::unlimit_coredumps();
            }
        } else if let Some(v) = arg.strip_prefix("--macos_qos_class=") {
            zel_server::sys::set_macos_qos_class(v);
        } else if let Some(v) = arg.strip_prefix("--batch_cpu_scheduling=") {
            if truthy(v) {
                zel_server::sys::set_batch_cpu_scheduling();
            }
        } else if let Some(v) = arg.strip_prefix("--io_nice_level=") {
            zel_server::sys::set_io_nice_level(v.parse().unwrap_or(-1));
        } else if let Some(v) = arg.strip_prefix("--product_name=") {
            zel_server::commands::set_product_name(&v.to_ascii_lowercase());
        } else if let Some(v) = arg.strip_prefix("--install_md5=") {
            zel_server::commands::set_install_md5(v);
        }
    }

    let Ok(listener) = TcpListener::bind(("127.0.0.1", command_port)) else {
        return ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code();
    };
    let runtime =
        create_default_runtime(workspace_dir, output_base, install_base, output_user_root);
    match zel_server::daemon::serve_grpc(listener, runtime, cfg) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("FATAL: server failed: {e}");
            ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code()
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_detached_daemon(
    exe_path: &std::path::Path,
    startup: &StartupOptions,
    workspace_dir: &PathFragment,
    output_base: &PathFragment,
    install_base: &PathFragment,
    output_user_root: &PathFragment,
    request_cookie: &str,
    response_cookie: &str,
) -> io::Result<u32> {
    let _ = startup;
    let child = Command::new(exe_path)
        .arg("--internal_server_daemon")
        .arg(format!("--workspace_directory={workspace_dir}"))
        .arg(format!("--output_base={output_base}"))
        .arg(format!("--install_base={install_base}"))
        .arg(format!("--output_user_root={output_user_root}"))
        .arg(format!("--request_cookie={request_cookie}"))
        .arg(format!("--response_cookie={response_cookie}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(child.id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_vfs::InMemoryFileSystem;

    #[test]
    fn test_end_to_end_cli_commands() {
        let fs = Arc::new(InMemoryFileSystem::default());
        let ws = PathFragment::create("/workspace");
        fs.create_directory_and_parents(&ws).unwrap();
        fs.write_file(
            &ws.get_relative_str("MODULE.bazel"),
            b"module(name = \"my_ws\", version = \"1.0.0\")\n",
        )
        .unwrap();
        fs.write_file(&ws.get_relative_str(".bazelrc"), b"build:opt -c opt\n")
            .unwrap();

        let pkg = ws.get_relative_str("app");
        fs.create_directory_and_parents(&pkg).unwrap();
        fs.write_file(
            &pkg.get_relative_str("BUILD.bazel"),
            b"filegroup(name = \"hello\", srcs = [\"hello.cc\"])\n",
        )
        .unwrap();

        // 1. Test `bazel version`
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = execute_cli(
            &["--batch".to_string(), "version".to_string()],
            &ws,
            Arc::clone(&fs) as Arc<dyn FileSystem>,
            &mut out,
            &mut err,
        );
        assert_eq!(code, 0);
        assert!(String::from_utf8_lossy(&out).contains("Build label: 9.3.2"));

        // 2. Test `bazel canonicalize-flags -- --config=opt`
        out.clear();
        err.clear();
        let code = execute_cli(
            &[
                "--batch".to_string(),
                "canonicalize-flags".to_string(),
                "--".to_string(),
                "--config=opt".to_string(),
            ],
            &ws,
            Arc::clone(&fs) as Arc<dyn FileSystem>,
            &mut out,
            &mut err,
        );
        assert_eq!(code, 0);
        assert_eq!(
            String::from_utf8_lossy(&out).trim(),
            "--config=opt"
        );

        // 3. Test `bazel query //app/...`
        out.clear();
        err.clear();
        let code = execute_cli(
            &[
                "--batch".to_string(),
                "query".to_string(),
                "//app/...".to_string(),
            ],
            &ws,
            Arc::clone(&fs) as Arc<dyn FileSystem>,
            &mut out,
            &mut err,
        );
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        assert_eq!(String::from_utf8_lossy(&out).trim(), "//app:hello");
    }
}
