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

pub use bazelisk::{
    escape_bazelisk_source_name, plan_invocation, populate_bazelisk_cache_entry,
    resolve_version_binary, resolve_version_spec, BazeliskEnv, LauncherAction, ResolvedVersionSpec,
    VersionSource,
};

use std::collections::HashSet;
use std::io::{self, BufReader, BufWriter, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use zel_options::{split_command_line, BazelrcConfig, StartupOptions};
use zel_proto::command_server::{PingRequest, PingResponse, RunRequest, RunResponse, ServerInfo};
use zel_proto::grpc_framing::{read_grpc_frame, write_grpc_frame};
use zel_server::{
    cleanup_server_status_files, create_default_runtime, serve_command_server, BlazeDirectories,
    BlazeRuntime, RpcMethod,
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

/// Walks up from `cwd` looking for a Bazel workspace boundary marker file.
pub fn find_workspace_root(fs: &dyn FileSystem, cwd: &PathFragment) -> PathFragment {
    let mut curr = Some(cwd.clone());
    while let Some(dir) = curr {
        for marker in WORKSPACE_BOUNDARY_FILES {
            if fs.exists(&dir.get_relative_str(marker), true) {
                return dir;
            }
        }
        curr = dir.parent_directory();
    }
    cwd.clone()
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

/// Reads `$OUTPUT_BASE/server/build_label.txt` if present.
pub fn read_existing_server_build_label(
    fs: &dyn FileSystem,
    output_base: &PathFragment,
) -> Option<String> {
    let label_path = output_base
        .get_relative_str("server")
        .get_relative_str("build_label.txt");
    let bytes = fs.read_file(&label_path).ok()?;
    let label = String::from_utf8_lossy(&bytes).trim().to_string();
    if label.is_empty() {
        None
    } else {
        Some(label)
    }
}

/// Entrypoint for running a CLI invocation and returning the numeric exit code.
pub fn execute_cli<O: Write + Send, E: Write + Send>(
    raw_args: &[String],
    cwd: &PathFragment,
    fs: Arc<dyn FileSystem>,
    stdout_writer: &mut O,
    stderr_writer: &mut E,
) -> i32 {
    let proc_env = BazeliskEnv::from_process_env();
    let is_host_fs = proc_env
        .current_exe
        .as_ref()
        .is_some_and(|exe| fs.exists(exe, true));
    let env = if is_host_fs {
        proc_env
    } else {
        BazeliskEnv::default()
    };
    execute_cli_with_env(raw_args, cwd, fs, &env, stdout_writer, stderr_writer)
}

/// Entrypoint for running a CLI invocation with an explicit `BazeliskEnv` configuration.
pub fn execute_cli_with_env<O: Write + Send, E: Write + Send>(
    raw_args: &[String],
    cwd: &PathFragment,
    fs: Arc<dyn FileSystem>,
    env: &BazeliskEnv,
    stdout_writer: &mut O,
    stderr_writer: &mut E,
) -> i32 {
    // Internal flag used when spawning the background server process
    if raw_args.first().map(|s| s.as_str()) == Some("--internal_server_daemon") {
        return run_background_daemon(&raw_args[1..]);
    }

    let workspace_dir = find_workspace_root(fs.as_ref(), cwd);

    // Built-in Bazelisk version resolution, enforcement, and wrapper delegation
    match bazelisk::plan_invocation(fs.as_ref(), cwd, &workspace_dir, raw_args, env) {
        Ok(LauncherAction::RunInProcess { spec }) => {
            if let Some(ref warn) = spec.warn_message {
                let _ = writeln!(stderr_writer, "{warn}");
            }
        }
        Ok(LauncherAction::ExecExternal {
            program,
            args,
            env_vars,
            spec,
        }) => {
            if let Some(ref warn) = spec.warn_message {
                let _ = writeln!(stderr_writer, "{warn}");
            }
            match bazelisk::execute_external_action(
                &program,
                &args,
                &env_vars,
                cwd,
                env.inherit_stdio,
                stdout_writer,
                stderr_writer,
            ) {
                Ok(code) => return code,
                Err(e) => {
                    let _ = writeln!(stderr_writer, "ERROR: {e}");
                    return ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code();
                }
            }
        }
        Err(e) => {
            let _ = writeln!(stderr_writer, "ERROR: {e}");
            return ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code();
        }
    }

    let (mut startup, command, raw_cmd_args) = match split_command_line(raw_args) {
        Ok(res) => res,
        Err(e) => {
            let _ = writeln!(stderr_writer, "ERROR: {e}");
            return ExitCode::COMMAND_LINE_ERROR.numeric_exit_code();
        }
    };

    // Fast path for `zel --version` / `bazel --version` (does not start server or load rc files)
    if command == "version" && raw_cmd_args.iter().any(|a| a == "--startup_version") {
        let prefix = zel_server::version_product_prefix();
        let _ = writeln!(stdout_writer, "{prefix} {}", env.current_version);
        return ExitCode::SUCCESS.numeric_exit_code();
    }

    // Load .zelrc and .bazelrc files unless --ignore_all_rc_files is set
    let mut rc_config = BazelrcConfig::default();
    let mut visited_rcs = HashSet::new();
    if !startup.ignore_all_rc_files {
        if startup.workspace_rc {
            for rc_name in [".zelrc", ".bazelrc"] {
                let ws_rc = workspace_dir.get_relative_str(rc_name);
                if let Ok(bytes) = fs.read_file(&ws_rc) {
                    visited_rcs.insert(ws_rc.clone());
                    let content = String::from_utf8_lossy(&bytes);
                    let _ = rc_config.parse_str(
                        &content,
                        ws_rc.path_string(),
                        &workspace_dir,
                        fs.as_ref(),
                        &mut visited_rcs,
                    );
                }
            }
        }
        for custom_rc in startup.bazelrc_files.clone() {
            if custom_rc == "/dev/null" {
                continue;
            }
            let rc_pf = PathFragment::create(&custom_rc);
            let full_rc = if rc_pf.is_absolute() {
                rc_pf
            } else {
                workspace_dir.get_relative(&rc_pf)
            };
            if let Ok(bytes) = fs.read_file(&full_rc) {
                visited_rcs.insert(full_rc.clone());
                let content = String::from_utf8_lossy(&bytes);
                if let Err(e) = rc_config.parse_str(
                    &content,
                    full_rc.path_string(),
                    &workspace_dir,
                    fs.as_ref(),
                    &mut visited_rcs,
                ) {
                    let _ = writeln!(stderr_writer, "ERROR: {e}");
                    return ExitCode::COMMAND_LINE_ERROR.numeric_exit_code();
                }
            }
        }
        if let Some(startup_rc_opts) = rc_config.sections.get("startup") {
            for (flag, src) in startup_rc_opts {
                let _ = startup.apply_flag(flag, src);
            }
        }
    }

    let effective_cmd = if command.is_empty() {
        "help".to_string()
    } else {
        command
    };

    let rc_command = if effective_cmd == "canonicalize-flags" {
        raw_cmd_args
            .iter()
            .find_map(|a| a.strip_prefix("--for_command="))
            .unwrap_or("build")
    } else {
        effective_cmd.as_str()
    };

    let expanded_cmd_args = match rc_config.expand_command_args(rc_command, &raw_cmd_args) {
        Ok(args) => args,
        Err(e) => {
            let _ = writeln!(stderr_writer, "ERROR: {e}");
            return ExitCode::COMMAND_LINE_ERROR.numeric_exit_code();
        }
    };

    let user = std::env::var("USER").unwrap_or_else(|_| "bazel".to_string());
    let home = std::env::var("HOME").ok();
    let output_user_root = startup.resolve_output_user_root(&user, home.as_deref());
    let output_base = startup.resolve_output_base(&workspace_dir, &user, home.as_deref());
    let install_key = format!("zel-{}-install-key", env.current_version);
    let install_base = startup.resolve_install_base(&install_key, &user, home.as_deref());

    let mut full_argv_bytes = Vec::with_capacity(1 + expanded_cmd_args.len());
    full_argv_bytes.push(effective_cmd.as_bytes().to_vec());
    for a in &expanded_cmd_args {
        full_argv_bytes.push(a.as_bytes().to_vec());
    }

    let mut run_req = RunRequest {
        cookie: "batch-cookie".to_string(),
        arg: full_argv_bytes,
        block_for_lock: startup.block_for_lock,
        client_description: format!("pid={}", std::process::id()),
        invocation_policy: String::new(),
        startup_options: startup.raw_tagged_options.clone(),
        preemptible: false,
        command_extensions: Vec::new(),
        quiet: startup.quiet,
        block_for_lock_timeout_ms: None,
    };

    // If not in batch mode and using LocalFileSystem, check if a server daemon is already running
    if !startup.batch {
        if let Some(info) = read_existing_server_info(fs.as_ref(), &output_base) {
            if ping_server(&info.address, &info.request_cookie, &info.response_cookie) {
                let server_version_matches =
                    read_existing_server_build_label(fs.as_ref(), &output_base)
                        .map(|srv_ver| {
                            bazelisk::normalize_version(&srv_ver)
                                == bazelisk::normalize_version(&env.current_version)
                        })
                        .unwrap_or(false);

                if server_version_matches {
                    run_req.cookie = info.request_cookie.clone();
                    if let Ok(resp) = send_run_request(
                        &info.address,
                        &run_req,
                        &info.response_cookie,
                        stdout_writer,
                        stderr_writer,
                    ) {
                        return resp.exit_code;
                    }
                } else {
                    // Existing server is running a different or unknown Zel version; shut it down
                    // and wait for it to clean up its status files before starting the new server
                    let shutdown_req = RunRequest {
                        cookie: info.request_cookie.clone(),
                        arg: vec![b"shutdown".to_vec()],
                        block_for_lock: true,
                        client_description: format!("pid={}", std::process::id()),
                        ..Default::default()
                    };
                    let mut sink_out = Vec::new();
                    let mut sink_err = Vec::new();
                    let _ = send_run_request(
                        &info.address,
                        &shutdown_req,
                        &info.response_cookie,
                        &mut sink_out,
                        &mut sink_err,
                    );
                    let server_dir = output_base.get_relative_str("server");
                    for _ in 0..400 {
                        let status_cleared =
                            read_existing_server_info(fs.as_ref(), &output_base).is_none();
                        let server_alive =
                            ping_server(&info.address, &info.request_cookie, &info.response_cookie);
                        if status_cleared && !server_alive {
                            break;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    if !ping_server(&info.address, &info.request_cookie, &info.response_cookie) {
                        cleanup_server_status_files(fs.as_ref(), &server_dir);
                    }
                }
            }
        }

        // Try starting an in-process or background server over localhost TCP
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", 0)) {
            let req_cookie = "cookie-req-zel".to_string();
            let resp_cookie = "cookie-resp-zel".to_string();
            run_req.cookie = req_cookie.clone();

            // If we can spawn a detached daemon binary from current_exe, do so; otherwise serve in a thread
            let addr = listener.local_addr().unwrap().to_string();
            let dirs = BlazeDirectories {
                workspace_dir: workspace_dir.clone(),
                output_base: output_base.clone(),
                install_base: install_base.clone(),
                output_user_root: output_user_root.clone(),
            };
            let runtime = Arc::new(BlazeRuntime::new(dirs, Arc::clone(&fs), std::process::id()));
            let rt_clone = Arc::clone(&runtime);
            let req_c = req_cookie.clone();
            let resp_c = resp_cookie.clone();

            let server_handle = thread::spawn(move || {
                // Handle single request in embedded server mode if background daemon isn't detached
                let _ = serve_command_server(listener, rt_clone, req_c, resp_c);
            });

            // Wait briefly for server status files and connect
            for _ in 0..20 {
                if ping_server(&addr, &req_cookie, &resp_cookie) {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }

            if let Ok(resp) =
                send_run_request(&addr, &run_req, &resp_cookie, stdout_writer, stderr_writer)
            {
                // Signal shutdown for the embedded single-shot server thread
                runtime
                    .shutdown_requested
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                let _ = TcpStream::connect(&addr);
                let _ = server_handle.join();
                return resp.exit_code;
            }
        }
    }

    // Fallback / `--batch` mode execution
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
    resp.exit_code
}

fn run_background_daemon(args: &[String]) -> i32 {
    let mut workspace_dir = PathFragment::create("/");
    let mut output_base = PathFragment::create("/tmp/zel-ob");
    let mut install_base = PathFragment::create("/tmp/zel-ib");
    let mut output_user_root = PathFragment::create("/tmp/zel-root");
    let mut request_cookie = "cookie-req".to_string();
    let mut response_cookie = "cookie-resp".to_string();

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
            request_cookie = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--response_cookie=") {
            response_cookie = v.to_string();
        }
    }

    let Ok(listener) = TcpListener::bind(("127.0.0.1", 0)) else {
        return ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code();
    };
    let runtime =
        create_default_runtime(workspace_dir, output_base, install_base, output_user_root);
    match serve_command_server(listener, runtime, request_cookie, response_cookie) {
        Ok(()) => 0,
        Err(_) => ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code(),
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
            b"cc_binary(name = \"hello\", srcs = [\"hello.cc\"])\n",
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
        assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&err));
        assert!(String::from_utf8_lossy(&out).contains("Build label: 0.0.1"));

        // 2. Test `bazel canonicalize-flags --config=opt`
        out.clear();
        err.clear();
        let code = execute_cli(
            &[
                "--batch".to_string(),
                "canonicalize-flags".to_string(),
                "--config=opt".to_string(),
            ],
            &ws,
            Arc::clone(&fs) as Arc<dyn FileSystem>,
            &mut out,
            &mut err,
        );
        assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&err));
        assert_eq!(
            String::from_utf8_lossy(&out).trim(),
            "--compilation_mode=opt"
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
        assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&err));
        assert_eq!(String::from_utf8_lossy(&out).trim(), "//app:hello");
    }

    #[test]
    fn test_version_resolution_precedence_and_files() {
        let fs = Arc::new(InMemoryFileSystem::default());
        let ws = PathFragment::create("/workspace");
        fs.create_directory_and_parents(&ws).unwrap();
        fs.write_file(
            &ws.get_relative_str("MODULE.zel"),
            b"module(name = \"ws\")\n",
        )
        .unwrap();
        fs.write_file(&ws.get_relative_str(".bazelversion"), b"# comment\n8.0.0\n")
            .unwrap();

        let mut env = BazeliskEnv {
            current_version: "0.0.1".to_string(),
            os_name: "darwin".to_string(),
            arch_name: "arm64".to_string(),
            raw_arch_name: "aarch64".to_string(),
            ..BazeliskEnv::default()
        };

        // 1. .bazelversion is picked up when .zelversion and env vars are absent
        let spec = resolve_version_spec(fs.as_ref(), &ws, &ws, &env).unwrap();
        assert_eq!(spec.version, "8.0.0");
        assert_eq!(
            spec.source,
            VersionSource::BazelVersionFile(ws.get_relative_str(".bazelversion"))
        );

        // 2. .zelversion takes precedence over .bazelversion
        fs.write_file(&ws.get_relative_str(".zelversion"), b"0.0.2\n")
            .unwrap();
        let spec = resolve_version_spec(fs.as_ref(), &ws, &ws, &env).unwrap();
        assert_eq!(spec.version, "0.0.2");
        assert_eq!(
            spec.source,
            VersionSource::ZelVersionFile(ws.get_relative_str(".zelversion"))
        );

        // 3. USE_BAZEL_VERSION takes precedence over .zelversion and .bazelversion
        env.use_bazel_version = Some("9.1.0".to_string());
        let spec = resolve_version_spec(fs.as_ref(), &ws, &ws, &env).unwrap();
        assert_eq!(spec.version, "9.1.0");
        assert_eq!(spec.source, VersionSource::UseBazelVersionEnv);

        // 4. USE_ZEL_VERSION takes highest precedence
        env.use_zel_version = Some("v0.0.3".to_string());
        let spec = resolve_version_spec(fs.as_ref(), &ws, &ws, &env).unwrap();
        assert_eq!(spec.version, "v0.0.3");
        assert_eq!(spec.source, VersionSource::UseZelVersionEnv);
    }

    #[test]
    fn test_version_enforcement_and_sibling_delegation() {
        let fs = Arc::new(InMemoryFileSystem::default());
        let ws = PathFragment::create("/workspace");
        let bin_dir = PathFragment::create("/usr/local/bin");
        fs.create_directory_and_parents(&ws).unwrap();
        fs.create_directory_and_parents(&bin_dir).unwrap();
        fs.write_file(&ws.get_relative_str("MODULE.bazel"), b"")
            .unwrap();
        fs.write_file(&ws.get_relative_str(".zelversion"), b"0.0.2\n")
            .unwrap();

        let env = BazeliskEnv {
            current_exe: Some(bin_dir.get_relative_str("zel")),
            current_version: "0.0.1".to_string(),
            no_download: true,
            os_name: "darwin".to_string(),
            arch_name: "arm64".to_string(),
            raw_arch_name: "aarch64".to_string(),
            ..BazeliskEnv::default()
        };

        // 1. Missing required version with no_download=true produces a version enforcement error
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = execute_cli_with_env(
            &["--batch".to_string(), "version".to_string()],
            &ws,
            Arc::clone(&fs) as Arc<dyn FileSystem>,
            &env,
            &mut out,
            &mut err,
        );
        assert_ne!(code, 0);
        let err_str = String::from_utf8_lossy(&err);
        assert!(err_str.contains("requires Zel version '0.0.2'"));

        // 2. Installing sibling binary `zel-0.0.2-darwin-arm64` resolves to ExecExternal
        let sibling = bin_dir.get_relative_str("zel-0.0.2-darwin-arm64");
        fs.write_file(&sibling, b"#!/bin/sh\n").unwrap();
        let action = plan_invocation(fs.as_ref(), &ws, &ws, &["build".to_string()], &env).unwrap();
        match action {
            LauncherAction::ExecExternal { program, .. } => {
                assert_eq!(program, sibling);
            }
            other => panic!("Expected ExecExternal, got {other:?}"),
        }

        // 3. Matching version runs in-process without external delegation
        fs.write_file(&ws.get_relative_str(".zelversion"), b"v0.0.1\n")
            .unwrap();
        let action = plan_invocation(fs.as_ref(), &ws, &ws, &["build".to_string()], &env).unwrap();
        assert!(matches!(action, LauncherAction::RunInProcess { .. }));
    }

    #[test]
    fn test_auto_download_and_sha256_verification() {
        let fs = Arc::new(InMemoryFileSystem::default());
        let ws = PathFragment::create("/workspace");
        let mirror = PathFragment::create("/mirror/releases");
        let cache_home = PathFragment::create("/cache/zel-home");
        fs.create_directory_and_parents(&ws).unwrap();
        fs.create_directory_and_parents(&mirror.get_relative_str("9.3.1"))
            .unwrap();
        fs.write_file(&ws.get_relative_str("MODULE.bazel"), b"")
            .unwrap();
        fs.write_file(&ws.get_relative_str(".bazelversion"), b"9.3.1\n")
            .unwrap();

        let fake_bin_bytes = b"fake-zel-9.3.1-binary-payload";
        let fake_sha = zel_util::sha256::hex_digest(fake_bin_bytes);
        let release_art = mirror
            .get_relative_str("9.3.1")
            .get_relative_str("zel-9.3.1-linux-x86_64");
        let release_sha = mirror
            .get_relative_str("9.3.1")
            .get_relative_str("zel-9.3.1-linux-x86_64.sha256");
        fs.write_file(&release_art, fake_bin_bytes).unwrap();
        fs.write_file(
            &release_sha,
            format!("{fake_sha}  zel-9.3.1-linux-x86_64\n").as_bytes(),
        )
        .unwrap();

        let mut env = BazeliskEnv {
            current_version: "0.0.1".to_string(),
            base_url: Some(format!("file://{}", mirror)),
            home_dir: Some(cache_home.clone()),
            os_name: "linux".to_string(),
            arch_name: "x86_64".to_string(),
            raw_arch_name: "x86_64".to_string(),
            ..BazeliskEnv::default()
        };

        let action = plan_invocation(fs.as_ref(), &ws, &ws, &["info".to_string()], &env).unwrap();
        let expected_cached = cache_home
            .get_relative_str("downloads/zel")
            .get_relative_str("zel-9.3.1-linux-x86_64");
        match action {
            LauncherAction::ExecExternal { program, .. } => {
                assert_eq!(program, expected_cached);
            }
            other => panic!("Expected ExecExternal, got {other:?}"),
        }
        assert_eq!(
            fs.read_file(&expected_cached).unwrap(),
            fake_bin_bytes.to_vec()
        );

        // Verify tampered SHA-256 is rejected
        env.verify_sha256 =
            Some("0000000000000000000000000000000000000000000000000000000000000000".to_string());
        let err = plan_invocation(fs.as_ref(), &ws, &ws, &["info".to_string()], &env).unwrap_err();
        assert!(err.contains("SHA-256 checksum mismatch"));
    }

    #[test]
    fn test_workspace_wrapper_delegation_and_fallback_env() {
        let fs = Arc::new(InMemoryFileSystem::default());
        let ws = PathFragment::create("/workspace");
        let tools_dir = ws.get_relative_str("tools");
        let exe_path = PathFragment::create("/usr/local/bin/zel");
        fs.create_directory_and_parents(&tools_dir).unwrap();
        fs.write_file(&ws.get_relative_str("MODULE.bazel"), b"")
            .unwrap();
        let wrapper = tools_dir.get_relative_str("zel");
        fs.write_file(&wrapper, b"#!/bin/sh\nexec \"$ZEL_REAL\" \"$@\"\n")
            .unwrap();

        let env = BazeliskEnv {
            current_exe: Some(exe_path.clone()),
            current_version: "0.0.1".to_string(),
            fallback_version: Some((
                "USE_ZEL_FALLBACK_VERSION".to_string(),
                "warn:0.0.1".to_string(),
            )),
            ..BazeliskEnv::default()
        };

        let action = plan_invocation(fs.as_ref(), &ws, &ws, &["build".to_string()], &env).unwrap();
        match action {
            LauncherAction::ExecExternal {
                program,
                env_vars,
                spec,
                ..
            } => {
                assert_eq!(program, wrapper);
                assert!(spec.warn_message.is_some());
                assert!(env_vars.contains(&("ZEL_REAL".to_string(), exe_path.to_string())));
                assert!(env_vars.contains(&("BAZEL_REAL".to_string(), exe_path.to_string())));
                assert!(env_vars.contains(&("ZEL_SKIP_WRAPPER".to_string(), "true".to_string())));
                assert!(
                    env_vars.contains(&("BAZELISK_SKIP_WRAPPER".to_string(), "true".to_string()))
                );
            }
            other => panic!("Expected ExecExternal to tools/zel wrapper, got {other:?}"),
        }
    }

    #[test]
    fn test_bazeliskrc_and_bazelisk_cache_interoperability() {
        let fs = Arc::new(InMemoryFileSystem::default());
        let ws = PathFragment::create("/workspace");
        let bhome = PathFragment::create("/cache/bazelisk-home");
        fs.create_directory_and_parents(&ws).unwrap();
        fs.write_file(&ws.get_relative_str("MODULE.bazel"), b"")
            .unwrap();
        fs.write_file(
            &ws.get_relative_str(".bazeliskrc"),
            b"BAZELISK_BASE_URL=https://github.com/scottpledger/zel/releases/download\nBAZELISK_HOME=/cache/bazelisk-home\n",
        )
        .unwrap();
        fs.write_file(&ws.get_relative_str(".bazelversion"), b"9.3.1\n")
            .unwrap();

        let env = BazeliskEnv {
            current_version: "0.0.1".to_string(),
            no_download: true,
            os_name: "darwin".to_string(),
            arch_name: "arm64".to_string(),
            raw_arch_name: "aarch64".to_string(),
            ..BazeliskEnv::default()
        };

        // Populate Bazelisk's content-addressable cache (`downloads/metadata/...` + `downloads/sha256/<sha>/bin/bazel`)
        let payload = b"#!/bin/sh\necho \"bazel 9.3.1\"\n";
        let sha_bin = populate_bazelisk_cache_entry(
            fs.as_ref(),
            &bhome,
            "9.3.1",
            "https://github.com/scottpledger/zel/releases/download",
            payload,
            &env,
        )
        .unwrap();

        // `zel` resolves the binary from Bazelisk's metadata/sha256 cache configured via `.bazeliskrc`
        let action = plan_invocation(fs.as_ref(), &ws, &ws, &["info".to_string()], &env).unwrap();
        match action {
            LauncherAction::ExecExternal { program, .. } => {
                assert_eq!(program, sha_bin);
            }
            other => panic!("Expected ExecExternal to cached sha256 binary, got {other:?}"),
        }

        // When invoked by external `bazelisk` (`bazelisk_skip_wrapper = true`), `.bazelversion` was
        // already handled by `bazelisk`, so `zel` runs in-process.
        let mut under_bazelisk = env.clone();
        under_bazelisk.bazelisk_skip_wrapper = true;
        let action = plan_invocation(
            fs.as_ref(),
            &ws,
            &ws,
            &["info".to_string()],
            &under_bazelisk,
        )
        .unwrap();
        assert!(matches!(action, LauncherAction::RunInProcess { .. }));

        // If `.zelversion` is also present, `zel` enforces `.zelversion` even when invoked via `bazelisk`
        fs.write_file(&ws.get_relative_str(".zelversion"), b"9.3.1\n")
            .unwrap();
        let action = plan_invocation(
            fs.as_ref(),
            &ws,
            &ws,
            &["info".to_string()],
            &under_bazelisk,
        )
        .unwrap();
        match action {
            LauncherAction::ExecExternal { program, .. } => {
                assert_eq!(program, sha_bin);
            }
            other => panic!(
                "Expected ExecExternal when .zelversion differs under bazelisk, got {other:?}"
            ),
        }

        // Workspace `.bazeliskrc` (`USE_BAZEL_VERSION`) takes precedence over user home `.zeliskrc` (`USE_ZEL_VERSION`)
        let user_home = PathFragment::create("/home/user");
        fs.create_directory_and_parents(&user_home).unwrap();
        fs.write_file(
            &ws.get_relative_str(".bazeliskrc"),
            b"USE_BAZEL_VERSION=9.3.1\nBAZELISK_HOME=/cache/bazelisk-home\n",
        )
        .unwrap();
        fs.write_file(
            &user_home.get_relative_str(".zeliskrc"),
            b"USE_ZEL_VERSION=0.0.1\n",
        )
        .unwrap();
        let mut with_home_env = env.clone();
        with_home_env.user_home = Some(user_home);
        let merged = with_home_env.with_rc_files(fs.as_ref(), &ws);
        let spec = resolve_version_spec(fs.as_ref(), &ws, &ws, &merged).unwrap();
        assert_eq!(spec.version, "9.3.1");
        assert_eq!(spec.source, VersionSource::UseBazelVersionEnv);
    }
}
