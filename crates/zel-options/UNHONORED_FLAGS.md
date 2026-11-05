# Unhonored Bazel Flags in Zel

All 1,114+ command options and 47 startup options from Bazel are registered, typed, and parsed by `zel-options` with full schema fidelity. Unknown flags are rejected with exit code 2 and spellchecking suggestions.

However, certain flags are not yet honored by other subsystems in Zel (e.g. execution, sandboxing, remote caching, Skyframe, rule providers). They are parsed and stored in `ParsedCommandOptions.parsed_options` and `StartupOptions.parsed_options`, but are currently inert in downstream execution.

Below is the comprehensive audit and registry of unhonored flags by subsystem.

---

## 1. Startup & Server Lifecycle Options

Currently honored: `--batch`, `--output_base`, `--output_user_root`, `--install_base`, `--quiet`, `--ignore_all_rc_files`, `--system_rc`, `--workspace_rc`, `--home_rc`, `--bazelrc`, `--max_idle_secs`, `--invocation_policy`.

Unhonored startup flags:
- `--batch_cpu_scheduling`: Linux `SCHED_BATCH` scheduling priority.
- `--block_for_lock`: Server output base locking (Zel does not yet acquire `fcntl`/`flock` on `$OUTPUT_BASE/lock`).
- `--client_debug`: Verbose diagnostic client socket logging.
- `--command_port`: Port for gRPC server socket.
- `--connect_timeout_secs`: Connection timeout to server daemon.
- `--delete_output_base_on_shutdown`: Deletion of output base upon daemon exit.
- `--digest_function`: Configurable action digest function (Zel uses BLAKE3 / SHA-256 hardcoded in `zel-util`).
- `--experimental_cgroup_parent`: Linux cgroup parent directory for server process.
- `--experimental_remote_repo_contents_cache`: Remote cache for external repositories.
- `--experimental_run_in_user_cgroup`: Runs server inside user cgroup.
- `--experimental_use_compact_object_headers`: JVM compact object header flag.
- `--extra_classpath`: Additional server classpath jars.
- `--failure_detail_out`: Binary path for writing `FailureDetail` proto on exit.
- `--fatal_event_bus_exceptions`: Crash daemon on EventBus handler exception.
- `--host_jvm_args`: JVM flags for Java-based Bazel server.
- `--host_jvm_debug`: Starts server JVM with JDWP debugging.
- `--host_jvm_profile`: JVM profiling agent flags.
- `--idle_server_tasks`: Background cache and memory cleanup while idle.
- `--io_nice_level`: Linux IO scheduling nice level.
- `--local_startup_timeout_secs`: Client timeout waiting for daemon to become healthy.
- `--lock_install_base`: Shared flock on install base.
- `--macos_qos_class`: macOS pthread QoS class.
- `--option_sources`: Server-side provenance logging.
- `--preemptible`: Mark invocation preemptible by later invocations.
- `--product_name`: Custom binary product name.
- `--server_javabase`: Javabase used for server JVM.
- `--server_jvm_out`: Server daemon stderr/stdout log file path.
- `--shutdown_on_low_sys_mem`: Server daemon emergency shutdown on memory exhaustion.
- `--unix_digest_hash_attribute_name`: Filesystem extended attribute for digest caching.
- `--unlimit_coredumps`: Core dump size removal.
- `--windows_enable_symlinks`: Windows native symlinks in execroot.
- `--workspace_directory`: Explicit workspace directory override.
- `--write_command_log`: Writing command log to `$OUTPUT_BASE/command.log`.

---

## 2. Execution & Action Strategy Options (`ExecutionOptions`, `LocalExecutionOptions`)

Honored (audited against `zel-server/src/exec_phase.rs`, `zel-exec`): `--subcommands`, `--jobs`, `--spawn_strategy`,
`--genrule_strategy`, `--strategy`, `--strategy_regexp`, `--verbose_failures`, `--local_resources`, `--local_test_jobs`,
`--local_termination_grace_seconds` / `--local_sigkill_grace_seconds`, `--allowed_local_actions_regex`, `--flaky_test_attempts`,
`--test_summary`, `--check_up_to_date`, `--experimental_local_lockfree_output` (dynamic default local list),
`--internal_spawn_scheduler` / `--experimental_spawn_scheduler`, `--dynamic_local_strategy`, `--dynamic_remote_strategy`,
`--dynamic_local_execution_delay`, `--experimental_dynamic_ignore_local_signals`,
`--experimental_dynamic_cancel_remote_branch_on_local_win`, `--debug_spawn_scheduler` (parsed only),
`--remote_local_fallback_strategy`.

Unhonored flags:
- `--materialize_param_files`: Disk materialization of `@params` files.
- `--test_keep_going`: Continuing test execution on failure.
- `--test_strategy`: Selects the `TestActionContext` (`standalone`/`exclusive`); only the standalone strategy exists.
- `--test_tmpdir`: Base directory for test temp directories.
- `--experimental_dynamic_slow_remote_time`, `--experimental_dynamic_local_load_factor`,
  `--experimental_dynamic_exclude_tools`: dynamic-execution scheduler tuning (no thread limiter / tool configuration info).
  Note: dynamic's remote branch cannot be interrupted mid-RPC; it is flagged and finishes in the background.

---

## 3. Sandboxing & Worker Options (`SandboxOptions`, `WorkerOptions`)

Honored: `--sandbox_base`, `--sandbox_debug`, `--sandbox_default_allow_network`, `--sandbox_tmpfs_path`,
`--sandbox_writable_path`, `--sandbox_block_path`, `--sandbox_add_mount_pair`, `--sandbox_fake_hostname`,
`--sandbox_fake_username`, `--sandbox_explicit_pseudoterminal`, `--worker_max_instances`, `--worker_max_multiplex_instances`,
`--worker_multiplex`, `--worker_sandboxing`, `--worker_verbose`, `--worker_quit_after_build`, `--worker_extra_flag`,
`--experimental_worker_*` (cancellation, strict flagfiles, multiplex sandboxing).

Unhonored flags:
- `--experimental_use_hermetic_linux_sandbox`: Linux unshare / namespaces sandbox runner.
- `--experimental_use_windows_sandbox`: Windows process isolation sandbox.
- `--reuse_sandbox_directories`: Fast recycling of persistent sandbox directories.

---

## 4. Remote Execution & Caching Options (`RemoteOptions`, `AuthAndTLSOptions`)

Honored via `zel-remote` + `ExecRuntime` (`remote` strategy, remote cache wrapped around local strategies, `dynamic`):
`--remote_executor`, `--remote_cache`, `--remote_instance_name`, `--disk_cache`, `--remote_header`, `--remote_cache_header`,
`--remote_exec_header`, `--remote_download_outputs` (+ legacy shorthands), `--remote_upload_local_results`,
`--remote_accept_cached`, `--remote_timeout`, `--remote_retries`, `--remote_retry_max_delay`, `--remote_max_connections`,
`--remote_local_fallback`, `--remote_default_exec_properties`, `--credential_helper`, `--remote_proxy=unix:...`,
the server `--digest_function` (SHA256/MD5 only).

Unhonored (rejected with an explicit error or warning, see `crates/zel-remote/README.md`):
- `--tls_certificate`, `--tls_enabled`, `grpcs://`: no TLS stack (rejected).
- `--google_credentials`: needs an OAuth token exchange over TLS (rejected when a remote cache is configured).
- `--remote_cache_compression`: warning, blobs are uncompressed.
- `--remote_download_regex`, HTTP/1.1 caches, circuit breaker, lease extension, `--remote_grpc_log`.
- Tests keep absolute paths: test spawns are never executed remotely (`no-remote-exec`) but use the caches.

---

## 5. Core Configuration & Toolchain Options (`CoreOptions`, `CppOptions`, `JavaOptions`, etc.)

Currently honored: `--compilation_mode` (`fastbuild`, `dbg`, `opt`), `--define` (`key=val`), `--config`, `--flag_alias`.

Honored since the integration pass: `--platforms`, `--extra_toolchains`, `--extra_execution_platforms`, `--deleted_packages`, `--repo_env`.

Unhonored flags (environment flags are being wired by the env-semantics work):
- `--action_env`: Passing specific host environment variables to actions.
- `--host_action_env`: Host configuration action environment.
- `--cpu`: Target CPU architecture.
- `--host_cpu`: Host CPU architecture.
- `--features`: Toolchain features (e.g. `opt`, `pic`, `thin_lto`).
- `--copt`, `--cxxopt`, `--conlyopt`: C/C++ compiler arguments.
- `--linkopt`: Linker arguments.
- `--strip`: Binary stripping mode (`always`, `sometimes`, `never`).
- `--javacopt`: Java compiler options.
- `--jvmopt`: JVM execution options for Java binaries.
- `--java_language_version`: Java source/target version.
- `--stamp`: Build information stamping (`BUILD_EMBED_LABEL`, etc.).

---

## 6. Starlark & Package Semantics Options (`BuildLanguageOptions`, `PackageOptions`)

Unhonored flags:
- `--incompatible_*`: Backwards-incompatible Starlark semantic flags.
- `--default_visibility`: Default visibility for packages without explicit `default_visibility`.
- `--package_path`: Search path for package roots.
- `--max_computation_steps`: Limit on Starlark execution steps to prevent infinite loops.

---

## 7. Testing Options (`TestOptions`, `TestSummaryOptions`)

Honored: `--test_filter`, `--test_arg`, `--test_env` (see environment notes), `--test_timeout`, `--cache_test_results`,
`--runs_per_test`, `--test_sharding_strategy`, `--test_tag_filters`, `--test_size_filters`, `--test_timeout_filters`,
`--test_lang_filters`, `--test_output`, `--build_tests_only`, coverage (`--instrumentation_filter`,
`--instrument_test_targets`, `--combined_report=lcov`).

Unhonored flags:
- `--coverage_support` / `--coverage_report_generator` label overrides (embedded Bazel scripts are used).

---

## 8. Build Event Protocol (BEP) & Logging (`BuildEventProtocolOptions`, `BuildEventServiceOptions`, `UiOptions`)

Currently honored: `--build_event_json_file`, `--profile`.

Unhonored flags:
- `--build_event_binary_file`: Protobuf binary format BEP file destination.
- `--build_event_text_file`: Protobuf text format BEP file destination.
- `--bes_backend`: Build Event Service gRPC streaming endpoint.
- `--bes_results_url`: Base URL for viewing BES build results.
- `--color`: ANSI terminal color escape codes (`yes`, `no`, `auto`).
- `--curses`: Interactive curses terminal output with dynamic progress bars.
- `--terminal_columns`: Forced terminal width.
- `--show_progress`: Console progress update display.
