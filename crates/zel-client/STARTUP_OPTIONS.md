# Startup options: where each of the 47 is honored

`zel-options` registers all 47 startup options of Bazel (`ALL_STARTUP_OPTIONS`). They reach the server as `--name=value`
arguments (`ServerSpec::server_args`), are part of the restart comparison (`AreStartupOptionsDifferent`, minus the
volatile ones `max_idle_secs`, `connect_timeout_secs`, `local_startup_timeout_secs`, `client_debug`, `preemptible`,
`option_sources`) and are applied as follows.

| Option | Behavior |
|---|---|
| `batch` | In-process execution, no server (`execute_cli_with_exec`). |
| `batch_cpu_scheduling` | Daemon: `SCHED_BATCH` on Linux; no-op elsewhere (as in Bazel). |
| `block_for_lock` | Client lock acquisition (`acquire_output_base_lock`) and `RunRequest.block_for_lock`. |
| `quiet` | Forwarded in `RunRequest.quiet`. |
| `client_debug` | Client prints `INFO: connected to server pid=… at …`; volatile (no restart). |
| `preemptible` | `RunRequest.preemptible`; volatile. |
| `fatal_event_bus_exceptions` | JVM EventBus concept: n/a (accepted, restart-relevant). |
| `host_jvm_debug`, `host_jvm_args`, `host_jvm_profile`, `server_javabase`, `autodetect_server_javabase`, `default_system_javabase`, `extra_classpath`, `experimental_use_compact_object_headers` | JVM-only: n/a for a native server; accepted, and (like Bazel) a change restarts the server. |
| `idle_server_tasks` | Daemon persists the action cache after 10 s of idleness (the native analog of idle GC). |
| `shutdown_on_low_sys_mem` | Daemon exits when idle ≥ 10 s and free system memory < 5 % (`sys::free_memory_fraction`). |
| `delete_output_base_on_shutdown` | Daemon removes the output base on exit. |
| `ignore_all_rc_files`, `system_rc`, `workspace_rc`, `home_rc`, `bazelrc` | Client rc discovery (`zel-options::rc`). |
| `unlimit_coredumps` | Daemon: `setrlimit(RLIMIT_CORE, ∞)`. |
| `write_command_log` | Server: `command.log` is only written when enabled (`clean` never writes it). |
| `windows_enable_symlinks` | Windows only: n/a on Unix. |
| `experimental_remote_repo_contents_cache` | Not implemented (accepted; restart-relevant). |
| `experimental_run_in_user_cgroup`, `experimental_cgroup_parent` | Linux cgroups: not implemented (accepted; restart-relevant). |
| `command_port` | Daemon binds `127.0.0.1:<port>` (default ephemeral); recorded in `server/command_port`. |
| `connect_timeout_secs` | gRPC connect timeout of the client. |
| `local_startup_timeout_secs` | How long the client waits for a new daemon (default 120). |
| `digest_function` | Server applies it per command (`RunRequest.startup_options`). |
| `unix_digest_hash_attribute_name` | xattr digests are not used: n/a. |
| `invocation_policy` | `RunRequest.invocation_policy`. |
| `io_nice_level` | Daemon: best-effort I/O class on Linux; no-op elsewhere. |
| `install_base`, `output_base`, `output_user_root`, `workspace_directory` | Path computation matches `blaze.cc` (`md5(workspace)`, `install/<md5 of binary>`). |
| `macos_qos_class` | Daemon: `pthread_set_qos_class_self_np` on macOS. |
| `max_idle_secs` | Daemon idle shutdown (0 = never); volatile. |
| `server_jvm_out` | stdout/stderr file of the daemon (default `server/jvm.out`). |
| `failure_detail_out` | Client writes the serialized `FailureDetail` of a failed command. |
| `lock_install_base` | Shared lock on `<install_base>.lock` while a command runs. |
| `product_name` | Product name in help/info/messages (daemon). |
| `option_sources` | Accepted; provenance is not logged. |
