# Zel vs. Bazel: Differences Audit

Audit in progress (started 2026-11-04). Zel: `~/Development/zel` (~13.7k lines of Rust across 19 crates). Bazel reference: `~/Development/bazel`.

## Smoke-test observations (manual run of `target/debug/zel --batch` in a tiny temp workspace)

- **[DIFFERENT]** `zel build //:nonexistent` prints `INFO: Analyzed 1 target.` / `Build completed successfully, 1 total actions` and exits 0. Bazel fails with `ERROR: no such target '//:nonexistent'` (exit 1).
- **[DIFFERENT]** Unknown flags are accepted without complaint (`zel build --bogus_flag //:g` exits 0). Bazel errors with `ERROR: --bogus_flag :: Unrecognized option: --bogus_flag` (exit 2).
- **[MISSING]** No convenience symlinks get created (`bazel-bin`, `bazel-out`, `bazel-testlogs`, `bazel-<ws>`). Outputs only exist under `<output_base>/execroot/_main/bazel-out/...`.
- **[DIFFERENT]** Output base layout is different: `action_cache/action_cache.bin` sits at the output_base root (Bazel uses `action_cache/` along with `server/`, `external/`, `execroot/<ws>/`, `install` symlink, `command.log`, `java.log`, `lock`, and so on). In `--batch` mode Zel creates no `server/`, `external/`, or `command.log`.
- **[DIFFERENT]** A failing `sh_test` prints only `//:t FAILED in 0.1s`, *before* the analysis/build INFO lines. Bazel shows `FAIL: //:t (see .../test.log)`, the test summary block, `Executed 1 out of 1 test: 1 fails locally.`, and so on. Exit code 3 matches Bazel.
- **[DIFFERENT]** The genrule action counts as "1 executed" even when the action is a no-op re-run, and Bazel's `INFO: Analyzed 1 target (N packages loaded, M targets configured).`, `INFO: Found 1 target...`, and `INFO: Elapsed time:` lines are absent or abbreviated.
- **[PARTIAL]** `query --output=xml` returns a bare `<rule class="genrule" name="//:g"/>` with no `location` attribute, no `<string>`/`<list>` attribute children, and no `<rule-input>`/`<rule-output>` elements.

---

# Executive summary

Zel is currently a prototype and is far from a 1:1 Bazel replacement. Rough parity per area, as estimated by the auditors:

| Area | Est. parity | Headline gaps |
|---|---|---|
| Client / startup / server lifecycle | low | No persistent daemon. Custom TCP protocol, not gRPC. 17 of 42 startup flags rejected, 14 are no-ops. No locking. `zel run` never execs. Bazel's rc file locations ignored. Wrong macOS cache dir. |
| Commands | low | `config`, `dump`, `license`, `print_action` are stubs. `mobile-install` missing. `fetch`, `vendor`, `coverage` aliased to build. `clean --expunge` does not delete. `help <cmd>` and `info` keys incomplete. |
| Options framework / flags | ~2% | Honors 19 of 1,110 Bazel flags. Unknown flags are silently accepted. `-- //foo` builds nothing. No converters, expansion, or invocation policy. |
| Starlark | 15-20% | No Resolver, bigint, float, or set. `5\|3 == 8`, `1<5<10 == False`, reversed `%` args, list has no methods, no json or proto modules. |
| Loading / packages / labels | 20-30% | Nonexistent targets succeed. Negative and external patterns broken. Regex fallback masks Starlark errors. `package()` is a no-op. Macros missing. |
| Bzlmod / repos | <5% | No `repository_ctx`, `module_ctx`, extensions, registries, lockfile, or transitive MVS. `mod` subcommands faked. |
| Query / cquery / aquery | ~15% | Single-package traversal only. cquery and aquery are stubs. Most formatters, functions, and flags missing. |
| Analysis / rules | very low | `rule()` and `aspect()` are dummies, so `ctx` does not exist. `cc_*`, `java_*`, and `proto_*` write fake outputs. No toolchains, platforms, or transitions. |
| Execution / test / remote | ~15% | Failed actions reported as success. No runfiles, no convenience symlinks. Sandbox and workers are stubs. No REAPI client. `--jobs` ignored. |
| Events / BEP / profiler / protos | 15-20% | No curses or color UI. BEP emits 6 of 25+ event types. `analyze-profile` is a no-op. Only 3 of 65+ failure categories. `--digest_function` ignored. |

Detailed per-area reports follow; each is complete, with source references on both sides.


## Client, Startup Options, and Server Lifecycle

### Overview & Parity Rating

**Overall Parity Level: Low to Moderate (~35% functional parity).**

While `crates/zel-options`, `crates/zel-client`, and `crates/zel-server` replicate high-level Bazel CLI syntax, the actual client and server lifecycle diverges substantially from real Bazel (`src/main/cpp` and `src/main/java/com/google/devtools/build/lib/server`):
- **Server Persistence:** Real Bazel spawns a persistent daemon process across CLI invocations that holds memory state (Skyframe graph, action cache). In Zel, `execute_cli` in non-batch mode starts an embedded server thread inside the client process, executes a single command, immediately signals shutdown, and terminates. The function `spawn_detached_daemon` exists but is completely dead code. Every `zel` command runs ephemerally from scratch.
- **Wire Protocol Incompatibility:** Real Bazel uses standard HTTP/2-framed gRPC (`command_server.proto`). Zel uses a custom raw TCP framing (`[5-byte length][1-byte method enum][proto payload]`), making `zel-client` completely unable to communicate with real Bazel servers, and real Bazel clients unable to communicate with `zel-server`.
- **Startup Flag Coverage:** Out of 42 C++ startup flags registered by Bazel, Zel completely rejects 17 as unknown options, accepts 14 as no-op stubs that are parsed but discarded, implements 7 with partial/divergent semantics, and fully honors only ~4.
- **Concurrency & Locking:** Real Bazel acquires exclusive `fcntl`/`flock` locks on `$OUTPUT_BASE/lock` and shared locks on `$INSTALL_BASE.lock`. Zel does no file locking whatsoever; multiple concurrent invocations on the same output base will corrupt state or encounter TCP port conflicts.
- **Missing Core Client Features:** Client-side signals (Ctrl-C / `SIGINT`) do not send `CancelRequest`; `ExecRequest` returned by `bazel run` is completely ignored by `zel-client` (the built target is never executed); `command.log` and `java.log` are never written to; and rc-file discovery omits `/etc/bazel.bazelrc`, `$HOME/.bazelrc`, and the `BAZELRC` environment variable.

---

### Startup Options Parity: Comprehensive Enumeration

Bazel registers 42 startup flags in C++ (`src/main/cpp/startup_options.cc`, `bazel_startup_options.cc`) and mirrors them in Java (`BlazeServerStartupOptions.java`). Below is the complete enumeration of all startup flags and Zel's level of support in `crates/zel-options/src/lib.rs`.

1. **`--autodetect_server_javabase` / `--noautodetect_server_javabase`**
   - **[STUB]** Bazel: Controls whether Bazel searches for an installed JDK when `--server_javabase` is omitted (`startup_options.cc:152`).
   - Zel: Included in `NULLARY_STARTUP_FLAGS` (`crates/zel-options/src/lib.rs:52`), but `apply_flag` does not assign it to any field and ignores it.
2. **`--batch` / `--nobatch`**
   - **[DIFFERENT]** Bazel: Runs the command in non-daemon batch mode; the server exits immediately after the command completes (`startup_options.cc:113`, `blaze.cc:1714`).
   - Zel: Parsed into `startup.batch` (`crates/zel-options/src/lib.rs:89`). However, Zel's normal daemon mode also terminates immediately after running one command (lines 331-337 in `crates/zel-client/src/lib.rs`), making `--batch` and non-batch functionally equivalent except that `--batch` bypasses gRPC wire framing and calls `BlazeRuntime::dispatch_command` directly.
3. **`--batch_cpu_scheduling` / `--nobatch_cpu_scheduling`**
   - **[MISSING]** Bazel: Uses Linux `SCHED_BATCH` CPU policy for the server process (`startup_options.cc:114`, `blaze_util_posix.cc:387`).
   - Zel: Completely absent from `NULLARY_STARTUP_FLAGS`. Passing `--batch_cpu_scheduling` returns error exit code 2: `Unknown startup option`.
4. **`--bazelrc=<path>`**
   - **[DIFFERENT]** Bazel: Specifies an additional rc file or `/dev/null`. Can be repeated (`bazel_startup_options.cc:39`). Cannot be specified inside an rc file (`bazel_startup_options.cc:55`). Non-existent files cause client error exit code 2.
   - Zel: Parsed into `startup.bazelrc_files` (`crates/zel-options/src/lib.rs:218`). Non-existent files are silently ignored (`crates/zel-client/src/lib.rs:208`). If specified inside a `.bazelrc`, Zel allows it instead of erroring out. Moreover, in `.bazelrc`, passing `--bazelrc <path>` (space-separated) errors or fails to parse because `apply_flag` expects `--bazelrc=<path>`.
5. **`--block_for_lock` / `--noblock_for_lock` / `--block_for_lock=<timeout>`**
   - **[STUB]** Bazel: When the output base lock is held, waits if true (or up to timeout); if false, exits immediately with exit code 9 (`startup_options.cc:116-121`, `blaze.cc:136-141`).
   - Zel: Recognized in `NULLARY_STARTUP_FLAGS` (`crates/zel-options/src/lib.rs:39`) and stored in `startup.block_for_lock`. Sent in `RunRequest`, but `zel-server` implements no lock handling and completely ignores `req.block_for_lock`.
6. **`--client_debug` / `--noclient_debug`**
   - **[STUB]** Bazel: Emits verbose diagnostic messages from C++ client about sockets, process spawning, lock waiting, and option parsing (`startup_options.cc:127`, `blaze_util.cc`).
   - Zel: Recognized in `NULLARY_STARTUP_FLAGS` (`crates/zel-options/src/lib.rs:61`), but not stored in `StartupOptions` and never outputs any client debug logs.
7. **`--command_port=<port>`**
   - **[MISSING]** Bazel: Unary flag specifying the port on which the server should listen (`startup_options.cc:166`).
   - Zel: Unrecognized by client startup parser. (Only `crates/zel-server/src/main.rs` has `--port=`, but the CLI client rejects `--command_port`).
8. **`--connect_timeout_secs=<int>`**
   - **[STUB]** Bazel: Timeout in seconds for client to connect to running server socket (default 30s) (`startup_options.cc:167`).
   - Zel: Present in `UNARY_STARTUP_FLAGS` (`crates/zel-options/src/lib.rs:77`), but not stored in `StartupOptions`. `ping_server` uses a hardcoded 2-second timeout (`crates/zel-client/src/lib.rs:63`).
9. **`--delete_output_base_on_shutdown` / `--nodelete_output_base_on_shutdown`**
   - **[MISSING]** Bazel: Deletes the output base directory when the server shuts down (`startup_options.cc:147`).
   - Zel: Unrecognized.
10. **`--digest_function=<hash>`**
    - **[STUB]** Bazel: Configures digest function (SHA256, BLAKE3, etc.) for action inputs/outputs (`startup_options.cc:169`).
    - Zel: Present in `UNARY_STARTUP_FLAGS` (`crates/zel-options/src/lib.rs:79`), but discarded. Digest functions in `zel-util` and `zel-exec` are hardcoded.
11. **`--experimental_cgroup_parent=<path>`**
    - **[MISSING]** Bazel: Unary flag specifying cgroup parent for server process (`startup_options.cc:184`).
    - Zel: Unrecognized.
12. **`--experimental_remote_repo_contents_cache`**
    - **[MISSING]** Bazel: Nullary flag enabling remote repo contents cache (`startup_options.cc:159`).
    - Zel: Unrecognized.
13. **`--experimental_run_in_user_cgroup`**
    - **[MISSING]** Bazel: Nullary flag running server inside user cgroup (`startup_options.cc:163`).
    - Zel: Unrecognized.
14. **`--experimental_use_compact_object_headers`**
    - **[MISSING]** Bazel: Nullary flag passing JVM compact object headers flag (`startup_options.cc:161`).
    - Zel: Unrecognized.
15. **`--extra_classpath=<path>`**
    - **[MISSING]** Bazel: Unary flag adding jars to server classpath (`startup_options.cc:185`).
    - Zel: Unrecognized.
16. **`--failure_detail_out=<path>`**
    - **[STUB]** Bazel: Path where server writes `FailureDetail` proto if command fails (`startup_options.cc:182`).
    - Zel: Present in `UNARY_STARTUP_FLAGS` (`crates/zel-options/src/lib.rs:76`), but discarded; file is never written.
17. **`--fatal_event_bus_exceptions`**
    - **[MISSING]** Bazel: Nullary flag crashing server on EventBus handler exceptions (`startup_options.cc:133`).
    - Zel: Unrecognized.
18. **`--home_rc` / `--nohome_rc`**
    - **[STUB]** Bazel: Enables/disables reading `$HOME/.bazelrc` (`bazel_startup_options.cc:36`).
    - Zel: Parsed into `startup.home_rc` (`crates/zel-options/src/lib.rs:97`), but `execute_cli` in `crates/zel-client/src/lib.rs` never attempts to read `$HOME/.bazelrc` or `$HOME/.zelrc`.
19. **`--host_jvm_args=<arg>`**
    - **[STUB]** Bazel: Passes JVM startup arguments to Java server (`startup_options.cc:172`).
    - Zel: Parsed into `startup.host_jvm_args` (`crates/zel-options/src/lib.rs:98`), but ignored.
20. **`--host_jvm_debug`**
    - **[MISSING]** Bazel: Starts JVM with JDWP debugging flags (`startup_options.cc:135`).
    - Zel: Unrecognized.
21. **`--host_jvm_profile=<path>`**
    - **[MISSING]** Bazel: Passes JVM profiling agent flags (`startup_options.cc:173`).
    - Zel: Unrecognized.
22. **`--idle_server_tasks` / `--noidle_server_tasks`**
    - **[STUB]** Bazel: Runs periodic GC / cache cleanup while idle (`startup_options.cc:154`).
    - Zel: Present in `NULLARY_STARTUP_FLAGS` (`crates/zel-options/src/lib.rs:57`), ignored.
23. **`--ignore_all_rc_files` / `--noignore_all_rc_files`**
    - **[PARTIAL]** Bazel: Ignores system rc, workspace rc, home rc, and `BAZELRC` env var, but still processes explicit `--bazelrc` flags passed on the CLI (`startup_options.cc:150`, `option_processor.cc:280-325`).
    - Zel: Handled in `crates/zel-client/src/lib.rs:181`. If set, skips all rc files, including explicit `--bazelrc` passed on CLI (inconsistent with Bazel).
24. **`--install_base=<path>`**
    - **[PARTIAL]** Bazel: Directory containing extracted binary tools and server runtime (`startup_options.cc:174`).
    - Zel: Parsed into `startup.install_base` (`crates/zel-options/src/lib.rs:88`). However, Zel does not extract any embedded files into this directory.
25. **`--invocation_policy=<policy>`**
    - **[MISSING]** Bazel: Startup option passing an InvocationPolicy proto (string or base64) to modify command arguments (`startup_options.cc:174`).
    - Zel: Unrecognized by startup parser. (The field exists in `RunRequest` in `zel-proto`, but cannot be set via startup flag).
26. **`--io_nice_level=<int>`**
    - **[MISSING]** Bazel: Sets Linux IO scheduling priority (`startup_options.cc:175`).
    - Zel: Unrecognized.
27. **`--local_startup_timeout_secs=<int>`**
    - **[STUB]** Bazel: Maximum seconds (default 120s) client waits for newly spawned server to become healthy (`startup_options.cc:168`).
    - Zel: Present in `UNARY_STARTUP_FLAGS` (`crates/zel-options/src/lib.rs:78`), but ignored. Zel client uses a hardcoded 20-iteration sleep loop (total 100ms) in `crates/zel-client/src/lib.rs:321`.
28. **`--macos_qos_class=<class>`**
    - **[MISSING]** Bazel: Sets macOS pthread QoS class (`startup_options.cc:176`).
    - Zel: Unrecognized.
29. **`--max_idle_secs=<int>`**
    - **[STUB]** Bazel: Shuts down server if idle for this duration (default 10800s = 3h) (`startup_options.cc:177`).
    - Zel: Parsed and validated into `startup.max_idle_secs` (`crates/zel-options/src/lib.rs:92`), but never passed to `BlazeRuntime` or `serve_command_server`. The server listener loop blocks indefinitely on `listener.incoming()` with no timeout.
30. **`--output_base=<path>`**
    - **[PARTIAL]** Bazel: Root for all output trees, execroot, server state, logs (`startup_options.cc:178`).
    - Zel: Supported and parsed. Divergences: Zel lacks server lock files (`output_base/lock`), does not support the Windows 8-char base-32 directory naming scheme, and does not create `command.log`.
31. **`--output_user_root=<path>`**
    - **[DIFFERENT]** Bazel: Defaults to `$HOME/Library/Caches/bazel/_bazel_$USER` (macOS), `$HOME/.cache/bazel/_bazel_$USER` (Linux), `$USERPROFILE/_bazel_$USERNAME` (Windows), or `$TEST_TMPDIR/_bazel_$USER` (`startup_options.cc:753-764`, `blaze_util_darwin.cc:112-137`).
    - Zel: Hardcodes `/var/tmp/_bazel_{user}` on macOS (`crates/zel-options/src/lib.rs:131`). Completely ignores `XDG_CACHE_HOME`, `TEST_TMPDIR`, `USERPROFILE`, and `USERNAME`.
32. **`--preemptible` / `--nopreemptible`**
    - **[STUB]** Bazel: Marks the command preemptible by subsequent client invocations (`startup_options.cc:129`).
    - Zel: Present in `NULLARY_STARTUP_FLAGS` (`crates/zel-options/src/lib.rs:63`), but ignored. `run_req.preemptible` is hardcoded to `false` in `crates/zel-client/src/lib.rs:273`.
33. **`--quiet` / `--noquiet`**
    - **[STUB]** Bazel: Suppresses console output except errors (`startup_options.cc:125`).
    - Zel: Parsed and forwarded in `RunRequest`, but ignored by `zel-server` command handlers.
34. **`--server_javabase=<path>`**
    - **[STUB]** Bazel: Path to JDK used to run server JVM (`startup_options.cc:171`).
    - Zel: Present in `UNARY_STARTUP_FLAGS` (`crates/zel-options/src/lib.rs:75`), ignored.
35. **`--server_jvm_out=<path>`**
    - **[MISSING]** Bazel: Path for redirecting server JVM log output (`startup_options.cc:181`).
    - Zel: Unrecognized.
36. **`--shutdown_on_low_sys_mem` / `--noshutdown_on_low_sys_mem`**
    - **[STUB]** Bazel: Shuts down server if available RAM is critically low (`startup_options.cc:144`).
    - Zel: Present in `NULLARY_STARTUP_FLAGS` (with duplicate typo `--Shutdown_on_low_sys_mem`), ignored.
37. **`--system_rc` / `--nosystem_rc`**
    - **[STUB]** Bazel: Loads `/etc/bazel.bazelrc` (Unix) or `%ProgramData%\bazel.bazelrc` (Windows) (`bazel_startup_options.cc:37`).
    - Zel: Present in `NULLARY_STARTUP_FLAGS` (with duplicate typo `--System_rc`) and parsed into `startup.system_rc`, but Zel NEVER reads `/etc/bazel.bazelrc` or any system rc file.
38. **`--unix_digest_hash_attribute_name=<name>`**
    - **[STUB]** Bazel: Extended filesystem attribute used to cache digests (`startup_options.cc:170`).
    - Zel: Present in `UNARY_STARTUP_FLAGS` (`crates/zel-options/src/lib.rs:80`), ignored.
39. **`--unlimit_coredumps`**
    - **[MISSING]** Bazel: Removes core dump size limits (`startup_options.cc:152`).
    - Zel: Unrecognized.
40. **`--windows_enable_symlinks`**
    - **[MISSING]** Bazel: Enables Windows symlink support in execroot (`startup_options.cc:156`).
    - Zel: Unrecognized.
41. **`--workspace_rc` / `--noworkspace_rc`**
    - **[PARTIAL]** Bazel: Loads `%workspace%/.bazelrc` (`bazel_startup_options.cc:38`).
    - Zel: Parsed into `startup.workspace_rc`. Reads both `.zelrc` and `.bazelrc`, but missing rc file validation and deduplication.
42. **`--write_command_log` / `--nowrite_command_log`**
    - **[STUB]** Bazel: Writes command output to `$OUTPUT_BASE/command.log` (default true) (`startup_options.cc:154`).
    - Zel: Present in `NULLARY_STARTUP_FLAGS` (`crates/zel-options/src/lib.rs:59`), but ignored. `command.log` is never created.

---

### RC File Discovery Order, Semantics, and Syntax

In Bazel (`src/main/cpp/option_processor.cc:270-363`), RC files are resolved and read in the following strict order:
1. **System RC:** `/etc/bazel.bazelrc` (Unix) or `%ProgramData%\bazel.bazelrc` (Windows) (unless `--nosystem_rc`).
2. **Workspace RC:** `%workspace%/.bazelrc` (unless `--noworkspace_rc`).
3. **User RC:** `$HOME/.bazelrc` (unless `--nohome_rc`).
4. **Environment RC:** Files listed in `$BAZELRC` (comma-separated).
5. **Command-line RC:** `--bazelrc=<path>` flags passed on the CLI.
Before parsing, Bazel deduplicates these paths (`internal::DedupeBlazercPaths`), canonicalizes symlinks, and validates that non-default paths exist.

**Parity Gaps in Zel (`crates/zel-client/src/lib.rs:178-228`, `crates/zel-options/src/lib.rs:277-444`):**
- **[MISSING] System RC:** Zel does not read `/etc/bazel.bazelrc` or any system RC file, even though `--system_rc` is true by default.
- **[MISSING] User Home RC:** Zel does not read `$HOME/.bazelrc` or `$HOME/.zelrc`, even though `--home_rc` is true by default.
- **[MISSING] `BAZELRC` Environment Variable:** Zel never reads the `BAZELRC` environment variable.
- **[DIFFERENT] Non-existent RC File Error:** Bazel exits with error code 2 if a specified `--bazelrc` does not exist. Zel silently ignores missing files with `if let Ok(bytes) = fs.read_file(&full_rc)`.
- **[DIFFERENT] Non-standard `.zelrc`:** Zel checks for `.zelrc` before `.bazelrc` in the workspace directory. Real Bazel only knows `.bazelrc`.
- **[MISSING] `try-import-if-bazel-version`:** Real Bazel supports version-gated conditional imports (`try-import-if-bazel-version <op> <version> <path>`, `rc_file.cc:166-207`). Zel does not parse this directive; it treats the entire line as a command section named `"try-import-if-bazel-version"`.
- **[DIFFERENT] Startup Options in RC Files:** In Bazel, startup options in rc files are parsed in phase 1 (`OptionProcessor::ParseOptions`) and can affect subsequent RC file loading (e.g., `startup --noworkspace_rc` or `startup --bazelrc=...`). In Zel, workspace rc is already loaded before the `startup` section is applied (`crates/zel-client/src/lib.rs:223-227`).
- **[DIFFERENT] `--bazelrc` inside RC Files:** Real Bazel forbids `--bazelrc` inside rc files and exits with `BAD_ARGV` (`bazel_startup_options.cc:55`). Zel allows it.
- **[DIFFERENT] Space-separated unary flags in `.bazelrc`:** In `.bazelrc`, `startup --output_base /foo` fails in Zel because `apply_flag` expects `--output_base=/foo` and silently ignores the error (`let _ = startup.apply_flag(...)`).

---

### Workspace Root Discovery and Boundary Files

In Bazel (`src/main/cpp/workspace_layout.cc:34-56`):
- Boundary files: `"MODULE.bazel"`, `"REPO.bazel"`, `"WORKSPACE.bazel"`, `"WORKSPACE"`.
- A directory with one of these names is explicitly rejected (`!blaze_util::IsDirectory(boundaryFilePath)`).
- If no boundary file is found when walking up the directory tree to the filesystem root, `GetWorkspace(cwd)` returns `""` (empty string).
- In `blaze.cc`, commands that require a workspace (like `build`, `test`, `run`) exit with error: `"The 'build' command is only supported from within a workspace..."`.

**Parity Gaps in Zel (`crates/zel-client/src/lib.rs:35-56`):**
- **[DIFFERENT] Fallback on Root:** If no workspace boundary is found, `find_workspace_root` returns `cwd.clone()` (`crates/zel-client/src/lib.rs:55`). It never returns an empty workspace indicator. Consequently, running `zel build //...` in `/tmp` treats `/tmp` as the workspace instead of emitting Bazel's workspace requirement error.
- **[DIFFERENT] Directory Boundary Check:** Zel uses `fs.exists(marker, true)` which does not verify that the path is a file; a directory named `WORKSPACE` will be mistakenly recognized as a workspace boundary.
- **[DIFFERENT] Non-standard Boundary Names:** Zel looks for `"MODULE.zel"` and `"WORKSPACE.zel"` (`crates/zel-client/src/lib.rs:36-40`), which do not exist in Bazel.

---

### Output Base, Output User Root, and Install Base Computation

In Bazel (`src/main/cpp/startup_options.cc:753-784`, `blaze_util_darwin.cc:112-137`, `blaze_util_linux.cc:55-75`, `blaze_util_windows.cc:384-405, 949-979`):
1. **`output_user_root` Defaults:**
   - Linux: `$XDG_CACHE_HOME/bazel/_bazel_$USER` (fallback: `$HOME/.cache/bazel/_bazel_$USER`).
   - macOS: `$XDG_CACHE_HOME/bazel/_bazel_$USER` (fallback: `$HOME/Library/Caches/bazel/_bazel_$USER`).
   - Windows: `$XDG_CACHE_HOME/bazel/_bazel_$USERNAME` (fallback: `%USERPROFILE%/_bazel_%USERNAME%` or `%HOME%`).
   - Test mode: If `$TEST_TMPDIR` is set, Bazel uses `$TEST_TMPDIR/_bazel_$USER` on all platforms.
2. **`output_base` Computation:**
   - Linux / macOS: `$OUTPUT_USER_ROOT/<md5(workspace_path)>` (32 hex characters).
   - Windows: `$OUTPUT_USER_ROOT/<base32(md5(workspace_path))[0..8]>` (custom 8-character base-32 encoding using alphabet `abcdefghijklmnopqrstuvwxyz234567` to avoid path length limit issues).
3. **`install_base` Extraction:**
   - Computed as `$OUTPUT_USER_ROOT/install/<install_md5>`.
   - Client takes shared lock on `$INSTALL_BASE.lock` and unpacks embedded binaries and tools into `install_base`.

**Parity Gaps in Zel (`crates/zel-options/src/lib.rs:126-171`):**
- **[DIFFERENT] macOS Root:** Zel hardcodes `/var/tmp/_bazel_{user}` (`crates/zel-options/src/lib.rs:131`). Real Bazel uses `$HOME/Library/Caches/bazel/_bazel_$USER`.
- **[MISSING] `XDG_CACHE_HOME`:** Completely ignored on all platforms.
- **[MISSING] `TEST_TMPDIR`:** Zel does not check `TEST_TMPDIR` to make output roots hermetic during tests.
- **[DIFFERENT] Windows Output Base Hash:** Zel computes a 32-character hex MD5 on Windows (`crates/zel-options/src/lib.rs:154`); Bazel uses an 8-character base-32 hash.
- **[STUB] Install Base Extraction:** `install_md5` is hardcoded to `"zel-0.0.1-install-key"` (`crates/zel-client/src/lib.rs:258`), and nothing is ever extracted to the directory.

---

### Server Process Lifecycle, Lock Files, Daemon Spawning, and Idle Timeout

In Bazel (`src/main/cpp/blaze.cc:108-144, 1684-1730`, `server_process_info.cc`):
- **Server Lock File:** Client acquires an exclusive file lock (`fcntl` / `flock`) on `$OUTPUT_BASE/lock` (`blaze.cc:136-141`). If locked and `--noblock_for_lock` is set, it fails fast with exit code 9 (`LOCK_HELD_NOBLOCK_FOR_LOCK`).
- **Persistent Daemon:** If no server is running, the client starts the server binary as a detached daemon process in the background, waits for `$OUTPUT_BASE/server/server_info.rawproto` to appear and accepts connections, and keeps it running across commands.
- **Option Change Detection & Server Restart:** The client records server startup options in `$OUTPUT_BASE/server/cmdline`. If a client runs with different startup options, Bazel logs `"Restarting server because startup options have changed:"`, kills the old server, and starts a new one (`blaze.cc:1387-1430`).
- **Idle Timeout:** Server automatically shuts down after `--max_idle_secs` of inactivity.

**Parity Gaps in Zel (`crates/zel-client/src/lib.rs`, `crates/zel-server/src/lib.rs`):**
- **[MISSING] Server Locking:** No lock file (`$OUTPUT_BASE/lock`) is created or checked. Multiple concurrent invocations on the same output base can run simultaneously and race on files.
- **[MISSING] Persistent Daemon Spawning:** In `crates/zel-client/src/lib.rs:296-339`, if an existing server is not running, the client spins up a TCP listener, spawns a single-threaded server in a background *thread* within the client process, executes the single command, sets `shutdown_requested = true`, joins the thread, and terminates! Function `spawn_detached_daemon` (`lib.rs:392`) is never called anywhere in the codebase. As a result, no Skyframe graph or memoized state survives across CLI invocations.
- **[MISSING] Server Restart on Option Change:** Zel has no mechanism to compare startup options with a running server. It will send commands to any existing server regardless of mismatched flags.
- **[STUB] Idle Timeout:** In `crates/zel-server/src/lib.rs:1491-1500`, `listener.incoming()` blocks indefinitely. There is no idle timeout thread or timer; `--max_idle_secs` has no effect.
- **[MISSING] Status Files:** Bazel writes `server.pid.txt`, `server.pid.txt.lock`, `server.starttime`, and `cmdline`. Zel writes `server.pid.txt`, `command_port`, `request_cookie`, `response_cookie`, and `server_info.rawproto`, but omits `server.starttime`, `cmdline`, and the pid lock file.

---

### CommandServer gRPC Protocol vs Fake TCP Framing

In Bazel (`src/main/protobuf/command_server.proto`, `src/main/cpp/blaze.cc:2109-2250`, `GrpcServerImpl.java`):
- Communication uses genuine HTTP/2-based gRPC with HTTP/2 frames (HEADERS, DATA, SETTINGS), standard `:path` routing (`/command_server.CommandServer/Run`, `/command_server.CommandServer/Ping`), and gRPC compression/trailers.

**Parity Gaps in Zel (`crates/zel-proto/src/lib.rs:1058-1083`, `crates/zel-client/src/lib.rs`, `crates/zel-server/src/lib.rs:1506-1587`):**
- **[DIFFERENT] Custom Wire Framing:** Zel does NOT use gRPC or HTTP/2. It uses a bespoke raw TCP frame format:
  `[0u8 (compression byte)][4-byte big-endian length][1-byte RpcMethod enum][protobuf payload]`.
- **Incompatibility:** A real Bazel client connecting to `zel-server` sends an HTTP/2 connection preface (`PRI * HTTP/2\r\n\r\nSM\r\n\r\n`), which `zel-server` fails to parse as a 5-byte length frame and drops. A `zel-client` connecting to real Bazel gets an immediate HTTP/2 RST_STREAM / connection reset.
- **[MISSING] HTTP/2 Multiplexing:** Zel handles connections sequentially on a single thread (`for stream in listener.incoming()`), processing one connection at a time.

---

### Signal Handling, Cancellation, and Ctrl-C

In Bazel (`src/main/cpp/blaze.cc:980-1050, 2200-2220`):
- The C++ client installs signal handlers for `SIGINT` (Ctrl-C) and `SIGTERM`.
- When `SIGINT` is received, the client sends a `CancelRequest` RPC over gRPC to the server with the active `command_id` to cancel the command in flight.
- If Ctrl-C is pressed again, the client forcibly terminates the server process or aborts immediately.

**Parity Gaps in Zel (`crates/zel-client/src/lib.rs`):**
- **[MISSING] Signal Handlers:** `zel-client` installs NO signal handlers for `SIGINT`, `SIGTERM`, or `SIGWINCH`.
- **[MISSING] Cancellation:** If the user presses Ctrl-C, the OS terminates the `zel` client process immediately. No `CancelRequest` is sent. (A stub `RpcMethod::Cancel` exists in `crates/zel-server/src/lib.rs:1538`, but `zel-client` never calls it).
- **[MISSING] Terminal Resizing (`SIGWINCH`):** Bazel client catches `SIGWINCH` and calls `UpdateTerminalSize`. Zel client never sends `UpdateTerminalSize`.

---

### Stdout, Stderr, command.log, and java.log Handling

In Bazel (`src/main/cpp/blaze.cc:2160-2240`, `GrpcServerImpl.java`):
- `RunResponse` messages are streamed back in real-time as actions execute, tests run, and progress updates occur.
- All stdout and stderr output is simultaneously appended to `$OUTPUT_BASE/command.log`.
- Server JVM output is written to `$OUTPUT_BASE/java.log` (or `--server_jvm_out`).

**Parity Gaps in Zel (`crates/zel-server/src/lib.rs:1572-1583`, `crates/zel-client/src/lib.rs:119-126`):**
- **[DIFFERENT] No Real-time Output Streaming:** In `crates/zel-server/src/lib.rs:1581`, the server calls `runtime.dispatch_command(&req, ...)`, which runs the entire command synchronously to completion, accumulates all stdout and stderr in memory buffers (`Vec<u8>`), and then writes a single response frame at the very end. Progress is not streamed to the client while actions execute.
- **[MISSING] `command.log` Never Written:** The path method `dirs.command_log()` exists (`crates/zel-server/src/lib.rs:135`), and `bazel info command_log` prints the path, but neither `zel-client` nor `zel-server` ever opens or writes to `command.log`.
- **[MISSING] `java.log` / `server.log` Never Written:** The path method `dirs.server_log()` exists, and `bazel info server_log` prints it, but the file is never created.

---

### Exit Codes and Failure Details

In Bazel (`src/main/cpp/util/exit_code.h`, `ExitCode.java`):
- Standard numeric exit codes include:
  - `0`: SUCCESS
  - `1`: BUILD_FAILURE
  - `2`: COMMAND_LINE_ERROR / BAD_ARGV
  - `3`: TESTS_FAILED
  - `4`: NO_TESTS_FOUND
  - `6`: RUN_FAILURE
  - `7`: ANALYSIS_FAILURE
  - `8`: INTERRUPTED
  - `9`: LOCK_HELD_NOBLOCK_FOR_LOCK
  - `32`: REMOTE_ENVIRONMENTAL_ERROR
  - `33`: OOM_ERROR
  - `34`: REMOTE_ERROR
  - `36`: LOCAL_ENVIRONMENTAL_ERROR
  - `37`: BLAZE_INTERNAL_ERROR
  - `38`: PUBLISH_ERROR
  - `39`: REMOTE_CACHE_EVICTED
  - `45`: PERSISTENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR
  - `48`: EXTERNAL_DEPS_ERROR
- When commands fail, structured `FailureDetail` proto is written to `$OUTPUT_BASE/failure_detail.rawproto`.

**Parity Gaps in Zel (`crates/zel-util/src/lib.rs:21-64`, `crates/zel-client/src/lib.rs`):**
- **[PARTIAL] ExitCode Constants:** `crates/zel-util/src/lib.rs` correctly defines the numeric values and names of Bazel's exit codes.
- **[MISSING] Lock Failure Exit Code:** Exit code 9 (`LOCK_HELD_NOBLOCK_FOR_LOCK`) is never returned because locking is not implemented.
- **[MISSING] `failure_detail.rawproto` File:** Even though `FailureDetail` protobuf definitions exist in `zel-proto`, `zel-client` never writes `failure_detail.rawproto` upon command failure.

---

### Command Execution (`ExecRequest` / `run` command)

In Bazel (`src/main/cpp/blaze.cc:1720-1740`, `blaze_util_posix.cc`):
- When running `bazel run //package:target`, the server builds the executable and returns an `ExecRequest` proto inside the final `RunResponse` (`argv`, `working_directory`, `environment_variable`, `should_exec`).
- The Bazel C++ client unpacks `ExecRequest` and invokes POSIX `execve` (or Windows `CreateProcess`) to replace the client process with the built binary, preserving stdin/stdout/stderr and PID.

**Parity Gaps in Zel (`crates/zel-server/src/lib.rs:1113`, `crates/zel-client/src/lib.rs`):**
- **[STUB] `ExecRequest` Ignored by Client:** In `crates/zel-server/src/lib.rs:1113`, `cmd_run` correctly constructs an `ExecRequest`. However, `crates/zel-client/src/lib.rs` completely ignores `resp.exec_request` in all execution paths (`lines 291, 337, 353`) and immediately exits with `resp.exit_code` (0).
- **Result:** Running `zel run //foo:bar` builds the target, but the target binary is **never actually executed**!

---

### Miscellaneous Client & Server Behaviors

1. **`--version` & `version` command:**
   - **[DIFFERENT]** In Bazel, `bazel --version` fast-path prints `bazel <version>`. Full `bazel version` prints `Build label`, `Build target`, `Build time`, and `Build timestamp`. In Zel, `crates/zel-server/src/lib.rs:231, 330` hardcodes `"Build target: @@//crates:zel"`, `"Build time: Wed Nov 4 2026"`, and `"Build timestamp: 1793833200"`.
2. **`--` Argument Handling Before Command:**
   - **[DIFFERENT]** In Bazel, passing `--` before the command (e.g. `bazel -- build //...`) terminates startup option processing. In Zel (`crates/zel-options/src/lib.rs:244`), `--` is treated as a startup flag, fails `apply_flag`, and exits with `ERROR: Unknown startup option: '--'`.
3. **`clean --expunge` and `clean --expunge_async`:**
   - **[STUB]** In Bazel, `--expunge` removes the entire `$OUTPUT_BASE` directory from disk; `--expunge_async` moves it and deletes it asynchronously in a background process. In Zel (`crates/zel-server/src/lib.rs:240`), `cmd_clean` only deletes `execroot` and `action_cache.bin`. If `--expunge` or `--expunge_async` is passed, it only prints `"INFO: Expunging output base and shutting down server..."` and requests shutdown—it never actually deletes `$OUTPUT_BASE`.
4. **`help` Command Topics:**
   - **[STUB]** `bazel help flags-as-proto` outputs a serialized options protobuf in Bazel. In Zel (`crates/zel-server/src/lib.rs:375`), it outputs an empty string `""`.
   - **[STUB]** `bazel help everything-as-html` in Zel outputs a hardcoded stub `"<html><body><h1>Bazel Help</h1></body></html>"`.
   - `bazel help startup_options` in Zel lists only 7 options, omitting 35 options supported by Bazel.
5. **Divergence with `zel-bazelisk`:**
   - **[DIFFERENT]** The copy in `~/Development/zel/zel-bazelisk` includes an extra 1,400-line module `bazelisk.rs` implementing version resolution (`.bazelversion`, `USE_BAZEL_VERSION`), wrapper delegation (`tools/bazel`), and release binary downloads. The primary codebase (`crates/zel-client`) does not include this functionality.

---


## Command Inventory and Non-Build Command Behavior

Zel aims to be a 1:1 drop-in replacement for Bazel written in Rust. However, an in-depth audit of command dispatching in `crates/zel-server/src/lib.rs`, the client CLI runner in `crates/zel-client/src/lib.rs`, and startup/option parsing in `crates/zel-options/src/lib.rs` against Bazel's implementation (`com/google/devtools/build/lib/runtime/commands/*.java`, `BlazeCommandDispatcher.java`, `blaze.cc`) reveals that Zel's command parity is currently at a **Low to Moderate** level. While Zel implements basic dispatcher routing for standard commands, non-build commands (`config`, `dump`, `license`, `print_action`, `coverage`, `fetch`, `vendor`) are largely empty stubs (`INFO: Running '<command>'...`) or misrouted to the build engine. Critical flags are parsed and silently ignored or missing altogether (e.g., `clean --async`, `canonicalize-flags --invocation_policy`, `shutdown --iff_heap_size_greater_than`, `info --show_make_env`). Crucially, `run` generates an `ExecRequest` on the server that the client binary completely ignores without executing, `clean --expunge` does not actually delete the output base, and workspace boundary preconditions (`mustRunInWorkspace`) are never enforced. In addition, two commands deleted from Bazel (`analyze-profile` and `sync`) are retained in Zel, while one active Bazel command (`mobile-install`) is completely missing.

---

### Command Inventory Comparison Table

| Command | Bazel Status & Implementation | Zel Status & Implementation | Parity Assessment |
| :--- | :--- | :--- | :--- |
| `analyze-profile` | **Deleted** in Bazel ([commit `0fd3ae4f66`](https://github.com/bazelbuild/bazel/commit/0fd3ae4f66)); replaced by JSON trace profiler tooling. | Dispatched to stub in `crates/zel-server/src/lib.rs:254`; listed in `help` and `completion`. | **[DIFFERENT]** / **[STUB]** Dead command retained in Zel as a 1-line stub. |
| `aquery` | Active in Bazel (`AqueryCommand.java:55`). Full action graph query engine with text/proto/json output. | Dispatched in `crates/zel-server/src/lib.rs:248` to `cmd_query`. Supports basic text formatting. | **[PARTIAL]** Lacks proto/json output formats and aquery filter flags. |
| `build` | Active in Bazel (`BuildCommand.java:46`). Core build engine. | Dispatched in `crates/zel-server/src/lib.rs:250` to `cmd_build_or_test`. | **[PARTIAL]** Supported for simple workflows; differs on workspace requirement and options. |
| `canonicalize-flags` | Active in Bazel (`CanonicalizeCommand.java:59`). Canonicalizes native and Starlark options; supports policy canonicalization. | Dispatched in `crates/zel-server/src/lib.rs:234` to `cmd_canonicalize_flags`. | **[PARTIAL]** Ignores `--for_command`, missing invocation policy and Starlark flags. |
| `clean` | Active in Bazel (`CleanCommand.java:56`). Deletes outputs, convenience symlinks, and optionally expunges output base sync/async. | Dispatched in `crates/zel-server/src/lib.rs:240` to `cmd_clean`. | **[PARTIAL]** / **[STUB]** Does not delete output base on `--expunge`; `--async` missing; symlinks not cleaned. |
| `config` | Active in Bazel (`ConfigCommand.java:74`). Diffs and displays configurations; marked `hidden = true`. | Dispatched in `crates/zel-server/src/lib.rs:254` to stub: prints `INFO: Running 'config'...`. | **[STUB]** Completely unimplemented; not hidden in `help`. |
| `coverage` | Active in Bazel (`CoverageCommand.java:105`). Collects code coverage and generates lcov reports. | Dispatched in `crates/zel-server/src/lib.rs:250` to `cmd_build_or_test`. | **[STUB]** / **[DIFFERENT]** Treated as `build`; test binaries are never run and no coverage data is collected. |
| `cquery` | Active in Bazel (`CqueryCommand.java:61`). Post-analysis configured target query. | Dispatched in `crates/zel-server/src/lib.rs:248` to `cmd_query`. | **[PARTIAL]** Minimal output formatter; lacks transition queries and proto formatters. |
| `dump` | Active in Bazel (`DumpCommand.java:138`). Dumps packages, action cache, Skyframe, rules, memory. | Dispatched in `crates/zel-server/src/lib.rs:254` to stub: prints `INFO: Running 'dump'...`. | **[STUB]** Completely unimplemented. |
| `fetch` | Active in Bazel (`FetchCommand.java:59`). Fetches external repository dependencies (`--all`, `--repo`). | Dispatched in `crates/zel-server/src/lib.rs:250` to `cmd_build_or_test`. | **[STUB]** / **[DIFFERENT]** Does not fetch repositories; runs target build or reports 0 targets. |
| `help` | Active in Bazel (`HelpCommand.java:83`). Help index, command help with options, and help topics. | Dispatched in `crates/zel-server/src/lib.rs:232` to `cmd_help`. | **[PARTIAL]** / **[STUB]** `help <command>` prints global summary; topics are hardcoded stubs or blank. |
| `info` | Active in Bazel (`InfoCommand.java:95`). Displays 33 keys, make env, respects hidden keys & workspace rules. | Dispatched in `crates/zel-server/src/lib.rs:233` to `cmd_info`. | **[PARTIAL]** Missing 5 keys; hardcoded/fake values; displays hidden keys; missing `--show_make_env`. |
| `license` | Active in Bazel (`LicenseCommand.java:36`). Displays license of Bazel and bundled JVM runtimes. | Dispatched in `crates/zel-server/src/lib.rs:254` to stub: prints `INFO: Running 'license'...`. | **[STUB]** Completely unimplemented. |
| `mobile-install` | Active in Bazel (`MobileInstallCommand.java:69`). Installs apps on mobile devices. | **Not implemented** anywhere in Zel. | **[MISSING]** Calling `zel mobile-install` produces unknown command error. |
| `mod` | Active in Bazel (`ModCommand.java:108`). Bzlmod graph inspection (`graph`, `deps`, `show_repo`, etc.). | Dispatched in `crates/zel-server/src/lib.rs:246` to `cmd_mod`. | **[PARTIAL]** Implements graph, deps, explain, show_repo; stubs `show_extension` and `tidy`. |
| `print_action` | Active in Bazel (`PrintActionCommand.java:71`). Prints compiler/action commands for source files. | Dispatched in `crates/zel-server/src/lib.rs:254` to stub: prints `INFO: Running 'print_action'...`. | **[STUB]** Completely unimplemented. |
| `query` | Active in Bazel (`QueryCommand.java:58`). Target graph query engine. | Dispatched in `crates/zel-server/src/lib.rs:248` to `cmd_query`. | **[PARTIAL]** Basic query expressions supported; falls back to workspace scanning on failure. |
| `run` | Active in Bazel (`RunCommand.java:109`). Builds target executable and runs it via `ExecRequest`. | Server builds target and builds `ExecRequest` (`lib.rs:1072`), but client (`zel-client/lib.rs`) never executes it. | **[STUB]** / **[DIFFERENT]** The executable is built, but the client process exits without running it! |
| `shutdown` | Active in Bazel (`ShutdownCommand.java:31`). Shuts down server (`--iff_heap_size_greater_than`). | Dispatched in `crates/zel-server/src/lib.rs:235`. Signals shutdown immediately. | **[PARTIAL]** Ignores all options; unconditionally kills server daemon. |
| `sync` | **Deleted** in Bazel ([commit `8396531ec0`](https://github.com/bazelbuild/bazel/commit/8396531ec0)); replaced by `fetch --all`. | Dispatched in `crates/zel-server/src/lib.rs:250` to `cmd_build_or_test`. Listed in `help` and `completion`. | **[DIFFERENT]** Dead command retained in Zel. |
| `test` | Active in Bazel (`TestCommand.java:68`). Runs test actions, generates XML/log outputs, sets exit codes. | Dispatched in `crates/zel-server/src/lib.rs:250` to `cmd_build_or_test`. | **[PARTIAL]** Test execution present but lacks test filtering, sharding, XML aggregation flags. |
| `vendor` | Active in Bazel (`VendorCommand.java:99`). Vendors external repos into `--vendor_dir`. | Dispatched in `crates/zel-server/src/lib.rs:250` to `cmd_build_or_test`. | **[STUB]** / **[DIFFERENT]** Does not vendor external dependencies. |
| `version` | Active in Bazel (`VersionCommand.java:39`). Prints version summary (`--gnu_format`). | Dispatched in `crates/zel-server/src/lib.rs:231` to `cmd_version`. | **[PARTIAL]** Hardcoded timestamp, build target, and build time; key name mismatch (`Build timestamp`). |


### Command Dispatching, Unknown Commands, Abbreviations, and Workspace Enforcement

* **[MISSING] Workspace Requirement Enforcement (`mustRunInWorkspace`)**:
  * **Bazel Behavior**: In Bazel, the C++ client explicitly guards all commands that require a workspace via `src/main/cpp/blaze.cc:809-821` and `CanRunOutsideWorkspace(command)` (`blaze.cc:507`). In `src/main/java/com/google/devtools/build/lib/runtime/Command.java:171`, the `@Command` annotation defines `boolean mustRunInWorkspace() default true;`. Only 7 commands set `mustRunInWorkspace = false`: `canonicalize-flags`, `dump`, `help`, `info`, `license`, `shutdown`, and `version`. All other 14 commands require a valid workspace. If invoked outside a workspace (no `MODULE.bazel` or boundary file found), Bazel immediately terminates in the client before launching the server:
    ```text
    bazel 'build' must be invoked from within a workspace (below a directory having a MODULE.bazel file).
    See documentation at https://bazel.build/concepts/build-ref#workspace
    ```
    This exits with `blaze_exit_code::BAD_ARGV` (exit code `2`).
  * **Zel Behavior**: Zel has **no workspace boundary checks**. In `crates/zel-client/src/lib.rs:45-56`, `find_workspace_root` walks up parent directories looking for boundary markers, but if none are found, it falls back to `cwd.clone()`. The command proceeds to execute with `cwd` treated as the workspace root. For example, executing `zel build //foo:bar` outside of a workspace attempts package discovery in the current directory and exits with code `7` (`ANALYSIS_FAILURE`, `ERROR: no such package 'foo': BUILD file not found`), completely bypassing the expected exit code `2` and client-side error check.

* **[DIFFERENT] Unknown Command Error Message & Formatting**:
  * **Bazel Behavior**: `BlazeCommandDispatcher.java:220-227` formats unknown commands without an `ERROR:` prefix:
    ```text
    Command 'foo' not found. Try 'bazel help'.
    ```
    It uses `runtime.getProductName()` dynamically and returns `DetailedExitCode` with `FailureDetails.Command.Code.COMMAND_NOT_FOUND` (exit code `2`).
  * **Zel Behavior**: `crates/zel-server/src/lib.rs:258-266` formats the message with an `ERROR: ` prefix and hardcodes `'bazel help'`:
    ```text
    ERROR: Command 'foo' not found. Try 'bazel help'.
    ```
    Even when invoked as `zel`, it hardcodes `bazel help` instead of using `version_product_prefix()`. The numeric exit code matches (`2`), and `CommandCode::CommandNotFound` failure detail is attached.

* **[DIFFERENT] Help Flag Startup Alias (`-help`)**:
  * **Bazel Behavior**: In `BlazeCommandDispatcher.java:114-116`, `ALL_HELP_OPTIONS` includes `{"--help", "-help", "-h"}`. Any of these is intercepted and treated as the `help` command.
  * **Zel Behavior**: In `crates/zel-options/src/lib.rs:249-251`, `split_command_line` checks `arg == "--help" || arg == "-h"`, but **omits `-help`**. Passing `zel -help` triggers an option parsing error: `ERROR: Unknown startup option: '-help'. For more info, run 'bazel help startup_options'.` with exit code `2`.

* **[FULL] Command Abbreviations Not Supported**:
  * **Parity**: Both Bazel (`BlazeRuntime.java:333`, `CommandNameCacheImpl.java:47`) and Zel (`crates/zel-server/src/lib.rs:230`) use exact string key matching on command names. Neither supports command prefix abbreviations (e.g., `bazel b` or `zel b` fails as an unrecognized command).


### `info` Command Deep-Dive

Bazel defines a total of **33 distinct `InfoItem` keys** (30 hardwired in `InfoCommand.java:386-425` and 3 contributed by modules: `CommandLogInfoItem`, `RepositoryCacheInfoItem`, and `PeakMemInfoItem`), plus dynamic keys injected by `--show_make_env`.

#### Complete Key Inventory & Comparison

| Info Key | Bazel Source & Properties | Zel Implementation (`lib.rs:515`) | Status & Divergence |
| :--- | :--- | :--- | :--- |
| `bazel-bin` | `BlazeBinInfoItem.java:27` (Workspace: Yes, Hidden: No) | `self.dirs.bazel_bin(...)` | **[FULL]** Evaluated against host CPU & config. |
| `bazel-genfiles` | `BlazeGenfilesInfoItem.java:27` (Workspace: Yes, Hidden: No) | `self.dirs.bazel_genfiles(...)` | **[FULL]** Matches `bazel-bin` directory path. |
| `bazel-testlogs` | `BlazeTestlogsInfoItem.java:27` (Workspace: Yes, Hidden: No) | `self.dirs.bazel_testlogs(...)` | **[FULL]** Evaluated against host CPU & config. |
| `build-language` | `BuildLanguageInfoItem.java:62` (Workspace: Yes, Hidden: Yes) | *Not in `collect_info_map`* | **[MISSING]** Bazel dumps build language proto; absent in Zel. |
| `character-encoding` | `CharacterEncodingInfoItem.java:26` (Workspace: No, Hidden: No) | `"UTF-8"` | **[FULL]** Static `"UTF-8"`. |
| `client-env` | `ClientEnv.java:26` (Workspace: No, Hidden: Yes) | *Not in `collect_info_map`* | **[MISSING]** Bazel dumps project-specific rc environment variables; absent in Zel. |
| `command_log` | `CommandLogModule.java:121` (Workspace: No, Hidden: No) | `self.dirs.command_log()` | **[FULL]** Path to `command.log`. |
| `committed-heap-size` | `CommittedHeapSizeInfoItem.java:27` (Workspace: No, Hidden: No) | `"0MB"` | **[STUB]** Hardcoded `"0MB"` (no JVM / memory tracking in Zel). |
| `default-package-path` | `DefaultPackagePathInfoItem.java:39` (Workspace: Yes, Hidden: Yes) | `"%workspace%"` | **[DIFFERENT]** Marked hidden in Bazel; always printed in argumentless `zel info`. |
| `execution_root` | `ExecutionRootInfoItem.java:27` (Workspace: Yes, Hidden: No) | `self.dirs.execroot()` | **[FULL]** Path to `output_base/execroot/_main`. |
| `external_directory` | `ExternalDirectoryInfoItem.java:28` (Workspace: No, Hidden: No) | *Not in `collect_info_map`* | **[MISSING]** Bazel prints external repo directory; absent in Zel. |
| `gc-count` | `GcCountInfoItem.java:27` (Workspace: No, Hidden: No) | `"0"` | **[STUB]** Hardcoded `"0"`. |
| `gc-time` | `GcTimeInfoItem.java:27` (Workspace: No, Hidden: No) | `"0ms"` | **[STUB]** Hardcoded `"0ms"`. |
| `install_base` | `InstallBaseInfoItem.java:27` (Workspace: No, Hidden: No) | `self.dirs.install_base` | **[FULL]** Path to installation base. |
| `install_md5` | `InstallMd5InfoItem.java:27` (Workspace: No, Hidden: Yes) | `self.dirs.install_base.base_name()` | **[DIFFERENT]** Marked hidden in Bazel; always printed in argumentless `zel info`. |
| `java-home` | `JavaHomeInfoItem.java:27` (Workspace: No, Hidden: No) | `"none (native Rust)"` | **[STUB]** Fake description string instead of real path. |
| `java-runtime` | `JavaRuntimeInfoItem.java:25` (Workspace: No, Hidden: No) | `"none (native Rust)"` | **[STUB]** Fake description string instead of JVM name/version. |
| `java-vm` | `JavaVirtualMachineInfoItem.java:25` (Workspace: No, Hidden: No) | `"none (native Rust)"` | **[STUB]** Fake description string instead of JVM engine name. |
| `local_resources` | `LocalResourcesInfoItem.java:27` (Workspace: No, Hidden: No) | `"RAM=16384.0MB, CPU=8.0"` | **[STUB]** Hardcoded static values instead of host hardware detection. |
| `max-heap-size` | `MaxHeapSizeInfoItem.java:26` (Workspace: No, Hidden: No) | `"0MB"` | **[STUB]** Hardcoded `"0MB"`. |
| `output_base` | `OutputBaseInfoItem.java:27` (Workspace: No, Hidden: No) | `self.dirs.output_base` | **[FULL]** Path to server output base. |
| `output_path` | `OutputPathInfoItem.java:27` (Workspace: Yes, Hidden: No) | `self.dirs.output_path()` | **[FULL]** Path to `output_base/execroot/_main/bazel-out`. |
| `package_path` | `PackagePathInfoItem.java:32` (Workspace: Yes, Hidden: No) | `"%workspace%"` | **[FULL]** Default package path `"%workspace%"`. |
| `peak-heap-size` | `PostGCMemoryUseRecorder.java:373` (Workspace: No, Hidden: Yes) | *Not in `collect_info_map`* | **[MISSING]** Bazel tracks peak post-GC heap; absent in Zel. |
| `release` | `ReleaseInfoItem.java:26` (Workspace: No, Hidden: No) | `"release 0.0.1 (zel)"` | **[DIFFERENT]** Returns `"release 0.0.1 (zel)"` rather than Bazel release version. |
| `repository_cache` | `BazelRepositoryModule.java:192` (Workspace: No, Hidden: No) | `output_user_root/cache/repos/v1` | **[FULL]** Path to repository download cache. |
| `server_log` | `ServerLogInfoItem.java:38` (Workspace: No, Hidden: No) | `self.dirs.server_log()` | **[FULL]** Path to server `java.log`. |
| `server_pid` | `ServerPidInfoItem.java:25` (Workspace: No, Hidden: No) | `self.server_pid.to_string()` | **[FULL]** Server PID. |
| `starlark-semantics` | `StarlarkSemanticsInfoItem.java:35` (Workspace: No, Hidden: Yes) | `"{}"` | **[STUB]** / **[DIFFERENT]** Hardcoded `"{}"` and printed in argumentless `zel info`. |
| `used-heap-size` | `UsedHeapSizeInfoItem.java:26` (Workspace: No, Hidden: No) | `"0MB"` | **[STUB]** Hardcoded `"0MB"`. |
| `used-heap-size-after-gc` | `UsedHeapSizeAfterGcInfoItem.java:29` (Workspace: No, Hidden: No) | `"0MB"` | **[STUB]** Hardcoded `"0MB"`. |
| `worker_metrics` | `WorkerMetricsInfoItem.java:33` (Workspace: No, Hidden: Yes) | *Not in `collect_info_map`* | **[MISSING]** Persistent worker metrics absent in Zel. |
| `workspace` | `WorkspaceInfoItem.java:27` (Workspace: Yes, Hidden: No) | `self.dirs.workspace_dir` | **[FULL]** Workspace root directory. |

#### Functional Differences in `info`

* **[MISSING] Five Builtin Info Keys**:
  Bazel provides `build-language`, `client-env`, `external_directory`, `peak-heap-size`, and `worker_metrics`. None of these 5 keys exist in Zel's `collect_info_map`. Requesting them with `zel info <key>` fails with `ERROR: unknown info key '<key>'`.
* **[DIFFERENT] Hidden Keys Leaked on Bare `zel info`**:
  In Bazel (`InfoCommand.java:352`), items marked `hidden = true` (`install_md5`, `default-package-path`, and `starlark-semantics`, as well as `build-language`, `client-env`, `worker_metrics`, `peak-heap-size`) are suppressed when `bazel info` is called with no arguments. Zel (`crates/zel-server/src/lib.rs:594-598`) iterates over all keys in `collect_info_map`, printing `default-package-path`, `install_md5`, and `starlark-semantics` into standard output on bare invocations.
* **[MISSING] Option `--show_make_env`**:
  In Bazel (`InfoCommand.java:118-124, 187-193, 256-278`), `--show_make_env` dynamically extracts all Make environment variable definitions from `BuildConfigurationValue.getMakeEnvironment()` and adds each variable as an info item. Zel does not recognize, parse, or implement `--show_make_env`.
* **[MISSING] Option `--info_output_type`**:
  In Bazel (`InfoCommand.java:127-136`), `--info_output_type=stdout|response_proto` controls whether results are formatted directly on stdout or packed as gRPC response extensions. Zel only supports stdout text formatting.
* **[STUB] Hardcoded JVM and Resource Values**:
  Zel emits hardcoded placeholders for 8 JVM and system metrics: `committed-heap-size` (`"0MB"`), `max-heap-size` (`"0MB"`), `used-heap-size` (`"0MB"`), `used-heap-size-after-gc` (`"0MB"`), `gc-count` (`"0"`), `gc-time` (`"0ms"`), `java-home` (`"none (native Rust)"`), `java-runtime` (`"none (native Rust)"`), `java-vm` (`"none (native Rust)"`), `local_resources` (`"RAM=16384.0MB, CPU=8.0"`), and `starlark-semantics` (`"{}"`).
* **[DIFFERENT] Key Error Handling & Spell Check Suggestions**:
  When an unknown key is specified (e.g. `bazel info bazel-binn`), Bazel's `InfoCommand.java:311-320` invokes `SpellChecker.didYouMean` to suggest candidates:
  `ERROR: unknown key(s): 'bazel-binn' (did you mean 'bazel-bin'?)` with DetailedExitCode `KEY_NOT_RECOGNIZED`.
  Zel (`crates/zel-server/src/lib.rs:611`) prints `ERROR: unknown info key 'bazel-binn'` without suggestions.
* **[DIFFERENT] Workspace-Dependent Key Filtering Outside Workspace**:
  Bazel (`InfoCommand.java:295, 321-336, 355`) tracks `requiresWorkspace()` on each `InfoItem`. When invoked outside a workspace, bare `bazel info` omits the 9 workspace-dependent keys, and requesting one explicitly errors out (`KEY_NOT_SUPPORTED_OUTSIDE_WORKSPACE`). Zel never checks whether a workspace exists, printing artificial paths for all keys unconditionally.


### `help` Command Deep-Dive

Bazel's `HelpCommand.java` serves as both an index of commands and a dynamic generator for command manual pages, bash auto-completion, proto schemas, and HTML documentation. Zel's `cmd_help` (`crates/zel-server/src/lib.rs:300-513`) implements topic routing but replaces almost all dynamic generation with static strings or empty stubs.

* **[STUB] Command-Specific Help (`help <command>`)**:
  * **Bazel Behavior**: When invoked with a command name (e.g. `bazel help build`), Bazel looks up the command in `runtime.getCommandMap()`, collects all option classes associated with that command and its inherited command classes (`BlazeCommandUtils.getOptions`), and formats an exhaustive manual page with syntax, option descriptions, default values, option tags, and categories.
  * **Zel Behavior**: In `crates/zel-server/src/lib.rs:427-434`, Zel groups all 22 command names into the exact same pattern arm as `"summary"`:
    ```rust
    "summary" | "analyze-profile" | "aquery" | "build" | "canonicalize-flags" | "clean"
    | "config" | "coverage" | "cquery" | "dump" | "fetch" | "help" | "info" | "license"
    | "mod" | "print_action" | "query" | "run" | "shutdown" | "sync" | "test"
    | "vendor" | "version" => { ... }
    ```
    Running `zel help build` or `zel help clean` prints the generic list of all available commands instead of help for `build` or `clean`. Specific command usage and flags are never displayed.

* **[PARTIAL] Topic `startup_options`**:
  * **Bazel Behavior**: `HelpCommand.java:214-224` dynamically parses all startup option classes (`BlazeServerStartupOptions`, etc.) and displays all startup flags with documentation categories, effect tags, and default values.
  * **Zel Behavior**: `crates/zel-server/src/lib.rs:390-420` hardcodes a static list of 7 options (`--output_base`, `--output_user_root`, `--install_base`, `--[no]batch`, `--[no]block_for_lock`, `--max_idle_secs`, `--bazelrc`). More than 20 other standard Bazel startup options (e.g., `--host_jvm_args`, `--server_javabase`, `--client_debug`, `--connect_timeout_secs`, `--digest_function`, `--shutdown_on_low_sys_mem`, `--idle_server_tasks`) are omitted.

* **[PARTIAL] Topic `target-syntax`**:
  * **Bazel Behavior**: `HelpCommand.java:416-425` renders `resource:target-syntax.txt`, providing extensive documentation on canonical labels (`@repo//pkg:target`), package-relative targets (`:target`), recursive package wildcards (`//pkg/...`), target wildcards (`//pkg:*`), and target pattern exclusion rules.
  * **Zel Behavior**: `crates/zel-server/src/lib.rs:422-426` prints a single hardcoded line:
    ```text
    Target syntax: //package:target, //package/..., :target
    ```

* **[DIFFERENT] Topic `info-keys`**:
  * **Bazel Behavior**: `HelpCommand.java:427-432` renders a two-column aligned table formatting each info key and its description: `%-23s %s\n`.
  * **Zel Behavior**: `crates/zel-server/src/lib.rs:369-373` prints only the info key names on separate lines without any descriptions. Furthermore, it only iterates Zel's 28 keys, omitting the 5 keys absent from Zel.

* **[STUB] Topic `completion` (Bash Auto-Completion)**:
  * **Bazel Behavior**: `HelpCommand.java:226-249`:
    * When invoked with `bash` (`bazel help completion bash`), it outputs `/scripts/bazel-complete-header.bash`, followed by dynamically generated `BAZEL_COMMAND_LIST`, `BAZEL_INFO_KEYS`, `BAZEL_STARTUP_OPTIONS`, `BAZEL_COMMAND_<CMD>_ARGUMENT`, and `BAZEL_COMMAND_<CMD>_FLAGS` for every command, followed by `/scripts/bazel-complete-template.bash`.
    * When invoked without arguments (`bazel help completion`), it outputs only the completion variables.
  * **Zel Behavior**: `crates/zel-server/src/lib.rs:307-368`:
    * When invoked with `bash`, it outputs a dummy placeholder header: `#!/bin/bash\n# Bazel bash completion script` instead of the actual completion scripts.
    * For every command in the command list, it outputs the exact same hardcoded 8 flags (`--compilation_mode`, `--config`, `--cpu`, `--jobs`, `--keep_going`, `--nokeep_going`, `--subcommands`, `--nosubcommands`). Command-specific options (e.g. `test` flags, `clean` flags, `info` flags) are not emitted.
    * It never emits `BAZEL_COMMAND_<CMD>_ARGUMENT` lines.
    * `BAZEL_STARTUP_OPTIONS` is hardcoded to 11 flags.

* **[STUB] Topic `flags-as-proto`**:
  * **Bazel Behavior**: `HelpCommand.java:302-330` traverses all options classes, builds a `BazelFlagsProto.FlagCollection` message containing full flag definitions (documentation, types, tags, defaults, expansions), Base64 encodes it, and prints it to stdout.
  * **Zel Behavior**: `crates/zel-server/src/lib.rs:375-378` prints an empty string: `writeln_bytes(stdout, "");`.

* **[STUB] Topic `everything-as-html`**:
  * **Bazel Behavior**: `HelpCommand.java:467-643` generates a complete HTML document containing comprehensive tables for all commands, options, metadata tags, and startup flags.
  * **Zel Behavior**: `crates/zel-server/src/lib.rs:379-382` returns a hardcoded 53-character HTML snippet:
    ```html
    <html><body><h1>Bazel Help</h1></body></html>
    ```

* **[MISSING] Options `--help_verbosity`, `--long` (`-l`), and `--short`**:
  * **Bazel Behavior**: `HelpCommand.java:105-132` provides `--help_verbosity=short|medium|long`, with `--short` showing only option names, and `--long` (`-l`) showing full option help text and types.
  * **Zel Behavior**: None of these flags are recognized or implemented in Zel.


### `version` Command Deep-Dive

Bazel's `VersionCommand.java:39-100` and `BlazeVersionInfo.java:34-187` output build stamping metadata and support GNU-standard version formatting.

* **[DIFFERENT] Key Name Mismatch for Build Timestamp**:
  * **Bazel Behavior**: Bazel formats the Unix epoch timestamp with the key name `"Build timestamp as int"`:
    ```text
    Build label: 8.0.0
    Build target: bazel-out/...
    Build time: Fri Jan 01 00:00:00 2026 (1767225600)
    Build timestamp as int: 1767225600
    ```
  * **Zel Behavior**: `crates/zel-server/src/lib.rs:295` formats the timestamp as `"Build timestamp: 1793833200"`, omitting `" as int"`. Tools or scripts expecting the exact Bazel key name `"Build timestamp as int"` will fail to find it.

* **[STUB] Hardcoded Version Metadata**:
  * **Bazel Behavior**: Bazel populates `Build label`, `Build target`, `Build time`, and `Build timestamp as int` from build workspace stamping files (`build-info.txt` and `build-changelist.txt`).
  * **Zel Behavior**: In `crates/zel-server/src/lib.rs:293-295`, `Build target` is hardcoded to `"@@//crates:zel"`, `Build time` is hardcoded to `"Wed Nov 4 2026"`, and `Build timestamp` is hardcoded to `"1793833200"`. Only `Build label` is configurable via `ZEL_EMBED_LABEL` / `BAZEL_EMBED_LABEL`.

* **[PARTIAL] Option `--gnu_format`**:
  * **Bazel Behavior**: `VersionCommand.java:89-98`: When `--gnu_format` is provided, Bazel outputs `<productName> <version>` (e.g. `bazel 8.0.0` or `bazel no_version`).
  * **Zel Behavior**: `crates/zel-server/src/lib.rs:288-290`: Supports `--gnu_format` and writes `{prefix} {label}` (e.g. `zel 0.0.1` or `bazel 0.0.1`). In `crates/zel-client/src/lib.rs:170`, client-side invocation with `--version` maps to `--startup_version`, returning before daemon startup.

---

### `clean` Command Deep-Dive

Bazel's `CleanCommand.java:56-308` handles file tree removal, convenience symlink teardown, cache eviction, and server process termination.

* **[STUB] / [DIFFERENT] `--expunge` Does Not Delete the Output Base**:
  * **Bazel Behavior**: In `CleanCommand.java:213-241`, when `--expunge` is specified, Bazel closes log files and stdio descriptors, performs `outputBase.deleteTreesBelow()`, deletes the entire `output_base` directory itself, and halts the server.
  * **Zel Behavior**: In `crates/zel-server/src/lib.rs:659-671`, `cmd_clean` deletes only `self.dirs.execroot_base()` and `self.dirs.action_cache_file()`. When `parsed.expunge` is true:
    ```rust
    if parsed.expunge || parsed.expunge_async {
        writeln_bytes(
            stderr,
            "INFO: Expunging output base and shutting down server...",
        );
        self.shutdown_requested.store(true, Ordering::SeqCst);
        *termination_expected = true;
    }
    ```
    It logs `"INFO: Expunging output base and shutting down server..."` and requests shutdown, but **never calls `fs.delete(&self.dirs.output_base)`**! The entire `output_base` (including server directory, socket files, metadata, logs) remains on disk.

* **[MISSING] Option `--async`**:
  * **Bazel Behavior**: `CleanCommand.java:96-104` provides `--async` (and `--expunge_async`, which expands to `--expunge --async`). It moves the directory to be deleted to a temporary directory in `output_user_root` and spawns a background deletion process using an embedded `daemonize` utility (`AsyncDirectoryCleaner.spawnDaemonizedDeletion`), allowing the CLI command to exit immediately.
  * **Zel Behavior**: `--async` is **completely absent from Zel's option parser** (`crates/zel-options/src/lib.rs`). While `ParsedCommandOptions` includes an `expunge_async` boolean, it is only triggered by `--expunge_async` and executes synchronously without background daemonization. Passing `--async` fails option parsing or is rejected.

* **[MISSING] Workspace Convenience Symlinks Removal**:
  * **Bazel Behavior**: `CleanCommand.java:141` retrieves the configured `symlinkPrefix` and removes convenience symlinks in the workspace root (`bazel-bin`, `bazel-out`, `bazel-testlogs`).
  * **Zel Behavior**: Zel does not delete or unlink any convenience symlinks created in the workspace.

* **[DIFFERENT] Output Banner Difference**:
  * **Bazel Behavior**: For synchronous clean without `--async`, Bazel outputs (`CleanCommand.java:172`):
    `INFO: Starting clean (this may take a while). Use --async if the clean takes more than several minutes.`
  * **Zel Behavior**: `crates/zel-server/src/lib.rs:658` always prints `INFO: Starting clean.`.

* **[DIFFERENT] Residue Handling**:
  * **Bazel Behavior**: `CleanCommand.java:123-129`: Non-option command residue is rejected: `Unrecognized arguments: <residue>` with DetailedExitCode `ARGUMENTS_NOT_RECOGNIZED`.
  * **Zel Behavior**: Any non-option residues passed to `zel clean` are silently ignored.

---

### `canonicalize-flags` Command Deep-Dive

Bazel's `CanonicalizeCommand.java:59-292` canonicalizes option lists against specific command schemas and invocation policies.

* **[DIFFERENT] Option `--for_command` Silently Dropped on Server**:
  * **Bazel Behavior**: `CanonicalizeCommand.java:78-83, 152-167`: `--for_command=<cmd>` selects which command schema governs the canonicalization. If the specified command does not exist in `runtime.getCommandMap()`, it fails with exit code `2` and DetailedExitCode `FOR_COMMAND_INVALID`:
    ```text
    Not a valid command: 'unknown' (should be one of build, test, ...)
    ```
  * **Zel Behavior**: In `crates/zel-client/src/lib.rs:236-243`, the client inspects `--for_command` solely to choose which `.bazelrc` section to apply. On the server (`crates/zel-server/src/lib.rs:627`), it simply discards the argument:
    ```rust
    let filtered: Vec<String> = args
        .iter()
        .filter(|a| !a.starts_with("--for_command=") && *a != "--")
        .cloned()
        .collect();
    ```
    The server never validates whether the target command exists, and always canonicalizes against the generic `ParsedCommandOptions` schema rather than command-specific options.

* **[MISSING] Option `--invocation_policy`**:
  * **Bazel Behavior**: `CanonicalizeCommand.java:86-91, 233-246`: Parses an `InvocationPolicy` protobuf (in base64 or text) and enforces policies (`SetValue`, `UseDefault`, `DisallowValues`, etc.) on the options before canonicalizing.
  * **Zel Behavior**: Zel does not support, parse, or enforce invocation policies.

* **[MISSING] Option `--canonicalize_policy`**:
  * **Bazel Behavior**: `CanonicalizeCommand.java:94-103, 248-257`: When true, outputs the effective canonical `InvocationPolicy` proto instead of the canonicalized flag arguments.
  * **Zel Behavior**: Completely missing in Zel.

* **[MISSING] Starlark Options Canonicalization**:
  * **Bazel Behavior**: `CanonicalizeCommand.java:211-224, 260`: Syncs package loading and utilizes `StarlarkOptionsParser` to canonicalize user-defined Starlark build settings (flags prefixed with `//` or `@repo//`).
  * **Zel Behavior**: Zel only canonicalizes hardcoded native flags in `ParsedCommandOptions`. Starlark flags are not supported.

* **[MISSING] Option `--show_warnings` & Deprecation Warnings**:
  * **Bazel Behavior**: Emits warnings for conflicting expansion flags and deprecated options.
  * **Zel Behavior**: Zel emits no warnings and lacks `--show_warnings`.

---

### `shutdown` Command Deep-Dive

Bazel's `ShutdownCommand.java:31-80` allows controlling server process termination and conditional shutdown.

* **[MISSING] Option `--iff_heap_size_greater_than`**:
  * **Bazel Behavior**: `ShutdownCommand.java:47-55, 62-75`: Accepts `--iff_heap_size_greater_than=<int>` (memory limit in MB). If specified and non-zero, Bazel queries JVM `Runtime.getRuntime().totalMemory()`. If memory consumption does not exceed the limit, the server **remains running** and returns `BlazeCommandResult.success()` (exit code `0`).
  * **Zel Behavior**: `crates/zel-server/src/lib.rs:235-239` parses no options and unconditionally marks shutdown:
    ```rust
    "shutdown" => {
        self.shutdown_requested.store(true, Ordering::SeqCst);
        termination_expected = true;
        ExitCode::SUCCESS
    }
    ```
    The flag `--iff_heap_size_greater_than` is completely ignored.

* **[DIFFERENT] Residue and Option Parsing**:
  * **Bazel Behavior**: `ShutdownCommand.java:35` sets `allowResidue = false`. Supplying unrecognized flags or extra positional arguments results in a command line error (`ARGUMENTS_NOT_RECOGNIZED`).
  * **Zel Behavior**: Zel ignores all arguments passed to `zel shutdown`.


### Non-Build Commands Deep-Dive

#### `run` Command
* **[STUB] / [DIFFERENT] Client Completely Ignores `ExecRequest`**:
  * **Bazel Behavior**: `RunCommand.java:109-250`: After building the executable target, Bazel populates an `ExecRequest` protobuf containing the binary path, arguments, environment variables, working directory, and execution flags. The C++ client (`src/main/cpp/blaze.cc`) receives this in `RunResponse`, prepares the execution environment, and invokes `execv` / `blaze_util::ExecuteProgram`, directly replacing or spawning the process to run the built executable and forwarding stdio.
  * **Zel Behavior**: In `crates/zel-server/src/lib.rs:1072-1129` (`cmd_run`), the server builds the executable target and constructs an `ExecRequest`. However, in `crates/zel-client/src/lib.rs:328-338` (daemon mode) and `350-354` (batch mode), and in `crates/zel-client/src/main.rs:30-31`:
    ```rust
    let resp = runtime.dispatch_command(&run_req, "batch-cookie");
    let _ = stdout_writer.write_all(&resp.standard_output);
    let _ = stderr_writer.write_all(&resp.standard_error);
    resp.exit_code
    ```
    The client **never inspects or executes `resp.exec_request`**! It simply writes stdout/stderr and exits with code `0`. Invoking `zel run //my:app` builds the binary, logs `INFO: Running command line: bazel-out/.../my/app`, and exits immediately without running the application.

#### `coverage` Command
* **[STUB] / [DIFFERENT] Treated as `build` (Tests Never Executed, No Coverage Reports)**:
  * **Bazel Behavior**: `CoverageCommand.java:105-180`: Inherits from `TestCommand`. Collects code coverage data during test execution, instruments test binaries, collects `.gcda` / coverage profiles, invokes the coverage post-processor (lcov merger), and writes coverage reports to `bazel-testlogs/<target>/coverage.dat`.
  * **Zel Behavior**: Dispatched in `crates/zel-server/src/lib.rs:250` to `cmd_build_or_test`. However, line 999 specifically checks:
    ```rust
    let mut any_test_failed = false;
    if command == "test" {
        for ct in &configured_targets { ... }
    }
    ```
    Because `command == "coverage"` does not match `"test"`, `zel coverage` **does not even execute the test binaries**! It only builds them, performs no coverage instrumentation, collects no coverage data, and produces no `coverage.dat`.

#### `config` Command
* **[STUB] 1-Line Stub Output & Visibility Difference**:
  * **Bazel Behavior**: `ConfigCommand.java:74-491`: Config inspection tool marked `hidden = true`. Allows dumping all configuration IDs (`--dump_all`), diffing two configurations when two hashes are provided, and formatting in text or JSON (`--output=text|json`).
  * **Zel Behavior**: Dispatched in `crates/zel-server/src/lib.rs:254`:
    ```rust
    "dump" | "config" | "analyze-profile" | "license" | "print_action" => {
        writeln_bytes(&mut stdout, &format!("INFO: Running '{command}'..."));
        ExitCode::SUCCESS
    }
    ```
    Outputs `INFO: Running 'config'...` and exits `0`. All flags and arguments are ignored. Furthermore, Zel lists `config` in public `help`, whereas Bazel hides it.

#### `dump` Command
* **[STUB] Completely Unimplemented**:
  * **Bazel Behavior**: `DumpCommand.java:138-1230`: Provides diagnostic inspection of Bazel server memory and cache state: `--packages`, `--action_cache`, `--rule_classes`, `--rules`, `--skyframe=summary|count|deps`, `--memory`, and `--skylark_memory`.
  * **Zel Behavior**: Routed to the stub in `crates/zel-server/src/lib.rs:254`. Outputs `INFO: Running 'dump'...` and exits `0`. No internal Skyframe or action cache state can be dumped.

#### `license` Command
* **[STUB] Completely Unimplemented**:
  * **Bazel Behavior**: `LicenseCommand.java:36-118`: Prints embedded `license/LICENSE` and discovers/displays licenses for bundled JDK and JRE installations.
  * **Zel Behavior**: Routed to the stub in `crates/zel-server/src/lib.rs:254`. Outputs `INFO: Running 'license'...` and exits `0`. No license text is printed.

#### `print_action` Command
* **[STUB] Completely Unimplemented**:
  * **Bazel Behavior**: `PrintActionCommand.java:71-410`: Resolves configured targets for specified source files, filters actions by `--print_action_mnemonics`, and prints compilation command lines and inputs/outputs.
  * **Zel Behavior**: Routed to the stub in `crates/zel-server/src/lib.rs:254`. Outputs `INFO: Running 'print_action'...` and exits `0`.

#### `fetch` & `vendor` Commands
* **[STUB] / [DIFFERENT] Routed to Build Engine Without Fetching or Vendoring**:
  * **Bazel Behavior**:
    * `FetchCommand.java:59`: Fetches external dependencies defined in `MODULE.bazel` or `WORKSPACE` without running a build (`--all`, `--repo`, `--configure`, `--force`).
    * `VendorCommand.java:99`: Vendors external repositories into the directory specified by `--vendor_dir`.
  * **Zel Behavior**: Both commands are routed in `crates/zel-server/src/lib.rs:250` directly to `cmd_build_or_test`. If invoked without target labels (the standard usage for repo fetching, e.g. `bazel fetch --all`), Zel reports `INFO: Found 0 targets...` and exits `0`. If target labels are supplied, it attempts to build the targets rather than fetching external repositories.

#### Commands Removed from Bazel but Retained in Zel: `analyze-profile` & `sync`
* **[DIFFERENT] Obsolete Commands Kept in Zel**:
  * In Bazel, `analyze-profile` was deleted in commit `0fd3ae4f66` and `sync` was deleted in commit `8396531ec0` (in favor of `fetch --all`). Running either command in Bazel produces `Command '<cmd>' not found. Try 'bazel help'.` (exit code `2`).
  * In Zel, `analyze-profile` is retained as a stub (`crates/zel-server/src/lib.rs:254`), and `sync` is aliased to `cmd_build_or_test` (`line 250`). Both commands remain advertised in Zel's `help` and `completion` lists.

#### Missing Command: `mobile-install`
* **[MISSING] Not Implemented**:
  * Bazel provides `MobileInstallCommand.java:69` in `MobileInstallModule.java` for incremental deployment to Android mobile devices.
  * Zel has no handler for `mobile-install`. Running `zel mobile-install` produces exit code `2` with `ERROR: Command 'mobile-install' not found. Try 'bazel help'.`.


---


## Options Parsing Framework and Command Flags

**Overall Parity Level: Rudimentary (~2% functional parity / skeleton)**

Zel's options parsing implementation is a minimal, bespoke parser contained in a single 782-line file (`crates/zel-options/src/lib.rs`). While Zel advertises itself as a 1:1 drop-in replacement for Bazel, it implements none of Bazel's rich, reflective, annotation-driven options framework (`com.google.devtools.common.options`). Out of **1,102 unique flags** declared across Bazel's options classes in `src/main/java`, Zel recognizes only **43 flags** (3.9%), of which only **19 flags (1.7%) are actually honored** by the runtime. An additional **24 flags (2.2%) are parsed but completely ignored** (no-ops). The remaining **1,059 flags (96.1%) are completely unknown**. Crucially, Zel's command-line parser does not reject unknown flags—it silently captures them into a canonical flag map and proceeds without error or warning, which masks invalid invocations and configurations. Furthermore, critical RC file inheritance, platform-specific config resolution, Starlark flag parsing, invocation policy enforcement, and proper `--` residue handling are missing or suffer from severe behavioral defects.

### 1. Options Parsing Framework Architecture

- **[MISSING] Declarative Options Specification and Reflection Architecture**: In Bazel (`src/main/java/com/google/devtools/common/options/`), options are declared declaratively via `@Option` annotations on typed fields in classes extending `OptionsBase` (`OptionsBase.java`, `OptionDefinition.java`, `IsolatedOptionsData.java`, `OptionsData.java`). Bazel inspects these classes via reflection, building immutable lookup caches for flags, abbreviations, categories, documentation, and deprecations. In Zel (`crates/zel-options/src/lib.rs`), there is no options framework or schema system whatsoever: options are hardcoded into an ad-hoc sequential loop over CLI strings in `ParsedCommandOptions::parse()` (lines 534–705) and `StartupOptions::apply_flag()` (lines 173–234).

- **[MISSING] Type Converters Architecture**: Bazel provides a modular, extensible type-conversion framework centered on `Converter<T>` (`src/main/java/com/google/devtools/common/options/Converter.java` and `Converters.java`). Over 50 converters exist in Bazel (e.g., `BooleanConverter`, `TriStateConverter`, `DurationConverter`, `ByteSizeConverter`, `LabelConverter`, `CompilationModeConverter`, `OutputFormatConverter`). Converters provide precise error formatting (e.g. `'foo' is not a boolean`). In Zel (`crates/zel-options/src/lib.rs`), conversion is hardcoded inline with primitive `val.parse::<usize>()` or string comparisons; there is no abstraction for converters, type descriptors, or custom conversion logic.

- **[MISSING] Option Expansion Framework (`expansion` & `expansionFunction`)**: Bazel's `@Option` annotation supports static expansion (`expansion = {"--flag1", "--flag2"}`) and dynamic expansion functions (`expansionFunction = ...`) as seen in `CoreOptions.java` and `ExecutionOptions.java`. When expanded, Bazel's parser tracks the provenance of expanded flags (`ParsedOptionDescription.getExpandedFrom()`). Zel has no expansion mechanism for flags other than `.bazelrc` `--config` macro expansion.

- **[MISSING] Implicit Requirements Framework (`implicitRequirements`)**: In Bazel, options can define `implicitRequirements = {"--dep_flag=val"}`, ensuring that whenever a flag is asserted, its dependent flags are automatically set at the appropriate priority. In Zel, implicit requirements do not exist.

- **[MISSING] Option Priority and Precedence Ordering**: Bazel implements a strict priority hierarchy via `OptionPriority.PriorityCategory` (`src/main/java/com/google/devtools/common/options/OptionPriority.java`): `DEFAULT` < `COMPUTED_DEFAULT` < `RC_FILE` < `COMMAND_LINE` < `INVOCATION_POLICY`. Each category tracks an index to resolve overrides, allowing later CLI flags to override earlier CLI flags and CLI flags to override RC flags regardless of argument array order. Zel has no priority representation; it simply iterates over an array of strings in order and overwrites struct fields (`crates/zel-options/src/lib.rs:539-705`).

- **[MISSING] Invocation Policy Enforcement (`InvocationPolicyEnforcer`)**: Bazel supports machine-enforced build policies via `InvocationPolicyEnforcer.java` and the `InvocationPolicy` protobuf (`src/main/protobuf/invocation_policy.proto`). Policies can enforce `SetValue`, `UseDefault`, `DisallowValues`, `DisallowOption`, `SetNewDefault`, and prepend/append configs. In Zel, while `RunRequest` has an `invocation_policy` string field (`crates/zel-proto/src/lib.rs`), `zel-server` never parses or enforces invocation policies—it is completely ignored (`crates/zel-server/src/lib.rs`).

- **[MISSING] Multiple Option Accumulation (`allowMultiple`)**: In Bazel, options with `allowMultiple = true` accumulate values into `List<T>`. In Zel, only `--define` (`opts.user_defines: BTreeMap<String, String>`) accumulates; all other flags overwrite earlier occurrences without checking or validation.

- **[MISSING] Legacy Name and Abbreviation Aliasing (`oldName`, `abbrev`)**: Bazel's `@Option` supports `oldName` for backward-compatible flag renaming and `abbrev` for single-character short options. Zel only supports a tiny hardcoded set of abbreviations (`-c`, `-k`, `-s`, `-j`). Any other short option or old name is unrecognized.

### 2. `.bazelrc` and Config File Processing Parity

- **[MISSING] System RC and User Home RC File Loading**: Bazel automatically discovers and loads system-wide configuration (`/etc/bazel.bazelrc` or `%ProgramData%\bazel.bazelrc`) and user home configuration (`~/.bazelrc` or `$HOME/.bazelrc`) unless explicitly disabled (`src/main/cpp/option_processor.cc:436`). In Zel, while `StartupOptions` defines `pub system_rc: bool` and `pub home_rc: bool` (`crates/zel-options/src/lib.rs:95-97`), `zel-client/src/lib.rs:181-228` **never checks or loads system RC or home RC files**. Only workspace `.zelrc` / `.bazelrc` and explicit `--bazelrc` flags are read.

- **[MISSING] Command Inheritance in RC Files**: In Bazel (`BlazeOptionHandler.java:686-692`), commands inherit RC options according to the command hierarchy: `test` inherits from `build`, `run` inherits from `build`, `coverage` inherits from `test` and `build`, and `cquery`/`aquery` inherit from `build`. In Zel (`crates/zel-options/src/lib.rs:379`), `expand_command_args` only checks `always`, `common`, and the literal `command`: `for prefix in ["always", "common", command]`. As a result, options configured under `build` in `.bazelrc` (e.g., `build --compilation_mode=opt`) **are completely ignored when running `zel test` or `zel run`**.

- **[MISSING] Platform-Specific Config Auto-Expansion (`--enable_platform_specific_config`)**: Bazel automatically enables `<command>:<os>` configs (e.g. `build:macos`, `build:linux`, `build:windows`) when `--enable_platform_specific_config` is set (`ConfigExpander.java:45-82`). Zel has no awareness of `--enable_platform_specific_config` or OS-specific configs in `zel-options`.

- **[DIFFERENT] `--config` Expansion and Cycle Detection**: In Bazel (`ConfigExpander.java:274-362`), cycle detection tracks the full ancestor chain (`configAncestorSet`), correctly allowing diamond config dependencies (e.g., `config A` and `config B` both expanding `config C`) while rejecting circular expansions (`config A` -> `config B` -> `config A`) with an error citing the chain. In Zel (`crates/zel-options/src/lib.rs:417`), cycle tracking uses a flat `seen_configs: HashSet<String>`. If a config is referenced multiple times across diamond dependencies, Zel's parser skips subsequent references entirely; it does not report cycles or warn when recursion exceeds limits.

- **[DIFFERENT] Import Path Resolution**: In Bazel (`src/main/cpp/rc_file.cc:209`), non-`%workspace%` relative import paths in rc files are resolved relative to the directory containing the importing RC file. In Zel (`crates/zel-options/src/lib.rs:334`), relative imports are always resolved relative to `workspace_dir`, which breaks nested imports across subdirectories.

- **[MISSING] Conditional Version Imports (`try-import-if-bazel-version`)**: Bazel supports `try-import-if-bazel-version <op><ver> <path>` (`rc_file.cc:45-60`). Zel treats `try-import-if-bazel-version` as an unrecognized command section and fails or misparses it.

- **[DIFFERENT] Import Loop Handling**: When Bazel encounters an import cycle, it terminates with `ParseError::IMPORT_LOOP` and displays the cycle stack (`rc_file.cc:240-248`). Zel uses `visited.contains(&resolved_pf)` and silently ignores recursive imports without reporting an error (`crates/zel-options/src/lib.rs:336`).

### 3. Command-Line Syntax, Starlark Flags, and Unknown-Flag Behavior

- **[MISSING] Starlark Build Settings / User-Defined Flags (`--//pkg:flag`)**: Bazel natively parses user-defined Starlark flags on the CLI and in `.bazelrc` (e.g. `--//tools/build:my_flag=true`, `--@rules_cc//cc:linkstatic`) via `StarlarkOptionsParser.java` and loads their definitions through Skyframe. In Zel, Starlark flags are completely unsupported. If passed, Zel treats `--//pkg:flag=val` as a generic string flag and puts it in `canonical_flags`, but it never configures any target, transitions, or rule attributes.

- **[MISSING] Flag Aliasing (`--flag_alias`)**: Bazel allows users to alias long Starlark flags using `--flag_alias=<alias>=<actual_flag>` (`CommonCommandOptions.java`). Zel has no flag alias resolution mechanism.

- **[DIFFERENT] Boolean Flag Value Syntax**: Bazel accepts `--<flag>=[true|false|yes|no|1|0|t|f|y|n]`, `--<flag>`, `--no<flag>`, and `-<abbrev>-` (e.g. `-k-`) for boolean flags via `BooleanConverter` (`Converters.java:38-59`). In Zel (`crates/zel-options/src/lib.rs:573-603`), boolean flags are only recognized in exact `--<flag>`, `-<abbrev>`, and `--no<flag>` forms. If a user passes `--keep_going=true`, `--keep_going=1`, or `--subcommands=false`, Zel **does not set `opts.keep_going` or `opts.subcommands`**—the argument falls through to the generic `--<key>=<val>` handler and is ignored by the runtime.

- **[DIFFERENT] Command-Line Residue (`--`) Handling & Critical Target Loss Bug**: In Bazel, `--` stops options parsing; all subsequent tokens become residue (target labels for `build`, or binary args for `run`). In Zel (`crates/zel-options/src/lib.rs:541-544`), any arguments following `--` are appended to `opts.passthrough_args` and excluded from `opts.targets`. As a result, running `zel build -- //foo:bar` results in `INFO: Found 0 targets...` because Zel ignores the target!

- **[DIFFERENT] Unknown Flag Handling and Error Reporting**: When an unknown flag is provided to Bazel (e.g. `bazel build --unknown_flag`), Bazel aborts immediately with `UnrecognizedOptionException: Unrecognized option: --unknown_flag`, offers spellchecker suggestions (`Did you mean ...?`), and exits with exit code 2 (`COMMAND_LINE_ERROR`). In Zel (`crates/zel-options/src/lib.rs:693-705`), **all unknown command flags are silently accepted** into `opts.canonical_flags` and ignored. Running `zel build --this_flag_does_not_exist` exits with code 0 without any error or warning.

### 4. Group-by-Group Command Flags Parity Enumeration

Below is the comprehensive audit of Bazel's options classes, enumerating which flags Zel honors, which it parses but ignores, and which are unknown.

#### Server Startup Options (`BlazeServerStartupOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/BlazeServerStartupOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 39 | **Honored:** 6 | **Parsed but Ignored:** 13 | **Unknown/Missing:** 20
- **Description:** Controls Bazel server lifecycle, daemon JVM/binary execution, memory limits, and socket connections.
- **[HONORED] (6):** `--batch`, `--ignore_all_rc_files`, `--install_base`, `--output_base`, `--output_user_root`, `--quiet`
- **[STUB / IGNORED] (13):** `--autodetect_server_javabase`, `--block_for_lock`, `--client_debug`, `--connect_timeout_secs`, `--digest_function`, `--failure_detail_out`, `--idle_server_tasks`, `--local_startup_timeout_secs`, `--max_idle_secs`, `--preemptible`, `--shutdown_on_low_sys_mem`, `--unix_digest_hash_attribute_name`, `--write_command_log`
- **[MISSING] (20):** `--batch_cpu_scheduling`, `--command_port`, `--default_system_javabase`, `--delete_output_base_on_shutdown`, `--experimental_cgroup_parent`, `--experimental_run_in_user_cgroup`, `--experimental_use_compact_object_headers`, `--extra_classpath`, `--fatal_event_bus_exceptions`, `--install_md5`, `--invocation_policy`, `--io_nice_level`, `--lock_install_base`, `--macos_qos_class`, `--option_sources`, `--product_name`, `--server_jvm_out`, `--unlimit_coredumps`, `--windows_enable_symlinks`, `--workspace_directory`

#### Host JVM Startup Options (`HostJvmStartupOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/HostJvmStartupOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 3 | **Honored:** 0 | **Parsed but Ignored:** 2 | **Unknown/Missing:** 1
- **Description:** Options controlling JVM arguments and javabase paths for the server.
- **[STUB / IGNORED] (2):** `--host_jvm_args`, `--server_javabase`
- **[MISSING] (1):** `--host_jvm_debug`

#### Common Command Options (`CommonCommandOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/CommonCommandOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 46 | **Honored:** 2 | **Parsed but Ignored:** 1 | **Unknown/Missing:** 43
- **Description:** Options shared by all Blaze commands (e.g. config, client environment, logging, toolchains).
- **[HONORED] (2):** `--config`, `--profile`
- **[STUB / IGNORED] (1):** `--write_command_log`
- **[MISSING] (43):** `--binary_path`, `--build_metadata`, `--build_request_id`, `--command_wait_time`, `--enable_platform_specific_config`, `--experimental_allow_project_files`, `--experimental_collect_load_average_in_profiler`, `--experimental_collect_pressure_stall_indicators`, `--experimental_collect_resource_estimation`, `--experimental_collect_skyframe_counts_in_profiler`, `--experimental_collect_system_network_usage`, `--experimental_collect_worker_data_in_profiler`, `--experimental_forced_gc_interval`, `--experimental_non_deterministic_memory_optimizations`, `--experimental_profile_additional_tasks`, `--experimental_profile_include_primary_output`, `--experimental_profile_include_target_configuration`, `--experimental_profile_include_target_label`, `--experimental_strict_repo_env`, `--experimental_tool_command_line`, `--extract_data_time`, `--generate_json_trace_profile`, `--heap_dump_on_oom`, `--heuristically_drop_nodes`, `--http_connector_attempts`, `--http_connector_retry_max_timeout`, `--http_max_parallel_downloads`, `--http_timeout_scaling`, `--invocation_id`, `--memory_profile`, `--memory_profile_stable_heap_parameters`, `--oom_message`, `--profiles_to_retain`, `--record_full_profiler_data`, `--redirect_local_instrumentation_output_writes`, `--repo_env`, `--restart_reason`, `--slim_profile`, `--starlark_cpu_profile`, `--startup_time`, `--tool_tag`, `--track_incremental_state`, `--unconditional_warning`

#### Build Request Options (`BuildRequestOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/buildtool/BuildRequestOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 40 | **Honored:** 0 | **Parsed but Ignored:** 1 | **Unknown/Missing:** 39
- **Description:** Top-level build parameters (e.g. jobs, aspects, build actions, output paths). Note: `jobs` is parsed by Zel but builds run strictly serially in a single-threaded loop.
- **[STUB / IGNORED] (1):** `--jobs`
- **[MISSING] (39):** `--analyze`, `--aspects`, `--aspects_parameters`, `--build`, `--bust_action_caches`, `--directory_creation_cache`, `--enforce_project_configs`, `--experimental_aquery_dump_after_build_format`, `--experimental_aquery_dump_after_build_output_file`, `--experimental_async_execution`, `--experimental_async_execution_max_concurrent_actions`, `--experimental_check_rewound_action_outputs`, `--experimental_convenience_symlinks`, `--experimental_convenience_symlinks_bep_event`, `--experimental_fsvc_threads`, `--experimental_max_repeated_lost_inputs`, `--experimental_merged_skyframe_analysis_execution`, `--experimental_precise_rewinding`, `--experimental_skyframe_memory_dump`, `--experimental_skymeld_analysis_overlap_percentage`, `--experimental_use_validation_aspect`, `--explain`, `--extra_default_output_groups`, `--hide_aspect_results`, `--hide_output_group_results`, `--incompatible_fail_on_unknown_output_groups`, `--incompatible_skip_genfiles_symlink`, `--output_filter`, `--output_groups`, `--output_tree_tracking`, `--progress_report_interval`, `--rewind_lost_inputs`, `--run_validations`, `--show_result`, `--symlink_prefix`, `--target_pattern_file`, `--target_query`, `--target_query_file`, `--use_action_cache`

#### Execution Options (`ExecutionOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/exec/ExecutionOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 34 | **Honored:** 1 | **Parsed but Ignored:** 3 | **Unknown/Missing:** 30
- **Description:** Action execution and strategy flags. Zel recognizes `subcommands` (honored), and parses `spawn_strategy`, `test_output`, `verbose_failures` (all 3 ignored).
- **[HONORED] (1):** `--subcommands`
- **[STUB / IGNORED] (3):** `--spawn_strategy`, `--test_output`, `--verbose_failures`
- **[MISSING] (30):** `--allow_one_action_on_resource_unavailable`, `--allowed_strategies_by_exec_platform`, `--cache_computed_file_digests`, `--check_tests_up_to_date`, `--check_up_to_date`, `--execution_log_binary_file`, `--execution_log_compact_file`, `--execution_log_json_file`, `--execution_log_mnemonic_filter`, `--execution_log_sort`, `--expand_param_files`, `--experimental_cpu_load_scheduling`, `--experimental_cpu_load_scheduling_window_size`, `--experimental_enable_critical_path_profiling`, `--experimental_materialize_param_files_directly`, `--experimental_memory_load_scheduling`, `--experimental_remote_cache_eviction_retries`, `--experimental_stats_summary`, `--flaky_test_attempts`, `--genrule_strategy`, `--local_resources`, `--local_test_jobs`, `--materialize_param_files`, `--max_test_output_bytes`, `--strategy`, `--strategy_regexp`, `--test_keep_going`, `--test_strategy`, `--test_summary`, `--test_tmpdir`

#### Core Build Configuration Options (`CoreOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/analysis/config/CoreOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 64 | **Honored:** 2 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 62
- **Description:** Core target configuration options (CPU, compilation mode, defines, action environment, stamps). Zel honors `compilation_mode` and `define`.
- **[HONORED] (2):** `--compilation_mode`, `--define`
- **[MISSING] (62):** `--action_env`, `--allow_analysis_failures`, `--allow_unresolved_symlinks`, `--allowed_cpu_values`, `--analysis_testing_deps_limit`, `--archived_tree_artifact_mnemonics_filter`, `--build_runfile_links`, `--build_runfile_manifests`, `--check_licenses`, `--check_visibility`, `--collect_code_coverage`, `--cpu`, `--enable_runfiles`, `--enforce_constraints`, `--evaluating for analysis test`, `--exec_aspects`, `--experimental_action_listener`, `--experimental_allow_map_directory`, `--experimental_collect_code_coverage_for_generated_files`, `--experimental_debug_selects_always_succeed`, `--experimental_enforce_transitive_visibility`, `--experimental_exclude_defines_from_exec_config`, `--experimental_exec_config`, `--experimental_extended_sanity_checks`, `--experimental_output_paths`, `--experimental_propagate_custom_flag`, `--experimental_remotable_source_manifests`, `--experimental_throttle_action_cache_check`, `--experimental_use_platforms_in_output_dir_legacy_heuristic`, `--experimental_writable_outputs`, `--features`, `--flag_alias`, `--host_action_env`, `--host_compilation_mode`, `--host_cpu`, `--host_features`, `--include_config_fragments_provider`, `--incompatible_always_include_files_in_data`, `--incompatible_auto_exec_groups`, `--incompatible_bazel_test_exec_run_under`, `--incompatible_bep_cpu_from_platform`, `--incompatible_check_testonly_for_output_files`, `--incompatible_compact_repo_mapping_manifest`, `--incompatible_disable_select_on`, `--incompatible_exclude_starlark_flags_from_exec_config`, `--incompatible_filegroup_runfiles_for_data`, `--incompatible_limit_platforms_in_output_dir_to`, `--incompatible_merge_genfiles_directory`, `--incompatible_prefer_depending_configuration_runfiles`, `--incompatible_target_cpu_from_platform`, `--instrument_test_targets`, `--instrumentation_filter`, `--is exec configuration`, `--min_param_file_size`, `--modify_execution_info`, `--override_platform_cpu_name`, `--platform_suffix`, `--run_under`, `--scl_config`, `--stamp`, `--target_environment`, `--verbose_visibility_errors`

#### Test Configuration Options (`TestOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/.../TestOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 19 | **Honored:** 0 | **Parsed but Ignored:** 1 | **Unknown/Missing:** 18
- **Description:** Test filtering, timeouts, sharding, and caching. Zel parses `test_filter` into `opts.test_filter` but never reads it—all tests execute unfiltered.
- **[STUB / IGNORED] (1):** `--test_filter`
- **[MISSING] (18):** `--allow_local_tests`, `--cache_test_results`, `--coverage_support`, `--default_test_resources`, `--experimental_cancel_concurrent_tests`, `--experimental_fetch_all_coverage_outputs`, `--experimental_retain_test_configuration_across_testonly`, `--experimental_split_coverage_postprocessing`, `--incompatible_exclusive_test_sandboxed`, `--runs_per_test`, `--runs_per_test_detects_flakes`, `--test_arg`, `--test_env`, `--test_runner_fail_fast`, `--test_sharding_strategy`, `--test_timeout`, `--trim_test_configuration`, `--zip_undeclared_test_outputs`

#### Test Summary Options (`TestSummaryOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/TestSummaryOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 3 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 3
- **Description:** Test summary verbosity and formatting.
- **[MISSING] (3):** `--print_relative_test_log_paths`, `--test_verbose_timeout_warnings`, `--verbose_test_summary`

#### Remote Execution & Caching Options (`RemoteOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/remote/options/RemoteOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 64 | **Honored:** 0 | **Parsed but Ignored:** 1 | **Unknown/Missing:** 63
- **Description:** gRPC remote execution, cache endpoints, headers, and local disk cache. Zel parses `disk_cache` into `_disk_cache` but never uses it during action execution.
- **[STUB / IGNORED] (1):** `--disk_cache`
- **[MISSING] (63):** `--bep_maximum_open_remote_upload_files`, `--disk_cache_gc_idle_delay`, `--disk_cache_gc_max_age`, `--disk_cache_gc_max_size`, `--experimental_circuit_breaker_strategy`, `--experimental_remote_cache_chunking`, `--experimental_remote_cache_chunking_function`, `--experimental_remote_cache_compression_threshold`, `--experimental_remote_cache_lease_extension`, `--experimental_remote_capture_corrupted_outputs`, `--experimental_remote_circuit_breaker_recovery_delay`, `--experimental_remote_discard_merkle_trees`, `--experimental_remote_execution_keepalive`, `--experimental_remote_failure_rate_threshold`, `--experimental_remote_failure_window_interval`, `--experimental_remote_mark_tool_inputs`, `--experimental_remote_min_call_count_to_compute_failure_rate`, `--experimental_remote_min_fail_count_to_compute_failure_rate`, `--experimental_remote_output_service`, `--experimental_remote_output_service_output_path_prefix`, `--experimental_remote_require_cached`, `--experimental_remote_scrubbing_config`, `--experimental_throttle_remote_action_building`, `--guard_against_concurrent_changes`, `--incompatible_remote_local_fallback_for_remote_cache`, `--max outbound message size`, `--remote_accept_cached`, `--remote_build_event_upload`, `--remote_bytestream_uri_prefix`, `--remote_cache`, `--remote_cache_async`, `--remote_cache_compression`, `--remote_cache_header`, `--remote_default_exec_properties`, `--remote_download_all`, `--remote_download_minimal`, `--remote_download_outputs`, `--remote_download_symlink_template`, `--remote_download_toplevel`, `--remote_downloader`, `--remote_downloader_header`, `--remote_downloader_local_fallback`, `--remote_downloader_propagate_credentials`, `--remote_exec_header`, `--remote_execution_priority`, `--remote_executor`, `--remote_grpc_download_idle_timeout`, `--remote_grpc_log`, `--remote_grpc_service_config`, `--remote_header`, `--remote_instance_name`, `--remote_local_fallback`, `--remote_local_fallback_strategy`, `--remote_max_concurrency_per_connection`, `--remote_max_connections`, `--remote_print_execution_messages`, `--remote_proxy`, `--remote_result_cache_priority`, `--remote_retries`, `--remote_retry_max_delay`, `--remote_timeout`, `--remote_upload_local_results`, `--remote_verify_downloads`

#### Authentication & TLS Options (`AuthAndTLSOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/authandtls/AuthAndTLSOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 16 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 16
- **Description:** gRPC and HTTP TLS credentials, client certs, and authentication headers.
- **[MISSING] (16):** `--credential_helper`, `--credential_helper_cache_duration`, `--credential_helper_timeout`, `--google_auth_scopes`, `--google_credentials`, `--google_default_credentials`, `--grpc_keepalive_time`, `--grpc_keepalive_timeout`, `--grpc_tcp_keepalive`, `--grpc_tcp_keepalive_count`, `--grpc_tcp_keepalive_interval`, `--grpc_tcp_keepalive_time`, `--tls_authority_override`, `--tls_certificate`, `--tls_client_certificate`, `--tls_client_key`

#### Remote Analysis Caching Options (`RemoteAnalysisCachingOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/skyframe/serialization/analysis/RemoteAnalysisCachingOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 10 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 10
- **Description:** Remote caching of analysis-phase Skyframe nodes.
- **[MISSING] (10):** `--experimental_analysis_cache_bail_on_missing_fingerprint`, `--experimental_analysis_cache_key_distinguisher_for_testing`, `--experimental_analysis_cache_server_checksum_override`, `--experimental_remote_analysis_cache_mode`, `--experimental_skycache_analysis_only`, `--experimental_skycache_check_nested_set_owner`, `--experimental_skycache_minimize_memory`, `--remote_analysis_cache_emit_bep_upload_events`, `--serialized_frontier_profile`, `--skycache_interactive_messages`

#### Remote Analysis Caching Services Options (`RemoteAnalysisCachingServicesOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/skyframe/serialization/analysis/RemoteAnalysisCachingServicesOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 14 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 14
- **Description:** Endpoints and services for remote analysis cache.
- **[MISSING] (14):** `--experimental_analysis_cache_enable_metadata_queries`, `--experimental_analysis_cache_service`, `--experimental_remote_analysis_cache_concurrency`, `--experimental_remote_analysis_cache_cpu_concurrency`, `--experimental_remote_analysis_cache_deadline`, `--experimental_remote_analysis_cache_keep_services_on_for_testing`, `--experimental_remote_analysis_cache_max_batch_size`, `--experimental_remote_analysis_cache_max_in_flight_read_requests`, `--experimental_remote_analysis_cache_max_write_concurrency`, `--experimental_remote_analysis_cache_reader_max_batch_size`, `--experimental_remote_analysis_cache_storage`, `--experimental_remote_analysis_cache_target_write_concurrency`, `--experimental_remote_analysis_write_proxy`, `--remote_analysis_debug_entries`

#### Common Remote Options (`CommonRemoteOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/remote/options/CommonRemoteOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 2 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 2
- **Description:** Common remote execution flags.
- **[MISSING] (2):** `--experimental_remote_cache_ttl`, `--remote_download_regex`

#### Sandbox Options (`SandboxOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/sandbox/SandboxOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 26 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 26
- **Description:** Sandboxing strategies (Linux namespaces, macOS sandbox-exec, Docker, tmpfs mounts, network isolation). Zel provides zero sandboxing options.
- **[MISSING] (26):** `--experimental_docker_image`, `--experimental_docker_privileged`, `--experimental_docker_use_customized_images`, `--experimental_docker_verbose`, `--experimental_enable_docker_sandbox`, `--experimental_inmemory_sandbox_stashes`, `--experimental_sandbox_async_tree_delete_idle_threads`, `--experimental_sandbox_enforce_resources_regexp`, `--experimental_sandbox_limits`, `--experimental_sandbox_memory_limit_mb`, `--experimental_sandboxfs_map_symlink_targets`, `--experimental_use_hermetic_linux_sandbox`, `--experimental_use_windows_sandbox`, `--experimental_windows_sandbox_path`, `--reuse_sandbox_directories`, `--sandbox_add_mount_pair`, `--sandbox_base`, `--sandbox_block_path`, `--sandbox_debug`, `--sandbox_default_allow_network`, `--sandbox_enable_loopback_device`, `--sandbox_explicit_pseudoterminal`, `--sandbox_fake_hostname`, `--sandbox_fake_username`, `--sandbox_tmpfs_path`, `--sandbox_writable_path`

#### Local Execution Options (`LocalExecutionOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/exec/local/LocalExecutionOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 5 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 5
- **Description:** Local execution resource allocation and strategy fallbacks.
- **[MISSING] (5):** `--allowed_local_actions_regex`, `--experimental_local_lockfree_output`, `--experimental_local_retries_on_crash`, `--experimental_process_wrapper_graceful_sigterm`, `--local_termination_grace_seconds`

#### Dynamic Execution Options (`DynamicExecutionOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/dynamic/DynamicExecutionOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 11 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 11
- **Description:** Dynamic local/remote race execution strategies.
- **[MISSING] (11):** `--debug_spawn_scheduler`, `--dynamic_local_execution_delay`, `--dynamic_local_strategy`, `--dynamic_remote_strategy`, `--experimental_dynamic_cancel_remote_branch_on_local_win`, `--experimental_dynamic_exclude_tools`, `--experimental_dynamic_ignore_local_signals`, `--experimental_dynamic_local_load_factor`, `--experimental_dynamic_slow_remote_time`, `--experimental_spawn_scheduler`, `--internal_spawn_scheduler`

#### Persistent Worker Options (`WorkerOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/worker/WorkerOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 17 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 17
- **Description:** Persistent worker processes, multiplex workers, worker sandboxing.
- **[MISSING] (17):** `--experimental_shrink_worker_pool`, `--experimental_total_worker_memory_limit_mb`, `--experimental_worker_allowlist`, `--experimental_worker_cancellation`, `--experimental_worker_memory_limit_mb`, `--experimental_worker_metrics_poll_interval`, `--experimental_worker_multiplex_sandboxing`, `--experimental_worker_sandbox_hardening`, `--experimental_worker_sandbox_inmemory_tracking`, `--experimental_worker_strict_flagfiles`, `--experimental_worker_use_cgroups_on_linux`, `--worker_max_instances`, `--worker_max_multiplex_instances`, `--worker_multiplex`, `--worker_quit_after_build`, `--worker_sandboxing`, `--worker_verbose`

#### Build Language (Starlark) Options (`BuildLanguageOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/packages/semantics/BuildLanguageOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 68 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 68
- **Description:** Starlark language semantics, incompatible change flags, builtins injection.
- **[MISSING] (68):** `--add_go_exec_groups_to_binary_rules`, `--allow_experimental_loads`, `--bzl_file_size_limit_allowlist`, `--check_bzl_visibility`, `--experimental_builtins_bzl_path`, `--experimental_builtins_dummy`, `--experimental_builtins_injection_override`, `--experimental_bzl_visibility`, `--experimental_cc_shared_library`, `--experimental_cc_skylark_api_enabled_packages`, `--experimental_dormant_deps`, `--experimental_enable_android_migration_apis`, `--experimental_enable_first_class_macros`, `--experimental_enable_starlark_set`, `--experimental_google_legacy_api`, `--experimental_isolated_extension_usages`, `--experimental_platforms_api`, `--experimental_repo_remote_exec`, `--experimental_repository_ctx_execute_wasm`, `--experimental_repository_ctx_wasm_compilation`, `--experimental_rule_extension_api`, `--experimental_sibling_repository_layout`, `--experimental_single_package_toolchain_binding`, `--experimental_starlark_dynamic_type_checking`, `--experimental_starlark_static_type_checking`, `--experimental_starlark_type_checking`, `--experimental_starlark_type_syntax`, `--experimental_starlark_types`, `--experimental_starlark_types_allowed_paths`, `--force_starlark_stack_trace`, `--incompatible_allow_tags_propagation`, `--incompatible_always_check_depset_elements`, `--incompatible_check_external_repo_source_dir_package_boundary`, `--incompatible_disable_non_executable_java_binary`, `--incompatible_disable_objc_library_transition`, `--incompatible_disable_starlark_host_transitions`, `--incompatible_disable_target_default_provider_fields`, `--incompatible_disable_transitions_on`, `--incompatible_disallow_ctx_resolve_tools`, `--incompatible_disallow_empty_glob`, `--incompatible_enable_deprecated_label_apis`, `--incompatible_enable_proto_toolchain_resolution`, `--incompatible_enforce_starlark_utf8`, `--incompatible_existing_rules_default_omit_macro_internals`, `--incompatible_fail_on_unknown_attributes`, `--incompatible_fix_package_group_reporoot_syntax`, `--incompatible_java_info_merge_runtime_module_flags`, `--incompatible_locations_prefers_executable`, `--incompatible_no_attr_license`, `--incompatible_no_implicit_file_export`, `--incompatible_no_rule_outputs_param`, `--incompatible_package_group_has_public_syntax`, `--incompatible_require_matching_aspect_hints_providers`, `--incompatible_require_mnemonic_for_run_actions`, `--incompatible_resolve_select_keys_eagerly`, `--incompatible_run_shell_command_string`, `--incompatible_simplify_unconditional_selects_in_rule_attrs`, `--incompatible_stop_exporting_build_file_path`, `--incompatible_symbolic_macro_strict_attrs`, `--incompatible_unambiguous_label_stringification`, `--internal_starlark_flag_test_canary`, `--internal_starlark_utf_8_byte_strings`, `--max_bzl_file_size`, `--max_computation_steps`, `--nested_set_count_limit`, `--nested_set_count_limit_allowlist`, `--nested_set_depth_limit`, `--soft_max_bzl_file_size`

#### Loading Phase Options (`LoadingOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/pkgcache/LoadingOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 10 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 10
- **Description:** Target pattern parsing, test suite expansion, tag filters (`build_tag_filters`), manual test exclusion.
- **[MISSING] (10):** `--build_manual_tests`, `--build_tag_filters`, `--build_tests_only`, `--compile_one_dependency`, `--expand_test_suites`, `--experimental_skyframe_target_pattern_evaluator`, `--test_lang_filters`, `--test_size_filters`, `--test_tag_filters`, `--test_timeout_filters`

#### Package Loading Options (`PackageOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/pkgcache/PackageOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 12 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 12
- **Description:** Package specification, default visibility, BUILD file names.
- **[MISSING] (12):** `--default_visibility`, `--deleted_packages`, `--experimental_check_external_other_files`, `--experimental_check_output_files`, `--experimental_lazy_macro_expansion_packages`, `--experimental_max_directories_to_eagerly_visit_in_globbing`, `--fetch`, `--incompatible_config_setting_private_default_visibility`, `--incompatible_enforce_config_setting_visibility`, `--legacy_globbing_threads`, `--package_path`, `--show_loading_progress`

#### UI and Terminal Options (`UiOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/UiOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 15 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 15
- **Description:** Terminal progress bars, ANSI colors, curses, event rate limits.
- **[MISSING] (15):** `--attempt_to_print_relative_paths`, `--color`, `--curses`, `--emacs`, `--experimental_ui_debug_all_events`, `--experimental_ui_max_stdouterr_bytes`, `--isatty`, `--progress_in_terminal_title`, `--show_progress`, `--show_progress_rate_limit`, `--show_timestamps`, `--terminal_columns`, `--terminal_hyperlinks`, `--ui_actions_shown`, `--ui_event_filters`

#### Build Event Protocol Options (`BuildEventProtocolOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/buildeventstream/BuildEventProtocolOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 9 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 9
- **Description:** BEP transport options and upload parameters.
- **[MISSING] (9):** `--build_event_upload_max_retries`, `--experimental_bep_target_summary`, `--experimental_build_event_expand_filesets`, `--experimental_build_event_output_group_mode`, `--experimental_build_event_upload_retry_minimum_delay`, `--experimental_build_event_upload_strategy`, `--experimental_run_bep_event_include_residue`, `--experimental_stream_log_file_uploads`, `--legacy_important_outputs`

#### Build Event Stream Options (`BuildEventStreamOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/buildeventstream/transports/BuildEventStreamOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 12 | **Honored:** 1 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 11
- **Description:** BEP file output destinations. Zel honors `build_event_json_file`.
- **[HONORED] (1):** `--build_event_json_file`
- **[MISSING] (11):** `--build_event_binary_file`, `--build_event_binary_file_path_conversion`, `--build_event_binary_file_upload_mode`, `--build_event_json_file_path_conversion`, `--build_event_json_file_upload_mode`, `--build_event_max_named_set_of_file_entries`, `--build_event_publish_all_actions`, `--build_event_text_file`, `--build_event_text_file_path_conversion`, `--build_event_text_file_upload_mode`, `--keep_backend_build_event_connections_alive`

#### Build Event Service (BES) Options (`BuildEventServiceOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/buildeventservice/BuildEventServiceOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 15 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 15
- **Description:** Streaming BEP to remote endpoints (e.g. ResultStore, BuildBuddy).
- **[MISSING] (15):** `--bes_backend`, `--bes_check_preceding_lifecycle_events`, `--bes_header`, `--bes_instance_name`, `--bes_keywords`, `--bes_lifecycle_events`, `--bes_oom_finish_upload_timeout`, `--bes_outerr_buffer_size`, `--bes_outerr_chunk_size`, `--bes_proxy`, `--bes_results_url`, `--bes_system_keywords`, `--bes_timeout`, `--bes_upload_mode`, `--experimental_memory_profile_await_bes_quiescence`

#### Common Query Options (`CommonQueryOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/query2/common/CommonQueryOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 29 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 29
- **Description:** Query output formatters, universe scope, edge filtering.
- **[MISSING] (29):** `--aspect_deps`, `--consistent_labels`, `--experimental_explicit_aspects`, `--graph:factored`, `--graph:node_limit`, `--implicit_deps`, `--include_aspects`, `--incompatible_package_group_build_output`, `--incompatible_package_group_includes_double_slash`, `--infer_universe_scope`, `--line_terminator_null`, `--nodep_deps`, `--null`, `--output_file`, `--proto:default_values`, `--proto:definition_stack`, `--proto:flatten_selects`, `--proto:include_attribute_source_aspects`, `--proto:include_starlark_rule_env`, `--proto:include_synthetic_attribute_hash`, `--proto:instantiation_stack`, `--proto:locations`, `--proto:output_rule_attrs`, `--proto:rule_classes`, `--proto:rule_inputs_and_outputs`, `--query_file`, `--relative_locations`, `--tool_deps`, `--universe_scope`

#### Query Options (`QueryOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/query2/query/output/QueryOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 12 | **Honored:** 1 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 11
- **Description:** Standard `bazel query` options. Zel honors `output` (`--output={label,label_kind,package,build}`).
- **[HONORED] (1):** `--output`
- **[MISSING] (11):** `--experimental_graphless_query`, `--experimental_parallel_streamed_proto_output`, `--graph:conditional_edges_limit`, `--incompatible_lexicographical_output`, `--noorder_results`, `--order_output`, `--order_results`, `--output:display_full_kind`, `--strict_test_suite`, `--xml:default_values`, `--xml:line_numbers`

#### Configured Query Options (`CqueryOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/query2/cquery/CqueryOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 6 | **Honored:** 1 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 5
- **Description:** Configured target graph evaluation (`cquery`).
- **[HONORED] (1):** `--output`
- **[MISSING] (5):** `--proto:include_configurations`, `--show_config_fragments`, `--starlark:expr`, `--starlark:file`, `--transitions`

#### Action Graph Query Options (`AqueryOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/query2/aquery/AqueryOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 8 | **Honored:** 1 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 7
- **Description:** Action graph evaluation (`aquery`).
- **[HONORED] (1):** `--output`
- **[MISSING] (7):** `--include_artifacts`, `--include_commandline`, `--include_file_write_contents`, `--include_param_files`, `--include_pruned_inputs`, `--prune_unused_actions`, `--skyframe_state`

#### Platform & Toolchain Options (`PlatformOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/analysis/PlatformOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 7 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 7
- **Description:** Host/target platforms, toolchain resolution, platform mappings.
- **[MISSING] (7):** `--extra_execution_platforms`, `--extra_toolchains`, `--host_platform`, `--incompatible_use_toolchain_resolution_for_java_rules`, `--platform_mappings`, `--platforms`, `--toolchain_resolution_debug`

#### Repository & External Deps Options (`RepositoryOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/bazel/repository/RepositoryOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 25 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 25
- **Description:** Bzlmod registries, repository cache, vendoring, override directories.
- **[MISSING] (25):** `--allow_yanked_versions`, `--check_bazel_compatibility`, `--check_direct_dependencies`, `--distdir`, `--downloader_config`, `--experimental_check_external_repository_files`, `--experimental_repository_cache_hardlinks`, `--experimental_repository_downloader_retries`, `--experimental_scale_timeouts`, `--ignore_dev_dependency`, `--incompatible_require_repo_extension_metadata`, `--inject_module`, `--inject_repository`, `--lockfile_mode`, `--module_mirrors`, `--override_module`, `--override_repository`, `--registry`, `--repo_contents_cache`, `--repo_contents_cache_gc_idle_delay`, `--repo_contents_cache_gc_max_age`, `--repository_cache`, `--repository_disable_download`, `--vendor_dir`, `--watch_registry`

#### Bzlmod Inspection Options (`ModOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/modcommand/ModOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 14 | **Honored:** 1 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 13
- **Description:** `bazel mod` inspection command options.
- **[HONORED] (1):** `--output`
- **[MISSING] (13):** `--all_repos`, `--all_visible_repos`, `--base_module`, `--charset`, `--cycles`, `--depth`, `--extension_filter`, `--extension_info`, `--extension_usages`, `--from`, `--include_builtin`, `--include_unused`, `--verbose`

#### Fetch Command Options (`FetchOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/bazel/commands/FetchOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 4 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 4
- **Description:** External repository fetching options.
- **[MISSING] (4):** `--all`, `--configure`, `--force`, `--repo`

#### Vendor Command Options (`VendorOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/bazel/commands/VendorOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 2 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 2
- **Description:** Repository vendoring options.
- **[MISSING] (2):** `--lockfile_deps`, `--repo`

#### Clean Command Options (`CleanCommand.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/commands/CleanCommand.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 3 | **Honored:** 2 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 1
- **Description:** Clean options (`expunge`, `expunge_async`, `async`). Zel honors `expunge` and `expunge_async`.
- **[HONORED] (2):** `--expunge`, `--expunge_async`
- **[MISSING] (1):** `--async`

#### Info Command Options (`InfoCommand.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/commands/InfoCommand.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 2 | **Honored:** 0 | **Parsed but Ignored:** 1 | **Unknown/Missing:** 1
- **Description:** Runtime info keys. Zel parses `show_make_env` but ignores it.
- **[STUB / IGNORED] (1):** `--show_make_env`
- **[MISSING] (1):** `--info_output_type`

#### Run Command Options (`RunOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/.../RunOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 7 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 7
- **Description:** Execution of built binaries.
- **[MISSING] (7):** `--emit_script_path_in_exec_request`, `--omit_run_args`, `--portable_paths`, `--run`, `--run_env`, `--run_in_cwd`, `--script_path`

#### Dump Command Options (`DumpOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/.../DumpOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 10 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 10
- **Description:** Internal state inspection (skyframe, memory, rules, action cache).
- **[MISSING] (10):** `--action_cache`, `--memory`, `--packages`, `--rule_classes`, `--rules`, `--skycache_cache`, `--skycache_fingerprint`, `--skyframe`, `--skykey_filter`, `--skylark_memory`

#### Canonicalize Flags Options (`CanonicalizeCommand.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/commands/CanonicalizeCommand.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 4 | **Honored:** 1 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 3
- **Description:** Flag canonicalization options. Zel honors `for_command`.
- **[HONORED] (1):** `--for_command`
- **[MISSING] (3):** `--canonicalize_policy`, `--experimental_include_default_values`, `--invocation_policy`

#### C++ Rule Options (`CppOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/rules/cpp/CppOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 69 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 69
- **Description:** C++ toolchain, optimization flags, debug symbols, PIC, sanitizers, dynamic linking.
- **[MISSING] (69):** `--apple_generate_dsym`, `--build_test_dwp`, `--cc_dotd_files`, `--cc_include_scanning`, `--cc_output_directory_tag`, `--compiler`, `--conlyopt`, `--copt`, `--crosstool_top`, `--cs_fdo_absolute_path`, `--cs_fdo_instrument`, `--cs_fdo_profile`, `--custom_malloc`, `--cxxopt`, `--dynamic_mode`, `--enable_propeller_optimize_absolute_paths`, `--enable_remaining_fdo_absolute_paths`, `--experimental_cpp_compile_resource_estimation`, `--experimental_cpp_modules`, `--experimental_generate_llvm_lcov`, `--experimental_inmemory_dotd_files`, `--experimental_link_static_libraries_once`, `--experimental_omitfp`, `--experimental_save_feature_state`, `--experimental_unsupported_and_brittle_include_scanning`, `--experimental_use_cpp_compile_action_args_params_file`, `--experimental_use_llvm_covmap`, `--fdo_instrument`, `--fdo_optimize`, `--fdo_prefetch_hints`, `--fdo_profile`, `--fission`, `--force_pic`, `--grte_top`, `--host_conlyopt`, `--host_copt`, `--host_cxxopt`, `--host_grte_top`, `--host_linkopt`, `--host_per_file_copt`, `--incompatible_dont_enable_host_nonhost_crosstool_features`, `--incompatible_remove_legacy_whole_archive`, `--incompatible_use_cpp_compile_header_mnemonic`, `--interface_shared_objects`, `--legacy_whole_archive`, `--linkopt`, `--ltobackendopt`, `--ltoindexopt`, `--memprof_profile`, `--minimum_os_version`, `--objc_enable_binary_stripping`, `--objc_generate_linkmap`, `--objc_use_dotd_pruning`, `--objccopt`, `--per_file_copt`, `--per_file_ltobackendopt`, `--process_headers_in_dependencies`, `--propeller_optimize`, `--propeller_optimize_absolute_cc_profile`, `--propeller_optimize_absolute_ld_profile`, `--proto_profile`, `--proto_profile_path`, `--save_temps`, `--share_native_deps`, `--start_end_lib`, `--strict_system_includes`, `--strip`, `--stripopt`, `--xbinary_fdo`

#### Java Rule Options (`JavaOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/rules/java/JavaOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 35 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 35
- **Description:** Javac options, JVM flags, bytecode optimization, dependency checking.
- **[MISSING] (35):** `--bytecode_optimization_pass_actions`, `--bytecode_optimizers`, `--enforce_proguard_file_extension`, `--experimental_enable_jspecify`, `--experimental_fix_deps_tool`, `--experimental_inmemory_jdeps_files`, `--experimental_java_classpath`, `--experimental_local_java_optimization_configuration`, `--experimental_local_java_optimizations`, `--experimental_one_version_enforcement`, `--experimental_run_android_lint_on_java_rules`, `--experimental_strict_java_deps`, `--experimental_turbine_annotation_processing`, `--experimental_turbine_cpu_reservation`, `--experimental_turbine_output_root_relative_path`, `--explicit_java_test_deps`, `--host_java_launcher`, `--host_javacopt`, `--host_jvmopt`, `--incompatible_disallow_java_import_exports`, `--java_debug`, `--java_deps`, `--java_header_compilation`, `--java_language_version`, `--java_launcher`, `--java_runtime_version`, `--javacopt`, `--jvmopt`, `--one_version_enforcement_on_java_tests`, `--plugin`, `--proguard_top`, `--split_bytecode_optimization_pass`, `--tool_java_language_version`, `--tool_java_runtime_version`, `--use_ijars`

#### Apple Rule Options (`AppleCommandLineOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/rules/apple/AppleCommandLineOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 17 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 17
- **Description:** macOS/iOS SDK versions, CPU architectures, Xcode toolchain selection.
- **[MISSING] (17):** `--apple_platform_type`, `--apple_split_cpu`, `--experimental_include_xcode_execution_requirements`, `--experimental_prefer_mutual_xcode`, `--host_macos_minimum_os`, `--incompatible_remove_ctx_apple_fragment`, `--ios_minimum_os`, `--ios_multi_cpus`, `--macos_cpus`, `--macos_minimum_os`, `--tvos_cpus`, `--tvos_minimum_os`, `--visionos_cpus`, `--watchos_cpus`, `--watchos_minimum_os`, `--xcode_version`, `--xcode_version_config`

#### Objective-C Options (`ObjcCommandLineOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/rules/objc/ObjcCommandLineOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 6 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 6
- **Description:** Objective-C compilation flags.
- **[MISSING] (6):** `--device_debug_entitlements`, `--incompatible_builtin_objc_strip_action`, `--incompatible_disallow_sdk_frameworks_attributes`, `--incompatible_objc_alwayslink_by_default`, `--incompatible_remove_ctx_objc_fragment`, `--incompatible_strip_executable_safely`

#### Protocol Buffer Rule Options (`ProtoConfiguration.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/rules/proto/ProtoConfiguration.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 10 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 10
- **Description:** Protobuf toolchain and compiler options.
- **[MISSING] (10):** `--cc_proto_library_header_suffixes`, `--cc_proto_library_source_suffixes`, `--experimental_proto_descriptor_sets_include_source_info`, `--proto_compiler`, `--proto_toolchain_for_cc`, `--proto_toolchain_for_java`, `--proto_toolchain_for_javalite`, `--protocopt`, `--strict_proto_deps`, `--strict_public_imports`

#### Android Rule Options (`AndroidConfiguration.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/rules/android/AndroidConfiguration.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 24 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 24
- **Description:** Android SDK/NDK, DEX, AAPT2, manifest merging.
- **[MISSING] (24):** `--Android configuration distinguisher`, `--android_fixed_resource_neverlinking`, `--android_manifest_merger_order`, `--android_platforms`, `--android_resource_shrinking`, `--apk_signing_method`, `--desugar_for_android`, `--desugar_java8_libs`, `--dexopts_supported_in_dexsharder`, `--experimental_android_compress_java_resources`, `--experimental_android_library_exports_manifest_default`, `--experimental_android_resource_cycle_shrinking`, `--experimental_android_resource_name_obfuscation`, `--experimental_android_resource_path_shortening`, `--experimental_android_resource_shrinking`, `--experimental_get_android_java_resources_from_optimized_jar`, `--experimental_incremental_dexing_after_proguard`, `--incompatible_remove_ctx_android_fragment`, `--internal_persistent_android_dex_desugar`, `--internal_persistent_busybox_tools`, `--internal_persistent_multiplex_android_dex_desugar`, `--internal_persistent_multiplex_busybox_tools`, `--legacy_main_dex_list_generator`, `--optimizing_dexer`

#### Build Graveyard Options (`BuildGraveyardOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/.../BuildGraveyardOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 66 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 66
- **Description:** Deprecated/removed build flags preserved for compatibility.
- **[MISSING] (66):** `--action_cache_store_output_metadata`, `--announce`, `--apple_platforms`, `--build_python_zip`, `--check_fileset_dependencies_recursively`, `--collapse_duplicate_defines`, `--defer_param_files`, `--discard_actions_after_execution`, `--enable_fdo_profile_absolute_path`, `--experimental_active_directories`, `--experimental_build_transitive_python_runfiles`, `--experimental_cc_implementation_deps`, `--experimental_collect_local_action_metrics`, `--experimental_collect_local_sandbox_action_metrics`, `--experimental_enable_skyfocus`, `--experimental_enable_starlark_doc_extract`, `--experimental_execution_graph_include_change_pruned_actions`, `--experimental_frontier_violation_check`, `--experimental_frontier_violation_verbose`, `--experimental_genquery_use_graphless_query`, `--experimental_objc_provider_from_linked`, `--experimental_parallel_aquery_output`, `--experimental_parse_headers_skipped_if_corresponding_srcs_found`, `--experimental_platform_cc_test`, `--experimental_platform_in_output_dir`, `--experimental_proto_extra_actions`, `--experimental_remote_analysis_cache`, `--experimental_remote_analysis_unreachable_cache_retry_interval`, `--experimental_require_availability_info`, `--experimental_show_artifacts`, `--experimental_skip_ttvs_for_genquery`, `--experimental_skyfocus_dump_keys`, `--experimental_skyfocus_dump_post_gc_stats`, `--experimental_skyfocus_handling_strategy`, `--experimental_skyframe_native_filesets`, `--experimental_skyframe_prepare_analysis`, `--experimental_starlark_cc_import`, `--experimental_use_new_worker_pool`, `--experimental_use_semaphore_for_jobs`, `--experimental_worker_as_resource`, `--high_priority_workers`, `--host_crosstool_top`, `--incompatible_auto_configure_host_platform`, `--incompatible_default_to_explicit_init_py`, `--incompatible_disable_legacy_flags_cc_toolchain_api`, `--incompatible_disable_nocopts`, `--incompatible_enable_apple_toolchain_resolution`, `--incompatible_enable_profile_by_default`, `--incompatible_modify_execution_info_additive`, `--incompatible_multi_release_deploy_jars`, `--incompatible_override_toolchain_transition`, `--incompatible_python_disallow_native_rules`, `--incompatible_remove_ctx_py_fragment`, `--incompatible_require_javaplugininfo_in_javacommon`, `--incompatible_use_new_cgroup_implementation`, `--incompatible_use_python_toolchains`, `--j2objc_dead_code_removal`, `--j2objc_dead_code_report`, `--j2objc_translation_flags`, `--mode`, `--python_native_rules_allowlist`, `--remote_analysis_json_log`, `--target_platform_fallback`, `--use_platforms_in_apple_crosstool_transition`, `--use_target_platform_for_tests`, `--use_top_level_targets_for_symlinks`

#### Bazel Build Graveyard Options (`BazelBuildGraveyardOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/.../BazelBuildGraveyardOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 27 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 27
- **Description:** Deprecated Bazel-specific build flags.
- **[MISSING] (27):** `--android_cpu`, `--android_crosstool_top`, `--android_grte_top`, `--android_sdk`, `--experimental_add_test_support_to_compile_time_deps`, `--experimental_java_test_auto_create_deploy_jar`, `--experimental_post_profile_started_event`, `--experimental_profile_action_counts`, `--experimental_python_import_all_repositories`, `--fat_apk_cpu`, `--force_ignore_dash_static`, `--host_compiler`, `--incompatible_disable_legacy_cc_provider`, `--incompatible_disable_native_apple_binary_rule`, `--incompatible_dont_use_javasourceinfoprovider`, `--incompatible_enable_cgo_toolchain_resolution`, `--incompatible_load_java_rules_from_bzl`, `--incompatible_load_proto_rules_from_bzl`, `--incompatible_load_python_rules_from_bzl`, `--incompatible_make_thinlto_command_lines_standalone`, `--incompatible_remove_binary_profile`, `--incompatible_require_ctx_in_configure_features`, `--incompatible_use_specific_tool_files`, `--incompatible_validate_top_level_header_inclusions`, `--make_variables_source`, `--python_path`, `--python_top`

#### All-Command Graveyard Options (`AllCommandGraveyardOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/.../AllCommandGraveyardOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 63 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 63
- **Description:** Deprecated flags preserved across all commands.
- **[MISSING] (63):** `--apple_crosstool_top`, `--auto_cpu_environment_group`, `--bes_best_effort`, `--disallow_strict_deps_for_jpl`, `--enable_bzlmod`, `--enable_workspace`, `--experimental_action_resource_set`, `--experimental_allow_runtime_deps_on_neverlink`, `--experimental_announce_profile_path`, `--experimental_cc_static_library`, `--experimental_disallow_legacy_java_toolchain_flags`, `--experimental_enable_aspect_hints`, `--experimental_enable_scl_dialect`, `--experimental_import_deps_checking`, `--experimental_java_header_compilation_direct_deps`, `--experimental_java_header_input_pruning`, `--experimental_java_proto_add_allowed_public_imports`, `--experimental_limit_android_lint_to_android_constrained_java`, `--experimental_py_binaries_include_label`, `--experimental_use_event_based_build_completion_status`, `--force_python`, `--host_force_python`, `--host_java_toolchain`, `--host_javabase`, `--incompatible_allow_python_version_transitions`, `--incompatible_autoload_externally`, `--incompatible_depset_for_java_output_source_jars`, `--incompatible_depset_for_libraries_to_link_getter`, `--incompatible_disable_autoloads_in_main_repo`, `--incompatible_disable_native_repo_rules`, `--incompatible_disable_target_provider_fields`, `--incompatible_disallow_legacy_py_provider`, `--incompatible_disallow_struct_provider_syntax`, `--incompatible_dont_collect_native_libraries_in_data`, `--incompatible_enable_cc_toolchain_resolution`, `--incompatible_existing_rules_immutable_view`, `--incompatible_legacy_local_fallback`, `--incompatible_macos_set_install_name`, `--incompatible_new_actions_api`, `--incompatible_no_implicit_watch_label`, `--incompatible_no_package_distribs`, `--incompatible_py2_outputs_are_suffixed`, `--incompatible_py3_is_default`, `--incompatible_python_disable_py2`, `--incompatible_remove_old_python_version_api`, `--incompatible_repo_env_ignores_action_env`, `--incompatible_require_linker_input_cc_api`, `--incompatible_top_level_aspects_require_providers`, `--incompatible_use_cc_configure_from_rules_cc`, `--incompatible_use_plus_in_repo_names`, `--incompatible_visibility_private_attributes_at_definition`, `--java_optimization_mode`, `--java_toolchain`, `--javabase`, `--jplPropagateCcLinkParamsStore`, `--legacy_bazel_java_test`, `--legacy_external_runfiles`, `--proto_toolchain_for_j2objc`, `--python_version`, `--repositories_without_autoloads`, `--separate_aspect_deps`, `--strict_deps_java_protos`, `--verbose_explanations`

#### Miscellaneous Options Classes

#### AdbOptions (`AdbOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/mobileinstall/AdbOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 7 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 7
- **Description:** Options class `AdbOptions`
- **[MISSING] (7):** `--adb`, `--adb_arg`, `--debug_app`, `--device`, `--incremental_install_verbosity`, `--start`, `--start_app`

#### AnalysisOptions (`AnalysisOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/analysis/AnalysisOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 11 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 11
- **Description:** Options class `AnalysisOptions`
- **[MISSING] (11):** `--allow_analysis_cache_discard`, `--discard_analysis_cache`, `--experimental_extra_action_filter`, `--experimental_extra_action_top_level_only`, `--experimental_keep_change_prunable_nodes_during_gc`, `--experimental_max_transitive_analysis_nodes`, `--experimental_oom_sensitive_skyfunctions_semaphore_size`, `--experimental_skyframe_cpu_heavy_skykeys_thread_pool_size`, `--max_config_changes_to_show`, `--skip_incompatible_explicit_targets`, `--version_window_for_dirty_node_gc`

#### BazelAndroidConfiguration.Options (`BazelAndroidConfiguration.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/rules/android/BazelAndroidConfiguration.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 1 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 1
- **Description:** Options class `BazelAndroidConfiguration.Options`
- **[MISSING] (1):** `--merge_android_manifest_permissions`

#### BazelCoverageReportModule.Options (`BazelCoverageReportModule.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/bazel/coverage/BazelCoverageReportModule.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 1 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 1
- **Description:** Options class `BazelCoverageReportModule.Options`
- **[MISSING] (1):** `--combined_report`

#### BazelStartupOptionsModule.Options (`BazelStartupOptionsModule.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/bazel/BazelStartupOptionsModule.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 4 | **Honored:** 2 | **Parsed but Ignored:** 2 | **Unknown/Missing:** 0
- **Description:** Options class `BazelStartupOptionsModule.Options`
- **[HONORED] (2):** `--bazelrc`, `--workspace_rc`
- **[STUB / IGNORED] (2):** `--home_rc`, `--system_rc`

#### BazelStrategyModule.Options (`BazelStrategyModule.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/bazel/rules/BazelStrategyModule.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 2 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 2
- **Description:** Options class `BazelStrategyModule.Options`
- **[MISSING] (2):** `--file_write_strategy`, `--template_expansion_strategy`

#### BuildEncyclopediaOptions (`BuildEncyclopediaOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/docgen/BuildEncyclopediaOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 8 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 8
- **Description:** Options class `BuildEncyclopediaOptions`
- **[MISSING] (8):** `--be_stardoc_proto`, `--denylist`, `--input_dir`, `--input_root`, `--output_dir`, `--output_file`, `--provider`, `--single_page`

#### ClientOptions (`ClientOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/ClientOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 3 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 3
- **Description:** Options class `ClientOptions`
- **[MISSING] (3):** `--client_env`, `--default_override`, `--rc_source`

#### CommandProfilerModule.Options (`CommandProfilerModule.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/profiler/CommandProfilerModule.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 1 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 1
- **Description:** Options class `CommandProfilerModule.Options`
- **[MISSING] (1):** `--experimental_command_profile`

#### CommonOptions (`CommonOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/docgen/CommonOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 4 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 4
- **Description:** Options class `CommonOptions`
- **[MISSING] (4):** `--api_stardoc_proto`, `--create_toc`, `--help`, `--link_map_path`

#### ConfigFeatureFlagOptions (`ConfigFeatureFlagOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/rules/config/ConfigFeatureFlagOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 2 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 2
- **Description:** Options class `ConfigFeatureFlagOptions`
- **[MISSING] (2):** `--all feature flag values are present (internal)`, `--enforce_transitive_configs_for_config_feature_flag`

#### ConfigOptions (`ConfigOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/.../ConfigOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 2 | **Honored:** 1 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 1
- **Description:** Options class `ConfigOptions`
- **[HONORED] (1):** `--output`
- **[MISSING] (1):** `--dump_all`

#### CoverageOptions (`CoverageOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/.../CoverageOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 2 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 2
- **Description:** Options class `CoverageOptions`
- **[MISSING] (2):** `--coverage_output_generator`, `--coverage_report_generator`

#### DebuggingOptions (`DebuggingOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/bazel/debug/DebuggingOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 1 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 1
- **Description:** Options class `DebuggingOptions`
- **[MISSING] (1):** `--experimental_workspace_rules_log_file`

#### ExecutionGraphOptions (`ExecutionGraphOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/.../ExecutionGraphOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 6 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 6
- **Description:** Options class `ExecutionGraphOptions`
- **[MISSING] (6):** `--execution_graph_log_queued_bytes_limit`, `--experimental_enable_execution_graph_log`, `--experimental_execution_graph_enable_edges_from_filewrite_actions`, `--experimental_execution_graph_log_dep_type`, `--experimental_execution_graph_log_path`, `--experimental_execution_graph_log_queue_size`

#### FlagClashCanaryOptions (`FlagClashCanaryOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/.../FlagClashCanaryOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 3 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 3
- **Description:** Options class `FlagClashCanaryOptions`
- **[MISSING] (3):** `--flag_clash_canary`, `--flag_clash_canary_expander1`, `--flag_clash_canary_expander2`

#### HelpCommand.Options (`HelpCommand.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/commands/HelpCommand.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 3 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 3
- **Description:** Options class `HelpCommand.Options`
- **[MISSING] (3):** `--help_verbosity`, `--long`, `--short`

#### KeepGoingOption (`KeepGoingOption`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/KeepGoingOption.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 1 | **Honored:** 1 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 0
- **Description:** Options class `KeepGoingOption`
- **[HONORED] (1):** `--keep_going`

#### KeepStateAfterBuildOption (`KeepStateAfterBuildOption`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/KeepStateAfterBuildOption.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 1 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 1
- **Description:** Options class `KeepStateAfterBuildOption`
- **[MISSING] (1):** `--keep_state_after_build`

#### LoadingPhaseThreadsOption (`LoadingPhaseThreadsOption`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/LoadingPhaseThreadsOption.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 1 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 1
- **Description:** Options class `LoadingPhaseThreadsOption`
- **[MISSING] (1):** `--loading_phase_threads`

#### LocalDiffAwareness.Options (`LocalDiffAwareness.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/skyframe/LocalDiffAwareness.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 2 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 2
- **Description:** Options class `LocalDiffAwareness.Options`
- **[MISSING] (2):** `--experimental_windows_watchfs`, `--watchfs`

#### MemoryPressureOptions (`MemoryPressureOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/MemoryPressureOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 8 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 8
- **Description:** Options class `MemoryPressureOptions`
- **[MISSING] (8):** `--gc_churning_threshold`, `--gc_churning_threshold_if_multiple_top_level_targets`, `--gc_thrashing_limits`, `--gc_thrashing_threshold`, `--jvm_heap_histogram_internal_object_pattern`, `--skyframe_high_water_mark_full_gc_drops_per_invocation`, `--skyframe_high_water_mark_minor_gc_drops_per_invocation`, `--skyframe_high_water_mark_threshold`

#### MetricsModule.Options (`MetricsModule.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/metrics/MetricsModule.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 2 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 2
- **Description:** Options class `MetricsModule.Options`
- **[MISSING] (2):** `--experimental_record_metrics_for_all_mnemonics`, `--experimental_record_skyframe_metrics`

#### MobileInstallCommand.Options (`MobileInstallCommand.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/mobileinstall/MobileInstallCommand.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 5 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 5
- **Description:** Options class `MobileInstallCommand.Options`
- **[MISSING] (5):** `--incremental`, `--mobile_install_aspect`, `--mobile_install_supported_rules`, `--run_in_client`, `--split_apks`
#### OutputFilteringModule.Options (`OutputFilteringModule.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/outputfilter/OutputFilteringModule.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 1 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 1
- **Description:** Options class `OutputFilteringModule.Options`
- **[MISSING] (1):** `--auto_output_filter`

#### PackageMetricsModule.Options (`PackageMetricsModule.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/packages/metrics/PackageMetricsModule.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 3 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 3
- **Description:** Options class `PackageMetricsModule.Options`
- **[MISSING] (3):** `--experimental_publish_package_metrics_in_bep`, `--log_top_n_packages`, `--record_metrics_for_all_packages`

#### PostGCMemoryUseRecorder.Options (`PostGCMemoryUseRecorder.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/metrics/PostGCMemoryUseRecorder.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 2 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 2
- **Description:** Options class `PostGCMemoryUseRecorder.Options`
- **[MISSING] (2):** `--experimental_force_gc_after_build`, `--experimental_force_gc_before_discard_analysis_cache`

#### PrintActionOptions (`PrintActionOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/.../PrintActionOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 1 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 1
- **Description:** Options class `PrintActionOptions`
- **[MISSING] (1):** `--print_action_mnemonics`

#### RemoteStartupOptions (`RemoteStartupOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/remote/options/RemoteStartupOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 1 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 1
- **Description:** Options class `RemoteStartupOptions`
- **[MISSING] (1):** `--experimental_remote_repo_contents_cache`

#### ShellConfiguration.Options (`ShellConfiguration.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/analysis/ShellConfiguration.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 1 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 1
- **Description:** Options class `ShellConfiguration.Options`
- **[MISSING] (1):** `--shell_executable`

#### ShutdownCommand.Options (`ShutdownCommand.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/commands/ShutdownCommand.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 1 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 1
- **Description:** Options class `ShutdownCommand.Options`
- **[MISSING] (1):** `--iff_heap_size_greater_than`

#### SkippedArgs (`SkippedArgs`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/.../SkippedArgs.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 1 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 1
- **Description:** Options class `SkippedArgs`
- **[MISSING] (1):** `--skipped args`

#### StarlarkDebuggerOptions (`StarlarkDebuggerOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/starlarkdebug/module/StarlarkDebuggerOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 5 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 5
- **Description:** Options class `StarlarkDebuggerOptions`
- **[MISSING] (5):** `--experimental_skylark_debug`, `--experimental_skylark_debug_reset_analysis`, `--experimental_skylark_debug_server_address`, `--experimental_skylark_debug_server_port`, `--experimental_skylark_debug_verbose_logging`

#### StarlarkDocumentationOptions (`StarlarkDocumentationOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/docgen/StarlarkDocumentationOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 1 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 1
- **Description:** Options class `StarlarkDocumentationOptions`
- **[MISSING] (1):** `--starlark_docs_root`

#### StrictActionEnvOptions (`StrictActionEnvOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/.../StrictActionEnvOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 1 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 1
- **Description:** Options class `StrictActionEnvOptions`
- **[MISSING] (1):** `--incompatible_strict_action_env`

#### TestSummaryOptions (`TestSummaryOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/runtime/TestSummaryOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 3 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 3
- **Description:** Options class `TestSummaryOptions`
- **[MISSING] (3):** `--print_relative_test_log_paths`, `--test_verbose_timeout_warnings`, `--verbose_test_summary`

#### VersionOptions (`VersionOptions`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/.../VersionOptions.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 1 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 1
- **Description:** Options class `VersionOptions`
- **[MISSING] (1):** `--gnu_format`

#### WorkspaceStatusAction.Options (`WorkspaceStatusAction.Options`)
- **Source:** `bazel/src/main/java/com/google/devtools/build/lib/analysis/WorkspaceStatusAction.java` | `zel/crates/zel-options/src/lib.rs`
- **Total Bazel Flags:** 2 | **Honored:** 0 | **Parsed but Ignored:** 0 | **Unknown/Missing:** 2
- **Description:** Options class `WorkspaceStatusAction.Options`
- **[MISSING] (2):** `--embed_label`, `--workspace_status_command`

### 5. Aggregate Parity Statistics by Options Class

| Options Class | Total Bazel Flags | Honored by Zel | Parsed but Ignored | Missing / Unknown |
| :--- | :---: | :---: | :---: | :---: |
| `CppOptions` | 69 | 0 | 0 | 69 |
| `BuildLanguageOptions` | 68 | 0 | 0 | 68 |
| `BuildGraveyardOptions` | 66 | 0 | 0 | 66 |
| `CoreOptions` | 64 | 2 | 0 | 62 |
| `RemoteOptions` | 64 | 0 | 1 | 63 |
| `AllCommandGraveyardOptions` | 63 | 0 | 0 | 63 |
| `CommonCommandOptions` | 46 | 2 | 1 | 43 |
| `BuildRequestOptions` | 40 | 0 | 1 | 39 |
| `BlazeServerStartupOptions` | 39 | 6 | 13 | 20 |
| `JavaOptions` | 35 | 0 | 0 | 35 |
| `ExecutionOptions` | 34 | 1 | 3 | 30 |
| `CommonQueryOptions` | 29 | 0 | 0 | 29 |
| `BazelBuildGraveyardOptions` | 27 | 0 | 0 | 27 |
| `SandboxOptions` | 26 | 0 | 0 | 26 |
| `RepositoryOptions` | 25 | 0 | 0 | 25 |
| `AndroidConfiguration.Options` | 24 | 0 | 0 | 24 |
| `TestOptions` | 19 | 0 | 1 | 18 |
| `AppleCommandLineOptions` | 17 | 0 | 0 | 17 |
| `WorkerOptions` | 17 | 0 | 0 | 17 |
| `AuthAndTLSOptions` | 16 | 0 | 0 | 16 |
| `BuildEventServiceOptions` | 15 | 0 | 0 | 15 |
| `UiOptions` | 15 | 0 | 0 | 15 |
| `ModOptions` | 14 | 1 | 0 | 13 |
| `RemoteAnalysisCachingServicesOptions` | 14 | 0 | 0 | 14 |
| `BuildEventStreamOptions` | 12 | 1 | 0 | 11 |
| `PackageOptions` | 12 | 0 | 0 | 12 |
| `QueryOptions` | 12 | 1 | 0 | 11 |
| `AnalysisOptions` | 11 | 0 | 0 | 11 |
| `DynamicExecutionOptions` | 11 | 0 | 0 | 11 |
| `DumpOptions` | 10 | 0 | 0 | 10 |
| `LoadingOptions` | 10 | 0 | 0 | 10 |
| `ProtoConfiguration.Options` | 10 | 0 | 0 | 10 |
| `RemoteAnalysisCachingOptions` | 10 | 0 | 0 | 10 |
| `BuildEventProtocolOptions` | 9 | 0 | 0 | 9 |
| `AqueryOptions` | 8 | 1 | 0 | 7 |
| `BuildEncyclopediaOptions` | 8 | 0 | 0 | 8 |
| `MemoryPressureOptions` | 8 | 0 | 0 | 8 |
| `AdbOptions` | 7 | 0 | 0 | 7 |
| `PlatformOptions` | 7 | 0 | 0 | 7 |
| `RunOptions` | 7 | 0 | 0 | 7 |
| `CqueryOptions` | 6 | 1 | 0 | 5 |
| `ExecutionGraphOptions` | 6 | 0 | 0 | 6 |
| `ObjcCommandLineOptions` | 6 | 0 | 0 | 6 |
| `LocalExecutionOptions` | 5 | 0 | 0 | 5 |
| `MobileInstallCommand.Options` | 5 | 0 | 0 | 5 |
| `StarlarkDebuggerOptions` | 5 | 0 | 0 | 5 |
| `BazelStartupOptionsModule.Options` | 4 | 2 | 2 | 0 |
| `CanonicalizeCommand.Options` | 4 | 1 | 0 | 3 |
| `CommonOptions` | 4 | 0 | 0 | 4 |
| `FetchOptions` | 4 | 0 | 0 | 4 |
| `IncludeScanningOptions` | 4 | 0 | 0 | 4 |
| `CleanCommand.Options` | 3 | 2 | 0 | 1 |
| `ClientOptions` | 3 | 0 | 0 | 3 |
| `FlagClashCanaryOptions` | 3 | 0 | 0 | 3 |
| `HelpCommand.Options` | 3 | 0 | 0 | 3 |
| `HostJvmStartupOptions` | 3 | 0 | 2 | 1 |
| `PackageMetricsModule.Options` | 3 | 0 | 0 | 3 |
| `TestSummaryOptions` | 3 | 0 | 0 | 3 |
| `BazelStrategyModule.Options` | 2 | 0 | 0 | 2 |
| `CommonRemoteOptions` | 2 | 0 | 0 | 2 |
| `ConfigFeatureFlagOptions` | 2 | 0 | 0 | 2 |
| `ConfigOptions` | 2 | 1 | 0 | 1 |
| `CoverageOptions` | 2 | 0 | 0 | 2 |
| `InfoCommand.Options` | 2 | 0 | 1 | 1 |
| `LocalDiffAwareness.Options` | 2 | 0 | 0 | 2 |
| `MetricsModule.Options` | 2 | 0 | 0 | 2 |
| `PostGCMemoryUseRecorder.Options` | 2 | 0 | 0 | 2 |
| `VendorOptions` | 2 | 0 | 0 | 2 |
| `WorkspaceStatusAction.Options` | 2 | 0 | 0 | 2 |
| `BazelAndroidConfiguration.Options` | 1 | 0 | 0 | 1 |
| `BazelCoverageReportModule.Options` | 1 | 0 | 0 | 1 |
| `CommandProfilerModule.Options` | 1 | 0 | 0 | 1 |
| `DebuggingOptions` | 1 | 0 | 0 | 1 |
| `KeepGoingOption` | 1 | 1 | 0 | 0 |
| `KeepStateAfterBuildOption` | 1 | 0 | 0 | 1 |
| `LoadingPhaseThreadsOption` | 1 | 0 | 0 | 1 |
| `OutputFilteringModule.Options` | 1 | 0 | 0 | 1 |
| `PrintActionOptions` | 1 | 0 | 0 | 1 |
| `RemoteStartupOptions` | 1 | 0 | 0 | 1 |
| `ShellConfiguration.Options` | 1 | 0 | 0 | 1 |
| `ShutdownCommand.Options` | 1 | 0 | 0 | 1 |
| `SkippedArgs` | 1 | 0 | 0 | 1 |
| `StarlarkDocumentationOptions` | 1 | 0 | 0 | 1 |
| `StrictActionEnvOptions` | 1 | 0 | 0 | 1 |
| `VersionOptions` | 1 | 0 | 0 | 1 |
| **TOTALS (across 88 classes)** | **1110** | **23** | **25** | **1062** |

---


## Starlark Language Interpreter

### Overview and Parity Level

The Starlark language implementation in Zel (`crates/zel-starlark`) compared against Bazel's reference implementation in Java (`src/main/java/net/starlark/java` and `src/main/java/com/google/devtools/build/lib/packages`) is at a **POOR / RUDIMENTARY** parity level (estimated **15–20%** functional parity).

Zel implements a basic, hand-rolled recursive descent parser and tree-walking interpreter in a single 2,364-line file (`crates/zel-starlark/src/lib.rs`). While this is sufficient to parse trivial `BUILD` files with simple top-level rule calls, basic lists, string concatenation, and simple list comprehensions, it falls drastically short of being a conformant Starlark interpreter. Zel lacks a semantic resolution phase (Resolver), has no real call-frame or local scoping model (function execution mutates the global symbol table directly), omits freezing and mutability tracking, fails to forbid recursion, truncates integer arithmetic to 64-bit silent wrapping overflow (lacking `BigInt`), omits floating-point numbers entirely, provides zero methods on `list`, omits the `json` and `proto` modules, implements `|` on integers as addition, mishandles string `%` format specifiers (reversing argument substitution order), leaks comprehension loop variables into the outer scope, and ignores all 68 Starlark semantics flags declared in `BuildLanguageOptions`.

---

### 1. Data Types and Value Representation

* **[DIFFERENT] Integer Representation and Overflow (Arbitrary-Precision BigInt vs 64-bit Wrapping):**
  * **Bazel:** Starlark specification (§2.1) and Bazel (`net/starlark/java/eval/StarlarkInt.java` lines 28–38, 73–80) mandate that integers are of arbitrary magnitude with exact arithmetic. `StarlarkInt` transparently promotes from unboxed `Int32` and `Int64` to `java.math.BigInteger`. Integer operations never overflow.
  * **Zel:** `Value::Int(i64)` (`crates/zel-starlark/src/lib.rs` line 194). Integer arithmetic operations use silent wrapping overflow: `a.wrapping_add(*b)` (line 270), `a.wrapping_sub(b)` (line 1830), and `a.wrapping_mul(b)` (line 1834). Large literals exceeding signed 64-bit bounds fail during parsing (`line[start..idx].parse::<i64>()`, line 612).

* **[MISSING] Floating-Point Numbers (`float` / `StarlarkFloat`):**
  * **Bazel:** Provides first-class IEEE 754 64-bit double-precision floating-point numbers via `StarlarkFloat.java` (`net/starlark/java/eval/StarlarkFloat.java` lines 29–80). Supports float literals (`3.14`, `1e-5`, `.5`), arithmetic promotion, division, formatting with `%g` and `%f`, and the `float()` builtin.
  * **Zel:** `Value` (`crates/zel-starlark/src/lib.rs` lines 191–216) has NO `Float` variant. The lexer does not recognize floating-point literals; encountering `1.5` lexes as `Token::Int(1)`, `Token::Dot`, `Token::Int(5)`, which fails parser postfix attribute validation (`Expected attribute name after '.'`, line 1321). The `float` constructor is absent from built-in functions.

* **[MISSING] List Mutation and Freezing (`StarlarkList`):**
  * **Bazel:** `StarlarkList.java` (`net/starlark/java/eval/StarlarkList.java` lines 395–490) implements full in-place mutability: `.append()`, `.extend()`, `.insert()`, `.remove()`, `.pop()`, `.clear()`, and `.index()`. Lists participate in the `Mutability` lifecycle: lists are mutable within their owning thread/module and become permanently frozen (immutable) once exported across `.bzl` boundaries or during active iteration.
  * **Zel:** `crates/zel-starlark/src/lib.rs` implements **ZERO** methods on `list` (lines 2213–2267). Method dispatch on `Value::List` falls through to the wildcard `_ => Ok(Value::None)` (line 2265). Lists cannot be mutated after creation (no `.append()` or `.extend()`), nor does Zel track freeze state or mutation during iteration.

* **[DIFFERENT] Dictionary Keys and Ordering (`Dict`):**
  * **Bazel:** Starlark dictionaries (`net/starlark/java/eval/Dict.java`) accept any hashable value as keys (strings, ints, booleans, tuples containing hashables, labels). Iteration strictly preserves insertion order.
  * **Zel:** `Value::Dict(BTreeMap<String, Value>)` (`crates/zel-starlark/src/lib.rs` line 198). Keys are strictly coerced to Rust `String` (`k = self.eval_expr(k_expr)?.to_string()`, line 1741). Because it uses `BTreeMap`, keys are iterated in lexicographical order, breaking insertion-order guarantees. Furthermore, non-string keys collide: `{1: "a", "1": "b"}` collapses into a single key `"1"`.

* **[MISSING] Set Type (`StarlarkSet` / `set`):**
  * **Bazel:** Implements `StarlarkSet.java` (`net/starlark/java/eval/StarlarkSet.java` lines 400–800) supporting full set algebra (`union`, `intersection`, `difference`, `symmetric_difference`, `issubset`, `issuperset`, `add`, `remove`, `discard`, `clear`) and O(1) membership testing.
  * **Zel:** Does not implement a `Set` value variant or builtin. Encountering `set()` results in an unresolved rule callable error.

* **[DIFFERENT] Range Type (`RangeList` vs Eager `List` Allocation):**
  * **Bazel:** `range()` produces an immutable, lazy `RangeList` (`net/starlark/java/eval/RangeList.java` lines 40–80) of type `"range"`, repr `range(0, 10)`, supporting O(1) indexing, slicing (`range(10)[::2]`), and `len()`. `step = 0` throws an immediate `EvalException: step cannot be 0`.
  * **Zel:** `range()` (`crates/zel-starlark/src/lib.rs` lines 2112–2126) eagerly evaluates a loop and returns an ordinary `Value::List(out)`. Calling `range(1_000_000_000)` exhausts memory. `type(range(5))` evaluates to `"list"`. If `step == 0`, the while loop termination condition relies on `step > 0 || step < 0`, terminating immediately with an empty list instead of throwing an error.

* **[PARTIAL] Depset Implementation and Traversal Orders:**
  * **Bazel:** `com.google.devtools.build.lib.collect.nestedset.Depset` and `Order` implement four strict traversal orders: `default` (`stable`), `postorder` (`compile`), `preorder` (`naive_link`), and `topological` (`link`). `topological` implements a DAG topological sort (parent before child, right-to-left postorder reversed). Direct elements must all be of the same type and must be hashable. Iterating directly over a depset (e.g. `for x in d:`) throws `iteration over depset is forbidden`. Depset equality is reference/identity based.
  * **Zel:** `Depset` (`crates/zel-starlark/src/lib.rs` lines 69–134, 1998–2030):
    * **[STUB] Fake Topological Order:** Lines 104–106 collapse `DepsetOrder::Default | DepsetOrder::Postorder | DepsetOrder::Topological` into `self.collect_postorder(&mut raw)`. Topological ordering is NOT implemented; it is faked as postorder (child-first, the exact opposite of parent-first topological order).
    * **[MISSING] Legacy Order Aliases:** Zel only parses `"default"`, `"postorder"`, `"preorder"`, `"topological"`. Passing `"stable"`, `"compile"`, `"naive_link"`, or `"link"` throws `Invalid depset order`.
    * **[DIFFERENT] Direct Iteration Permitted:** Line 1685 maps `Value::Depset(d) => Ok(d.to_list())` in `value_to_iter`. Writing `for x in depset(...):` is permitted in Zel, whereas Bazel explicitly forbids it.
    * **[MISSING] Element Type Checking:** Zel does not check element type homogeneity; `depset([1, "a", struct(x=1)])` is accepted silently.
    * **[DIFFERENT] Structural vs Identity Equality:** Zel derives `PartialEq` on `Depset`, comparing direct elements and transitive tree pointers, whereas Bazel specs that every depset instance is unique and equal to no other.

* **[DIFFERENT] Struct Representation, Equality, and Addition:**
  * **Bazel:** `StructImpl.java` (`com/google/devtools/build/lib/packages/StructImpl.java` lines 43–160):
    * Repr prints `struct(field1 = val1, field2 = val2)`.
    * Missing field access throws `EvalException: 'struct' value has no field or method 'foo'\nAvailable attributes: ...`.
    * Supports concatenation operator `+`: `struct(a=1) + struct(b=2)` creates `struct(a=1, b=2)`. Common fields throw an error.
  * **Zel:** `Value::Struct(BTreeMap<String, Value>)` (`crates/zel-starlark/src/lib.rs` lines 202, 368–376, 1883–1886):
    * **[DIFFERENT] Repr as Dict:** Line 368 formats `Value::Struct` identically to `Value::Dict`: `{ "field1": val1 }`. It does NOT print `struct(...)`.
    * **[DIFFERENT] Missing Field Returns None:** Line 1885 executes `fields.get(attr).cloned().unwrap_or(Value::None)`. Accessing a nonexistent attribute silently returns `None` instead of throwing an error.
    * **[MISSING] Struct Addition:** `struct + struct` throws `unsupported binary operation: struct + struct` (line 304).

---

### 2. Builtin Global Functions Parity (Grep `@StarlarkMethod` in `MethodLibrary.java` & Universe)

Bazel declares universal functions in `net/starlark/java/eval/MethodLibrary.java` (added to `Starlark.UNIVERSE` via `Starlark.java:141`). Zel handles builtins inside `crates/zel-starlark/src/lib.rs` (lines 1988–2154).

| Builtin Function | Bazel Reference (`MethodLibrary.java`) | Zel Status & Reference (`zel-starlark/src/lib.rs`) | Notes on Divergence |
|---|---|---|---|
| `abs(x)` | Line 196 | **[MISSING]** | Not recognized; falls back to `RuleCallable`. |
| `all(x)` | Line 222 | **[MISSING]** | Not recognized; falls back to `RuleCallable`. |
| `any(x)` | Line 234 | **[MISSING]** | Not recognized; falls back to `RuleCallable`. |
| `bool(x)` | Line 511 | **[PARTIAL]** Line 2140 | Only takes single positional argument. |
| `dict(**kwargs)` | Line 705 | **[MISSING]** | Not recognized; `dict()` constructor cannot be called dynamically. |
| `dir(x)` | Line 893 | **[MISSING]** | Not recognized. |
| `enumerate(list, start=0)`| Line 764 | **[MISSING]** | Not recognized. |
| `fail(*args, sep=" ", attr=None)` | Line 904 | **[DIFFERENT]** Line 2144 | Only inspects first arg (`eval_pos.first()`), ignores additional positional args (`sep` parameter unsupported). Returns `StarlarkError` with no file/line/column diagnostic or call stack. Package loader swallows failure silently. |
| `float(x)` | Line 525 | **[MISSING]** | Not recognized; float data type does not exist. |
| `getattr(x, name, default)` | Line 868 | **[MISSING]** | Dynamic attribute lookup is unsupported. |
| `hasattr(x, name)` | Line 853 | **[MISSING]** | Attribute reflection unsupported. |
| `hash(x)` | Line 787 | **[MISSING]** | String hashing unsupported. |
| `int(x, base=None)` | Line 615 | **[PARTIAL]** Line 2131 | Only parses decimal strings, ints, booleans; `base` parameter unsupported. Overflows 64-bit int. |
| `len(x)` | Line 464 | **[PARTIAL]** Line 2103 | Only supports String, List, Tuple, Dict, Struct. For any other type, returns `0` silently instead of raising `type '...' has no len()`. |
| `list(x)` | Line 450 | **[MISSING]** | `list()` constructor function is missing. |
| `max(*args, key=None)` | Line 80 | **[MISSING]** | Not recognized. |
| `min(*args, key=None)` | Line 42 | **[MISSING]** | Not recognized. |
| `print(*args, sep=" ")` | Line 981 | **[STUB]** Line 2196 | `print` is NOT handled in `eval_call`. Calling `print("foo")` creates a `RuleCallable` and records a target named `"print"` into `rule_calls`. Emits no stdout/stderr output. |
| `range(start, stop, step)`| Line 802 | **[DIFFERENT]** Line 2112 | Eagerly builds `Value::List` (allocates full vector in RAM). Returns `"list"`, not `"range"`. Does not error on `step == 0`. |
| `repr(x)` | Line 500 | **[DIFFERENT]** Line 2127 | Calls `v.to_string()`. Strings are unquoted (`repr("foo")` outputs `foo`, not `"\"foo\""`). Structs format as `{}` dicts. |
| `reversed(sequence)` | Line 417 | **[MISSING]** | Sequence reversal builtin unsupported. |
| `set(elements)` | Line 733 | **[MISSING]** | Set type and constructor unsupported. |
| `sorted(iterable, key, reverse)` | Line 256 | **[MISSING]** | Sequence sorting builtin unsupported. |
| `str(x)` | Line 487 | **[DIFFERENT]** Line 2127 | Handled identically to `repr` via `v.to_string()`. |
| `tuple(x)` | Line 434 | **[MISSING]** | `tuple()` constructor function is missing. |
| `type(x)` | Line 1021 | **[MISSING]** | Not handled in `eval_call`; calling `type(x)` falls back to `RuleCallable`. |
| `zip(*args)` | Line 1042 | **[MISSING]** | Iteration utility `zip()` unsupported. |

---

### 3. Builtin Type Methods Parity (Grep `@StarlarkMethod`)

Bazel attaches methods to primitive types using `@StarlarkMethod` in `StringModule.java`, `StarlarkList.java`, `Dict.java`, `StarlarkSet.java`, and `Depset.java`. Zel handles method calls in `crates/zel-starlark/src/lib.rs` (lines 2207–2267).

#### A. String Methods (`StringModule.java` vs `zel-starlark/src/lib.rs`)

Bazel exposes **32** methods on `str`. Zel implements only **9** (and several of those are defective):

| Method | Bazel Reference (`StringModule.java`) | Zel Status & Reference (`zel-starlark/src/lib.rs`) | Differences & Edge Cases |
|---|---|---|---|
| `s.capitalize()` | Line 538 | **[MISSING]** | Not implemented. |
| `s.count(sub, start, end)` | Line 898 | **[MISSING]** | Substring counting not implemented. |
| `s.elems()` | Line 943 | **[MISSING]** | Character iterator not implemented. |
| `s.endswith(suffix, start, end)` | Line 960 | **[PARTIAL]** Line 2220 | Does not support `start`/`end` offsets or tuple of suffixes (`s.endswith((".c", ".cc"))`). |
| `s.find(sub, start, end)` | Line 635 | **[MISSING]** | Substring search returning index or -1 not implemented. |
| `s.format(*args, **kwargs)` | Line 1014 | **[DIFFERENT]** Line 2245 | **Defective:** Ignores `**kwargs` entirely (`_kw_args` parameter is unused in line 2212). Does not support escaped braces (`{{` or `}}`). Sequential replacement with `.replacen("{}", ...)` and `.replace("{i}", ...)` fails if replacement strings themselves contain `{}`. |
| `s.index(sub, start, end)` | Line 699 | **[MISSING]** | Substring search raising error when not found is missing. |
| `s.isalnum()` | Line 779 | **[MISSING]** | Character classification predicate missing. |
| `s.isalpha()` | Line 769 | **[MISSING]** | Character classification predicate missing. |
| `s.isdigit()` | Line 789 | **[MISSING]** | Character classification predicate missing. |
| `s.islower()` | Line 809 | **[MISSING]** | Character classification predicate missing. |
| `s.isspace()` | Line 799 | **[MISSING]** | Character classification predicate missing. |
| `s.istitle()` | Line 831 | **[MISSING]** | Character classification predicate missing. |
| `s.isupper()` | Line 820 | **[MISSING]** | Character classification predicate missing. |
| `s.join(elements)` | Line 158 | **[PARTIAL]** Line 2235 | Only joins elements convertible via `to_string_list()`. |
| `s.lower()` | Line 188 | **[PARTIAL]** Line 2244 | ASCII/Unicode lowercase. |
| `s.lstrip(chars)` | Line 253 | **[MISSING]** | Left stripping missing. |
| `s.partition(sep)` | Line 484 | **[MISSING]** | 3-tuple partitioning missing. |
| `s.removeprefix(prefix)` | Line 1093 | **[MISSING]** | Prefix removal missing. |
| `s.removesuffix(suffix)` | Line 1109 | **[MISSING]** | Suffix removal missing. |
| `s.replace(old, new, maxsplit)` | Line 355 | **[PARTIAL]** Line 2224 | Ignores `maxsplit` count parameter. |
| `s.rfind(sub, start, end)` | Line 605 | **[MISSING]** | Reverse search missing. |
| `s.rindex(sub, start, end)`| Line 665 | **[MISSING]** | Reverse search raising error missing. |
| `s.rpartition(sep)` | Line 496 | **[MISSING]** | Reverse 3-tuple partitioning missing. |
| `s.rsplit(sep, maxsplit)` | Line 443 | **[MISSING]** | Reverse split missing. |
| `s.rstrip(chars)` | Line 283 | **[MISSING]** | Right stripping missing. |
| `s.split(sep, maxsplit)` | Line 404 | **[DIFFERENT]** Line 2229 | **Semantic divergence:** Bazel `s.split()` with no separator splits on consecutive whitespace runs and strips leading/trailing spaces. Zel defaults `sep` to `" "` (single space), producing empty string elements for consecutive spaces. Ignores `maxsplit`. |
| `s.splitlines(keepends)` | Line 733 | **[MISSING]** | Line splitting missing. |
| `s.startswith(prefix, start, end)` | Line 1042 | **[PARTIAL]** Line 2216 | Does not support `start`/`end` offsets or tuple of prefixes (`s.startswith(("lib", "src"))`). |
| `s.strip(chars)` | Line 313 | **[PARTIAL]** Line 2242 | Does not accept custom `chars` to strip. |
| `s.title()` | Line 552 | **[MISSING]** | Title casing missing. |
| `s.upper()` | Line 196 | **[PARTIAL]** Line 2243 | Uppercasing. |

#### B. List Methods (`StarlarkList.java` vs `zel-starlark/src/lib.rs`)

Bazel exposes **7** methods on `list`. Zel implements **NONE** (0 / 7):

| Method | Bazel Reference (`StarlarkList.java`) | Zel Status (`zel-starlark/src/lib.rs`) |
|---|---|---|
| `l.append(item)` | Line 413 | **[MISSING]** (Returns `Value::None`, list unchanged) |
| `l.clear()` | Line 421 | **[MISSING]** |
| `l.extend(items)` | Line 441 | **[MISSING]** |
| `l.index(item, start, end)` | Line 449 | **[MISSING]** |
| `l.insert(index, item)` | Line 424 | **[MISSING]** |
| `l.pop(index)` | Line 485 | **[MISSING]** |
| `l.remove(item)` | Line 395 | **[MISSING]** |

#### C. Dictionary Methods (`Dict.java` vs `zel-starlark/src/lib.rs`)

Bazel exposes **9** methods on `dict`. Zel implements **4**:

| Method | Bazel Reference (`Dict.java`) | Zel Status & Reference (`zel-starlark/src/lib.rs`) | Differences |
|---|---|---|---|
| `d.clear()` | Line 557 | **[MISSING]** | In-place dictionary clearing missing. |
| `d.get(key, default=None)` | Line 220 | **[PARTIAL]** Line 2260 | Key coerced to String; lookup in BTreeMap. |
| `d.items()` | Line 372 | **[PARTIAL]** Line 2253 | Returns list of 2-tuples in **alphabetical** order, not insertion order. |
| `d.keys()` | Line 382 | **[PARTIAL]** Line 2258 | Returns list of keys in **alphabetical** order, not insertion order. |
| `d.pop(key, default)` | Line 254 | **[MISSING]** | Removing key and returning value missing. |
| `d.popitem()` | Line 272 | **[MISSING]** | Removing LIFO/arbitrary pair missing. |
| `d.setdefault(key, default)` | Line 281 | **[MISSING]** | Conditional insertion missing. |
| `d.update(*args, **kwargs)` | Line 298 | **[MISSING]** | Dictionary merging method missing. |
| `d.values()` | Line 363 | **[PARTIAL]** Line 2259 | Returns values sorted by key alphabetical order, not insertion order. |

#### D. Set Methods (`StarlarkSet.java` vs `zel-starlark/src/lib.rs`)

Bazel defines **16** methods on `StarlarkSet` (`add`, `clear`, `difference`, `difference_update`, `discard`, `intersection`, `intersection_update`, `isdisjoint`, `issubset`, `issuperset`, `pop`, `remove`, `symmetric_difference`, `symmetric_difference_update`, `union`, `update`).
Zel: **[MISSING]** All 16 methods and the underlying `set` type are absent.

#### E. Depset Methods (`Depset.java` vs `zel-starlark/src/lib.rs`)

* **`depset.to_list()`** (Bazel `Depset.java:374`, Zel `lib.rs:2215`):
  * **Bazel:** Flattens transitive DAG into deduplicated list according to specified `order`.
  * **Zel:** **[PARTIAL]** Implemented, but `topological` order is faked as postorder, and deduplication executes in $O(N^2)$ via linear search on a vector.

---

### 4. Standard Modules and Dialects

* **[MISSING] `json` Module:**
  * **Bazel:** Implements full RFC 8259 JSON encoder/decoder in `net/starlark/java/lib/json/Json.java` (lines 78–680), providing `json.encode(x)`, `json.decode(x, default=...)`, `json.indent(s, prefix="", indent="\t")`, and `json.encode_indent(x, ...)`. Supports custom serialization via `StarlarkEncodable`.
  * **Zel:** Does NOT provide the `json` module. Referencing `json` or calling `json.encode(...)` fails.

* **[MISSING] `proto` Module:**
  * **Bazel:** Implements `com/google/devtools/build/lib/packages/Proto.java` (lines 52–100), providing `proto.encode_text(struct)` to serialize Starlark structs, sequences, and dicts into Protocol Buffer text format.
  * **Zel:** Does NOT provide the `proto` module.

* **[DIFFERENT] `select()` and `SelectorList` Mechanics:**
  * **Bazel:** `SelectorList.java` (`com/google/devtools/build/lib/packages/SelectorList.java` lines 182–227):
    * `select(dict, no_match_error = ...)` supports concatenating configurable expressions.
    * Uses `+` when the underlying type is a sequence/list, and `|` (`TokenKind.PIPE`) when the underlying type is a mapping/dict (`select({"//a": {"x": 1}}) | {"y": 2}`).
    * If no conditions match and no `//conditions:default` is specified, Bazel fails analysis with `Configurable attribute ... doesn't match this configuration (no matching condition)`.
  * **Zel:** `SelectorList` (`crates/zel-starlark/src/lib.rs` lines 136–187, 1827):
    * **[DIFFERENT] Arbitrary First-Branch Fallback:** Lines 174–176: if no active condition matches and `//conditions:default` is absent, Zel does `matched = branches.values().next().cloned()`. It arbitrarily selects the first entry in the BTreeMap instead of reporting an error!
    * **[DIFFERENT] `|` Mapped Directly to `+`:** Line 1827 treats `BinOp::BitOr` identically to `BinOp::Add`. For integers, `5 | 3` calculates `5 + 3 = 8` (verified: spot check outputs `//:test_8`).
    * **[DIFFERENT] Illegal Dict Addition Allowed:** In Python/Starlark, dicts do not support `+`. Zel allows `dict + dict` (lines 297–303) and treats `select({...}) | select({...})` and `select({...}) + select({...})` as identical dictionary union.

* **[STUB] Rule, Aspect, and Extension Definition Builtins:**
  * **Bazel:** Rich APIs (`rule()`, `aspect()`, `provider()`, `repository_rule()`, `module_extension()`, `tag_class()`) that accept implementation callbacks, attribute schemas (`attr.label_list`, etc.), toolchains, outputs, and exec groups.
  * **Zel:** Lines 2068–2084:
    * `provider()` returns a dummy `Value::Provider { name: "Provider" }`.
    * `rule()`, `aspect()`, `repository_rule()`, etc. return a dummy `Value::RuleCallable { rule_class: "custom_rule", executable, test }`.
    * Cannot define custom rule implementations, analyze dependencies, or configure rule attributes in Starlark.

---

### 5. Syntax, Parser, and AST Coverage

* **[MISSING] Lexer: Non-Decimal, Float, and Byte Literals:**
  * **Bazel:** `net/starlark/java/syntax/Lexer.java`:
    * Integers: Hexadecimal (`0x1F`, `0XFF`), Octal (`0o755`, `0O77`), and Binary (`0b1010`, `0B1100`).
    * Floating-point: Decimals (`3.14`, `.5`, `10.`), scientific exponent notation (`1e-4`, `2.5E+3`).
    * Byte literals: `b"hello"`, `rb"bytes"`.
  * **Zel:** `crates/zel-starlark/src/lib.rs` lines 606–616:
    * Scans only ASCII digits (`bytes[idx].is_ascii_digit()`).
    * Hex (`0x`), octal (`0o`), binary (`0b`), float literals, and byte literals fail to lex as numbers. `0x10` lexes as `Int(0)` followed by `Ident("x10")`.

* **[PARTIAL] Lexer: String Escape Sequences:**
  * **Bazel:** Recognizes standard escapes (`\a`, `\b`, `\f`, `\n`, `\r`, `\t`, `\v`), octal escapes (`\000`–`\377`), hex escapes (`\x00`–`\xFF`), 16-bit unicode (`\u0000`–`\uFFFF`), 32-bit unicode (`\U00000000`–`\U0010FFFF`), and escaped newlines (`\\\n`).
  * **Zel:** Lines 788–800 recognize only `\n`, `\r`, `\t`, `\\`, `\"`, `\'`. All unicode escapes (`\u`, `\U`), hex (`\x`), and octal escapes are emitted literally (`\` followed by character).

* **[MISSING] Missing Operators and Augmented Assignments:**
  * **Bazel:** Full operator set: `&` (BitAnd), `^` (BitXor), `~` (BitNot), `<<` (ShiftLeft), `>>` (ShiftRight), `-=`, `*=`, `/=`, `//=`, `%=`, `&=`, `|=`, `^=`, `<<=`, `>>=`.
  * **Zel:** Tokens (lines 506–539) include only `+`, `-`, `*`, `**`, `/`, `//`, `%`, `|`, and `+=`. All bitwise bit-shift and logical operators (`&`, `^`, `~`, `<<`, `>>`) and all other augmented assignments (`-=`, `*=`, etc.) are missing.

* **[MISSING] Loop Control Flow (`break` and `continue`):**
  * **Bazel:** `FlowStatement.java` implements `break` and `continue` inside `for` loops.
  * **Zel:** `break` and `continue` do NOT exist in the lexer, parser, or AST. Loop execution cannot be terminated early or skipped.

* **[DIFFERENT] Function Definition: Keyword-Only Parameters Broken:**
  * **Bazel:** `def f(a, *, b):` marks `b` as keyword-only. Passing `f(1, 2)` raises `f() accepts no more than 1 positional argument but 2 were given`.
  * **Zel:** Lines 892–902: when encountering `Token::Star` followed by `Token::Comma`, Zel simply skips both tokens and continues parsing the next parameter as a standard positional parameter. Calling `my_func(1, 2)` accepts `2` positionally without error (verified: spot check builds `//:test_3`).

* **[DIFFERENT] Function Definition: Default Parameter Evaluation Timing:**
  * **Bazel:** Parameter defaults are evaluated once at function definition time in the declaring scope and frozen.
  * **Zel:** Line 2176: `let def_val = self.eval_expr(def_expr)?;` evaluates default expressions **dynamically on each function invocation**. If defaults contain mutable constructors or calls, Zel recomputes them on every call.

* **[MISSING] Lambda Expressions:**
  * **Bazel:** `LambdaExpression.java` implements anonymous functions: `lambda x: x + 1`.
  * **Zel:** `lambda` is not a keyword; anonymous functions are unsupported.

* **[DIFFERENT] Comprehensions: Scope Leakage & Syntax Gaps:**
  * **Bazel:** `Comprehension.java`:
    * Supports multiple `for` clauses: `[x for row in matrix for x in row]`.
    * Supports multiple `if` filters: `[x for x in list if cond1 if cond2]`.
    * Loop variables are scoped strictly to the comprehension and do not leak into enclosing scopes.
  * **Zel:** Lines 1433–1445, 1757, 1778:
    * **[MISSING] Multiple Clauses:** Only supports exactly one `for` clause and at most one `if` condition. Multiple `for` or `if` clauses fail with parse errors.
    * **[DIFFERENT] Loop Variable Overwrites Outer Scope:** Lines 1757 & 1778 call `self.bind_loop_vars`, which writes comprehension iteration variables directly into `self.globals`. Iterating with `x` in a list comprehension mutates and overwrites `x` in the enclosing module (verified: `x = "original"; [x for x in ["leaked"]]; cc_library(name = x)` creates target `//:test_leaked`).

* **[DIFFERENT] Chained Comparisons Evaluated Left-Associatively:**
  * **Bazel:** In Python / Starlark (§4.1), comparison chaining `1 < x < 10` desugars into `(1 < x) and (x < 10)` with short-circuiting where `x` is evaluated once.
  * **Zel:** Lines 1160–1215: `parse_cmp` parses left-associatively without chaining logic. `1 < 5 < 10` is parsed as `(1 < 5) < 10`. `1 < 5` evaluates to `Value::Bool(true)`. Then `true < 10` evaluates in line 1859 (`_ => Ok(Value::Bool(false))`), yielding `False` (verified: spot check outputs `//:test_False`).

* **[DIFFERENT] Slicing: Step Slicing Missing & Negative Indices Clamped to Zero:**
  * **Bazel:** `SliceExpression.java`: supports `seq[start:end:step]` with negative steps (`[::-1]`), negative indices (`s[-2:]`), on strings, lists, tuples, and ranges.
  * **Zel:** Lines 471–475, 1330–1369, 1918–1950:
    * `Expr::Slice` lacks a `step` field. Slicing with a step (`s[::2]`) fails during parsing (`Unexpected token in expression: Colon`).
    * Lines 1922, 1931 clamp indices to zero: `Value::Int(i) => i.max(0) as usize`. Negative slice indices do not count from the end of the sequence; `s[-2:]` clamps to index 0, returning the entire string (verified: `"hello"[-2:]` evaluates to `"hello"`).
    * Slicing `Value::Tuple` is not implemented (line 1948 returns `Value::None`).

---

### 6. Execution Semantics, Scoping, and Mutability

* **[MISSING] Static Resolution Phase (`Resolver.java`):**
  * **Bazel:** `net/starlark/java/syntax/Resolver.java` runs a complete static scope resolution pass prior to evaluation. It categorizes every identifier as `UNIVERSAL`, `GLOBAL`, `LOCAL`, `CELL`, or `FREE`. It statically detects:
    * Reading a local variable before assignment (`local variable 'x' is referenced before assignment`).
    * Defining a variable as both local and global.
    * Reassigning a global variable inside a function body.
    * Invalid use of `load()` statements inside nested blocks.
  * **Zel:** Has **NO static resolver pass**. Identifiers are resolved dynamically at runtime by checking `self.globals.get(name)`; if not present, Zel fabricates a `Value::RuleCallable` on the fly (lines 1715–1722). Undeclared identifiers do not trigger resolution errors—they are treated as rule targets!

* **[DIFFERENT] Call Frames and Scoping (Global Namespace Mutated During Function Calls):**
  * **Bazel:** `StarlarkThread.java` and `Eval.java` manage a genuine call stack of stack frames with local array indexing for local variables. Closures capture cell variables properly.
  * **Zel:** Lines 2159–2189:
    ```rust
    let saved_globals = self.globals.clone();
    // bind parameters into self.globals
    // execute body statements against self.globals
    self.globals = saved_globals;
    ```
    Zel implements function execution by cloning `self.globals`, dumping parameters and local variables directly into the global map, executing the body, and restoring the cloned map upon exit.
    * If a function calls another function or references outer variables, lexical closures are impossible.
    * Any assignment inside a function temporarily overwrites globals.

* **[MISSING] Freezing, Immutability, and Iteration Mutability Locks:**
  * **Bazel:** `net/starlark/java/eval/Mutability.java`:
    * Values created during `.bzl` evaluation are mutable only within the defining file's thread.
    * Once `.bzl` evaluation finishes, all exported data structures (lists, dicts, custom objects) are **frozen** permanently. Modifying a frozen list or dict in another module or macro throws `EvalException: trying to mutate a frozen list`.
    * Mutating a list or dict during active iteration throws `EvalException: list is mutated during iteration`.
  * **Zel:** Has NO concept of freezing, mutability tokens, or iteration locks. Collections have no frozen flags.

* **[MISSING] Recursion Prohibition:**
  * **Bazel:** Starlark specification (§4.4) strictly forbids recursion (direct or indirect) to guarantee bounded computation. `StarlarkThread` inspects call frames and raises `EvalException: function 'f' called recursively`.
  * **Zel:** Does not detect or prevent recursion. Functions can call themselves recursively until the host thread crashes with a Rust stack overflow (SIGSEGV).

* **[DIFFERENT] `load()` Semantics & Top-Level Requirement:**
  * **Bazel:** `load()` statements must appear at the top of the file before any other statements (except comments and string docstrings). They execute the loaded `.bzl` module, freeze it, and import only exported symbols.
  * **Zel:** Lines 982–1023, 1563–1576:
    * `load()` can appear anywhere in a file, including inside `if` statements or interleaved between rule declarations.
    * Inside `zel-starlark`, `load()` does not evaluate the target file; it simply fabricates a dummy `Value::RuleCallable` for each imported symbol (`lib.rs:1569`).

---

### 7. Formatting, Output Formats, and Diagnostics

* **[DIFFERENT] String `%` Formatting: Argument Substitution Reversal Bug:**
  * **Bazel:** `net/starlark/java/eval/FormatParser.java` implements full Python-compatible `%` formatting: specifiers (`%s`, `%r`, `%d`, `%i`, `%o`, `%u`, `%x`, `%X`, `%e`, `%E`, `%f`, `%F`, `%g`, `%G`, `%c`, `%%`), width, zero-padding, precision (`%04d`, `%.2f`), and dictionary mapping (`%(var)s`).
  * **Zel:** `crates/zel-starlark/src/lib.rs` lines 2279–2287:
    ```rust
    fn percent_format(fmt_str: &str, args: &[Value]) -> String {
        let mut out = fmt_str.to_string();
        for arg in args {
            if let Some(idx) = out.find("%s").or_else(|| out.find("%d")) {
                out.replace_range(idx..idx + 2, &arg.to_string());
            }
        }
        out
    }
    ```
    * **Severe Bug:** Because `out.find("%s")` executes before `.or_else(|| out.find("%d"))`, if `fmt_str` contains `"%d %s"`, Zel locates and replaces `"%s"` before `"%d"`, reversing the arguments!
    * **Spot-check confirmation:** In Zel, `"%d %s" % (1, "two")` produces `"two 1"` (verified: target name created was `//:two 1`).
    * **Missing Specifiers:** Omits `%r`, `%x`, `%f`, `%o`, and `%%` (literal `%%` cannot be escaped). Omits width, precision, and dictionary interpolation.

* **[DIFFERENT] `str.format()` Fragility & Kwarg Ignorance:**
  * **Bazel:** `StringModule.java:1014` supports positional arguments, named kwargs (`"{name}".format(name="val")`), nested fields, and double brace escapes (`{{}}`).
  * **Zel:** Lines 2245–2252:
    * Silently drops `kw_args` (parameter `_kw_args` on line 2212 is unused).
    * Performs naive substring replacement: if an argument itself contains `{}` or `{0}`, subsequent replacements will corrupt it.
    * Escaped braces `{{` and `}}` are not supported.

* **[STUB] `print()` Diagnostics:**
  * **Bazel:** `MethodLibrary.java:981` prints formatted messages to the console event stream formatted with file, line, and column: `DEBUG: /workspace/pkg/BUILD:12:8: message`.
  * **Zel:** Line 1988 does not match `print`. Calling `print("foo")` records a target named `"print"` into `rule_calls`. Nothing is printed to stdout or stderr.

* **[DIFFERENT] `fail()` Error Handling & Silent Failure in Package Loader:**
  * **Bazel:** `fail(*args, sep=" ")` outputs all arguments separated by `sep`, aborts evaluation, prints a full Starlark stack trace with line numbers, and causes Bazel to terminate with exit code 1 or 7.
  * **Zel:** Line 2144:
    * Takes only the first positional argument (`eval_pos.first()`); subsequent arguments (`fail("Code", 500, "Error")`) are silently discarded.
    * Omits stack trace, source file path, line number, and column.
    * In `crates/zel-packages/src/lib.rs` line 135: package loading errors from `eval_str` are not reported to the console; `zel build //:all` prints `INFO: Build completed successfully, 0 total actions` and exits with code 0.

* **[MISSING] Source Locations, Line/Column Tracking, and Spelling Suggestions:**
  * **Bazel:**
    * Every AST node tracks file path, start line, start column, end line, end column (`FileLocations.java`, `Location.java`).
    * Parser and evaluation errors print source snippets with a caret indicator (`^`) pointing to the offending token.
    * Typo detection via `SpellChecker.java`: accessing a misspelled attribute or identifier provides a suggestion (`did you mean 'srcs'?`).
  * **Zel:**
    * The `Token` enum (lines 506–539) contains NO source position or line number metadata.
    * Errors are emitted as bare strings (`StarlarkError("Expected attribute name after '.'")`). No file name, line number, or column offset is available.

---

### 8. Starlark Semantics Flags (`BuildLanguageOptions`)

Bazel configures the language dialect and semantics using command-line flags defined in `com/google/devtools/build/lib/packages/semantics/BuildLanguageOptions.java` and threaded through `net/starlark/java/eval/StarlarkSemantics.java`.

Zel has **NO** `StarlarkSemantics` abstraction and parses **NONE** of these 68 options. All flags listed below are **[MISSING]** in Zel:

#### Core Language, Types, and Syntax Flags
* **[MISSING]** `--experimental_starlark_type_syntax`: Enables type syntax annotations in Starlark.
* **[MISSING]** `--experimental_starlark_types`: Enables runtime typing validation.
* **[MISSING]** `--experimental_starlark_static_type_checking`: Enables static type checking in the resolver.
* **[MISSING]** `--experimental_starlark_dynamic_type_checking`: Enables dynamic type assertions.
* **[MISSING]** `--experimental_starlark_type_checking`: Configures Starlark type check mode.
* **[MISSING]** `--experimental_starlark_types_allowed_paths`: Whitelists file paths permitted to use types.
* **[MISSING]** `--experimental_enable_starlark_set`: Enables core `set` data type.
* **[MISSING]** `--incompatible_enforce_starlark_utf8`: Enforces valid UTF-8 encoding for Starlark source files.
* **[MISSING]** `--internal_starlark_utf_8_byte_strings`: Enforces UTF-8 byte string behavior.
* **[MISSING]** `--max_computation_steps`: Caps maximum evaluation steps to prevent runaway loops.
* **[MISSING]** `--force_starlark_stack_trace`: Forces printing full Starlark backtraces on errors.

#### Depset and Collection Semantics Flags
* **[MISSING]** `--incompatible_always_check_depset_elements`: Strictly enforces hashability and type homogeneity in `depset(direct=...)`.
* **[MISSING]** `--nested_set_depth_limit`: Enforces nested set depth limits to prevent stack overflow.
* **[MISSING]** `--nested_set_count_limit`: Enforces limit on nested set node counts.
* **[MISSING]** `--nested_set_count_limit_allowlist`: Allowlist for packages exempted from count limits.

#### Build & Macro Dialect Semantics Flags
* **[MISSING]** `--experimental_enable_first_class_macros`: Enables symbolic macros (`macro()`).
* **[MISSING]** `--incompatible_symbolic_macro_strict_attrs`: Enforces strict attributes on symbolic macros.
* **[MISSING]** `--incompatible_existing_rules_default_omit_macro_internals`: Hides macro implementation targets from `native.existing_rules()`.
* **[MISSING]** `--experimental_bzl_visibility`: Enables visibility restrictions on `.bzl` files.
* **[MISSING]** `--check_bzl_visibility`: Enforces checking of `.bzl` visibility.
* **[MISSING]** `--allow_experimental_loads`: Allows loading experimental bzl modules.
* **[MISSING]** `--incompatible_stop_exporting_build_file_path`: Stops defining `__build_file_path__`.
* **[MISSING]** `--incompatible_no_implicit_file_export`: Disallows implicitly referring to source files not explicitly exported.
* **[MISSING]** `--incompatible_no_rule_outputs_param`: Disallows the legacy `outputs` parameter of `rule()`.
* **[MISSING]** `--incompatible_disallow_empty_glob`: Causes `glob()` to fail if it matches no files.
* **[MISSING]** `--incompatible_fail_on_unknown_attributes`: Fails rule definitions that declare unrecognized attributes.
* **[MISSING]** `--incompatible_resolve_select_keys_eagerly`: Resolves condition labels in `select()` during parsing.
* **[MISSING]** `--incompatible_simplify_unconditional_selects_in_rule_attrs`: Inlines 1-branch `select({"//conditions:default": ...})`.
* **[MISSING]** `--incompatible_unambiguous_label_stringification`: Formats labels as `@@repo//pkg:target` instead of `@repo//pkg:target`.

#### Builtins Injection & Migration Flags
* **[MISSING]** `--experimental_builtins_bzl_path`: Path to builtins bzl definitions.
* **[MISSING]** `--experimental_builtins_injection_override`: Overrides built-in rules injection.
* **[MISSING]** `--experimental_builtins_dummy`: Builtin rule dummy flag.
* **[MISSING]** `--experimental_google_legacy_api`: Enables Google legacy Starlark builtins.
* **[MISSING]** `--experimental_platforms_api`: Enables platforms API in Starlark.
* **[MISSING]** `--incompatible_enable_deprecated_label_apis`: Enables deprecated label methods.
* **[MISSING]** `--incompatible_disable_target_default_provider_fields`: Disallows reading target fields as default provider.
* **[MISSING]** `--incompatible_run_shell_command_string`: Disallows string commands in `actions.run_shell`.
* **[MISSING]** `--incompatible_require_mnemonic_for_run_actions`: Mandates `mnemonic` parameter in `actions.run`.
* **[MISSING]** `--incompatible_no_attr_license`: Disallows `attr.license`.
* **[MISSING]** `--incompatible_package_group_has_public_syntax`: Enables `public` in package groups.
* **[MISSING]** `--incompatible_fix_package_group_reporoot_syntax`: Fixes repo root syntax in package groups.
* **[MISSING]** `--incompatible_allow_tags_propagation`: Propagates tags to action execution.
* **[MISSING]** `--incompatible_require_matching_aspect_hints_providers`: Enforces aspect hints providers matching.
* **[MISSING]** `--incompatible_disable_starlark_host_transitions`: Disallows `cfg = "host"` in Starlark transitions.
* **[MISSING]** `--incompatible_disable_objc_library_transition`: Disallows objc transitions.
* **[MISSING]** `--incompatible_disable_transitions_on`: Disallows transitions on specific attributes.
* **[MISSING]** `--add_go_exec_groups_to_binary_rules`: Adds go exec groups.
* **[MISSING]** `--incompatible_enable_proto_toolchain_resolution`: Enables proto toolchain resolution.
* **[MISSING]** `--incompatible_disable_non_executable_java_binary`: Disallows non-executable java binaries.
* **[MISSING]** `--experimental_rule_extension_api`: Enables rule extension APIs.
* **[MISSING]** `--experimental_dormant_deps`: Enables dormant dependencies.
* **[MISSING]** `--incompatible_disallow_ctx_resolve_tools`: Disallows `ctx.resolve_tools`.
* **[MISSING]** `--incompatible_check_external_repo_source_dir_package_boundary`: Enforces package boundaries in external repos.
* **[MISSING]** `--incompatible_locations_prefers_executable`: Prefers executable in `$(locations)`.
* **[MISSING]** `--experimental_repository_ctx_execute_wasm`: Enables WASM execution in repo rules.
* **[MISSING]** `--experimental_repository_ctx_wasm_compilation`: Enables WASM compilation in repo rules.
* **[MISSING]** `--max_bzl_file_size`: Caps maximum size of `.bzl` files.
* **[MISSING]** `--soft_max_bzl_file_size`: Soft warning limit on `.bzl` file size.
* **[MISSING]** `--bzl_file_size_limit_allowlist`: Allowlist for oversized `.bzl` files.
* **[MISSING]** `--experimental_cc_skylark_api_enabled_packages`: Packages permitted to use experimental C++ Skylark API.
* **[MISSING]** `--experimental_enable_android_migration_apis`: Enables Android migration APIs.
* **[MISSING]** `--experimental_isolated_extension_usages`: Isolates module extension usages.
* **[MISSING]** `--experimental_cc_shared_library`: Enables `cc_shared_library`.
* **[MISSING]** `--experimental_repo_remote_exec`: Enables remote execution in repo rules.
* **[MISSING]** `--experimental_sibling_repository_layout`: Enables sibling repository layout.
* **[MISSING]** `--internal_starlark_flag_test_canary`: Test canary flag.
* **[MISSING]** `--incompatible_java_info_merge_runtime_module_flags`: Merges JavaInfo flags.
* **[MISSING]** `--experimental_single_package_toolchain_binding`: Binds toolchains to single packages.

---

### 9. Spot-Check Verifications with Prebuilt Zel Binary

All spot checks were executed against `/Users/pledger/Development/zel/target/debug/zel` in an isolated temporary directory (`/tmp/zel-test-starlark`) using `--output_base=/tmp/zel-ob-starlark`:

#### Spot Check 1: String `%` Formatting Argument Substitution Reversal
* **BUILD File:**
  ```python
  val = "%d %s" % (1, "two")
  cc_library(name = val)
  ```
* **Bazel Expected:** Produces target `//:1 two`.
* **Zel Actual:** Produces target `//:two 1`.
* **Root Cause:** `crates/zel-starlark/src/lib.rs:2282`: `out.find("%s").or_else(|| out.find("%d"))` locates `"%s"` before `"%d"`, substituting arg 0 (`1`) into `"%s"` and arg 1 (`"two"`) into `"%d"`.

#### Spot Check 2: Bitwise OR `|` Evaluates to Addition
* **BUILD File:**
  ```python
  cc_library(name = "test_" + str(5 | 3))
  ```
* **Bazel Expected:** Produces target `//:test_7` ($5 \lor 3 = 7$).
* **Zel Actual:** Produces target `//:test_8` ($5 + 3 = 8$).
* **Root Cause:** `crates/zel-starlark/src/lib.rs:1827`: `BinOp::Add | BinOp::BitOr => l.add(&r)`. Zel maps `BitOr` directly to addition on `Value::Int`.

#### Spot Check 3: Chained Comparisons Evaluate Left-Associatively to False
* **BUILD File:**
  ```python
  cc_library(name = "test_" + str(1 < 5 < 10))
  ```
* **Bazel Expected:** Produces target `//:test_True` ($1 < 5 \land 5 < 10$).
* **Zel Actual:** Produces target `//:test_False`.
* **Root Cause:** `crates/zel-starlark/src/lib.rs:1160–1215`: Evaluates as `(1 < 5) < 10` $\rightarrow$ `True < 10`. Mixed comparison falls through to `_ => Ok(Value::Bool(false))` on line 1859.

#### Spot Check 4: Negative Slice Indices Clamped to Zero
* **BUILD File:**
  ```python
  s = "hello"
  cc_library(name = "test_" + s[-2:])
  ```
* **Bazel Expected:** Produces target `//:test_lo`.
* **Zel Actual:** Produces target `//:test_hello`.
* **Root Cause:** `crates/zel-starlark/src/lib.rs:1922`: `Value::Int(i) => i.max(0) as usize`. Negative index `-2` clamps to `0`, slicing the entire string from index 0.

#### Spot Check 5: Comprehension Loop Variable Leaks into Outer Scope
* **BUILD File:**
  ```python
  x = "original"
  dummy = [x for x in ["leaked"]]
  cc_library(name = "test_" + x)
  ```
* **Bazel Expected:** Produces target `//:test_original` (comprehensions have isolated scope).
* **Zel Actual:** Produces target `//:test_leaked`.
* **Root Cause:** `crates/zel-starlark/src/lib.rs:1757`: `self.bind_loop_vars` writes the comprehension variable directly into `self.globals`.

#### Spot Check 6: Keyword-Only Parameter Check Ignored
* **BUILD File:**
  ```python
  def my_func(a, *, b):
      return a + b
  cc_library(name = "test_" + str(my_func(1, 2)))
  ```
* **Bazel Expected:** Execution failure: positional arguments exceeding non-keyword-only parameters.
* **Zel Actual:** Produces target `//:test_3`.
* **Root Cause:** `crates/zel-starlark/src/lib.rs:895–898`: `*` followed by `,` is discarded and following parameters are treated as ordinary positional parameters.

#### Spot Check 7: `fail()` Silently Swallowed by Package Loader
* **BUILD File:**
  ```python
  fail("Error code:", 404, "Not Found")
  ```
* **Bazel Expected:** Package loading fails immediately, terminating the build with exit code 1 or 7 and printing a full stack trace.
* **Zel Actual:** `zel build //:all` exits with code 0 (`INFO: Build completed successfully, 0 total actions (0 executed)`).
* **Root Cause:** `crates/zel-starlark/src/lib.rs:2144` discards positional arguments after the first, and `crates/zel-packages/src/lib.rs:135` package loader does not surface the error to the CLI runner.

---

---


## Loading Phase — BUILD Files, Packages, Labels, and Target Patterns

### Parity Overview

Parity in the loading phase is **LOW to PARTIAL (approx. 20-30% functional parity)**. While Zel implements basic canonical label parsing, simple package identity, and an AST/token-based BUILD evaluator for basic targets, almost every subsystem lacks Bazel's validation, semantics, and advanced capabilities. 

Key architectural deficits include:
1. **Target Pattern Resolution**: External repository target wildcards (`@repo//...`, `@@repo//pkg:all`) are completely broken due to hardcoded main-repo constructors; relative target patterns completely ignore client working directory; negative target patterns (`-//...`) are never subtracted and become invalid package paths; and querying/building a non-existent target in an existing package causes Zel to synthesize a fake target and report success.
2. **BUILD Evaluation & Starlark Execution**: Any Starlark evaluation error or missing builtin in a BUILD file is silently masked by falling back to an ad-hoc regex string scanner (`parse_build_targets` in `zel-skyframe`); functions (`def`) are illegally permitted directly in BUILD files; and any unknown identifier called in a BUILD file is dynamically assumed to be a rule instantiation.
3. **External and Transitive `.bzl` Loading**: External `.bzl` loads (`@rules_cc//...`) are completely rejected by local path resolution; instead of failing, Zel fakes every imported symbol as an empty `RuleCallable`, breaking imported constants, macros, and structs. Transitive `.bzl` loads are not evaluated.
4. **Package Model & Rule Class Validation**: Zel has no concept of a `RuleClass` schema or attribute typing. Attributes are untyped dictionaries; duplicate target names and duplicate outputs silently overwrite existing targets; mandatory attributes and file extension restrictions are not enforced; and standard attributes (`tools`, `data`, `visibility`, `testonly`, `tags`, `deprecation`, `compatible_with`, `timeout`, `size`) are stripped when converting to internal targets.
5. **Missing Built-ins & Stubs**: `package()` is an empty no-op stub that ignores all arguments (`default_visibility`, `features`, `licenses`, `default_package_metadata`); `subpackages()`, `existing_rule()`, `existing_rules()`, `package_name()`, `repo_name()`, `package_relative_label()`, `environment_group()`, and symbolic macros `macro()` are completely missing.
6. **Workspace Discovery & Isolation**: `.bazelignore` is completely ignored; `REPO.bazel` is not supported; `--deleted_packages` is missing; `--package_path` is hardcoded to `%workspace%`; and directories named `BUILD` or `BUILD.bazel` are mistaken for BUILD files.

---

### 1. Target Patterns & Command-Line Target Resolution

* **[DIFFERENT] External Repository Wildcards Crash / Parse into Main Repo**:
  - *Bazel behavior*: Target patterns like `@rules_rs//...`, `@rules_rs//src:all`, `@@rules_rs+1.0//...`, or `@rules_rs//:*` correctly identify the target repository from repository mapping and traverse that repository's package tree (`TargetPattern.java:650-750`).
  - *Zel behavior*: In `crates/zel-cmdline/src/lib.rs` (lines 510, 521, 529, 538, 548), every single wildcard branch (`TargetsBelowDirectory` and `TargetsInPackage` for `:all`, `:*`, `...`) explicitly hardcodes `PackageIdentifier::create_in_main_repo(pkg_str)`. For a pattern like `@rules_rs//...`, `pkg_str` becomes `@rules_rs`, and calling `create_in_main_repo` fails package name validation (since `@` is invalid in package names). Even if it parsed, `zel-server` line 1173 calls `find_packages_recursively` strictly inside `self.dirs.workspace_dir`. External repository wildcards are completely unsupported and fail at command line.
  - *Source refs*: `com.google.devtools.build.lib.cmdline.TargetPattern.java:650-750`; `crates/zel-cmdline/src/lib.rs:509-551`.

* **[DIFFERENT] Relative Target Patterns Ignore Client Working Directory**:
  - *Bazel behavior*: When executing commands from a subdirectory (e.g. `cd src/core && bazel build :util` or `bazel build ...`), Bazel resolves `:util` to `//src/core:util` and `...` to `//src/core/...` using the client's working directory (`TargetPattern.java:84-86`, `mainRepoParser(offset)`).
  - *Zel behavior*: `TargetPattern::parse(raw, relative_dir)` accepts a `relative_dir`, but in `crates/zel-server/src/lib.rs` (lines 1133 and 1213), the server **always passes `&PathFragment::empty()`**. Running `cd pkg1 && zel query ':rule1'` resolves to `//:rule1` instead of `//pkg1:rule1`. Running `cd pkg1 && zel query '...'` resolves to `//...` and queries every package across the entire workspace.
  - *Source refs*: `com.google.devtools.build.lib.cmdline.TargetPattern.java:84-86`; `crates/zel-server/src/lib.rs:1131-1134, 1213`.

* **[STUB] / [DIFFERENT] Negative Target Patterns Are Never Subtracted and Cause Broken Paths**:
  - *Bazel behavior*: Prefixing a target pattern with `-` (e.g. `bazel build //... -//pkg1/sub/...`) subtracts matching targets from the positive pattern set (`TargetPattern.java:52-54`, `ContainsResult.DIRECTORY_EXCLUSION_WOULD_BE_EXACT`).
  - *Zel behavior*: `crates/zel-cmdline/src/lib.rs` defines a struct `SignedTargetPattern`, but it is **never used** anywhere in `zel-server` or `zel-client`. In `crates/zel-server/src/lib.rs` (lines 885-894), `handle_build` loops over `parsed.targets` and sends each pattern independently to `resolve_targets_in_workspace`. `TargetPattern::parse("-//pkg1/sub/...")` does not recognize leading `-`; it treats it as a relative pattern and normalizes it to `"//-//pkg1/sub/..."`. The negative pattern evaluates to an empty set of targets and leaves the positive targets untouched (e.g. `zel build //pkg1/... -//pkg1/sub/...` builds all 3 targets including `subrule`).
  - *Source refs*: `com.google.devtools.build.lib.cmdline.SignedTargetPattern.java`; `crates/zel-cmdline/src/lib.rs:565-585`; `crates/zel-server/src/lib.rs:885-894, 1131-1135`.

* **[MISSING] `--target_pattern_file` Flag**:
  - *Bazel behavior*: Supports `--target_pattern_file=<file>` (or `-` for stdin) to supply a newline-delimited list of target patterns (`BuildRequestOptions.java:514`, `TargetPatternsHelper.java:60-89`).
  - *Zel behavior*: The flag `--target_pattern_file` is completely missing from `crates/zel-options` and `crates/zel-server`.
  - *Source refs*: `com.google.devtools.build.lib.buildtool.BuildRequestOptions.java:514`; `crates/zel-options/src/lib.rs`.

* **[STUB] / [DIFFERENT] Target Pattern Forms `:all` vs `:*` / `:all-targets` (`include_files`)**:
  - *Bazel behavior*: `:all` matches all rule targets in a package; `:*` and `:all-targets` match all targets including source (`InputFile`) and generated (`OutputFile`) targets (`TargetPattern.java:534-551`).
  - *Zel behavior*: `TargetPattern::parse` sets `include_files: true` on `TargetsInPackage` for `:*` and `:all-targets`, but `crates/zel-server/src/lib.rs` (lines 1160-1170) **ignores `include_files` entirely** (`let TargetPattern::TargetsInPackage { package, .. } = pat`). Furthermore, `PackageValue.targets` in `zel-skyframe` (lines 634-647) only stores `pkg.rules()`, completely omitting source and generated files. Thus `:all` and `:*` behave identically in `zel build`, only returning rules.
  - *Source refs*: `com.google.devtools.build.lib.cmdline.TargetPattern.java:534-551`; `crates/zel-cmdline/src/lib.rs:468-471`; `crates/zel-server/src/lib.rs:1160-1170`; `crates/zel-skyframe/src/lib.rs:634-647`.

* **[DIFFERENT] Non-Existent Target Synthesis in Existing Packages**:
  - *Bazel behavior*: Querying or building a non-existent target name in an existing package (e.g. `bazel build //pkg:nonexistent`) fails with `NoSuchTargetException`: `no such target '//pkg:nonexistent': target 'nonexistent' not declared in package 'pkg'` (exit code 7).
  - *Zel behavior*: In `crates/zel-server/src/lib.rs` (lines 1148-1158), if `pkg_val.targets.get(label.name())` is `None`, Zel synthesizes a fake target:
    ```rust
    // Synthesize source/implicit target if package exists
    Ok(vec![RuleTarget {
        label: label.clone(),
        rule_class: "target".to_string(),
        srcs: Vec::new(), deps: Vec::new(), cmd: None, outs: Vec::new(),
    }])
    ```
    Running `zel build //pkg1:definitely_does_not_exist` or `zel query //pkg1:definitely_does_not_exist` returns exit code 0 and reports "Build completed successfully, 1 total actions (1 executed)". Non-existent targets are fabricated rather than rejected.
  - *Source refs*: `com.google.devtools.build.lib.packages.NoSuchTargetException.java`; `crates/zel-server/src/lib.rs:1148-1158`.

---
### 2. Labels, Repository Names, and Repository Mappings

* **[DIFFERENT] Repository Name Validation Omits Bzlmod Tilde (`~`)**:
  - *Bazel behavior*: Bzlmod canonical repository names contain tildes (`~`) to separate module names, versions, and extension names (e.g. `@@rules_cc~0.0.9`, `@@rules_rust~0.30.0~rust~rust_host`). `VALID_REPO_NAME` in `RepositoryName.java:54` accepts `[\w\-.~+]*` (where `\w` or explicit rules include tildes and alphanumeric chars).
  - *Zel behavior*: `RepositoryName::validate` in `crates/zel-cmdline/src/lib.rs` (lines 195-201) validates against:
    ```rust
    let valid = name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'));
    ```
    The tilde character `~` is missing from the whitelist. Canonical Bzlmod repository names containing `~` fail validation with `invalid repository name: repo names may contain only A-Z, a-z, 0-9, '-', '_', '.' and '+'`.
  - *Source refs*: `com.google.devtools.build.lib.cmdline.RepositoryName.java:54, 143-160`; `crates/zel-cmdline/src/lib.rs:185-203`.

* **[MISSING] User-Provided Repository Name Validation (`validateUserProvidedRepoName`)**:
  - *Bazel behavior*: Bazel distinguishes arbitrary internal canonical repo names from user-provided repo names (e.g. names in `MODULE.bazel` or `WORKSPACE`). User-provided repo names are restricted to `[a-zA-Z0-9][-.\w]*$` and cannot contain `+` (`RepositoryName.java:166-173`).
  - *Zel behavior*: Zel only provides a single permissive `validate` method that accepts `+` everywhere and does not restrict user-provided names.
  - *Source refs*: `com.google.devtools.build.lib.cmdline.RepositoryName.java:166-173`; `crates/zel-cmdline/src/lib.rs:185-203`.

* **[MISSING] Strict Bzlmod Repository Visibility Enforcement**:
  - *Bazel behavior*: `RepositoryMapping.get(preMappingName)` checks if the apparent repository is declared as a visible dependency for the context repository. If unmapped, it marks the repository name as non-visible (`RepositoryName.toNonVisible(contextRepo, didYouMeanSuffix)`). Using an unmapped apparent repo in a label fails with a strict visibility error explaining that the repo is not visible from the current module, with spelling suggestions (`RepositoryMapping.java:133-140`).
  - *Zel behavior*: `crates/zel-cmdline/src/lib.rs` (lines 255-264):
    ```rust
    self.entries.get(apparent_name).cloned().unwrap_or_else(|| RepositoryName::create_unvalidated(apparent_name))
    ```
    If an apparent repository is undeclared, Zel silently creates an unvalidated repository name with no visibility check, completely defeating Bzlmod module boundary isolation.
  - *Source refs*: `com.google.devtools.build.lib.cmdline.RepositoryMapping.java:133-140`; `crates/zel-cmdline/src/lib.rs:255-264`.

* **[MISSING] Inverse Repository Mapping (`getInverse`, `withCachedInverseMap`) & Display Form**:
  - *Bazel behavior*: `RepositoryMapping` provides `getInverse(postMappingName)` and cached inverse maps to translate canonical repository names (e.g. `@@rules_rust~0.30.0`) back to user-friendly apparent names (e.g. `@rules_rust`) when printing labels via `Label.getDisplayForm()` or `Label.getShorthandDisplayForm()` (`RepositoryMapping.java:145-165`, `Label.java:493-515`).
  - *Zel behavior*: Zel has no inverse repository mapping. `Label.shorthand_form()` in `crates/zel-cmdline/src/lib.rs` (lines 445-453) only strips `:target` if the target name matches the last directory segment of the package; it leaves canonical repo names intact (e.g. `@@foo+1.0//bar` instead of `@foo//bar`).
  - *Source refs*: `com.google.devtools.build.lib.cmdline.RepositoryMapping.java:145-165`; `com.google.devtools.build.lib.cmdline.Label.java:493-515`; `crates/zel-cmdline/src/lib.rs:445-453`.

* **[DIFFERENT] Starlark `Label()` Constructor Rejects Relative Labels and Defaults to Fake Value**:
  - *Bazel behavior*: In Starlark, `Label("//pkg:target")`, `Label(":target")`, and `Label("target")` are resolved relative to the calling `.bzl` file's repository and package context (`BazelModuleContext.java`, `Label.java:199-225`). Calling `Label()` with zero arguments is a TypeError.
  - *Zel behavior*: In `crates/zel-starlark/src/lib.rs` (lines 2085-2093):
    ```rust
    "Label" => {
        let s = eval_pos.first().and_then(|v| v.as_str()).unwrap_or("//:default");
        let label = Label::parse_canonical(s).map_err(|e| StarlarkError(e.to_string()))?;
        return Ok(Value::Label(label));
    }
    ```
    `Label()` only calls `parse_canonical(s)`. Relative labels like `Label(":foo")` or `Label("foo")` fail with `LabelSyntaxError: absolute label must start with '//' or '@'`. If called with no arguments (`Label()`), it silently returns `//:default`.
  - *Source refs*: `com.google.devtools.build.lib.cmdline.Label.java:199-225`; `crates/zel-starlark/src/lib.rs:2085-2093`.

* **[MISSING] Contextual Label Parsing and Label Interning**:
  - *Bazel behavior*: `Label.java` provides `parseWithRepoContext`, `parseWithPackageContext`, `getSamePackageLabel`, `getRelative`, unambiguous canonical forms (`getUnambiguousCanonicalForm`), and weak reference interning (`LabelInterner`) to deduplicate label instances across millions of graph nodes (`Label.java:112, 199, 216, 481, 562`).
  - *Zel behavior*: None of these methods exist. Labels and package identifiers store independent heap-allocated `String` and `PathFragment` objects with no deduplication or interning.
  - *Source refs*: `com.google.devtools.build.lib.cmdline.Label.java:112, 199-225, 481, 562`; `crates/zel-cmdline/src/lib.rs:324-460`.

---
### 3. BUILD File Evaluation, Starlark Execution, and Error Masking

* **[STUB] / [DIFFERENT] BUILD Evaluation Errors Silently Masked by Regex Fallback Scanner**:
  - *Bazel behavior*: Any syntax error, invalid statement, missing identifier, or failed `.bzl` load in a BUILD file causes Bazel to halt with a fatal `BuildFileContainsErrorsException` or `Starlark.EvalException` (`PackageFactory.java:561-566`).
  - *Zel behavior*: In `crates/zel-skyframe/src/lib.rs` (lines 626-656), if `Package::evaluate_build_file` returns `Err` or an empty map of targets, it **silently catches the error and falls back to `parse_build_targets`**:
    ```rust
    let (targets, package_opt) = match Package::evaluate_build_file(...) {
        Ok(pkg) => {
            let mut map = ...;
            if map.is_empty() {
                if let Ok(fallback) = parse_build_targets(pkg_id, &content) {
                    map = fallback;
                }
            }
            (map, Some(pkg))
        }
        Err(_) => (parse_build_targets(pkg_id, &content)?, None),
    };
    ```
    `parse_build_targets` (lines 731-820) is an ad-hoc regex/character scanner that extracts `rule(name = "...", srcs = [...])` from raw file text. If a BUILD file has invalid syntax, undefined variables, or unsupported builtins, the Starlark error is swallowed completely and the regex parser silently returns whatever it can scrape.
  - *Source refs*: `com.google.devtools.build.lib.packages.PackageFactory.java:561-566`; `crates/zel-skyframe/src/lib.rs:626-656, 731-820`.

* **[DIFFERENT] Function Definitions (`def`) Permitted in BUILD Files**:
  - *Bazel behavior*: Bazel's `DotBazelFileSyntaxChecker` strictly disallows `def` statements in BUILD files (`DotBazelFileSyntaxChecker.java:41, 72-79`). Attempting to define a function in a BUILD file raises: `syntax error: function definitions are not allowed in BUILD files; move them to a .bzl file and load them`.
  - *Zel behavior*: `crates/zel-starlark/src/lib.rs` has no AST syntax check restricting BUILD files. Functions defined via `def` execute directly in BUILD files without error. Zel's own test suite (`crates/zel-starlark/src/lib.rs:2331-2342`) defines macros with `def my_cc_macro(...)` directly inside BUILD strings.
  - *Source refs*: `com.google.devtools.build.lib.packages.DotBazelFileSyntaxChecker.java:34-45, 72-79`; `crates/zel-starlark/src/lib.rs:1550-1560, 2331-2342`.

* **[DIFFERENT] Any Unknown Identifier in BUILD File Evaluates as a Rule**:
  - *Bazel behavior*: In Bazel, only valid predefined rules or symbols loaded via `load()` are callable. Calling an unknown identifier raises an `EvalException` (`name 'foo_bar' is not defined`).
  - *Zel behavior*: In `crates/zel-starlark/src/lib.rs` (lines 1718-1723), when evaluating an identifier that is not in `globals`:
    ```rust
    Ok(Value::RuleCallable {
        rule_class: name.clone(),
        executable: name.ends_with("_binary"),
        test: name.ends_with("_test"),
    })
    ```
    Every misspelled identifier, undefined variable, or missing import automatically evaluates to a callable rule. Calling `nonexistent_tool(name = "x")` is accepted without error, recording a rule of class `"nonexistent_tool"`.
  - *Source refs*: `com.google.devtools.build.lib.packages.PackageFactory.java:544-550`; `crates/zel-starlark/src/lib.rs:1718-1723, 2196-2201`.

---
### 4. Package Model, RuleClass Schema, and Attribute Validation

* **[MISSING] `RuleClass` Schema and Attribute Type System**:
  - *Bazel behavior*: Every rule class (`RuleClass.java`, `Attribute.java`) defines a schema specifying attribute types (`Type.INTEGER`, `Type.STRING`, `BuildType.LABEL`, `BuildType.LABEL_LIST`, `BuildType.OUTPUT`, `BuildType.OUTPUT_LIST`, `Type.BOOLEAN`, `Type.TRISTATE`, `Types.STRING_LIST`, `Types.STRING_DICT`), mandatory attributes, allowed values, allowed file extensions, and default values. Rules are validated at instantiation time; providing an invalid type, unexpected attribute name, or missing mandatory attribute produces an immediate evaluation error (`RuleFactory.java`).
  - *Zel behavior*: Zel has no `RuleClass` registry, no attribute typing, and no schema validation. In `crates/zel-packages/src/lib.rs` (lines 144-224), attributes are stored directly as a raw `BTreeMap<String, Value>`. Any arbitrary attribute name and type is accepted. Mandatory attributes (such as `srcs` or `cmd` on `genrule`) are never enforced during package loading.
  - *Source refs*: `com.google.devtools.build.lib.packages.RuleClass.java`; `com.google.devtools.build.lib.packages.Attribute.java`; `com.google.devtools.build.lib.packages.RuleFactory.java`; `crates/zel-packages/src/lib.rs:144-224`.

* **[PARTIAL] Standard Rule Attributes Stripped During Conversion to `RuleTarget`**:
  - *Bazel behavior*: Rules retain all standard Bazel attributes (`tools`, `data`, `visibility`, `testonly`, `tags`, `compatible_with`, `target_compatible_with`, `exec_compatible_with`, `exec_properties`, `features`, `deprecation`, `flaky`, `local`, `shard_count`, `size`, `timeout`, `args`, `env`, etc.) throughout analysis and execution (`Rule.java`).
  - *Zel behavior*: Although `RuleInstance` in `crates/zel-packages/src/lib.rs` (lines 26-39) retains some attributes (`attrs`, `tools`, `data`, `visibility`, `testonly`), when converting to `RuleTarget` in `zel-skyframe` (lines 639-645) and `zel-server` (line 1150):
    ```rust
    RuleTarget {
        label: rule.label.clone(),
        rule_class: rule.rule_class.clone(),
        srcs: rule.srcs.clone(),
        deps: rule.deps.clone(),
        cmd: rule.cmd.clone(),
        outs: rule.outs.clone(),
    }
    ```
    `tools`, `data`, `visibility`, `testonly`, `tags`, `attrs`, and all other Bazel attributes are completely discarded.
  - *Source refs*: `com.google.devtools.build.lib.packages.Rule.java`; `crates/zel-packages/src/lib.rs:26-39`; `crates/zel-skyframe/src/lib.rs:639-645`; `crates/zel-server/src/lib.rs:1150-1157, 1292-1302`.

* **[DIFFERENT] Duplicate Target Names Silently Overwrite**:
  - *Bazel behavior*: Defining two targets with the same name in the same package raises a fatal `TargetAlreadyExistsException` (`Package.java`, `TargetDefinitionContext.java: duplicate target 'foo' declared in package '//pkg'`).
  - *Zel behavior*: In `crates/zel-packages/src/lib.rs` (lines 158, 223), Zel calls `targets.insert(name.clone(), Target::Rule(rule))`. The second target silently overwrites the first target with no warning or error.
  - *Source refs*: `com.google.devtools.build.lib.packages.Package.java`; `crates/zel-packages/src/lib.rs:158, 223`.

* **[DIFFERENT] Duplicate Output Files Silently Overwrite**:
  - *Bazel behavior*: If multiple rules declare the same output file or if an output file conflicts with a rule name, Bazel halts with a collision error (`target 'out.txt' is generated by both rules '//pkg:r1' and '//pkg:r2'`).
  - *Zel behavior*: In `crates/zel-packages/src/lib.rs` (lines 196-207), Zel iterates over `outs` and unconditionally calls `targets.insert(clean.to_string(), Target::OutputFile { ... })`. If two rules output the same file, the second silently overwrites the map entry.
  - *Source refs*: `com.google.devtools.build.lib.packages.Package.java`; `crates/zel-packages/src/lib.rs:196-207`.

* **[DIFFERENT] Input Files Lack Filesystem Existence Verification**:
  - *Bazel behavior*: Any source file referenced in `srcs` that is not declared by a rule is verified against the filesystem during package loading. If the file does not physically exist in the package directory on disk, loading fails: `no such target '//pkg:foo.txt': target 'foo.txt' not declared in package 'pkg' and no corresponding file exists on disk` (`Package.java`, `InputFile.java`).
  - *Zel behavior*: In `crates/zel-packages/src/lib.rs` (lines 185-195), Zel registers `Target::InputFile` for every string in `srcs` that does not start with `//` or `@` **without checking the filesystem**. A build rule referencing non-existent files registers those files as valid `InputFile` targets during package evaluation. Conversely, files existing on disk that are not referenced in `srcs` or `exports_files` are not registered in `Package.targets`.
  - *Source refs*: `com.google.devtools.build.lib.packages.InputFile.java`; `crates/zel-packages/src/lib.rs:185-195`.

* **[MISSING] Implicit Outputs Function**:
  - *Bazel behavior*: Rules declare implicit output functions (`ImplicitOutputsFunction.java`) that derive secondary target outputs from the rule's name or attributes (e.g. `foo.jar` and `foo_deploy.jar` for `java_binary(name = "foo")`; `foo.exe` on Windows for `cc_binary`). These implicit outputs can be queried and depended upon directly as labels.
  - *Zel behavior*: Zel only extracts outputs from literal `outs` or `out` attributes in `call.kwargs` (`crates/zel-packages/src/lib.rs:164`). Implicit outputs are not calculated or registered in `Package.targets`.
  - *Source refs*: `com.google.devtools.build.lib.packages.ImplicitOutputsFunction.java`; `crates/zel-packages/src/lib.rs:164, 196-207`.

---
### 5. Package-Level Functions & Globals in BUILD Files

* **[STUB] `package(...)` Function Is an Empty No-Op**:
  - *Bazel behavior*: The `package()` function sets package-wide metadata (`PackageCallable.java:52-80`, `PackageArgs.java:164-203`). It accepts:
    - `default_visibility` (sets base visibility for all rules in the package)
    - `default_testonly`
    - `default_deprecation`
    - `features`
    - `licenses`
    - `default_compatible_with`
    - `default_restricted_to`
    - `default_package_metadata` / `default_applicable_licenses`
    - `default_hdrs_check`
    - `transitive_visibility`
    Bazel errors if `package()` is called more than once per BUILD file, if called with unknown arguments, or if called with no arguments.
  - *Zel behavior*: In `crates/zel-starlark/src/lib.rs` (lines 2100-2102):
    ```rust
    "package" | "licenses" | "register_toolchains" | "register_execution_platforms" => {
        return Ok(Value::None);
    }
    ```
    `package()` is a hardcoded stub returning `Value::None`. All arguments are discarded; `default_visibility` in `Package` is permanently initialized to `Vec::new()` (`crates/zel-packages/src/lib.rs:231`); calling it multiple times does not error; and unknown arguments are silently ignored.
  - *Source refs*: `com.google.devtools.build.lib.packages.PackageCallable.java:52-80`; `com.google.devtools.build.lib.packages.PackageArgs.java:164-203`; `crates/zel-starlark/src/lib.rs:2100-2102`; `crates/zel-packages/src/lib.rs:231`.

* **[MISSING] Visibility Enforcement & `package_group` Includes**:
  - *Bazel behavior*: Bazel strictly validates visibility declarations (`//visibility:public`, `//visibility:private`, `//pkg:__pkg__`, `//pkg:__subpackages__`, or package group labels) and prevents targets from referencing dependencies outside their declared visibility (`RuleVisibility.java`, `PackageGroup.java`). `package_group` supports `packages` and `includes` (which recursively includes other package groups).
  - *Zel behavior*: In `crates/zel-packages/src/lib.rs` (lines 152-160), `package_group` only parses `packages`; `includes` is completely ignored. Target visibility is parsed as a string list on rules, but everywhere a rule is converted (`zel-server:1302`, `zel-skyframe:710`, `zel-rules:440`), visibility is reset to `Vec::new()`. Zel contains **zero visibility enforcement**; private targets can be depended upon from any package in the workspace without warning or error.
  - *Source refs*: `com.google.devtools.build.lib.packages.PackageGroup.java`; `com.google.devtools.build.lib.packages.RuleVisibility.java`; `crates/zel-packages/src/lib.rs:152-160`; `crates/zel-server/src/lib.rs:1302`.

* **[PARTIAL] / [DIFFERENT] `exports_files(...)` Ignores Keyword Arguments and Visibility**:
  - *Bazel behavior*: `exports_files(srcs, visibility=None, licenses=None)` allows exposing source files to other packages. It accepts positional or keyword arguments (`exports_files(srcs = ["foo.txt"], visibility = ["//visibility:public"])`). Exporting a non-existent file or a generated file fails loading (`StarlarkNativeModuleApi.java:222-256`).
  - *Zel behavior*: In `crates/zel-starlark/src/lib.rs` (lines 2094-2098):
    ```rust
    "exports_files" => {
        if let Some(first) = eval_pos.first() {
            self.exports_files.extend(first.to_string_list());
        }
        return Ok(Value::None);
    }
    ```
    If called with the standard keyword argument `exports_files(srcs = ["foo.txt"])`, `eval_pos.first()` is `None` and the call is a no-op! `visibility` and `licenses` arguments are completely ignored. Furthermore, `Package::evaluate_build_file` does not verify if exported files exist on disk.
  - *Source refs*: `com.google.devtools.build.lib.starlarkbuildapi.StarlarkNativeModuleApi.java:222-256`; `crates/zel-starlark/src/lib.rs:2094-2098`; `crates/zel-packages/src/lib.rs:138-142`.

* **[MISSING] `subpackages(...)` Builtin**:
  - *Bazel behavior*: Returns a sorted, mutable list of all direct subpackages of the current package (`subpackages(include, exclude=[], allow_empty=False)`), checking against glob patterns and failing if empty when `allow_empty=False` (`StarlarkNativeModuleApi.java:366-400`).
  - *Zel behavior*: `subpackages()` is not implemented in Zel. Calling it in a BUILD file triggers the unknown identifier fallback and evaluates to an empty `RuleCallable` named `"subpackages"`.
  - *Source refs*: `com.google.devtools.build.lib.starlarkbuildapi.StarlarkNativeModuleApi.java:366-400`; `crates/zel-starlark/src/lib.rs:2020-2140`.

* **[MISSING] `existing_rule(...)` & `existing_rules(...)` Builtins**:
  - *Bazel behavior*: `existing_rule(name)` returns an immutable dict-like object describing the attributes of a rule instantiated so far in the package. `existing_rules(omit_macro_internals=...)` returns a dictionary of all rules defined so far (`StarlarkNativeModuleApi.java:101-186`). Used extensively by Bazel macros and legacy build scripts.
  - *Zel behavior*: Neither `existing_rule` nor `existing_rules` is implemented in `zel-starlark`. Calling them in a BUILD file evaluates as a `RuleCallable` or crashes `.bzl` evaluation as an undefined symbol.
  - *Source refs*: `com.google.devtools.build.lib.starlarkbuildapi.StarlarkNativeModuleApi.java:101-186`; `crates/zel-starlark/src/lib.rs:2020-2140`.

* **[MISSING] `package_name()`, `repo_name()`, `repository_name()` Builtins**:
  - *Bazel behavior*: `package_name()` returns the current package name (e.g. `"foo/bar"` or `""` at root); `repo_name()` returns the canonical repository name (e.g. `""` for main repo); and `repository_name()` returns the `@`-prefixed repository name (`StarlarkNativeModuleApi.java:259-302`).
  - *Zel behavior*: None of these functions exist in `crates/zel-starlark`. Calling `package_name()` in a BUILD file fails Starlark evaluation (which then triggers the regex fallback parser, hiding the failure).
  - *Source refs*: `com.google.devtools.build.lib.starlarkbuildapi.StarlarkNativeModuleApi.java:259-302`; `crates/zel-starlark/src/lib.rs:2020-2140`.

* **[MISSING] `package_relative_label(input)` Builtin**:
  - *Bazel behavior*: Normalizes a string or label into a `Label` object in the context of the package currently being initialized (`StarlarkNativeModuleApi.java:304-337`).
  - *Zel behavior*: Function is missing.
  - *Source refs*: `com.google.devtools.build.lib.starlarkbuildapi.StarlarkNativeModuleApi.java:304-337`.

* **[MISSING] `module_name()` and `module_version()` Builtins**:
  - *Bazel behavior*: Returns the Bzlmod module name and version for the repository containing the package (`StarlarkNativeModuleApi.java:340-363`).
  - *Zel behavior*: Functions are missing.
  - *Source refs*: `com.google.devtools.build.lib.starlarkbuildapi.StarlarkNativeModuleApi.java:340-363`.

* **[MISSING] `environment_group(...)` Builtin**:
  - *Bazel behavior*: Declares sets of compatible environments and default environments for target constraint checking (`BuildGlobals.java:44-101`).
  - *Zel behavior*: Not implemented; falls back to an untyped `RuleCallable`.
  - *Source refs*: `com.google.devtools.build.lib.packages.BuildGlobals.java:44-101`.

* **[STUB] `licenses(...)` and Missing `distribs(...)`**:
  - *Bazel behavior*: Declares package license types (`BuildGlobals.java:103-134`) and distribution types.
  - *Zel behavior*: `licenses` is a no-op stub returning `None` (`crates/zel-starlark/src/lib.rs:2100`), and `distribs` is missing.
  - *Source refs*: `com.google.devtools.build.lib.packages.BuildGlobals.java:103-134`; `crates/zel-starlark/src/lib.rs:2100`.

---
### 6. Glob Semantics (`glob`)

* **[DIFFERENT] `allow_empty` Parameter Ignored**:
  - *Bazel behavior*: `glob(include, exclude=[], exclude_directories=1, allow_empty=...)` supports `allow_empty=False` (enforced by default under modern Bazel semantics). If an include pattern matches no files, or if all matches are excluded, Bazel raises an `EvalException: glob pattern '...' didn't match anything, but allow_empty is set to False` (`StarlarkNativeModuleApi.java:82-98`).
  - *Zel behavior*: In `crates/zel-starlark/src/lib.rs` (lines 2031-2064), the `glob` handler parses `include`, `exclude`, and `exclude_directories`, but completely ignores `allow_empty`. If a glob matches nothing, it silently returns an empty list `[]` without error even when `allow_empty = False` is explicitly specified.
  - *Source refs*: `com.google.devtools.build.lib.starlarkbuildapi.StarlarkNativeModuleApi.java:82-98`; `crates/zel-starlark/src/lib.rs:2031-2064`.

* **[DIFFERENT] Positional `exclude_directories` Not Supported**:
  - *Bazel behavior*: Bazel accepts `exclude_directories` as the 3rd positional argument: `glob(["*.cc"], [], 0)` (`StarlarkNativeModuleApi.java:95`).
  - *Zel behavior*: In `crates/zel-starlark/src/lib.rs` (lines 2042-2049), `exclude_dirs` is only read from `eval_kw.get("exclude_directories")`. Positional `exclude_directories` arguments are completely ignored.
  - *Source refs*: `com.google.devtools.build.lib.starlarkbuildapi.StarlarkNativeModuleApi.java:95`; `crates/zel-starlark/src/lib.rs:2042-2049`.

* **[MISSING] Skyframe Dependency Tracking for Globs**:
  - *Bazel behavior*: In Bazel, glob evaluation in Skyframe (`GlobFunction.java`, `DirectoryListingStateFunction.java`) registers fine-grained Skyframe dependency edges on directory listings. Adding or deleting a file matching a glob invalidates the glob node and triggers automatic package re-evaluation.
  - *Zel behavior*: `evaluate_glob` in `crates/zel-vfs/src/lib.rs` (lines 837-867) directly invokes `fs.readdir` recursively on the physical filesystem. It does not create or register any Skyframe nodes. Dynamic changes to the directory structure on disk do not properly invalidate Skyframe `PackageValue` nodes for incremental builds.
  - *Source refs*: `com.google.devtools.build.lib.skyframe.GlobFunction.java`; `crates/zel-vfs/src/lib.rs:837-867`.

* **[DIFFERENT] Subpackage Boundary & Directory File Confusion in Globs**:
  - *Bazel behavior*: Globs stop traversing subpackages (directories containing a regular `BUILD` or `BUILD.bazel` file). Globs also respect `.bazelignore` and `--deleted_packages`.
  - *Zel behavior*: In `crates/zel-vfs/src/lib.rs` (lines 885-890):
    ```rust
    // Skip subpackages (directories containing BUILD or BUILD.bazel)
    if fs.exists(&child_abs.get_relative_str("BUILD"), true)
        || fs.exists(&child_abs.get_relative_str("BUILD.bazel"), true)
    {
        continue;
    }
    ```
    1. If a directory named `BUILD/` or `BUILD.bazel/` exists, `fs.exists` returns `true` (since `stat` succeeds), incorrectly treating non-BUILD directories as package boundaries.
    2. Zel never checks `.bazelignore` or `--deleted_packages` during glob traversal. Files inside `.bazelignore` directories are returned by `glob` unless explicitly excluded by pattern.
  - *Source refs*: `com.google.devtools.build.lib.vfs.UnixGlob.java`; `crates/zel-vfs/src/lib.rs:885-890`.

---
### 7. Macros & Starlark `.bzl` Loading Semantics

* **[MISSING] Symbolic Macros (`macro(...)`)**:
  - *Bazel behavior*: Bazel 7/8 introduced Symbolic Macros (`MacroClass.java`, `MacroInstance.java`) created via `macro(implementation = ..., attrs = ..., doc = ..., finalizer = False)`. Symbolic macros enforce strict schema typing on arguments, isolate macro internals from `existing_rules()`, enforce naming conventions on generated targets, and support rule finalizers.
  - *Zel behavior*: Zel has no support for `macro()`. The `macro` builtin does not exist in `zel-starlark`.
  - *Source refs*: `com.google.devtools.build.lib.packages.MacroClass.java`; `com.google.devtools.build.lib.packages.MacroInstance.java`.

* **[DIFFERENT] External `.bzl` Files Are Rejected and Symbols Faked as `RuleCallable`**:
  - *Bazel behavior*: In Bazel, `load("@rules_rust//rust:defs.bzl", "rust_library")` fetches/loads the external repository's `.bzl` file and evaluates its exported symbols.
  - *Zel behavior*: In `crates/zel-packages/src/lib.rs` (lines 257-279), `resolve_local_bzl_path` explicitly refuses to resolve external modules:
    ```rust
    if module_label.starts_with('@') {
        return None;
    }
    ```
    External `.bzl` files are never read or evaluated. Then, in `crates/zel-starlark/src/lib.rs` (lines 1564-1574):
    ```rust
    for (local_name, exported_name) in &load.symbols {
        if !self.globals.contains_key(local_name) {
            self.globals.insert(
                local_name.clone(),
                Value::RuleCallable {
                    rule_class: exported_name.clone(),
                    executable: exported_name.ends_with("_binary"),
                    test: exported_name.ends_with("_test"),
                },
            );
        }
    }
    ```
    Zel **fakes all loaded symbols** as `RuleCallable` objects. If a `.bzl` loads a helper function, a struct, or a constant (e.g. `load("@rules_cc//cc:defs.bzl", "CC_TOOLCHAIN_TYPE")`), the symbol becomes a fake callable rule rather than its actual value.
  - *Source refs*: `crates/zel-packages/src/lib.rs:257-279`; `crates/zel-starlark/src/lib.rs:1564-1574`.

* **[MISSING] Transitive `.bzl` Loading and Cycle Detection**:
  - *Bazel behavior*: When a `.bzl` file loads another `.bzl` file (`a.bzl` -> `b.bzl`), Skyframe evaluates the load graph transitively (`BzlLoadFunction.java`), caching evaluation results and checking for dependency cycles (`cycle in load graph: a.bzl -> b.bzl -> a.bzl`).
  - *Zel behavior*: In `crates/zel-packages/src/lib.rs` (lines 118-132), local `.bzl` files are evaluated in an isolated `EvalContext` with no recursive load resolution. Any `load()` inside a `.bzl` file falls back to faking loaded symbols as `RuleCallable`. Load cycles are not detected or handled.
  - *Source refs*: `com.google.devtools.build.lib.skyframe.BzlLoadFunction.java`; `crates/zel-packages/src/lib.rs:118-132`.

* **[MISSING] Private Symbol Enforcement in `.bzl` Files**:
  - *Bazel behavior*: Identifiers starting with an underscore (`_`) in `.bzl` files are private. Attempting to load a private identifier (`load(":foo.bzl", "_private_helper")`) produces an error: `symbol '_private_helper' is private and cannot be loaded`.
  - *Zel behavior*: Zel does not check leading underscores on loaded symbols; all symbols defined in `.bzl` globals can be loaded externally.
  - *Source refs*: `com.google.devtools.build.lib.packages.PackageFactory.java`; `crates/zel-packages/src/lib.rs:125-130`.

* **[MISSING] `.bzl` Visibility (`bzl_library` / `visibility()`)**:
  - *Bazel behavior*: Bazel supports `.bzl` file visibility declarations (`visibility("private")`, `visibility(["//pkg/..."])`), restricting which packages may load a `.bzl` file (`BzlVisibility.java`).
  - *Zel behavior*: Zel does not support `.bzl` visibility.
  - *Source refs*: `com.google.devtools.build.lib.packages.BzlVisibility.java`.

---
### 8. Workspace Boundaries, Package Discovery & Filesystem Integration

* **[MISSING] `.bazelignore` Support**:
  - *Bazel behavior*: Bazel parses `.bazelignore` files at repository roots (`IgnoredSubdirectories.java`, `IgnoredSubdirectoriesFunction.java`) to prune entire directory trees (e.g. `node_modules`, `target`, `vendor`) during recursive package discovery (`//...`), globbing, and target pattern matching.
  - *Zel behavior*: Zel completely lacks `.bazelignore` parsing and awareness. In `crates/zel-server/src/lib.rs` (lines 1201-1206):
    ```rust
    if entry.dirent_type == DirentType::Directory
        && !entry.name.starts_with('.')
        && !entry.name.starts_with("bazel-")
    {
        self.find_packages_recursively(&rel_dir.get_relative_str(&entry.name), out);
    }
    ```
    Directories matching `.bazelignore` are still traversed and parsed if they contain `BUILD` or `BUILD.bazel` files.
  - *Source refs*: `com.google.devtools.build.lib.cmdline.IgnoredSubdirectories.java:39-80`; `com.google.devtools.build.lib.skyframe.IgnoredSubdirectoriesFunction.java`; `crates/zel-server/src/lib.rs:1188-1209`.

* **[MISSING] `REPO.bazel` File Support**:
  - *Bazel behavior*: Bazel 7/8 evaluates `REPO.bazel` at repository root (`RepoFileFunction.java`, `RepoFileGlobals.java`), providing `repo()` to declare repository-wide package defaults (`default_visibility`, `default_package_metadata`) and `ignore_directories()` for repo-level path exclusion.
  - *Zel behavior*: Zel does not look for, evaluate, or support `REPO.bazel`.
  - *Source refs*: `com.google.devtools.build.lib.skyframe.RepoFileFunction.java:55-185`; `com.google.devtools.build.lib.packages.RepoFileGlobals.java:29-85`.

* **[MISSING] `--deleted_packages` Flag**:
  - *Bazel behavior*: Bazel supports `--deleted_packages=<list>`, allowing users or tools to mark specific packages as nonexistent so Bazel ignores any BUILD files present in those directories (`PackageOptions.java:95-109`, `PackageLookupValue.java:55-73`).
  - *Zel behavior*: Zel does not support `--deleted_packages` in options, package lookup, or package discovery.
  - *Source refs*: `com.google.devtools.build.lib.pkgcache.PackageOptions.java:95-109`; `crates/zel-options/src/lib.rs`.

* **[STUB] `--package_path` Multi-Root Support**:
  - *Bazel behavior*: `--package_path` accepts a colon-separated list of filesystem directories (`dir1:dir2:%workspace%`). Packages and BUILD files are resolved across all roots in priority order (`PackageOptions.java:80-94`).
  - *Zel behavior*: Zel only has a hardcoded string `map.insert("package_path", "%workspace%")` in `zel-server` (line 567) returned for `zel info package_path`. The server and Skyframe's `PackageLookupFunction` (`crates/zel-skyframe/src/lib.rs:528-583`) only check the single `workspace_root` directory. Multiple roots cannot be specified or searched.
  - *Source refs*: `com.google.devtools.build.lib.pkgcache.PackageOptions.java:80-94`; `crates/zel-skyframe/src/lib.rs:528-583`; `crates/zel-server/src/lib.rs:567`.

* **[DIFFERENT] Directory vs File Collision for `BUILD` / `BUILD.bazel`**:
  - *Bazel behavior*: Bazel explicitly checks that `BUILD.bazel` or `BUILD` is a regular file (`PackageLookupFunction.java`). If a directory is named `BUILD` or `BUILD.bazel`, Bazel ignores it or reports an error; it does not treat the parent directory as a package.
  - *Zel behavior*: `find_packages_recursively` in `crates/zel-server/src/lib.rs` (lines 1190-1193) and `collect_package_files_recursive` in `crates/zel-vfs/src/lib.rs` (lines 886-887) use `fs.exists(path, true)`. If a directory named `BUILD` or `BUILD.bazel` exists, `fs.exists` returns `true`, causing Zel to treat non-package directories as packages.
  - *Source refs*: `com.google.devtools.build.lib.skyframe.PackageLookupFunction.java`; `crates/zel-server/src/lib.rs:1190-1193`; `crates/zel-vfs/src/lib.rs:886-887`.

* **[STUB] `test_suite` Expansion & Tag Filtering**:
  - *Bazel behavior*: When resolving targets for test execution or target patterns, Bazel expands `test_suite(name = "suite", tests = [...], tags = [...])` recursively into its constituent test targets (`TestSuite.java`, `TestSuiteImplicitTestsAccumulator.java`). If `tests` is omitted, it defaults to all tests in the package (`$implicit_tests`). It filters tests based on suite tags (e.g. `tags = ["-manual", "small"]`), automatically excluding `manual` tests unless explicitly specified.
  - *Zel behavior*: In `crates/zel-packages/src/lib.rs` (line 43), `is_test_rule` checks `self.rule_class == "test_suite"`, but `zel-rules` (line 124) treats `test_suite` as an empty no-op rule (`"filegroup" | "alias" | "test_suite" => Ok(...)`). There is no test expansion, no `$implicit_tests` fallback, and no tag filtering. Running `zel test //pkg:my_test_suite` tests zero targets.
  - *Source refs*: `com.google.devtools.build.lib.rules.test.TestSuite.java`; `com.google.devtools.build.lib.packages.TestSuiteImplicitTestsAccumulator.java`; `crates/zel-packages/src/lib.rs:43`; `crates/zel-rules/src/lib.rs:124`.

* **[MISSING] Native Rule Catalog in BUILD Files**:
  - *Bazel behavior*: Bazel defines an explicit catalog of native rules (`cc_library`, `cc_binary`, `java_library`, `java_binary`, `genrule`, `filegroup`, `sh_binary`, `sh_library`, `sh_test`, `alias`, `test_suite`, `config_setting`, `package_group`). Rules not in the catalog (or not imported via `.bzl`) are rejected with an unknown identifier error.
  - *Zel behavior*: Zel has no native rule table in `crates/zel-starlark`. Any arbitrary identifier called in a BUILD file is dynamically assumed to be a rule.
  - *Source refs*: `com.google.devtools.build.lib.packages.RuleClassProvider.java`; `crates/zel-starlark/src/lib.rs:1718-1723`.

* **[DIFFERENT] Query `buildfiles()` Operator Ignores `BUILD.bazel` and Loaded `.bzl` Files**:
  - *Bazel behavior*: `buildfiles(//pkg:all)` in `bazel query` returns `//pkg:BUILD.bazel` (or `BUILD`) and all `.bzl` files transitively loaded by that package's build file.
  - *Zel behavior*: In `crates/zel-query/src/lib.rs` (lines 433-443):
    ```rust
    if let Ok(build_lbl) = Label::create(lbl.package_identifier().clone(), "BUILD") {
        out.insert(build_lbl);
    }
    ```
    It hardcodes the target name `"BUILD"`, returning `//pkg:BUILD` even when the package is defined by `BUILD.bazel`. Furthermore, it completely omits all loaded `.bzl` files from the query result.
  - *Source refs*: `com.google.devtools.build.lib.query2.engine.BuildFilesFunction.java`; `crates/zel-query/src/lib.rs:433-443`.

---


## Bzlmod, External Repositories, and Mod/Fetch/Sync/Vendor Commands

### Overview & Parity Rating

Parity Rating: **Critical Gaps / Non-functional Prototype (< 5% Parity)**

While Zel contains a crate named `crates/zel-bzlmod` (430 lines) and commands named `mod`, `fetch`, `sync`, and `vendor`, the implementation is virtually non-functional beyond a toy AST parser for a tiny subset of `MODULE.bazel`. There is no repository downloading, decompression, or fetching mechanism; no repository rule execution engine (`repository_ctx` has 0 methods implemented); no module extension evaluation (`module_ctx` does not exist); no registry support (BCR, `--registry`, `source.json`, and `bazel_registry.json` are absent); no lockfile management (`MODULE.bazel.lock` and `--lockfile_mode` are absent); and external repositories cannot actually be resolved or loaded into Skyframe (which looks only inside the local `workspace_root`).

The CLI commands are almost entirely stubs or misrouted: `mod graph`, `mod deps`, and `mod explain` are routed to the identical 1-level printer of direct dependencies; `mod show_repo` prints a hardcoded fake `http_archive` string; `mod show_extension` and `mod tidy` are silent no-ops; and `mod all_paths`, `mod path`, and `mod dump_repo_mapping` are unrecognized. `fetch`, `sync`, and `vendor` are routed directly to `cmd_build_or_test`, completely bypassing their intended external repository functions and attempting normal target action execution instead.

---

### MODULE.bazel Directives & Built-in Functions (`ModuleFileGlobals`)

In Bazel, `MODULE.bazel` files are evaluated via [`ModuleFileGlobals.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java) within a Starlark environment dedicated to module evaluation. In Zel, `crates/zel-bzlmod/src/lib.rs` uses `zel_starlark::EvalContext` to evaluate the file and inspect recorded rule calls.

* **[PARTIAL] `module()` Directive Attributes and Validation:**
  - **Bazel:** Declared in [`ModuleFileGlobals.java:81-160`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java#L81-L160). Accepts `name`, `version`, `compatibility_level` (deprecated no-op), `repo_name`, and `bazel_compatibility` (list of version constraint strings like `>=7.0.0`, `-7.1.0`). Validates that `module()` is called at most once, is the very first directive in `MODULE.bazel`, validates module name format (`[a-z]([a-z0-9._-]*[a-z0-9])?`), and parses versions into relaxed SemVer.
  - **Zel:** In [`crates/zel-bzlmod/src/lib.rs:153-166`](file:///Users/pledger/Development/zel/crates/zel-bzlmod/src/lib.rs#L153-L166), parses `name`, `version`, `repo_name`, and `compatibility_level`. It completely ignores `bazel_compatibility`. It does not validate module name syntax, does not enforce that `module()` is the first directive, does not check that `module()` is called only once, and sets fallback values `name = "_main"` and `version = "0.0.0"`.

* **[PARTIAL] `bazel_dep()` Arguments and Dependency Semantics:**
  - **Bazel:** Declared in [`ModuleFileGlobals.java:234-300`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java#L234-L300). Accepts `name`, `version`, `max_compatibility_level` (deprecated no-op), `repo_name`, and `dev_dependency`. Setting `repo_name = None` marks the dependency as "nodep" (only takes effect if the module is pulled in by something else). Disallows self-dependencies (`name.equals(context.getModuleBuilder().getName())`). Checks `--ignore_dev_dependency` to drop dev dependencies.
  - **Zel:** In [`crates/zel-bzlmod/src/lib.rs:167-202`](file:///Users/pledger/Development/zel/crates/zel-bzlmod/src/lib.rs#L167-L202), parses `name`, `version`, `repo_name`, `dev_dependency`, and `max_compatibility_level`. However, nodep semantics (`repo_name = None`) simply omit the mapping entry without verifying whether another dependency introduces the module. Self-dependencies are not checked. `dev_dependency` is parsed but never filtered during resolution or checked against `--ignore_dev_dependency`.

* **[STUB] `register_toolchains()` in Module Files:**
  - **Bazel:** Declared in [`ModuleFileGlobals.java:373-423`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java#L373-L423). Accepts varargs `*toolchain_labels` and keyword `dev_dependency`. Toolchains are resolved, mapped through the module's repository mapping, and registered in Skyframe toolchain resolution order.
  - **Zel:** In [`crates/zel-bzlmod/src/lib.rs:132, 148`](file:///Users/pledger/Development/zel/crates/zel-bzlmod/src/lib.rs#L132), `registered_toolchains: Vec<String>` exists on `InterimModule`, but in `InterimModule::parse` lines 151-257, `"register_toolchains"` is missing from the match arms and is never populated. In [`crates/zel-starlark/src/lib.rs:2100-2102`](file:///Users/pledger/Development/zel/crates/zel-starlark/src/lib.rs#L2100-L2102), `register_toolchains` is a global builtin that immediately returns `Value::None`.

* **[STUB] `register_execution_platforms()` in Module Files:**
  - **Bazel:** Declared in [`ModuleFileGlobals.java:332-371`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java#L332-L371). Accepts varargs `*platform_labels` and keyword `dev_dependency`. Registers execution platforms for toolchain resolution.
  - **Zel:** In [`crates/zel-bzlmod/src/lib.rs:133, 149`](file:///Users/pledger/Development/zel/crates/zel-bzlmod/src/lib.rs#L133), `registered_execution_platforms` exists on `InterimModule`, but is not parsed from rule calls and remains empty. In `zel-starlark:2100-2102`, returns `Value::None`.

* **[MISSING] `use_extension()` Directive:**
  - **Bazel:** Declared in [`ModuleFileGlobals.java:425-480`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java#L425-L480). Accepts `extension_bzl_file`, `extension_name`, `dev_dependency`, and experimental `isolate`. Returns a `ModuleExtensionProxy` object whose methods dynamically correspond to the tag classes exported by the extension.
  - **Zel:** Completely missing in `crates/zel-bzlmod/src/lib.rs`. Calling `use_extension()` in `MODULE.bazel` causes Starlark evaluation failure.

* **[MISSING] `use_repo()` Directive:**
  - **Bazel:** Declared in [`ModuleFileGlobals.java:643-690`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java#L643-L690). Takes `extension_proxy`, positional repo names, and keyword rename arguments `**kwargs` (supporting `{name}` and `{version}` placeholders). Injects repos created by module extensions into the apparent repository mapping of the calling module.
  - **Zel:** Completely missing in `crates/zel-bzlmod/src/lib.rs`.

* **[MISSING] `override_repo()` Directive:**
  - **Bazel:** Declared in [`ModuleFileGlobals.java:692-753`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java#L692-L753). Allows root module to override repos generated by module extensions with existing visible repos. Ignored if `--ignore_dev_dependency` is enabled.
  - **Zel:** Completely missing in `crates/zel-bzlmod/src/lib.rs`.

* **[MISSING] `inject_repo()` Directive:**
  - **Bazel:** Declared in [`ModuleFileGlobals.java:755-813`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java#L755-L813). Allows root module to inject new repositories into a module extension without overriding existing ones.
  - **Zel:** Completely missing in `crates/zel-bzlmod/src/lib.rs`.

* **[MISSING] `use_repo_rule()` Directive:**
  - **Bazel:** Declared in [`ModuleFileGlobals.java:815-885`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java#L815-L885). Returns a proxy callable in `MODULE.bazel` to instantiate repository rules directly without needing a separate module extension. Supports `name` and `dev_dependency`.
  - **Zel:** Completely missing in `crates/zel-bzlmod/src/lib.rs`.

* **[MISSING] `include()` Directive:**
  - **Bazel:** Declared in [`ModuleFileGlobals.java:887-924`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java#L887-L924). Textually includes `.MODULE.bazel` segment files starting with `//` and ending with `.MODULE.bazel`. Supports `dev_dependency`.
  - **Zel:** Completely missing in `crates/zel-bzlmod/src/lib.rs`.

* **[PARTIAL] `single_version_override()`:**
  - **Bazel:** Declared in [`ModuleFileGlobals.java:927-1020`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java#L927-L1020). Accepts `module_name`, `version`, `registry`, `patches`, `patch_cmds`, and `patch_strip`. Pins module version, redirects registry, and applies patch files and patch commands.
  - **Zel:** In [`crates/zel-bzlmod/src/lib.rs:203-224`](file:///Users/pledger/Development/zel/crates/zel-bzlmod/src/lib.rs#L203-L224), parses only `module_name`, `version`, and `patches`. Ignores `registry`, `patch_cmds`, and `patch_strip`. Overrides the version during `ResolvedModuleGraph::resolve`, but the parsed `patches` are never loaded, applied, or verified.

* **[MISSING] `multiple_version_override()`:**
  - **Bazel:** Declared in [`ModuleFileGlobals.java:1022-1079`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java#L1022-L1079). Accepts `module_name`, `versions`, `registry`. Allows multiple distinct versions of the same module to coexist in the dependency graph across different compatibility levels.
  - **Zel:** Completely missing in `crates/zel-bzlmod/src/lib.rs`.

* **[PARTIAL] `archive_override()`:**
  - **Bazel:** Declared in [`ModuleFileGlobals.java:1081-1124`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java#L1081-L1124). Forwards all `**kwargs` (`urls`, `sha256`, `integrity`, `strip_prefix`, `patches`, etc.) to backing `http_archive` repo rule.
  - **Zel:** In [`crates/zel-bzlmod/src/lib.rs:236-253`](file:///Users/pledger/Development/zel/crates/zel-bzlmod/src/lib.rs#L236-L253), only parses `urls` and `integrity`. `strip_prefix`, `patches`, `sha256`, etc., are ignored. The resulting `ModuleOverride::Archive` is never used during resolution or build; the archive is never downloaded or unpacked.

* **[MISSING] `git_override()`:**
  - **Bazel:** Declared in [`ModuleFileGlobals.java:1126-1169`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java#L1126-L1169). Directs module to be fetched from a Git commit/branch/tag using `git_repository`.
  - **Zel:** Completely missing in `crates/zel-bzlmod/src/lib.rs`.

* **[PARTIAL] `local_path_override()`:**
  - **Bazel:** Declared in [`ModuleFileGlobals.java:1171-1201`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java#L1171-L1201). Replaces module source with local directory on disk via `local_repository`.
  - **Zel:** In [`crates/zel-bzlmod/src/lib.rs:225-235`](file:///Users/pledger/Development/zel/crates/zel-bzlmod/src/lib.rs#L225-L235), parses `path` into `ModuleOverride::LocalPath`. However, `ResolvedModuleGraph::resolve` does not handle `LocalPath` overrides, and the local path is never symlinked, mounted, or read into Skyframe.

* **[MISSING] `flag_alias()`:**
  - **Bazel:** Declared in [`ModuleFileGlobals.java:1203-1230`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleFileGlobals.java#L1203-L1230). Aliases command-line flags to Starlark build settings (`--@repo//flags:foo`).
  - **Zel:** Completely missing in `crates/zel-bzlmod/src/lib.rs`.

---
### Dependency Resolution, Discovery, MVS & Version Comparison

In Bazel, dependency discovery and selection are implemented via [`Discovery.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/Discovery.java), [`Selection.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/Selection.java), and [`Version.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/Version.java).

* **[MISSING] Transitive Module Discovery Pipeline:**
  - **Bazel:** `Discovery.java` runs an iterative graph exploration: starts from root `MODULE.bazel`, inspects direct dependencies, requests each dependency's `MODULE.bazel` file from configured registries (or non-registry overrides), evaluates it, inspects its dependencies, and repeats until the full unpruned dependency graph is discovered.
  - **Zel:** Has NO discovery mechanism. In [`crates/zel-bzlmod/src/lib.rs:287`](file:///Users/pledger/Development/zel/crates/zel-bzlmod/src/lib.rs#L287), `ResolvedModuleGraph::resolve` requires an externally supplied `transitive_modules: &[InterimModule]` slice. When invoked by `zel-server` in [`cmd_mod`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L700), it passes `&[]` (an empty slice). As a result, transitive dependencies are never discovered or resolved.

* **[DIFFERENT] Minimum Version Selection (MVS) Semantics:**
  - **Bazel:** Implements Go-style Minimum Version Selection in `Selection.java`. For each module, among all requested versions that satisfy compatibility constraints, the maximum requested version is selected. It accounts for `multiple_version_override`, detects incompatible diamond dependencies, prunes unused branches, and verifies yanked versions.
  - **Zel:** In [`crates/zel-bzlmod/src/lib.rs:287-314`](file:///Users/pledger/Development/zel/crates/zel-bzlmod/src/lib.rs#L287-L314), iterates through `root.deps` and `transitive_modules` updating a `BTreeMap` with `if dep.version > *entry`. It does not handle compatibility levels, does not detect cycles, does not detect multiple conflicting versions when overrides are absent, and completely ignores dev-dependency exclusions.

* **[DIFFERENT] Version Parsing and SemVer Comparison:**
  - **Bazel:** In [`Version.java:31-150`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/Version.java#L31-L150), Bazel implements a relaxed SemVer specification that properly distinguishes release identifiers, prerelease identifiers (e.g. `1.0.0-alpha < 1.0.0`), build metadata, and BCR-style suffixes (`1.8.2.bcr.1`).
  - **Zel:** In [`crates/zel-bzlmod/src/lib.rs:45-65`](file:///Users/pledger/Development/zel/crates/zel-bzlmod/src/lib.rs#L45-L65), `Version::cmp` splits on both `.` and `-` uniformly (`split(['.', '-'])`) and attempts numeric parsing. This causes incorrect comparisons: for example, in standard SemVer `1.0.0-alpha` is strictly less than `1.0.0`, but in Zel, `1.0.0-alpha` splits into `["1", "0", "0", "alpha"]` (length 4) whereas `1.0.0` splits into `["1", "0", "0"]` (length 3), causing Zel to rank `1.0.0-alpha > 1.0.0` based on vector length comparison (`a.len().cmp(&b.len())`).

* **[MISSING] Yanked Versions and Compatibility Checks:**
  - **Bazel:** Enforces yanked version checks via [`YankedVersionsFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/YankedVersionsFunction.java) against BCR `metadata.json`. Yanked versions fail the build unless explicitly allowed via `--allow_yanked_versions`. Also checks Bazel version compatibility via `bazel_compatibility` declarations.
  - **Zel:** Completely missing. No metadata lookup, no yanked version checks, and `--allow_yanked_versions` is unsupported.

---

### Registries, Registry Files & Network Fetching

Bazel resolves external dependencies through Bazel Central Registry (BCR) and custom registries configured via `--registry`.

* **[MISSING] Registry Implementation & `--registry` Flag:**
  - **Bazel:** Defined in [`RegistryFactoryImpl.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/RegistryFactoryImpl.java) and [`IndexRegistry.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/IndexRegistry.java). Defaults to `https://bcr.bazel.build`. Accepts `--registry` multiple times supporting HTTP, HTTPS, and `file://` URLs.
  - **Zel:** Completely absent. Neither `crates/zel-bzlmod` nor `crates/zel-options` defines or parses `--registry`. There is zero network client code to query registries.

* **[MISSING] `source.json` and `bazel_registry.json` Schema Handling:**
  - **Bazel:** Parses `bazel_registry.json` for registry metadata, `modules/<name>/metadata.json` for version information and yanked status, and `modules/<name>/<version>/source.json` for repository archive URLs, SHA-256 integrity hashes, patch files, and patch strip levels.
  - **Zel:** No parser or data structures exist for `source.json`, `metadata.json`, or `bazel_registry.json`.

* **[MISSING] Registry Flags:**
  - **Bazel:** Supports `--watch_registry`, `--module_mirrors`, `--allow_yanked_versions`, and `--downloader_config` in [`RepositoryOptions.java:102-155`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/repository/RepositoryOptions.java#L102-L155).
  - **Zel:** None of these flags exist or are parsed in `crates/zel-options`.

---

### Lockfile Architecture & `--lockfile_mode`

Bazel records resolved module dependencies and extension evaluations into `MODULE.bazel.lock` to guarantee reproducible builds.

* **[MISSING] Lockfile Generation & Validation:**
  - **Bazel:** Implemented in [`BazelLockFileFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/BazelLockFileFunction.java) and [`BazelLockFileValue.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/BazelLockFileValue.java). Serializes module dependency graph, registry hashes, module extension tags, generated repo specs, and file hashes.
  - **Zel:** Completely missing. Although `crates/zel-bzlmod/src/lib.rs:16` mentions `MODULE.bazel.lock` in its top doc comment, the crate contains zero code to read, generate, or validate lockfiles.

* **[MISSING] `--lockfile_mode` Flag:**
  - **Bazel:** Defined in [`RepositoryOptions.java:453-460`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/repository/RepositoryOptions.java#L453-L460). Values: `off`, `update` (default), `refresh`, `error`. When `error`, fails the build if `MODULE.bazel.lock` does not match evaluation results.
  - **Zel:** Not recognized or parsed in `crates/zel-options`.

---

### Module Extensions, Tags & `module_ctx` API

Module extensions allow Starlark rules to generate external repositories dynamically based on tags declared in `MODULE.bazel`.

* **[MISSING] Module Extension Execution Engine:**
  - **Bazel:** Evaluated by [`SingleExtensionEvalFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/SingleExtensionEvalFunction.java) and [`ModuleExtensionContext.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleExtensionContext.java). Executes the extension's implementation callback, creates repositories using repo rules, and exports them.
  - **Zel:** Completely absent. In [`crates/zel-starlark/src/lib.rs:2073-2084`](file:///Users/pledger/Development/zel/crates/zel-starlark/src/lib.rs#L2073-L2084), calling `module_extension()` simply returns a dummy `Value::RuleCallable { rule_class: "custom_rule" }`. Extension implementations are never invoked.

* **[MISSING] `module_ctx` API Surface:**
  - **Bazel:** Declared in [`ModuleExtensionContext.java:40-200`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/ModuleExtensionContext.java#L40-L200) and [`StarlarkBaseExternalContext.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/repository/starlark/StarlarkBaseExternalContext.java). Provides:
    - Module metadata: `modules` (list of modules using this extension and their tags), `facts`, `is_dev_dependency(tag)`, `is_isolated`, `root_module_has_non_dev_dependency`, `tag_sort_key(tag)`, `extension_metadata(root_module_direct_deps, root_module_direct_dev_deps, reproducible)`.
    - Context operations: `download`, `download_and_extract`, `extract`, `file`, `copy`, `read`, `getenv`, `path`, `watch`, `report_progress`, `os`, `execute`, `load_wasm`, `execute_wasm`, `which`, `wait`.
  - **Zel:** 100% missing. `module_ctx` does not exist anywhere in Zel.

* **[MISSING] Canonical Repo Naming for Extensions:**
  - **Bazel:** Follows canonical naming syntax: `module+version+ext_name+repo_name` or `ext_name++repo_name` (e.g., `rules_rust++rust_host_tools`).
  - **Zel:** Only supports the flat `name+version` format in `crates/zel-bzlmod/src/lib.rs:84`. Extension repository naming is not implemented.

---
### Repository Rules, Engine & `repository_ctx` API

In Bazel, external repositories are fetched and populated by executing repository rules defined via `repository_rule()` and executed with `StarlarkRepositoryContext`.

* **[STUB] `repository_rule()` Declaration:**
  - **Bazel:** Declared in [`StarlarkRepositoryModule.java:58-104`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/repository/starlark/StarlarkRepositoryModule.java#L58-L104). Parameters: `implementation`, `attrs`, `local`, `environ`, `configure`, `remotable`, `doc`. Returns a callable repository rule that instantiates external repositories and sets up Skyframe dependency tracking.
  - **Zel:** In [`crates/zel-starlark/src/lib.rs:2073-2084`](file:///Users/pledger/Development/zel/crates/zel-starlark/src/lib.rs#L2073-L2084), `repository_rule` is bundled into the match pattern `"rule" | "repository_rule" | "module_extension" | "aspect" | "tag_class"` and returns a generic `Value::RuleCallable { rule_class: "custom_rule" }`. It does not store the implementation function, attributes, or environment variables.

* **[MISSING] `repository_ctx` API:**
  - **Bazel:** Declared in [`StarlarkRepositoryContext.java:120-640`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/repository/starlark/StarlarkRepositoryContext.java#L120-L640) and [`StarlarkBaseExternalContext.java:630-2560`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/repository/starlark/StarlarkBaseExternalContext.java#L630-L2560). Methods available on `repository_ctx`:
    - `name`: string name of the repository.
    - `original_name`: original apparent name before mapping.
    - `workspace_root`: path to the main workspace root directory.
    - `attr`: struct containing rule attribute values.
    - `symlink(target, link_name)`: creates symbolic links in repo directory.
    - `template(path, template, substitutions, executable)`: generates file from template.
    - `delete(path)`: deletes file or directory.
    - `rename(before, after)`: renames file or directory.
    - `patch(patch_file, strip)`: applies a patch file.
    - `watch_tree(path)`: watches a directory tree for file changes.
    - `repo_metadata(reproducible)`: provides repository metadata.
    - `download(url, output, sha256, integrity, executable, allow_fail, canonical_id, auth, headers)`: downloads file.
    - `download_and_extract(url, output, sha256, integrity, type, stripPrefix, allow_fail, canonical_id, auth, headers, patch_file, patch_cmds, patch_strip)`: downloads and unpacks archive.
    - `extract(archive, output, stripPrefix, patch_file, patch_cmds, patch_strip)`: extracts local archive.
    - `file(path, content, executable)`: writes content to file in repository.
    - `copy(src, dst)`: copies file within repository.
    - `read(path, watch)`: reads text content of file.
    - `getenv(name, default)`: reads environment variable.
    - `path(path_or_label)`: resolves label or string to a `StarlarkPath`.
    - `watch(path)`: watches individual file for changes.
    - `report_progress(status)`: reports fetching progress string to UI.
    - `os`: struct containing `environ`, `name`, `arch`.
    - `execute(arguments, timeout, environment, quiet, working_directory)`: executes host process synchronously.
    - `load_wasm`, `execute_wasm`: executes WebAssembly plugins.
    - `which(program)`: resolves executable path in system `PATH`.
  - **Zel:** Exactly 0 of these 24 methods exist. `repository_ctx` is completely non-existent.

* **[MISSING] Decompression and Archive Extraction Pipeline:**
  - **Bazel:** Implemented in [`src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/repository/decompressor/). Supports `.tar.gz`, `.tgz`, `.tar.bz2`, `.tbz`, `.tar.xz`, `.txz`, `.tar.zst`, `.tzst`, `.tar.br`, `.zip`, `.7z`, and `.ar`.
  - **Zel:** No decompression or archive extraction routines exist in any Zel crate.

* **[MISSING] Downloader and Network Caching Engine:**
  - **Bazel:** Implemented in [`HttpDownloader.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/HttpDownloader.java), [`DownloadManager.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/repository/downloader/DownloadManager.java), and [`RepositoryCache.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/repository/cache/RepositoryCache.java). Supports HTTP proxying, retry with exponential backoff, SHA-256 / SRI integrity validation, content-addressable repository cache under `~/.cache/bazel/_bazel_$USER/cache/repos/v1/`, and hardlinking.
  - **Zel:** Zel has zero downloader logic for external repositories. In `zel-server/src/lib.rs:570`, `repository_cache` is returned in `bazel info` as a static path string (`self.dirs.output_user_root.get_relative_str("cache/repos/v1")`), but this directory is never created or populated.

* **[MISSING] Skyframe External Package Resolution:**
  - **Bazel:** Package loading delegates external repository packages to `RepositoryDirectoryValue` via `RepositoryDelegatorFunction` and `BzlmodRepoRuleFunction`, ensuring repositories under `$(output_base)/external/<canonical_name>` are fetched on demand before their packages are loaded.
  - **Zel:** In [`crates/zel-skyframe/src/lib.rs:545-553`](file:///Users/pledger/Development/zel/crates/zel-skyframe/src/lib.rs#L545-L553), `PackageLookupFunction` resolves all packages strictly against `self.workspace_root`. It has no concept of external repository directories or fetching hooks. Any target or load referring to an external repo (e.g. `@protobuf//...`) fails package lookup unless the directory happens to exist in the root workspace.

---

### Repository Options, Overrides & Caching Flags

* **[MISSING] Repository Command-Line Options:**
  - **Bazel:** Declared in [`RepositoryOptions.java:40-475`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/repository/RepositoryOptions.java#L40-L475). Includes:
    - `--override_repository=<name>=<path>`: redirects repository to local disk path.
    - `--override_module=<name>=<path>`: redirects module to local disk path.
    - `--inject_repository=<name>=<path>`: injects external repository.
    - `--inject_module=<name>=<path>`: injects module.
    - `--repository_cache=<path>`: specifies repository cache directory.
    - `--repo_contents_cache=<path>`: specifies repository contents cache.
    - `--distdir=<path>`: specifies offline distribution directory containing pre-downloaded tarballs.
    - `--registry=<url>`: specifies module registries.
    - `--allow_yanked_versions=<versions>`: allows building against yanked BCR modules.
    - `--check_direct_dependencies=<off|warning|error>`: checks whether direct deps are explicitly declared in `MODULE.bazel`.
    - `--check_bazel_compatibility=<error|warning|off>`: checks Bazel version compatibility.
    - `--ignore_dev_dependency`: drops all dev-dependencies.
    - `--lockfile_mode=<off|update|refresh|error>`: controls lockfile synchronization.
    - `--vendor_dir=<path>`: specifies directory for vendored external dependencies.
    - `--repository_disable_download`: disables all network access for repo fetching.
    - `--downloader_config=<path>`: configuration for URL rewriting and credential injection.
    - `--experimental_repository_cache_hardlinks`: enables hardlinking from repo cache.
    - `--experimental_repository_downloader_retries`: max retries for failed downloads.
  - **Zel:** In [`crates/zel-options/src/lib.rs:85-121, 485-532`](file:///Users/pledger/Development/zel/crates/zel-options/src/lib.rs#L85-L121), not a single one of these repository flags is defined, parsed, or stored in `StartupOptions` or `ParsedCommandOptions`.

---

### WORKSPACE & `--enable_workspace` Parity

* **[PARTIAL] WORKSPACE File Support:**
  - **Bazel:** Supported `WORKSPACE` and `WORKSPACE.bzlmod` files historically for legacy repository rules (`local_repository`, `http_archive`, `git_repository`, `bind`). In Bazel 8 and 9, WORKSPACE mode is disabled by default (`--enable_workspace=false`) and marked deprecated in [`BazelRulesModule.java:1550-1558`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/rules/BazelRulesModule.java#L1550-L1558).
  - **Zel:** Only checks `WORKSPACE`, `WORKSPACE.bazel`, and `WORKSPACE.zel` in [`crates/zel-client/src/lib.rs:35-42`](file:///Users/pledger/Development/zel/crates/zel-client/src/lib.rs#L35-L42) as filesystem boundary markers to identify the workspace root. Zel never reads, parses, or evaluates `WORKSPACE` files. `--enable_workspace` is not recognized.

---

### `mod` Subcommands & CLI Flags

Bazel's `mod` command inspects and interacts with the Bzlmod dependency graph via [`ModCommand.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/commands/ModCommand.java) and [`ModOptions.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/modcommand/ModOptions.java).

* **[DIFFERENT] `mod graph` Subcommand:**
  - **Bazel:** Generates a full transitive dependency graph starting from `<root>` or `--from` modules. Supports cycle detection (`--cycles`), depth limits (`--depth`), unused dependency inclusion (`--include_unused`), extension info (`--extension_info`), and multiple output formats (`--output=text`, `json`, `graphviz`).
  - **Zel:** In [`crates/zel-server/src/lib.rs:698-718`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L698-L718) and [`crates/zel-bzlmod/src/lib.rs:351-370`](file:///Users/pledger/Development/zel/crates/zel-bzlmod/src/lib.rs#L351-L370), `format_graph_text()` only prints direct dependencies of `<root>`. Transitive dependencies are never printed because `resolve()` is called with `&[]`. Flags `--from`, `--depth`, `--output`, etc., are completely ignored.

* **[DIFFERENT] `mod deps` Subcommand:**
  - **Bazel:** Traverses and prints direct and transitive dependencies of the specified target module(s).
  - **Zel:** In [`crates/zel-server/src/lib.rs:698`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L698), `deps` is grouped into the exact same match arm as `graph` and `explain`, producing the identical direct-dependency tree of the root module rather than the dependencies of the requested module.

* **[DIFFERENT] `mod explain` Subcommand:**
  - **Bazel:** Explains why a specific module version was chosen during MVS or why a dependency is required by tracing all incoming dependency edges and version requests.
  - **Zel:** In [`crates/zel-server/src/lib.rs:698`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L698), `explain` is grouped into the exact same match arm as `graph`, returning the raw direct-dependency tree. It does not explain version selection.

* **[STUB] `mod show_repo` Subcommand:**
  - **Bazel:** Displays detailed repository rule definitions, attributes, rule class, and whether the repo was created by a module, override, or extension proxy. Supports `--all_repos`, `--all_visible_repos`, and multiple output formats (`text`, `streamed_jsonproto`, `streamed_proto`).
  - **Zel:** In [`crates/zel-server/src/lib.rs:720-729`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L720-L729) and [`crates/zel-bzlmod/src/lib.rs:372-380`](file:///Users/pledger/Development/zel/crates/zel-bzlmod/src/lib.rs#L372-L380), `format_show_repo()` prints a hardcoded template:
    ```
    ## <clean>:
    http_archive(
      name = "<clean>+",
      version = "<ver>",
    )
    ```
    It does not query actual repo rules or attributes, and assumes every repository is an `http_archive`.

* **[STUB] `mod show_extension` Subcommand:**
  - **Bazel:** Inspects module extensions, displaying extension definition files, exported repos, usages across modules, and tag arguments.
  - **Zel:** In [`crates/zel-server/src/lib.rs:730`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L730), `show_extension` is an empty stub that immediately returns `ExitCode::SUCCESS` without printing anything.

* **[STUB] `mod tidy` Subcommand:**
  - **Bazel:** Automatically modifies `MODULE.bazel` to add missing direct dependencies, remove unused `bazel_dep` declarations, add/remove `use_repo` declarations, and sort entries canonically via `BazelModTidyFunction` and `Buildozer`.
  - **Zel:** In [`crates/zel-server/src/lib.rs:730`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L730), `tidy` is an empty stub that immediately returns `ExitCode::SUCCESS` without modifying files.

* **[MISSING] `mod all_paths` Subcommand:**
  - **Bazel:** Computes and prints all dependency paths from `--from` module(s) to target module(s).
  - **Zel:** Unrecognized. In [`crates/zel-server/src/lib.rs:731-734`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L731-L734), falls through to `ERROR: Unknown mod subcommand 'all_paths'` with exit code 2 (`COMMAND_LINE_ERROR`).

* **[MISSING] `mod path` Subcommand:**
  - **Bazel:** Computes and prints the shortest dependency path from `--from` module(s) to target module(s).
  - **Zel:** Unrecognized. Falls through to `ERROR: Unknown mod subcommand 'path'` with exit code 2.

* **[MISSING] `mod dump_repo_mapping` Subcommand:**
  - **Bazel:** Defined in [`ModCommand.java:248-265, 340-365`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/commands/ModCommand.java#L248-L265). Dumps the JSON repository mapping dictionary for specified repositories.
  - **Zel:** Unrecognized. Falls through to `ERROR: Unknown mod subcommand 'dump_repo_mapping'` with exit code 2.

* **[MISSING] `mod` Command Flags:**
  - **Bazel:** Defined in [`ModOptions.java:37-225`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/modcommand/ModOptions.java#L37-L225):
    - `--from=<modules>` (default `<root>`)
    - `--depth=<n>` (default `-1`)
    - `--cycles` (default `true`)
    - `--include_unused` (default `false`)
    - `--extension_info=<hidden|usages|repos|all>` (default `hidden`)
    - `--extension_filter=<extensions>`
    - `--base_module=<module>` (default `<root>`)
    - `--charset=<utf8|ascii>` (default `utf8`)
    - `--output=<text|json|graph|streamed_proto|streamed_jsonproto>` (default `text`)
    - `--verbose` (default `false`)
    - `--all_repos` (default `false`)
    - `--all_visible_repos` (default `false`)
  - **Zel:** Exactly 0 of these flags are supported or parsed in `cmd_mod`.

---

### `fetch` Command & Options

Bazel's `fetch` command prepares external repositories without executing build actions via [`FetchCommand.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/commands/FetchCommand.java) and [`FetchOptions.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/commands/FetchOptions.java).

* **[DIFFERENT] Command Execution Routing:**
  - **Bazel:** Runs in `ANALYZES` phase with `--nobuild` injected by `TargetFetcher.injectNoBuildOption`. It resolves target prerequisites and forces Skyframe evaluation of `RepositoryDirectoryValue` to fetch external repos without compiling targets.
  - **Zel:** In [`crates/zel-server/src/lib.rs:250-252`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L250-L252), `"fetch"` is grouped with `"build"` and `"test"` into `cmd_build_or_test()`. Zel actually proceeds to execute actions and spawn compiler processes as if it were a normal build! If target arguments are missing, it prints `INFO: Found 0 targets...` and exits 0. If a repo label like `@rules_rs` is passed, it fails with `ERROR: no such package '': BUILD file not found` (exit code 7).

* **[MISSING] `fetch` Options:**
  - **Bazel:** Defined in [`FetchOptions.java:27-76`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/commands/FetchOptions.java#L27-L76):
    - `--all`: fetches all external repositories required for building any target or repo.
    - `--configure`: fetches only repositories marked as `configure = True` (system configuration).
    - `--repo=<repo>`: fetches only the specified repository (`@apparent` or `@@canonical`). Can be specified multiple times.
    - `--force`: invalidates Skyframe cache to force re-fetching.
  - **Zel:** None of these options are parsed or handled.

---

### `vendor` Command & `--vendor_dir`

Bazel's `vendor` command creates a local directory of external dependencies for offline builds via [`VendorCommand.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/commands/VendorCommand.java), [`VendorOptions.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/commands/VendorOptions.java), and [`VendorManager.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/bazel/bzlmod/VendorManager.java).

* **[DIFFERENT] Command Execution Routing:**
  - **Bazel:** Evaluates repositories required by targets, copies external repos into `<vendor_dir>`, copies registry metadata into `<vendor_dir>/_registries`, and writes `vendor/vendor.bazel`. Builds can subsequently run completely offline using `--vendor_dir`.
  - **Zel:** In [`crates/zel-server/src/lib.rs:250-252`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L250-L252), `"vendor"` is grouped directly with `"build"` into `cmd_build_or_test()`. It never vendors anything, never creates `<vendor_dir>`, and simply runs regular build targets.

* **[MISSING] `vendor` Options:**
  - **Bazel:** Supports `--vendor_dir=<path>` (in `RepositoryOptions`), `--repo=<repo>` (in `VendorOptions`), and `--lockfile_deps` (vendors repos needed for lockfile update).
  - **Zel:** None of these flags are parsed or supported.

---

### `sync` Command

* **[DIFFERENT] `sync` Command Parity:**
  - **Bazel:** In modern Bazel (Bazel 9 / commit `8396531ec0`), `sync` has been completely deleted because legacy WORKSPACE has been removed. In Bazel 7/8, it synced WORKSPACE repositories.
  - **Zel:** Still lists `sync` in `zel help` (`zel-server/src/lib.rs:492: "  sync                Syncs all repositories specified in the workspace file."`), but in [`crates/zel-server/src/lib.rs:250-252`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L250-L252), it routes `sync` directly to `cmd_build_or_test()`, running build actions rather than syncing repositories.

---


## QUERY, CQUERY, AQUERY

### Parity Overview

**Overall Parity Rating: Minimal / Stub (~15% parity)**

Zel's implementation of the Bazel query subsystem spans a tiny standalone crate ([`crates/zel-query`](file:///Users/pledger/Development/zel/crates/zel-query)) and a unified handler `cmd_query` in [`crates/zel-server/src/lib.rs`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L738-L854). While Zel implements a rudimentary AST and basic evaluators for a handful of query functions (`deps`, `rdeps`, `kind`, `filter`, `attr`, `tests`, `siblings`, `somepath`, `allpaths`), its overall fidelity compared to Bazel's `lib/query2` subsystem is minimal:

1. **Severe Cross-Package Query Defect**: Zel's query execution engine extracts only a single target pattern hint from the query expression via `extract_inner_query_target` and only loads that single package into `QueryEnvironment`. All dependencies or targets located in any other package cannot be resolved; `direct_deps` drops external package targets, and `QueryEnvironment::evaluate` filters out all targets not present in the preloaded package map. As a result, cross-package queries (e.g. `deps(//foo)` where `//foo` depends on `//bar`) silently drop external dependencies.
2. **False Match on Empty Results (Critical Bug)**: If a query expression evaluates to an empty set (`targets.is_empty()`), `cmd_query` falls through to an unvendored fallback branch that unconditionally resolves and prints the inner target string (`extract_inner_query_target`), returning non-matching targets with exit code 0 instead of producing empty output.
3. **Cquery & Aquery are Stubs**: `cquery` does not query the configured target graph or evaluate Starlark transitions; it evaluates the unconfigured target query and hardcodes printing `<label> (<hash>) (<cpu>-<mode>)`, completely ignoring `--output` formatters (`label_kind`, `transitions`, `build`, `proto`, `textproto`, `jsonproto`, `files`, `starlark`). `aquery` synthesizes basic actions from `analyze_target` and always outputs text format, ignoring `--output` (`commands`, `summary`, `proto`, `textproto`, `jsonproto`). Neither `cquery` nor `aquery` supports their specific query functions (`config()`, `inputs()`, `outputs()`, `mnemonic()`).
4. **Missing Formatting & Flags**: Output formatters `location`, `minrank`, `maxrank`, `proto`, `streamed_proto`, `streamed_jsonproto`, `textproto`, `jsonproto` are completely absent in `zel-query`. Formatters `xml` and `build` are hardcoded stubs omitting rule attributes and file locations. Virtually all query flags (`--order_output`, `--universe_scope`, `--query_file`, `--keep_going`, `--noimplicit_deps`, `--nohost_deps`, `--line_terminator_null`, `--proto:*`, `--xml:*`, `--graph:*`, `--transitions`, `--starlark:*`, `--include_commandline`) are either unparsed or parsed and ignored.

---

### Query Language Syntax, Lexer & Parser Parity

* **[DIFFERENT] Command-Line Multi-Argument Joining**:
  * **Bazel**: [`QueryOptionHelper.java:48`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/runtime/commands/QueryOptionHelper.java#L48) joins all command-line residue tokens with spaces (`Joiner.on(' ').join(options.getResidue())`). This allows unquoted query expressions such as `bazel query //pkg:lib + //pkg:bin`.
  * **Zel**: [`crates/zel-server/src/lib.rs:752`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L752) only extracts `parsed.targets.first()`. Any subsequent tokens (e.g. `+` and `//pkg:bin`) are silently discarded, executing only the first token.

* **[DIFFERENT] Syntax Error Handling & Exit Codes**:
  * **Bazel**: A syntax error in a query expression throws [`QuerySyntaxException`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/QuerySyntaxException.java), handled in [`QueryCommand.java:87-101`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/runtime/commands/QueryCommand.java#L87-L101) by emitting an error message context and returning exit code 2 (`COMMAND_LINE_ERROR`, mapped via `FailureDetails.Query.Code.SYNTAX_ERROR`).
  * **Zel**: In [`crates/zel-server/src/lib.rs:765-802`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L765-L802), when `QueryExpr::parse` fails, the error is discarded without reporting. Execution falls through to the workspace target resolution fallback on `extract_inner_query_target(expr_str)`, typically failing with `ERROR: invalid target name '': empty target name` or invalid package syntax with exit code 7 (`ANALYSIS_FAILURE`).

* **[DIFFERENT] Subtraction / Except Operator Tokenization**:
  * **Bazel**: [`Lexer.java:299`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/Lexer.java#L299) parses `-` as `TokenKind.MINUS` when scanned outside words. In `scanWord`, hyphens are allowed in target words (e.g. `//pkg:target-name`).
  * **Zel**: [`tokenize_query` in crates/zel-query/src/lib.rs:86-90](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L86-L90) checks `matches!(c, '(' | ')' | ',' | '+' | '^' | '=')`, omitting `-`. As a result, `-` is only recognized as a minus/except operator if surrounded by whitespace; expressions like `deps(//a)-//b` fail to parse.

* **[MISSING] `set()` Function**:
  * **Bazel**: [`QueryParser.java:353-362`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/QueryParser.java#L353-L362) implements `set(word ...)` supporting whitespace-separated target patterns without commas (e.g. `set(//pkg:a //pkg:b)`).
  * **Zel**: Not supported in [`crates/zel-query/src/lib.rs`](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L197-L290). Passing `set(...)` returns `Unknown query function: 'set'`.

* **[PARTIAL] `let` Expression & Variable Binding**:
  * **Bazel**: [`QueryParser.java:332-346`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/QueryParser.java#L332-L346) enforces strict `LET WORD = expr IN expr` syntax. The variable name must be an unadorned identifier (without `$`), `=` and `in` are mandatory tokens, and variable references `$var` are forbidden in non-expression positions (e.g. as function word arguments).
  * **Zel**: [`parse_query_union` in crates/zel-query/src/lib.rs:117-138](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L117-L138) strips leading `$` on the variable name (`trim_start_matches('$')`), and treats both `=` and `in` as optional tokens. Missing `)` or missing delimiter tokens are silently ignored rather than raising syntax errors.

* **[DIFFERENT] String Quoting & Unclosed Quotations**:
  * **Bazel**: [`Lexer.java:148-163`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/Lexer.java#L148-L163) strictly checks for closing quotation marks; unclosed quotes throw `QuerySyntaxException("unclosed quotation")`.
  * **Zel**: [`crates/zel-query/src/lib.rs:91-102`](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L91-L102) consumes characters up to EOF if a closing quote is missing, silently returning an incomplete string token without raising an error.

* **[MISSING] Target Literal Restrictions**:
  * **Bazel**: [`QueryParser.java:371-377`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/QueryParser.java#L371-L377) prohibits unquoted target literals from beginning with `-` or `*` to avoid ambiguity with operators and wildcards.
  * **Zel**: Does not perform target literal validation.

---

### Query Functions Parity

* **[DIFFERENT] `somepath(from, to)` Semantics**:
  * **Bazel**: [`SomePathFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/SomePathFunction.java) computes and outputs a **single deterministic path** (ordered list of nodes) from `from` to `to` along dependency edges.
  * **Zel**: In [`crates/zel-query/src/lib.rs:458-464`](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L458-L464), `SomePath` executes the exact same logic as `AllPaths`, returning the entire intersection set (`forward.intersection(&backward)`) sorted alphabetically, rather than a single linear path.

* **[PARTIAL] `allpaths(from, to)`**:
  * **Bazel**: [`AllPathsFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/AllPathsFunction.java) builds the complete subgraph containing all nodes along all directed paths from `from` to `to`.
  * **Zel**: Computes intersection of forward BFS deps and backward BFS rdeps in [`crates/zel-query/src/lib.rs:458-464`](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L458-L464). However, because `direct_deps` only resolves targets within preloaded packages, paths that traverse intermediary packages are truncated or omitted entirely.

* **[MISSING] `allrdeps(universe, [depth])`**:
  * **Bazel**: [`AllRdepsFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/AllRdepsFunction.java) and [`SkyQueryEnvironment.java:1497`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/SkyQueryEnvironment.java#L1497) implement `allrdeps(u)` to find all reverse dependencies within the precomputed universe scope without requiring explicit target arguments.
  * **Zel**: `allrdeps` is completely missing from `crates/zel-query`.

* **[DIFFERENT] `attr(attr_name, pattern, input)`**:
  * **Bazel**: [`AttrFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/AttrFunction.java) treats `pattern` as a compiled Java regular expression (`Pattern.compile(pattern)`). It supports all attribute types (lists, string dicts, integers, booleans, labels, selects, default attributes, computed attributes, and rule tags).
  * **Zel**: [`crates/zel-query/src/lib.rs:386-403`](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L386-L403) only inspects explicit entries in `r.attrs.get(attr_name)`, uses simple string substring check (`val.to_string().contains(pattern)`) instead of regular expression matching, and cannot match default/unspecified attribute values, computed attributes, or tags.

* **[STUB] `buildfiles(input)` & `loadfiles(input)`**:
  * **Bazel**: [`BuildFilesFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/BuildFilesFunction.java) and [`LoadFilesFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/LoadFilesFunction.java). `buildfiles` returns the package's BUILD file (or `BUILD.bazel`) along with all transitive `.bzl` files loaded by that BUILD file. `loadfiles` returns the set of `.bzl` extension files loaded by the packages of the input targets.
  * **Zel**: [`crates/zel-query/src/lib.rs:282-288`](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L282-L288) aliases both `buildfiles` and `loadfiles` to the same AST node `QueryExpr::BuildFiles`. Evaluation in [`crates/zel-query/src/lib.rs:433-443`](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L433-L443) hardcodes generating a label `//pkg:BUILD` (always named `BUILD`, never `BUILD.bazel`) and returns zero loaded `.bzl` files.

* **[DIFFERENT] `deps(target, [depth])`**:
  * **Bazel**: [`DepsFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/DepsFunction.java) traverses all dependency edges of a target, including `deps`, `srcs`, `data`, plugins, toolchains, implicit rule dependencies, and nodep dependencies (filtered via `--noimplicit_deps`, `--nohost_deps`, `--nodep_deps`).
  * **Zel**: [`crates/zel-query/src/lib.rs:444-447, 530-542`](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L444-L447) implements BFS deps that only checks three hardcoded fields: `r.deps`, `r.srcs`, and `r.tools`. It ignores `data`, plugins, toolchains, and implicit dependencies. Crucially, when an edge crosses package boundaries, if the target package was not preloaded, `self.lookup_target(dep_lbl)` returns `None` and the traversal stops immediately, dropping the entire sub-tree. Additionally, depth parsing in `expr_as_usize` ([line 303](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L303)) silently falls back to unbounded depth on unparseable inputs instead of raising a syntax error.

* **[MISSING] `executables(input)`**:
  * **Bazel**: [`ExecutablesFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/ExecutablesFunction.java) filters for targets that produce executable artifacts or tests.
  * **Zel**: Completely missing.

* **[DIFFERENT] `filter(pattern, input)`**:
  * **Bazel**: [`FilterFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/FilterFunction.java) evaluates `pattern` as a regular expression against the canonical target label.
  * **Zel**: [`crates/zel-query/src/lib.rs:379-385`](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L379-L385) performs a literal substring search: `lbl.canonical_form().contains(pattern)`. Regex wildcards like `filter('.*_test', //...)` fail to match.

* **[DIFFERENT] `kind(pattern, input)`**:
  * **Bazel**: [`KindFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/KindFunction.java) tests `pattern` as a regular expression against target kind (e.g. `kind('cc_.* rule', //...)` or `kind('source file', //...)`).
  * **Zel**: [`crates/zel-query/src/lib.rs:367-377`](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L367-L377) strips trailing `" rule"` and does a plain substring match `t.target_kind().contains(pat_clean)`. Regular expressions fail.

* **[MISSING] `labels(attr_name, input)`**:
  * **Bazel**: [`LabelsFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/LabelsFunction.java) extracts the set of target labels appearing in a named attribute across input rules.
  * **Zel**: Completely missing.

* **[MISSING] `rbuildfiles(file, ...)`**:
  * **Bazel**: [`RBuildFilesFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/RBuildFilesFunction.java) uses Skyframe reverse graph tracking to find all packages whose BUILD files or extension loads depend on specified source or `.bzl` files.
  * **Zel**: Completely missing.

* **[DIFFERENT] `rdeps(universe, target, [depth])`**:
  * **Bazel**: [`RdepsFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/RdepsFunction.java) evaluates reverse dependencies restricted within the precomputed transitive closure of `universe`.
  * **Zel**: [`crates/zel-query/src/lib.rs:448-457, 491-528`](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L448-L457) builds a local parent graph by iterating only over targets within the loaded package set. Dependencies spanning outside the single hint package are excluded.

* **[MISSING] `same_pkg_direct_rdeps(input)`**:
  * **Bazel**: [`SamePkgDirectRdepsFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/SamePkgDirectRdepsFunction.java) finds direct reverse dependencies strictly within the same package.
  * **Zel**: Completely missing.

* **[PARTIAL] `siblings(input)`**:
  * **Bazel**: [`SiblingsFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/SiblingsFunction.java) outputs all targets residing in the packages of `input`.
  * **Zel**: [`crates/zel-query/src/lib.rs:417-432`](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L417-L432) checks `self.packages.get(&pkg_id)`. If the target's package was not in the initial snapshot, returns empty.

* **[MISSING] `some(input)`**:
  * **Bazel**: [`SomeFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/SomeFunction.java) returns an arbitrary single element from `input` (useful to inspect an exemplar target).
  * **Zel**: Completely missing.

* **[DIFFERENT] `tests(input)`**:
  * **Bazel**: [`TestsFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/TestsFunction.java) resolves test targets and expands `test_suite` rules recursively, honoring suite inclusion/exclusion tags, size filters, and the `--strict_test_suite` flag.
  * **Zel**: [`crates/zel-query/src/lib.rs:404-416`](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L404-L416) filters rules by `r.is_test_rule()` (`r.rule_class.ends_with("_test")`). It has zero support for `test_suite` rules; passing a `test_suite` target returns an empty set.

* **[MISSING] `visible(predicate, input)`**:
  * **Bazel**: [`VisibleFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/VisibleFunction.java) filters `input` targets to only those visible to `predicate`.
  * **Zel**: Completely missing.

* **[MISSING] Cquery Function `config(input, configuration)`**:
  * **Bazel**: [`ConfigFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/ConfigFunction.java) filters configured targets in `input` to a specific configuration (e.g. `'target'`, `'null'`, or configuration hex hash).
  * **Zel**: Completely missing.

* **[MISSING] Aquery Functions `inputs()`, `outputs()`, `mnemonic()`, `config()`**:
  * **Bazel**: [`InputsFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/InputsFunction.java), [`OutputsFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/OutputsFunction.java), and [`MnemonicFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/MnemonicFunction.java) filter the action graph by regex matching on action inputs, action outputs, and action mnemonics.
  * **Zel**: None of these functions exist. Invoking `aquery "mnemonic('FileWrite', //...)"` fails to parse, drops into package parsing fallback, and errors with exit code 7.

---

### Query Output Formatters Parity

Bazel's query formatter registry is defined in [`OutputFormatters.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/query/output/OutputFormatters.java#L30-L44). Zel's formatters are implemented in [`crates/zel-query/src/lib.rs:600-677`](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L600-L677).

* **`label`**:
  * **Bazel**: [`LabelOutputFormatter.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/query/output/LabelOutputFormatter.java). Outputs canonical or display-form labels one per line.
  * **Zel**: Supported. Formats canonical form via `t.label().canonical_form()`.

* **`label_kind`**:
  * **Bazel**: Outputs `<target_kind> <canonical_label>`, e.g. `cc_library rule //pkg:lib` or `source file //pkg:lib.cc`.
  * **Zel**: Supported in `zel query`. Note: in `zel cquery`, `--output=label_kind` is completely ignored (see Cquery section).

* **[STUB] `build`**:
  * **Bazel**: [`BuildOutputFormatter.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/query/output/BuildOutputFormatter.java). Reconstructs complete, valid Starlark BUILD rule syntax including all explicitly defined rule attributes, inputs, dependencies, visibility, tags, licenses, and comments.
  * **Zel**: [`crates/zel-query/src/lib.rs:622-632`](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L622-L632). Hardcodes printing only `r.rule_class` and `name`:
    ```
    cc_library(
      name = "lib",
    )
    ```
    All attributes (`srcs`, `deps`, `visibility`, `copts`, etc.) are omitted!

* **`package`**:
  * **Bazel**: [`PackageOutputFormatter.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/query/output/PackageOutputFormatter.java). Prints deduplicated package paths of matching targets.
  * **Zel**: Supported. Deduplicates package names into a `BTreeSet` and prints one per line.

* **[PARTIAL] `graph`**:
  * **Bazel**: [`GraphOutputFormatter.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/query/output/GraphOutputFormatter.java) & [`GraphOutputWriter.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/query/output/GraphOutputWriter.java). Produces Graphviz DOT graphs with node label truncation (`--graph:node_limit`), topological node factoring (`--graph:factored`), and conditional dependency edge annotations (`--graph:conditional_edges_limit`).
  * **Zel**: [`crates/zel-query/src/lib.rs:634-647`](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L634-L647). Emits a basic `digraph mygraph` with boxes. Does not factor nodes, does not truncate long labels, does not show conditional edges, and only traverses `direct_deps` within preloaded packages.

* **[STUB] `xml`**:
  * **Bazel**: [`XmlOutputFormatter.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/query/output/XmlOutputFormatter.java). Generates a rich XML document matching `src/main/java/com/google/devtools/build/lib/query2/query/output/xml.dtd`. Contains full typed attribute lists (`<string>`, `<list>`, `<label>`, `<int>`, `<boolean>`), rule inputs, rule outputs, source files, generated files, package groups, and precise source code line numbers (`--xml:line_numbers`).
  * **Zel**: [`crates/zel-query/src/lib.rs:649-667`](file:///Users/pledger/Development/zel/crates/zel-query/src/lib.rs#L649-L667). Produces a hollow XML shell containing only `<rule class="..." name="..."/>` and `<source-file name="..."/>`. No attributes, no inputs, no outputs, and no line numbers are emitted.

* **[MISSING] `location`**:
  * **Bazel**: [`LocationOutputFormatter.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/query/output/LocationOutputFormatter.java). Emits `file:line:column: target_kind label` for each target.
  * **Zel**: Not implemented. Passing `--output=location` silently falls back to standard `label` output.

* **[MISSING] `minrank` & `maxrank`**:
  * **Bazel**: [`MinrankOutputFormatter.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/query/output/MinrankOutputFormatter.java) and [`MaxrankOutputFormatter.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/query/output/MaxrankOutputFormatter.java). Print the minimum/maximum topological distance of each target from roots, prefixed as `<rank> <label>`.
  * **Zel**: Not implemented. Silently falls back to `label` output.

* **[MISSING] `proto`**:
  * **Bazel**: [`ProtoOutputFormatter.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/query/output/ProtoOutputFormatter.java). Serializes the complete query graph into binary `blaze_query.QueryResult` proto (`src/main/protobuf/build.proto`).
  * **Zel**: Not implemented. Silently falls back to `label` output.

* **[MISSING] `streamed_proto` & `streamed_jsonproto`**:
  * **Bazel**: [`StreamedProtoOutputFormatter.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/query/output/StreamedProtoOutputFormatter.java) and [`StreamedJSONProtoOutputFormatter.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/query/output/StreamedJSONProtoOutputFormatter.java). Stream length-delimited binary `Target` protos or NDJSON-formatted JSON objects for memory-efficient querying over huge monorepo graphs.
  * **Zel**: Not implemented. Silently falls back to `label` output.

* **[MISSING] `textproto` & `jsonproto`**:
  * **Bazel**: Supported in `cquery` and `aquery` to output text-format protobuf and JSON protobuf streams.
  * **Zel**: Not implemented. In `cquery` and `aquery`, passing `--output=jsonproto` or `--output=textproto` is completely ignored.

---

### Query Command-Line Flags Parity

Bazel's query options are defined in [`QueryOptions.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/query/output/QueryOptions.java) and [`CommonQueryOptions.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/common/CommonQueryOptions.java). Zel's option parsing is in [`crates/zel-options/src/lib.rs`](file:///Users/pledger/Development/zel/crates/zel-options/src/lib.rs#L483-L719).

* **[PARTIAL] `--output`**:
  * **Bazel**: Validates allowed formatters per command (`build, graph, streamed_jsonproto, label, label_kind, location, maxrank, minrank, package, proto, streamed_proto, xml`). Invalid formats raise option parsing errors.
  * **Zel**: Accepts any string into `opts.query_output_format`. In `query`, unsupported formats fall back to `label`. In `cquery` and `aquery`, `opts.query_output_format` is completely ignored on successful evaluations.

* **[MISSING] `--order_output` (`auto`, `no`, `deps`, `full`)**:
  * **Bazel**: [`QueryOptions.java:82-96`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/query/output/QueryOptions.java#L82-L96). Configures whether query outputs are un-ordered (`no`), dependency-ordered (`deps`), or lexicographically/topologically sorted (`full` / `auto`).
  * **Zel**: Flag does not exist. Results from `QueryEnvironment::evaluate` are always sorted by label string alphabetically because labels are collected in a `BTreeSet<Label>`.

* **[STUB] `--keep_going` / `-k`**:
  * **Bazel**: In [`QueryCommand.java:65`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/runtime/commands/QueryCommand.java#L65), `--keep_going` instructs Skyframe and package loaders to report errors but continue query evaluation on undamaged packages, exiting with exit code 3 (`PARTIAL_ANALYSIS_FAILURE`).
  * **Zel**: Parsed into `opts.keep_going` in `zel-options`, but [`crates/zel-server/src/lib.rs:738-854`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L738-L854) (`cmd_query`) **never reads or uses `parsed.keep_going` anywhere**. It is a dead field in query commands.

* **[MISSING] `--universe_scope`**:
  * **Bazel**: [`CommonQueryOptions.java:39-55`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/common/CommonQueryOptions.java#L39-L55). Specifies the target patterns defining the universe of targets over which queries (particularly `allrdeps` and `cquery`) operate.
  * **Zel**: Flag is unrecognized by `zel-options`.

* **[MISSING] `--query_file`**:
  * **Bazel**: [`CommonQueryOptions.java:447-454`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/common/CommonQueryOptions.java#L447-L454). Reads the query string from a specified file rather than command-line arguments (preventing command-line length limits).
  * **Zel**: Flag does not exist. Passing `--query_file=<file>` results in `ERROR: missing query expression for 'query'`, and `--query_file <file>` treats the flag as `--query_file=1` and errors on `<file>` as an invalid package path.

* **[MISSING] `--noimplicit_deps` / `--implicit_deps`**:
  * **Bazel**: [`CommonQueryOptions.java:122-133`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/common/CommonQueryOptions.java#L122-L133). Allows omitting implicit dependencies (compiler runtimes, default toolchains, base configurations) from the query graph.
  * **Zel**: Flag does not exist; Zel does not model implicit rule dependencies in query.

* **[MISSING] `--nohost_deps` / `--notool_deps` / `--tool_deps`**:
  * **Bazel**: [`CommonQueryOptions.java:100-119`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/common/CommonQueryOptions.java#L100-L119). Filters out dependencies that cross an execution transition (such as compiler binaries, codegen tools).
  * **Zel**: Flag does not exist.

* **[MISSING] `--strict_test_suite`**:
  * **Bazel**: [`QueryOptions.java:132-140`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/query/output/QueryOptions.java#L132-L140). Causes `tests()` to emit an error when encountering non-test targets inside a `test_suite`.
  * **Zel**: Flag does not exist.

* **[MISSING] `--line_terminator_null` / `--null`**:
  * **Bazel**: [`CommonQueryOptions.java:58-72`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/common/CommonQueryOptions.java#L58-L72). Terminates output items with `\0` instead of newline for safe pipelining with `xargs -0`.
  * **Zel**: Flag does not exist. Output is always `\n`-delimited.

* **[MISSING] `--relative_locations`**:
  * **Bazel**: [`CommonQueryOptions.java:246-258`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/common/CommonQueryOptions.java#L246-L258). Prints BUILD file locations in XML/proto relative to workspace root instead of absolute paths.
  * **Zel**: Flag does not exist.

* **[MISSING] `--proto:*` Family of Flags**:
  * **Bazel**:
    * `--proto:default_values`: include rule attributes with default values.
    * `--proto:flatten_selects`: flatten `select()` dictionaries into lists.
    * `--proto:output_rule_attrs`: comma-separated attribute filter list.
    * `--proto:rule_inputs_and_outputs`: populate inputs/outputs.
    * `--proto:locations`: include file locations.
    * `--proto:include_synthetic_attribute_hash`: generate internal hash.
    * `--proto:instantiation_stack` & `--proto:definition_stack`: include Starlark call stacks.
    * `--proto:include_starlark_rule_env`: Starlark environment hash.
    * `--proto:rule_classes`: emit Stardoc-compatible rule definitions.
  * **Zel**: None of these flags exist in Zel options or query evaluators.

* **[MISSING] `--xml:*` Family of Flags**:
  * **Bazel**: `--xml:line_numbers` (toggle XML line number attributes) and `--xml:default_values` (print unassigned default attributes).
  * **Zel**: Neither flag exists.

* **[MISSING] `--graph:*` Family of Flags**:
  * **Bazel**: `--graph:node_limit` (max node label length before truncation), `--graph:factored` (merge topologically equivalent nodes), and `--graph:conditional_edges_limit`.
  * **Zel**: None of these flags exist.

* **[MISSING] Additional Common Query Flags**:
  * Bazel supports `--output_file` (direct query output stream to file avoiding stdout gRPC overhead), `--infer_universe_scope`, `--consistent_labels`, and `--aspect_deps=conservative|precise|off`. None of these flags are recognized in Zel.

---

### Configured Target Query (`cquery`) Parity

Bazel's `cquery` implementation resides in [`CqueryCommand.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/runtime/commands/CqueryCommand.java) and [`ConfiguredTargetQueryEnvironment.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/cquery/ConfiguredTargetQueryEnvironment.java). Zel's handling of `cquery` is multiplexed in [`crates/zel-server/src/lib.rs:774-786`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L774-L786).

* **[STUB] Core `cquery` Command Evaluation**:
  * **Bazel**: Runs through the analysis phase up to the configured target graph. It evaluates configuration transitions (exec transitions, split transitions, rule transitions, Starlark transitions) and resolves attributes under concrete build configurations (`BuildConfigurationValue`).
  * **Zel**: `cquery` does not evaluate configurations or transitions during query traversal. It runs the standard unconfigured `QueryEnvironment::evaluate` against the loading-phase target graph, synthesizes a single host configuration, and attaches that static configuration hash to every returned target.

* **[DIFFERENT] Configuration Hash & Label Output Format**:
  * **Bazel**: In [`LabelAndConfigurationOutputFormatterCallback.java:64-67`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/cquery/LabelAndConfigurationOutputFormatterCallback.java#L64-L67), default `label` output prints:
    ```
    //pkg:lib (62bffc4)
    ```
    Source files and targets with null configurations print `(null)`.
  * **Zel**: In [`crates/zel-server/src/lib.rs:777-784`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L777-L784), Zel prints:
    ```
    //pkg:lib (62bffc4) (darwin_arm64-fastbuild)
    ```
    It appends `(<cpu>-<compilation_mode>)`, which breaks scripts and parsers expecting standard Bazel cquery label format.

* **[MISSING] Format Ignored in `cquery`**:
  * When `QueryEnvironment::evaluate` succeeds in `cmd_query`:
    ```rust
    } else if cmd_kind == "cquery" {
        let short_hash = config.short_hash();
        for t in &targets {
            writeln_bytes(
                stdout,
                &format!(
                    "{} ({short_hash}) ({cpu}-{})",
                    t.label().canonical_form(),
                    parsed.compilation_mode
                ),
            );
        }
        return ExitCode::SUCCESS;
    ```
    Zel **unconditionally prints the label line and returns `ExitCode::SUCCESS`**, completely ignoring `--output=label_kind`, `--output=build`, `--output=proto`, `--output=jsonproto`, `--output=textproto`, `--output=files`, `--output=transitions`, and `--output=starlark`.

* **[MISSING] `--transitions` (`lite`, `full`, `none`) & `TransitionsOutputFormatterCallback`**:
  * **Bazel**: [`TransitionsOutputFormatterCallback.java:86-140`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/cquery/TransitionsOutputFormatterCallback.java#L86-L140). Inspects transitions along dependency edges:
    ```
    //pkg:bin (hash)
      deps#//pkg:lib#<transition_name> -> <config_hash>
        --compilation_mode: fastbuild -> opt
    ```
  * **Zel**: Not supported. The flag `--transitions` is not parsed and `--output=transitions` is ignored.

* **[MISSING] `--output=starlark` with `--starlark:expr` & `--starlark:file`**:
  * **Bazel**: [`StarlarkOutputFormatterCallback.java:58-120`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/cquery/StarlarkOutputFormatterCallback.java#L58-L120). Executes arbitrary Starlark code over each configured target, providing access to `target`, `target.label`, `build_options(target)`, and `providers(target)`.
  * **Zel**: Completely missing. Neither `--output=starlark` nor the `--starlark:expr` / `--starlark:file` flags exist.

* **[MISSING] `--output=files`**:
  * **Bazel**: [`FilesOutputFormatterCallback.java:33-78`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/cquery/FilesOutputFormatterCallback.java#L33-L78). Outputs the exec paths of all output artifacts and source files produced or referenced by the matched targets.
  * **Zel**: Completely missing.

* **[MISSING] Cquery Protobuf Output (`proto`, `textproto`, `jsonproto`, `streamed_proto`)**:
  * **Bazel**: [`ProtoOutputFormatterCallback.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/cquery/ProtoOutputFormatterCallback.java). Produces `analysis_v2.proto` or `build.proto` configured target records including configuration metadata (`--proto:include_configurations`).
  * **Zel**: Completely missing.

* **[MISSING] Function `config()` in `cquery`**:
  * **Bazel**: [`ConfigFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/ConfigFunction.java). Filters targets by configuration hash, `'target'`, or `'null'`.
  * **Zel**: Completely missing.

---

### Action Graph Query (`aquery`) Parity

Bazel's `aquery` implementation resides in [`AqueryCommand.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/runtime/commands/AqueryCommand.java), [`ActionGraphQueryEnvironment.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/aquery/ActionGraphQueryEnvironment.java), and [`AqueryOptions.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/aquery/AqueryOptions.java). Zel's `aquery` handling is in [`crates/zel-server/src/lib.rs:787-796`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L787-L796) and action text formatting in [`crates/zel-analysis/src/lib.rs:294-321`](file:///Users/pledger/Development/zel/crates/zel-analysis/src/lib.rs#L294-L321).

* **[STUB] Core `aquery` Engine**:
  * **Bazel**: Interrogates Skyframe's live action graph (`ActionAnalysisMetadata`, `ActionLookupValue`, `RuleConfiguredTargetValue`), supporting aspect actions, pruned inputs, action key generation, platform execution information, parameter files, and synthetic file write actions.
  * **Zel**: `aquery` runs `QueryEnvironment::evaluate`, invokes `analyze_target(t, &config)` on each matched target, and loops through the resulting synthesized `ct.actions`. It does not construct or query an actual action graph.

* **[MISSING] Aquery Filter Functions**:
  * **`inputs(pattern, expr)`**: ([`InputsFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/InputsFunction.java)) Filters actions whose input artifact paths match the regular expression `pattern`. Missing in Zel.
  * **`outputs(pattern, expr)`**: ([`OutputsFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/OutputsFunction.java)) Filters actions whose output artifact paths match `pattern`. Missing in Zel.
  * **`mnemonic(pattern, expr)`**: ([`MnemonicFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/engine/MnemonicFunction.java)) Filters actions matching action mnemonic (e.g. `CppCompile`, `FileWrite`). Missing in Zel.
  * **`config(expr, config)`**: Missing in Zel.

* **[DIFFERENT] `text` Output Format**:
  * **Bazel**: [`ActionGraphTextOutputFormatterCallback.java:167-260`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/aquery/ActionGraphTextOutputFormatterCallback.java#L167-L260). Text output starts with `action.prettyPrint()`, followed by `  Mnemonic: ...`, `  Target: ...`, `  Configuration: <mnemonic>`, `  Execution platform: ...`, `  AspectDescriptors: ...`, `  ActionKey: ...`, `  Inputs: [...]`, and `  Outputs: [...]`.
  * **Zel**: [`crates/zel-analysis/src/lib.rs:294-321`](file:///Users/pledger/Development/zel/crates/zel-analysis/src/lib.rs#L294-L321). Text output prints `action '<pretty_print>'` (with extra surrounding literal quotes), `Configuration: <mnemonic> (<short_hash>)` (differing from Bazel's output format), and omits execution platform, aspect descriptors, and environment variables.

* **[MISSING] `--output=commands`**:
  * **Bazel**: Emits an executable shell command representation of the action with target and input/output metadata commented above it.
  * **Zel**: Completely missing.

* **[MISSING] `--output=summary`**:
  * **Bazel**: [`ActionGraphSummaryOutputFormatterCallback.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/aquery/ActionGraphSummaryOutputFormatterCallback.java). Prints an aggregated table of action counts grouped by mnemonic.
  * **Zel**: Completely missing.

* **[MISSING] Aquery Protobuf Formats (`proto`, `textproto`, `jsonproto`, `streamed_proto`)**:
  * **Bazel**: [`ActionGraphProtoOutputFormatterCallback.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/aquery/ActionGraphProtoOutputFormatterCallback.java). Serializes actions into `analysis_v2.proto` (`ActionGraphContainer`).
  * **Zel**: Completely missing. Passing `--output=jsonproto` to `zel aquery` outputs text format.

* **[MISSING] Aquery Specific Flags**:
  * Bazel's [`AqueryOptions.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/aquery/AqueryOptions.java) defines:
    * `--include_commandline` (toggle action argv in output)
    * `--include_artifacts` (toggle inputs/outputs in output)
    * `--include_pruned_inputs` (include inputs pruned during dynamic execution)
    * `--prune_unused_actions` (only print actions transitively needed by target outputs)
    * `--include_param_files` (dump parameter file contents)
    * `--include_file_write_contents` (dump generated file contents)
    * `--skyframe_state` (dump action graph directly from Skyframe cache)
  * None of these flags exist or are implemented in Zel.

---

### Query Engine & Execution Architecture Deficiencies

* **[DIFFERENT] Single-Package Loading & Broken Cross-Package Traversal**:
  * In Bazel, query evaluation is backed by [`SkyQueryEnvironment.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/SkyQueryEnvironment.java) (or `BlazeQueryEnvironment`). As the query evaluates functions like `deps()` or `allpaths()`, Skyframe dynamically discovers, parses, and evaluates new packages on demand across the entire workspace or external repositories.
  * In Zel ([`crates/zel-server/src/lib.rs:766-768, 1211-1238`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L766-L768)), the query environment is loaded via:
    ```rust
    let pattern_hint = extract_inner_query_target(expr_str);
    let qenv = self.load_query_environment_for_pattern(&pattern_hint);
    ```
    `extract_inner_query_target` strips a few hardcoded function prefixes (`deps(`, `rdeps(`, `somepath(`, `allpaths(`, `kind(`, `filter(`, `attr(`) and parses only that single target pattern. If `pattern_hint` is `//pkg1:target`, only `pkg1` is evaluated into `qenv.packages`.
    When `deps()` traverses to a target in `//pkg2`, `direct_deps` returns the label `//pkg2:dep`, but line 325 of `evaluate`:
    ```rust
    for lbl in labels {
        if let Some(t) = self.lookup_target(&lbl) {
            out.push(t);
        }
    }
    ```
    drops `//pkg2:dep` because `self.packages.get("pkg2")` is `None`! Furthermore, any transitive dependencies of `//pkg2:dep` are never explored. Cross-package queries are fundamentally broken in Zel.

* **[DIFFERENT] Fallback Triggered on Empty Query Results (False Output Bug)**:
  * In [`crates/zel-server/src/lib.rs:769-805`](file:///Users/pledger/Development/zel/crates/zel-server/src/lib.rs#L769-L805), `cmd_query` checks:
    ```rust
    if let Ok(targets) = qenv.evaluate(&query_expr) {
        if !targets.is_empty() {
            // output targets and return ExitCode::SUCCESS
        }
    }
    // Fallback for packages with unvendored external .bzl loads
    let pattern_str = extract_inner_query_target(expr_str);
    match self.resolve_targets_in_workspace(&pattern_str) {
        Ok(targets) => {
            // outputs targets from pattern_str!
        }
    }
    ```
    If a query expression legitimately matches 0 targets (such as `kind('cc_binary', //pkg:lib)` where `//pkg:lib` is a `cc_library`), `targets.is_empty()` is true. Zel falls through to the fallback branch, extracts the raw target string `//pkg:lib`, resolves it in workspace, and **prints `//pkg:lib` to stdout with exit code 0**, claiming a match when 0 targets matched!

* **[MISSING] Skyframe Incremental & Parallel Query (SkyQuery)**:
  * Bazel's query engine runs directly against Skyframe, evaluating graph nodes concurrently using worker thread pools ([`SkyQueryEnvironment.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/query2/SkyQueryEnvironment.java)) and reusing previously loaded packages from Skyframe cache. Zel reconstructs a static `BTreeMap<PackageIdentifier, Package>` on each command and evaluates BFS synchronously.

* **[MISSING] Cycle Detection and Reporting**:
  * Bazel detects dependency cycles in query targets and reports detailed cycle paths (e.g. `cycle in dependency graph: //a -> //b -> //a`). Zel's `bfs_deps` uses a `visited` set to avoid infinite loops, but completely suppresses cycle reporting.

---

### `zel-bazelisk` Divergences

Comparison between `crates/zel-query` and `zel-bazelisk/crates/zel-query`, as well as `cmd_query` in `crates/zel-server`, shows no divergence in query functionality. Divergences in `zel-bazelisk` are restricted to binary version reporting, wrapper detection, and PID/build label handling.

---

### Parity Deficiency Summary Table

| Category | Bazel Feature | Zel Status | Primary Difference / Impact |
| :--- | :--- | :--- | :--- |
| **Parser** | Multi-token residue query | **[DIFFERENT]** | Bazel joins all residue args with spaces; Zel drops all but the first token |
| **Parser** | Subtraction operator `-` | **[DIFFERENT]** | Bazel parses `-` as punctuation; Zel requires surrounding whitespace |
| **Parser** | Syntax error reporting | **[DIFFERENT]** | Bazel reports syntax error with exit code 2; Zel swallows error and falls through to target resolution with exit code 7 |
| **Parser** | `set()` function | **[MISSING]** | Space-separated set of target literals missing in Zel |
| **Parser** | `let var = expr in expr` | **[PARTIAL]** | Zel makes `=` and `in` optional; allows `let $x = ...` |
| **Functions** | `somepath(from, to)` | **[STUB]** | Bazel returns a single path; Zel executes `allpaths` (returns all nodes) |
| **Functions** | `allrdeps(universe, [depth])` | **[MISSING]** | Missing in Zel |
| **Functions** | `attr(name, pattern, expr)` | **[DIFFERENT]** | Bazel uses regex and handles all attr types/defaults; Zel uses substring match on explicit rule attrs |
| **Functions** | `buildfiles(expr)` | **[STUB]** | Bazel returns BUILD + all loaded `.bzl` files; Zel hardcodes `//pkg:BUILD` |
| **Functions** | `loadfiles(expr)` | **[STUB]** | Bazel returns loaded `.bzl` files; Zel aliases to `buildfiles`, returning `//pkg:BUILD` |
| **Functions** | `deps(expr, [depth])` | **[DIFFERENT]** | Bazel traverses all edges; Zel only inspects `deps`, `srcs`, `tools`, and cannot cross package boundaries |
| **Functions** | `filter(pat, expr)` / `kind(pat, expr)` | **[DIFFERENT]** | Bazel uses regex; Zel uses literal substring search |
| **Functions** | `tests(expr)` | **[DIFFERENT]** | Bazel expands `test_suite` rules recursively; Zel only checks rule class suffix, returning empty for suites |
| **Functions** | `executables`, `labels`, `rbuildfiles`, `same_pkg_direct_rdeps`, `some`, `visible` | **[MISSING]** | All completely absent in Zel |
| **Formatters** | `build` | **[STUB]** | Bazel reconstructs full rule with attributes; Zel only outputs rule class and `name` |
| **Formatters** | `xml` | **[STUB]** | Bazel outputs complete typed attribute XML; Zel prints empty tags without attributes or locations |
| **Formatters** | `graph` | **[PARTIAL]** | Bazel supports factoring, node limits, conditional edges; Zel outputs bare digraph of local direct deps |
| **Formatters** | `location`, `minrank`, `maxrank`, `proto`, `streamed_proto`, `streamed_jsonproto`, `textproto`, `jsonproto` | **[MISSING]** | Missing in Zel query; falls back to `label` |
| **Flags** | `--order_output` (`auto, no, deps, full`) | **[MISSING]** | Missing; Zel results always sorted alphabetically |
| **Flags** | `--keep_going` / `-k` | **[STUB]** | Parsed by options but completely ignored in `cmd_query` |
| **Flags** | `--universe_scope`, `--query_file`, `--noimplicit_deps`, `--nohost_deps`, `--strict_test_suite`, `--line_terminator_null`, `--proto:*`, `--xml:*`, `--graph:*` | **[MISSING]** | Completely unparsed / missing in Zel |
| **Cquery** | Configured Target Graph & Transitions | **[STUB]** | Zel evaluates unconfigured target query and attaches host configuration string |
| **Cquery** | Label output format | **[DIFFERENT]** | Bazel prints `<label> (<hash>)`; Zel appends `(<cpu>-<mode>)` |
| **Cquery** | Formats (`label_kind`, `transitions`, `build`, `proto`, `files`, `starlark`) | **[MISSING]** | Zel completely ignores `--output` in cquery |
| **Cquery** | Flags `--transitions`, `--starlark:expr`, `--starlark:file` | **[MISSING]** | Unparsed / ignored in Zel |
| **Cquery** | `config()` function | **[MISSING]** | Missing in Zel |
| **Aquery** | Action Graph Query Engine | **[STUB]** | Zel analyzes targets individually; does not query action graph |
| **Aquery** | Filters `inputs()`, `outputs()`, `mnemonic()`, `config()` | **[MISSING]** | All missing in Zel |
| **Aquery** | Formats `commands`, `summary`, `proto`, `textproto`, `jsonproto` | **[MISSING]** | Zel ignores `--output` and only produces text |
| **Aquery** | Flags `--include_commandline`, `--include_artifacts`, etc. | **[MISSING]** | Unparsed / missing in Zel |
| **Engine** | Cross-Package Query Traversal | **[DIFFERENT]** | Zel only preloads one package; drops all targets in external packages |
| **Engine** | Empty-result fallback | **[DIFFERENT]** | If 0 targets match, Zel falls through and prints non-matching targets |

---


# Parity Audit: Analysis Phase, Configuration, Rule APIs, and Built-in Rules

## Analysis Phase, Configuration, Rule APIs, and Built-in Rules

### Overview & Parity Rating: Minimal / Primitive Stub (~10-15% Parity)

Zel claims to be a 1:1 drop-in replacement for Bazel written in native Rust. However, inspection of `crates/zel-analysis`, `crates/zel-rules`, `crates/zel-packages`, and `crates/zel-starlark` reveals that **the entire Starlark analysis phase and user-defined rule evaluation engine do not exist in Zel**. 

In Bazel, `rule()`, `aspect()`, `provider()`, and the `ctx` rule context object drive an extensible evaluation graph where target rules invoke Starlark implementation callbacks, declare artifacts, register typed actions via `ctx.actions`, resolve toolchains and configurations, and exchange strongly-typed providers. In Zel:
1. **Rule implementation functions (`implementation = _my_impl`) are never invoked**. `rule()` is a dummy constructor that returns an unconfigured callable (`Value::RuleCallable`). When called in `BUILD` files, it merely records string attribute kwargs into an in-memory package map.
2. **The `ctx` rule context object does not exist anywhere in Zel**. None of its attributes, methods, or action factories (`ctx.actions.*`, `ctx.attr.*`, `ctx.file(s)`, `ctx.executable`, `ctx.toolchains`, etc.) are implemented.
3. **The `attr` module (`attr.string`, `attr.label_list`, etc.) is completely absent**. Unrecognized attribute definitions in `.bzl` files silently evaluate to `Value::None`.
4. **Target analysis is hardcoded in Rust** (`zel_rules::analyze_target` / `analyze_rule`) with a primitive string `match rule.rule_class.as_str()`. Unsupported or user-defined rules fall into a wildcard `_` branch that writes a 0-byte dummy file via `FileWriteAction`.
5. **Key built-in rules are severely degraded or fake**: `cc_library` does not compile C++ or collect headers; it writes a hardcoded 8-byte dummy archive `!<arch>\n`. `py_binary` and `sh_binary` simply execute `/bin/cp -f <first_src> <bin>`. `java_*`, `proto_library`, `platform`, `toolchain`, `constraint_*`, and `label_flag` produce 0-byte dummy files.
6. **Configuration, toolchain resolution, visibility enforcement, and `testonly` checks are entirely omitted or stubbed**. `select()` resolution is evaluated eagerly during package loading without active config conditions, falling back to an arbitrary branch when no match is found.

---

### Starlark Rule Definition API (`rule()`, `aspect()`, `provider()`, `attr.*`)

* **[MISSING] No Execution of Starlark Rule Implementation Callbacks**:
  * *Bazel*: Defined in [`StarlarkRuleClassFunctions.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/analysis/starlark/StarlarkRuleClassFunctions.java#L280-L450) and [`StarlarkRuleConfiguredTargetUtil.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/analysis/starlark/StarlarkRuleConfiguredTargetUtil.java#L60-L150). When a rule is configured, Bazel instantiates a `StarlarkRuleContext` (`ctx`), sets up threads and mutability boundaries, and invokes the Starlark function assigned to `implementation`. The returned sequence of providers (or `DefaultInfo`) is bound to the `ConfiguredTarget`.
  * *Zel*: In [`zel-starlark/src/lib.rs:2073-2084`](file:///Users/pledger/Development/zel/crates/zel-starlark/src/lib.rs#L2073-L2084), `rule(...)` simply extracts `executable` and `test` booleans from `kwargs` and returns `Value::RuleCallable { rule_class: "custom_rule".to_string(), executable, test }`. The `implementation` parameter is ignored. During target analysis in [`zel-rules/src/lib.rs:88-353`](file:///Users/pledger/Development/zel/crates/zel-rules/src/lib.rs#L88-L353), the Starlark implementation function is never called.

* **[MISSING] 20 of 22 `rule()` Parameters Ignored or Unsupported**:
  * *Bazel*: [`StarlarkRuleFunctionsApi.java:428-763`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/starlarkbuildapi/StarlarkRuleFunctionsApi.java#L428-L763) defines: `implementation`, `test`, `attrs`, `outputs`, `executable`, `output_to_genfiles`, `fragments`, `host_fragments`, `_skylark_testable`, `toolchains`, `doc`, `provides`, `dependency_resolution_rule`, `exec_compatible_with`, `analysis_test`, `build_setting`, `cfg`, `exec_groups`, `initializer`, `parent`, `extendable`, and `subrules`.
  * *Zel*: Only `executable` and `test` kwargs are read. The remaining 20 parameters are completely ignored:
    * `attrs`: Completely ignored. Rules cannot specify schemas, defaults, mandatory flags, or allowed types.
    * `toolchains`: Ignored. Rules cannot request required toolchains.
    * `provides`: Ignored. Rules cannot declare advertised providers.
    * `cfg`: Ignored. Starlark incoming transitions cannot be attached to rules.
    * `fragments` & `host_fragments`: Ignored. No configuration fragment requirements can be specified.
    * `exec_groups`: Ignored. Cannot define auxiliary execution groups.
    * `initializer`: Ignored. Macro-like pre-analysis initializers are not executed.
    * `parent` & `extendable`: Ignored. Rule inheritance is unsupported.
    * `subrules`: Ignored. Subrule composition is unsupported.
    * `build_setting`: Ignored. User-defined build settings cannot be declared.
    * `analysis_test`: Ignored. Analysis tests cannot be created.

* **[MISSING] `attr` Module Completely Missing**:
  * *Bazel*: [`StarlarkAttrModuleApi.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/starlarkbuildapi/StarlarkAttrModuleApi.java#L40-L380) exports `attr.string()`, `attr.label()`, `attr.label_list()`, `attr.int()`, `attr.int_list()`, `attr.bool()`, `attr.string_list()`, `attr.string_dict()`, `attr.string_list_dict()`, `attr.label_keyed_string_dict()`, `attr.string_keyed_label_dict()`, `attr.output()`, `attr.output_list()`, `attr.license()`, and attribute configuration properties (`default`, `doc`, `mandatory`, `allow_files`, `allow_single_file`, `providers`, `flags`, `cfg`, `aspects`).
  * *Zel*: There is no `attr` module in `zel-starlark`. When `.bzl` scripts reference `attr.label_list(...)`, `attr` resolves via [`zel-starlark/src/lib.rs:1718-1723`](file:///Users/pledger/Development/zel/crates/zel-starlark/src/lib.rs#L1718-L1723) to `Value::RuleCallable { rule_class: "attr", ... }`, and the subsequent method call on it hits [`zel-starlark/src/lib.rs:2265`](file:///Users/pledger/Development/zel/crates/zel-starlark/src/lib.rs#L2265) returning `Value::None`.

* **[STUB] `aspect()` Is a Dummy Alias for `rule()`**:
  * *Bazel*: Defined in [`StarlarkRuleFunctionsApi.java:792-985`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/starlarkbuildapi/StarlarkRuleFunctionsApi.java#L792-L985). Supports `implementation`, `attr_aspects`, `toolchains_aspects`, `attrs`, `required_providers`, `required_aspect_providers`, `required_aspect_hints_providers`, `provides`, `requires`, `propagation_predicate`, `fragments`, `host_fragments`, `toolchains`, `doc`, `apply_to_generating_rules`, `exec_compatible_with`, `exec_groups`, and `subrules`. Propagates along target dependencies and creates shadow configured targets.
  * *Zel*: In [`zel-starlark/src/lib.rs:2073`](file:///Users/pledger/Development/zel/crates/zel-starlark/src/lib.rs#L2073), `"aspect"` is grouped together with `"rule"` in the match statement and returns `Value::RuleCallable`. No aspect propagation, aspect context, or aspect actions exist in Zel.

* **[PARTIAL] `provider()` Constructor Functionality**:
  * *Bazel*: [`StarlarkRuleFunctionsApi.java:90-204`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/starlarkbuildapi/StarlarkRuleFunctionsApi.java#L90-L204). Supports `fields` schema validation (list or dict with docstrings), and `init` callback constructors returning `(P, raw_constructor)` tuples for custom validation.
  * *Zel*: In [`zel-starlark/src/lib.rs:2068-2072`](file:///Users/pledger/Development/zel/crates/zel-starlark/src/lib.rs#L2068-L2072), `provider(...)` ignores all arguments and returns `Value::Provider { name: "Provider".to_string() }`. Calling it creates a `Value::ProviderInstance { provider_name, fields: eval_kw }` ([`zel-starlark/src/lib.rs:2192-2195`](file:///Users/pledger/Development/zel/crates/zel-starlark/src/lib.rs#L2192-L2195)). There is no field validation, no `init` callback, no raw constructor tuple, and providers cannot be queried from configured targets during analysis.

---

### Starlark Rule Context (`ctx`) API & Execution Model

* **[MISSING] Starlark `ctx` Object Does Not Exist**:
  * *Bazel*: Defined in [`StarlarkRuleContextApi.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/starlarkbuildapi/StarlarkRuleContextApi.java) and implemented in [`StarlarkRuleContext.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/analysis/starlark/StarlarkRuleContext.java). Exposes 34 `@StarlarkMethod` members to rule implementations:
    1. `ctx.actions`: Action factory for registering build actions.
    2. `ctx.attr`: Struct of resolved attribute values and configured target prerequisites.
    3. `ctx.split_attr`: Struct of 1:2+ transition outputs per attribute.
    4. `ctx.executable`: Struct of executable prerequisite artifacts.
    5. `ctx.file`: Struct of single prerequisite files.
    6. `ctx.files`: Struct of prerequisite file lists.
    7. `ctx.outputs`: Struct of declared/predeclared outputs.
    8. `ctx.rule`: Target rule attributes (for aspects).
    9. `ctx.aspect_ids`: List of aspect IDs applied to target.
    10. `ctx.label`: `Label` of the target being analyzed.
    11. `ctx.workspace_name`: Root workspace string.
    12. `ctx.configuration`: Target `BuildConfigurationValue`.
    13. `ctx.fragments`: Active configuration fragments (cpp, java, platform, etc.).
    14. `ctx.toolchains`: Toolchains resolved for default exec group.
    15. `ctx.exec_groups`: Collection of auxiliary execution groups.
    16. `ctx.bin_dir`: `FileRoot` for the binary output directory.
    17. `ctx.genfiles_dir`: `FileRoot` for the genfiles output directory.
    18. `ctx.var`: Dict of Make variables.
    19. `ctx.features`: List of enabled features.
    20. `ctx.disabled_features`: List of disabled features.
    21. `ctx.target_platform_has_constraint(c)`: Checks platform constraints.
    22. `ctx.build_setting_value`: Value of current target build setting.
    23. `ctx.coverage_instrumented(target)`: Checks test coverage eligibility.
    24. `ctx.runfiles(...)`: Factory for `Runfiles` objects.
    25. `ctx.expand_location(template, targets)`: Expands `$(location)` strings.
    26. `ctx.expand_make_variables(attribute_name, expression, additional_substitutions)`: Expands Make variables.
    27. `ctx.tokenize(option_string)`: Splits shell command strings into tokens.
    28. `ctx.check_placeholders(template, allowed_placeholders)`: Validates template strings.
    29. `ctx.resolve_command(command, attribute, expand_locations, make_variables, ...)`: Resolves bash/cmd commands.
    30. `ctx.resolve_tools(tools)`: Resolves runfiles and inputs for tools.
    31. `ctx.package_relative_label(input)`: Resolves relative labels to package.
    32. `ctx.info_file`: Target stable workspace status file.
    33. `ctx.version_file`: Target volatile workspace status file.
    34. `ctx.super()`: Invokes extended parent rule implementation.
  * *Zel*: **None of these exist**. In Zel, `ctx` only exists as the name of a local Rust variable `let mut ctx = EvalContext::default()` inside [`zel-starlark`](file:///Users/pledger/Development/zel/crates/zel-starlark/src/lib.rs#L2296) and [`zel-packages`](file:///Users/pledger/Development/zel/crates/zel-packages/src/lib.rs#L107). No Starlark `ctx` struct is ever constructed or exposed to Starlark code.

---

### Starlark Action Factory API (`ctx.actions.*`) & Action Graph Representation

* **[MISSING] `ctx.actions` API Completely Absent**:
  * *Bazel*: Defined in [`StarlarkActionFactoryApi.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/starlarkbuildapi/StarlarkActionFactoryApi.java). Implements 15 methods:
    1. `ctx.actions.declare_file(filename, *, sibling = None)`
    2. `ctx.actions.declare_directory(filename, *, sibling = None)`
    3. `ctx.actions.declare_symlink(filename, *, sibling = None)`
    4. `ctx.actions.declare_shareable_artifact(path, ...)`
    5. `ctx.actions.run(outputs, inputs, executable, arguments, mnemonic, ...)`
    6. `ctx.actions.run_shell(outputs, inputs, tools, arguments, mnemonic, command, ...)`
    7. `ctx.actions.write(output, content, is_executable = False)`
    8. `ctx.actions.symlink(output, target_file, target_path, is_executable, ...)`
    9. `ctx.actions.expand_template(template, output, substitutions, is_executable, ...)`
    10. `ctx.actions.args()`: Returns `Args` command-line builder.
    11. `ctx.actions.template_dict()`: Returns memory-efficient substitution dictionary.
    12. `ctx.actions.do_nothing(mnemonic, inputs)`
    13. `ctx.actions.map_directory(output, ...)`
    14. `ctx.actions.transform_version_file(...)`
    15. `ctx.actions.transform_info_file(...)`
  * *Zel*: Not exposed to Starlark. Actions can only be generated via Rust code inside `zel-rules/src/lib.rs`.

* **[MISSING] `ctx.actions.args()` Command Line Builder**:
  * *Bazel*: [`CommandLineArgsApi.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/starlarkbuildapi/CommandLineArgsApi.java). Provides `add()`, `add_all()`, `add_joined()`, `use_param_file()`, `set_param_file_format()`, and `set_param_file_path()`, with lazy depset evaluation, directory expansion, custom `map_each` callbacks, format strings, and response-file spilling.
  * *Zel*: Missing entirely.

* **[PARTIAL] Rust Action Analysis Representation**:
  * *Bazel*: Rich polymorphic hierarchy implementing `ActionAnalysisMetadata` with dozens of action types (`SpawnAction`, `CppCompileAction`, `CppLinkAction`, `JavaCompileAction`, `SymlinkAction`, `FileWriteAction`, `TemplateExpansionAction`, `MiddlemanAction`, `StarlarkAction`, etc.).
  * *Zel*: In [`zel-analysis/src/lib.rs:140-170`](file:///Users/pledger/Development/zel/crates/zel-analysis/src/lib.rs#L140-L170), `ActionAnalysisMetadata` is an enum with only 4 variants:
    1. `SpawnAction`: Contains `owner`, `mnemonic`, `progress_message`, `argv`, `env`, `inputs`, `outputs`, and `execution_info`. Missing: `tools`, `resource_set`, `use_default_shell_env`, param-file handling, and `unused_inputs_list`.
    2. `FileWriteAction`: Contains `owner`, `output`, `content`, `is_executable`.
    3. `SymlinkAction`: Contains `owner`, `input_path`, `output`, `is_executable`.
    4. `TemplateExpansionAction`: Contains `owner`, `template`, `output`, `substitutions`, `is_executable`.

* **[MISSING] TreeArtifacts / Directory Artifacts**:
  * *Bazel*: Full support for `declare_directory`, tree artifact inputs/outputs, middleman pruning, and directory expansion in actions.
  * *Zel*: `Artifact` struct in [`zel-analysis/src/lib.rs:93-101`](file:///Users/pledger/Development/zel/crates/zel-analysis/src/lib.rs#L93-L101) has `pub is_tree_artifact: bool`, but it is always hardcoded to `false` in `Artifact::source()` and `Artifact::derived()`. There is no mechanism in Zel to declare or expand directory artifacts.


---

### Built-in Providers & Provider System (`DefaultInfo`, `OutputGroupInfo`, `CcInfo`, etc.)

* **[PARTIAL] `DefaultInfo` Provider**:
  * *Bazel*: Defined in [`DefaultInfo.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/analysis/DefaultInfo.java) and [`DefaultInfoApi.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/starlarkbuildapi/DefaultInfoApi.java). Contains `files` (depset of `Artifact`), `runfiles`, `data_runfiles`, `default_runfiles`, `files_to_run` (`FilesToRunProvider` with executable and runfiles support), and `executable` (`Artifact`). Starlark scripts construct and inspect `DefaultInfo(...)`.
  * *Zel*: Defined only as an internal Rust struct in [`zel-analysis/src/lib.rs:326-331`](file:///Users/pledger/Development/zel/crates/zel-analysis/src/lib.rs#L326-L331):
    ```rust
    pub struct DefaultInfo {
        pub files: Vec<Artifact>,
        pub data_runfiles: Vec<Artifact>,
        pub default_runfiles: Vec<Artifact>,
        pub executable: Option<Artifact>,
    }
    ```
    Differences:
    1. Not exposed to Starlark. `.bzl` scripts cannot instantiate or query `DefaultInfo`.
    2. Uses flat `Vec<Artifact>` rather than memory-efficient `Depset<Artifact>`.
    3. Missing `files_to_run` and `FilesToRunProvider`.

* **[PARTIAL] `OutputGroupInfo` Provider**:
  * *Bazel*: Defined in [`OutputGroupInfo.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/analysis/OutputGroupInfo.java). Maps output group names to `NestedSet<Artifact>`, supports special output groups (`_validation`, `compilation_outputs`, `hidden_top_level`, `temp_only`), and merges across transitive dependencies.
  * *Zel*: Internal Rust struct in [`zel-analysis/src/lib.rs:334-337`](file:///Users/pledger/Development/zel/crates/zel-analysis/src/lib.rs#L334-L337):
    ```rust
    pub struct OutputGroupInfo {
        pub groups: BTreeMap<String, Vec<Artifact>>,
    }
    ```
    In [`zel-rules/src/lib.rs:356`](file:///Users/pledger/Development/zel/crates/zel-rules/src/lib.rs#L356), it only ever inserts a single `"default"` group populated with `out_artifacts`. Output groups cannot be created or populated from Starlark, and special validation output groups are not evaluated during builds.

* **[MISSING] All Other Standard Bazel Providers**:
  * *Bazel*: Exposes a rich ecosystem of core providers:
    * `RunEnvironmentInfo`: Environment variables and inherited host env for tests and executables.
    * `InstrumentedFilesInfo`: Baseline and instrumented artifacts for test coverage.
    * `ToolchainInfo`: Carries resolved toolchain data.
    * `PlatformInfo`: Target and execution platform constraint settings.
    * `ConstraintSettingInfo` / `ConstraintValueInfo`: Constraint definitions.
    * `PackageSpecificationInfo`: Evaluates package group specifications.
    * `CcInfo` (`CcCompilationContext`, `CcLinkingContext`, `CcDebugInfoContext`): Core C++ provider.
    * `JavaInfo`: Jars, plugins, and compile/runtime dependencies.
    * `PyInfo`: Transitive Python sources and imports.
    * `ProtoInfo`: Direct and transitive proto files and descriptor sets.
    * `TemplateVariableInfo`: Custom Make variables.
    * `AnalysisTestResultInfo` / `AnalysisFailureInfo`: Testing providers.
    * `ExecutionInfo`: Action scheduling hints (no-remote, local, etc.).
  * *Zel*: **None of these providers exist** in `zel-analysis` or `zel-starlark`. `ConfiguredTarget` in [`zel-analysis/src/lib.rs:341-349`](file:///Users/pledger/Development/zel/crates/zel-analysis/src/lib.rs#L341-L349) has no provider map; it only holds `default_info` and `output_groups`. Targets cannot share or pass language-specific providers to their dependents.

---

### Runfiles Support & Manifest Generation

* **[MISSING] No `Runfiles` Object or Tree Construction**:
  * *Bazel*: Defined in [`Runfiles.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/analysis/Runfiles.java) and [`RunfilesSupport.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/analysis/RunfilesSupport.java). Supports:
    1. Nested sets of runfiles artifacts.
    2. Symlink mappings (`root_symlinks` and `symlinks`).
    3. Empty file markers (`empty_filenames`).
    4. Merging algorithms (`merge`, `merge_all`).
    5. Generation of `MANIFEST` files and runfiles symlink trees in `<target>.runfiles/`.
    6. Runfiles environment variables (`RUNFILES_DIR`, `RUNFILES_MANIFEST_FILE`, `RUNFILES_MANIFEST_ONLY`).
    7. Cross-repo workspace prefixes (`_main/`, `@repo//`).
  * *Zel*: No `Runfiles` class or Starlark type exists. In [`zel-analysis/src/lib.rs:328-329`](file:///Users/pledger/Development/zel/crates/zel-analysis/src/lib.rs#L328-L329), `DefaultInfo` merely has two raw `Vec<Artifact>` fields: `data_runfiles` and `default_runfiles`. In [`zel-rules/src/lib.rs:364-365`](file:///Users/pledger/Development/zel/crates/zel-rules/src/lib.rs#L364-L365), `data_runfiles` is populated with `src_artifacts` and `default_runfiles` is always empty. No manifest is generated, no symlink forest is created, and executables cannot locate runtime data files via runfiles libraries.

---

### Build Configurations, Keys, and Transitions

* **[PARTIAL] `BuildConfigurationKey` Representation**:
  * *Bazel*: Defined in [`BuildConfigurationKey.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/analysis/config/BuildConfigurationKey.java) and backed by [`BuildOptions.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/analysis/config/BuildOptions.java). Contains a complete map of configuration fragments (`PlatformOptions`, `CoreOptions`, `CppOptions`, `JavaOptions`, etc.), handles diff-based checksumming, options trimming (removing unneeded fragments per rule), and target platform configurations.
  * *Zel*: In [`zel-analysis/src/lib.rs:27-32`](file:///Users/pledger/Development/zel/crates/zel-analysis/src/lib.rs#L27-L32), `BuildConfigurationKey` has only 4 fields:
    ```rust
    pub struct BuildConfigurationKey {
        pub cpu: String,
        pub compilation_mode: String,
        pub is_exec: bool,
        pub user_defines: BTreeMap<String, String>,
    }
    ```
    Missing: `--platforms`, `--host_platform`, `--extra_toolchains`, options trimming, and all language configuration options.

* **[DIFFERENT] Output Directory Path Construction**:
  * *Bazel*: Outputs are placed into paths like `bazel-out/<platform_or_cpu>-<compilation_mode>[-<hash>]/bin` (or `k8-fastbuild`, `darwin_arm64-opt-exec-ST-hash`, etc.). It supports `bin/`, `genfiles/`, `testlogs/`, and includes configuration hashes to prevent collisions between different configuration transitions.
  * *Zel*: In [`zel-analysis/src/lib.rs:53-71`](file:///Users/pledger/Development/zel/crates/zel-analysis/src/lib.rs#L53-L71) and [`zel-analysis/src/lib.rs:119-122`](file:///Users/pledger/Development/zel/crates/zel-analysis/src/lib.rs#L119-L122):
    ```rust
    pub fn mnemonic(&self) -> String {
        if self.is_exec {
            format!("{}-opt-exec", self.cpu)
        } else {
            format!("{}-{}", self.cpu, self.compilation_mode)
        }
    }
    // exec_path:
    format!("bazel-out/{config_mnemonic}/bin/{root_relative_path}")
    ```
    Differences:
    1. Derived artifacts are **always** placed in `bin/`; `genfiles/` and `testlogs/` are never used.
    2. The configuration hash is **omitted** from output artifact paths. Although `BuildConfigurationKey::short_hash()` exists, it is only used for `aquery` formatting, not in the filesystem path. Targets analyzed under different user defines (`--define`) or transitions will overwrite each other's outputs in `bazel-out/<cpu>-<mode>/bin/`!

* **[MISSING] Starlark Transitions & Build Settings**:
  * *Bazel*: [`StarlarkTransition.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/analysis/starlark/StarlarkTransition.java). Supports:
    1. User-defined transitions via `transition(implementation, inputs, outputs)`.
    2. 1:1 transitions and 1:2+ split transitions.
    3. Incoming rule transitions (`rule(cfg = ...)`).
    4. Outgoing attribute transitions (`attr.label(cfg = ...)`).
    5. User-defined build settings (`config.string(flag = True)`, `config.bool()`, etc.).
  * *Zel*: [`zel-analysis/src/lib.rs:75-89`](file:///Users/pledger/Development/zel/crates/zel-analysis/src/lib.rs#L75-L89) only defines a fixed Rust enum:
    ```rust
    pub enum ConfigurationTransition {
        None,
        Exec,
        Target,
    }
    ```
    Where `Exec` simply hardcodes `compilation_mode = "opt"` and `is_exec = true`. There are no user-defined transitions, no split transitions, no attribute-level transition applications, and no build settings.

---

### Config Settings & `select()` Resolution

* **[DIFFERENT] Eager Resolution at Package Load Time Instead of Analysis Time**:
  * *Bazel*: In Bazel, `select({...})` produces a `BuildType.SelectorList` stored on the rule attribute during package loading. Resolution is deferred until the **analysis phase**, where each label key in the dictionary is resolved against the target's active `BuildConfigurationValue` and configured `config_setting` targets.
  * *Zel*: In [`zel-packages/src/lib.rs:162-167`](file:///Users/pledger/Development/zel/crates/zel-packages/src/lib.rs#L162-L167) and [`zel-starlark/src/lib.rs:322-325`](file:///Users/pledger/Development/zel/crates/zel-starlark/src/lib.rs#L322-L325), `select()` values are resolved **immediately during package loading**:
    ```rust
    // zel-starlark/src/lib.rs:322-325
    Self::SelectorList(sel) => sel
        .resolve(&[])
        .map(|v| v.to_string_list())
        .unwrap_or_default(),
    ```
    `SelectorList::resolve` is called with an empty slice `&[]` of active conditions. Consequently, rules cannot have dynamic attributes that adapt to `--compilation_mode`, `--cpu`, `--define`, or platform constraints.

* **[DIFFERENT] Arbitrary First-Branch Fallback on No Match**:
  * *Bazel*: If no dictionary key in a `select()` matches the configuration, and no `//conditions:default` is specified, Bazel fails analysis with an error:
    `ERROR: Configurable attribute '<attr>' doesn't match this configuration. Would a default condition help?`
    If multiple conditions match and neither specializes the other, Bazel fails with:
    `ERROR: Attribute '<attr>' is ambiguous in configuration of target '<label>': Both '<cond1>' and '<cond2>' match, but neither is more specialized.`
  * *Zel*: In [`zel-starlark/src/lib.rs:171-177`](file:///Users/pledger/Development/zel/crates/zel-starlark/src/lib.rs#L171-L177):
    ```rust
    if matched.is_none() {
        matched = branches.get("//conditions:default").cloned();
    }
    if matched.is_none() {
        matched = branches.values().next().cloned();
    }
    ```
    If no condition matches and no `//conditions:default` exists, Zel arbitrarily takes the **first key/value pair** from the dictionary! This produces silent build correctness bugs instead of failing cleanly. Furthermore, there is no ambiguity checking or specialization hierarchy.

* **[STUB] `config_setting` Does Not Match Configurations**:
  * *Bazel*: Defined in [`ConfigSetting.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/rules/config/ConfigSetting.java). Evaluates `values`, `define_values`, `constraint_values`, and `flag_values` against options, flags, and platforms to generate a `ConfigMatchingProvider`.
  * *Zel*: In [`zel-rules/src/lib.rs:124-128`](file:///Users/pledger/Development/zel/crates/zel-rules/src/lib.rs#L124-L128), `config_setting` is grouped with `filegroup`:
    ```rust
    "filegroup" | "alias" | "test_suite" | "config_setting" => {
        if out_artifacts.is_empty() {
            out_artifacts = src_artifacts.clone();
        }
    }
    ```
    It performs no inspection of build flags, `--define`, or constraints, and is never queried by `select()`.


---

### Toolchain Resolution & Platform / Constraint Architecture

* **[MISSING] Toolchain Resolution Algorithm**:
  * *Bazel*: Defined in [`ToolchainResolutionFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/skyframe/toolchains/ToolchainResolutionFunction.java) and [`SingleToolchainResolutionFunction.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/skyframe/toolchains/SingleToolchainResolutionFunction.java). For every rule requesting toolchains:
    1. Collects execution platform constraints (`exec_compatible_with`) and target platform constraints (`target_compatible_with`).
    2. Iterates over registered toolchains (from `WORKSPACE` / `MODULE.bazel` / command line `--extra_toolchains`).
    3. Finds candidate toolchains satisfying both target and execution platform constraints.
    4. Binds selected `ToolchainInfo` instances to `ctx.toolchains[toolchain_type]`.
  * *Zel*: **Completely absent**. In `zel-starlark/src/lib.rs:2100`, `register_toolchains` and `register_execution_platforms` are discarded as no-ops (`return Ok(Value::None)`). In `zel-rules/src/lib.rs`, `toolchain` and `toolchain_type` are not recognized and fall into the wildcard `_` branch which writes 0 bytes. Target analysis performs zero toolchain resolution.

* **[MISSING] Platform & Constraint Rules (`platform`, `constraint_setting`, `constraint_value`)**:
  * *Bazel*: Defined in [`PlatformRules.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/rules/platform/PlatformRules.java). `constraint_setting` defines an orthogonal constraint dimension (e.g. `@platforms//os:os`); `constraint_value` specifies points along that axis (e.g. `@platforms//os:linux`); `platform` aggregates constraint values and flags (`PlatformInfo`).
  * *Zel*: None of these rules are handled in `zel-rules/src/lib.rs`. They hit the wildcard rule branch and produce empty dummy files.

---

### Visibility Enforcement & Target Access Controls

* **[MISSING] Complete Omission of Visibility Checking**:
  * *Bazel*: Defined in [`VisibilityProvider.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/analysis/VisibilityProvider.java) and checked in `RuleContext.checkVisibility()`. Validates every dependency edge against `visibility` declarations (`//visibility:public`, `//visibility:private`, subpackage specifications `//pkg:__pkg__`, `//pkg:__subpackages__`, and `package_group` rules). If a target depends on a private target outside its allowed packages, analysis immediately halts with a visibility violation error.
  * *Zel*: Although `RuleInstance` has `pub visibility: Vec<String>` in [`zel-packages/src/lib.rs:35`](file:///Users/pledger/Development/zel/crates/zel-packages/src/lib.rs#L35), **it is never checked anywhere in the codebase**. There is no visibility verification in `zel-packages`, `zel-rules`, `zel-analysis`, or `zel-server`. Any target can depend on private targets in any package or external repository without error.

* **[MISSING] `package(default_visibility = ...)` Not Extracted**:
  * *Bazel*: Package-level default visibility is inherited by all rules in the package that do not declare an explicit `visibility` attribute.
  * *Zel*: In [`zel-packages/src/lib.rs:231`](file:///Users/pledger/Development/zel/crates/zel-packages/src/lib.rs#L231), `Package.default_visibility` is hardcoded to `Vec::new()`. In [`zel-starlark/src/lib.rs:2100`](file:///Users/pledger/Development/zel/crates/zel-starlark/src/lib.rs#L2100), `package(...)` calls in `BUILD` files return `Value::None` and are discarded without extracting `default_visibility`.

* **[MISSING] `testonly` Enforcement**:
  * *Bazel*: Rules marked `testonly = True` may only be depended on by other `testonly = True` rules or test targets. If a production binary or library depends on a `testonly` target, Bazel rejects the build during analysis:
    `ERROR: non-test target '<target>' depends on testonly target '<dep>' and doesn't have testonly attribute set`.
  * *Zel*: In [`zel-packages/src/lib.rs:38`](file:///Users/pledger/Development/zel/crates/zel-packages/src/lib.rs#L38), `testonly` is stored on `RuleInstance`, but it is never checked against dependent targets during analysis.

---

### Make Variables & Location / Path Expansions

* **[PARTIAL] `genrule` Command Expansion**:
  * *Bazel*: Implemented in [`LocationTemplateContext.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/analysis/LocationTemplateContext.java) and [`MakeVariableSupplier.java`](file:///Users/pledger/Development/bazel/src/main/java/com/google/devtools/build/lib/analysis/MakeVariableSupplier.java).
    1. Supports `$(location <label>)`, `$(locations <label>)`, `$(execpath <label>)`, `$(execpaths <label>)`, `$(rootpath <label>)`, `$(rootpaths <label>)`, `$(rlocationpath <label>)`, `$(rlocationpaths <label>)`.
    2. Resolves labels against all target dependencies, tools, and output files across any package.
    3. Supports built-in Make variables: `$(BINDIR)`, `$(GENDIR)`, `$(TARGET_CPU)`, `$(COMPILATION_MODE)`, `$(RULEDIR)`, `$(@D)`, `$(JAVA)`, etc.
    4. Supports user `--define KEY=VAL` Make variables `$(KEY)`.
  * *Zel*: Defined in [`zel-rules/src/lib.rs:374-420`](file:///Users/pledger/Development/zel/crates/zel-rules/src/lib.rs#L374-L420) in `expand_genrule_cmd`:
    ```rust
    let mut cmd = raw_cmd
        .replace("$(SRCS)", &joined_srcs)
        .replace("$<", srcs_str.first().map(|s| s.as_str()).unwrap_or(""))
        .replace("$(OUTS)", &joined_outs)
        .replace("$@", outs_str.first().map(|s| s.as_str()).unwrap_or(""));
    ```
    Differences:
    1. **Missing Variables**: Does not support `$(rootpath)`, `$(rootpaths)`, `$(rlocationpath)`, `$(rlocationpaths)`, `$(BINDIR)`, `$(GENDIR)`, `$(RULEDIR)`, `$(@D)`, or any predefined Bazel Make variables.
    2. **Missing `--define`**: Does not expand user-defined variables from `--define`.
    3. **Primitive Label Resolution**: In lines 399-412, `$(location ...)` only searches `srcs` and `outs` of the current rule. If a label refers to a tool (from `tools = [...]`) or a dependency from another package, it fails to find the artifact and naively constructs `pkg_frag.get_relative_str(clean)`, producing invalid paths for derived tool binaries.
    4. **Plural Forms Not Handled Correctly**: `$(locations ...)` and `$(execpaths ...)` do not expand to multiple space-separated paths; they perform the identical single-artifact replacement as `$(location ...)`.

---

### Built-in Rules Deep Dive: Attribute Sets & Actions Parity

The table and detailed enumeration below compare the attribute sets and generated actions for every built-in rule in Bazel against Zel's implementation in `zel-rules/src/lib.rs`.

| Rule Class | Bazel Implementation | Zel Implementation | Parity Status | Key Differences in Zel |
| :--- | :--- | :--- | :--- | :--- |
| `genrule` | `BazelGenRuleRule.java` / `GenRule.java` | `zel_rules::analyze_rule` (`"genrule"`) | **[PARTIAL]** | Hardcodes `/bin/bash`; ignores `tools`, `executable`, `output_to_bindir`, `message`, `stamp`, `cmd_bash`/`cmd_bat`. Incomplete `$(location)` expansion. |
| `filegroup` | `FilegroupRule.java` / `Filegroup.java` | `zel_rules::analyze_rule` (`"filegroup"`) | **[PARTIAL]** | Ignores `output_group`, `data`. Copies `srcs` to outputs. No runfiles merging. |
| `alias` | `AliasRule.java` / `AliasConfiguredTarget.java` | `zel_rules::analyze_rule` (`"alias"`) | **[DIFFERENT]** | Does not forward `actual` target's providers, runfiles, or actions. Evaluates to empty outputs or dummy copy of `srcs`. |
| `config_setting` | `ConfigSettingRule.java` / `ConfigSetting.java` | `zel_rules::analyze_rule` (`"config_setting"`) | **[STUB]** | No action. Ignores `values`, `define_values`, `constraint_values`, `flag_values`. Never produces `ConfigMatchingProvider`. |
| `test_suite` | `TestSuiteRule.java` / `TestSuite.java` | `zel_rules::analyze_rule` (`"test_suite"`) | **[STUB]** | Treated as `filegroup`. Ignores `tests`, `tags` inclusion/exclusion filters. Does not expand or configure child tests. |
| `sh_binary` / `sh_test` | `BazelShRuleClasses.java` | `zel_rules::analyze_rule` (`"sh_*"` / `"py_*"`) | **[DIFFERENT]** | Copies first src via `/bin/cp -f` or writes `exit 0`. Ignores `deps`, `data`, `args`, `env`. No runfiles tree created. |
| `sh_library` | `BazelShRuleClasses.java` | Wildcard `_` branch | **[STUB]** | Writes 0-byte dummy file. Ignores `srcs`, `deps`, `data`. |
| `cc_binary` / `cc_test` | `CcBinary.java` / `CcCommon.java` | `zel_rules::analyze_rule` (`"cc_binary"`) | **[DIFFERENT]** | Single spawn action `cc [-O2\|-g] <srcs> -o <bin>`. No separate compilation, no header scanning, no `CcInfo` propagation, no include paths, ignores all `deps` and `copts`. |
| `cc_library` | `CcLibrary.java` / `CcCommon.java` | `zel_rules::analyze_rule` (`"cc_library"`) | **[STUB]** | **Fake archive write**: Writes hardcoded `!<arch>\n` (8 bytes) via `FileWriteAction`. Does not invoke compiler or `ar`. Ignores `srcs`, `hdrs`, `deps`. |
| `cc_import` / `cc_shared_library` | `CcImport.java` / `CcSharedLibrary.java` | Wildcard `_` branch | **[STUB]** | Writes 0-byte dummy file. |
| `cc_toolchain` / `cc_toolchain_suite` | `CcToolchainRule.java` | Wildcard `_` branch | **[STUB]** | Writes 0-byte dummy file. Toolchain configuration is ignored. |
| `rust_binary` / `rust_test` | `rules_rust` Starlark rules | `zel_rules::analyze_rule` (`"rust_*"`) | **[PARTIAL]** | Invokes `rustc` directly in a single action. Hardcodes `--edition=2021`. Manually tracks `--extern` for direct deps only; no transitive rlib linking, no cargo features, no proc-macros. |
| `rust_library` / `rust_shared_library`| `rules_rust` Starlark rules | `zel_rules::analyze_rule` (`"rust_*"`) | **[PARTIAL]** | Compiles to `.rlib` or `.dylib`. Single-crate compile; no proc-macro support. |
| `java_binary` / `java_test` | `JavaBinary.java` | Wildcard `_` branch | **[STUB]** | Writes 0-byte dummy file. Does not invoke `javac`, create deploy jar, or create wrapper scripts. |
| `java_library` / `java_import` | `JavaLibrary.java` / `JavaImport.java` | Wildcard `_` branch | **[STUB]** | Writes 0-byte dummy file. No compilation, header jar (`ijar`), or `JavaInfo` propagation. |
| `java_toolchain` | `JavaToolchainRule.java` | Wildcard `_` branch | **[STUB]** | Writes 0-byte dummy file. |
| `py_binary` / `py_test` | `PyBinary.java` / `rules_python` | `zel_rules::analyze_rule` (`"py_*"` branch) | **[DIFFERENT]** | Handled identically to `sh_binary` (`/bin/cp -f <first_src> <bin>`). No python interpreter invocation, no launcher script, no runfiles, no `PYTHONPATH` setup. |
| `py_library` / `py_runtime` | `PyLibrary.java` / `rules_python` | Wildcard `_` branch | **[STUB]** | Writes 0-byte dummy file. |
| `proto_library` / `proto_lang_toolchain` | `ProtoLibrary.java` / `BazelProtoCommon.java` | Wildcard `_` branch | **[STUB]** | Writes 0-byte dummy file. No `protoc` invocation, descriptor set generation, or `ProtoInfo` propagation. |
| `platform` | `PlatformRule.java` | Wildcard `_` branch | **[STUB]** | Writes 0-byte dummy file. No `PlatformInfo` generated. |
| `toolchain` / `toolchain_type` | `ToolchainRule.java` / `ToolchainType.java` | Wildcard `_` branch | **[STUB]** | Writes 0-byte dummy file. Not registered in toolchain catalog. |
| `constraint_setting` / `constraint_value`| `ConstraintSettingRule.java` | Wildcard `_` branch | **[STUB]** | Writes 0-byte dummy file. |
| `label_flag` / `label_setting` | `LabelBuildSettings.java` | Wildcard `_` branch | **[STUB]** | Writes 0-byte dummy file. Cannot be overridden on command line. |
| `config_feature_flag` | `ConfigFeatureFlagRule.java` | Wildcard `_` branch | **[STUB]** | Writes 0-byte dummy file. |
| `copy_file` | Skylib macro (`bazel-skylib`) | `zel_rules::analyze_rule` (`"copy_file"`) | **[PARTIAL]** | Native rule in Zel (not native in Bazel). Runs `/bin/cp -f src dst`. Ignores `is_executable`, `allow_symlink`. |

---

### Detailed Rule Comparison: Attributes & Actions

#### 1. `genrule`
* **Bazel**:
  * Attributes: `srcs` (label list), `outs` (output list, mandatory), `cmd` (string), `cmd_bash` (string), `cmd_bat` (string), `cmd_ps` (string), `tools` (label list, exec transitioned), `executable` (bool), `output_to_bindir` (bool), `message` (string), `heuristic_label_expansion` (bool), `stamp` (int), `local` (bool).
  * Actions: Creates a `SpawnAction` executing a bash command or Windows cmd script with complete environment isolation, tool inputs with exec transition, stamping support, and full Make variable substitution.
* **Zel** ([`zel-rules/src/lib.rs:129-150`](file:///Users/pledger/Development/zel/crates/zel-rules/src/lib.rs#L129-L150)):
  * Attributes: Only reads `srcs`, `outs`, and `cmd`.
  * Actions: Creates a single `SpawnAction` running `["/bin/bash", "-c", expanded_cmd]`.
  * Differences:
    * Hardcodes `/bin/bash` without fallback for Windows or non-standard bash locations.
    * `tools` is parsed in `zel-packages` but ignored in `zel-rules`; tools are not added as action inputs and not transitioned to exec configuration.
    * Progress message is hardcoded to `"Executing genrule <label>"`, ignoring `message`.
    * Ignores `executable`, `output_to_bindir`, `stamp`, `cmd_bash`, `cmd_bat`.

#### 2. `cc_library`
* **Bazel**:
  * Attributes: `srcs`, `hdrs`, `deps`, `copts`, `linkopts`, `includes`, `defines`, `local_defines`, `textual_hdrs`, `strip_include_prefix`, `include_prefix`, `alwayslink`, `linkstatic`, `linkstamp`.
  * Actions: Generates compile actions (`CppCompileAction`) for each source file to `.o` and `.pic.o`, runs header scanning and module mapping, creates static archive (`.a`) via `ar`/`llvm-ar`, optionally generates dynamic library (`.so`/`.dylib`), and exports `CcInfo` with header search paths and linker inputs.
* **Zel** ([`zel-rules/src/lib.rs:236-246`](file:///Users/pledger/Development/zel/crates/zel-rules/src/lib.rs#L236-L246)):
  * Attributes: None checked.
  * Actions: Registers a single `FileWriteAction` writing hardcoded ASCII `!<arch>\n` (the 8-byte magic header of an empty ar archive)!
  * Differences: **Completely fake**. No compiler or archiver is invoked. Sources and headers are never compiled. No `CcInfo` is produced. Downstream targets linking against this `cc_library` receive an empty dummy file.

#### 3. `cc_binary` and `cc_test`
* **Bazel**:
  * Attributes: `srcs`, `deps`, `copts`, `linkopts`, `includes`, `defines`, `linkshared`, `linkstatic`, `malloc`, `stamp`, `additional_linker_inputs`, `features`.
  * Actions: Compiles individual translation units to object files, gathers transitive `CcInfo` linker inputs from `deps`, runs the linker with configured flags, dynamic library rpaths, and standard libraries to produce the executable.
* **Zel** ([`zel-rules/src/lib.rs:208-235`](file:///Users/pledger/Development/zel/crates/zel-rules/src/lib.rs#L208-L235)):
  * Attributes: Only reads `srcs`.
  * Actions: Runs a single spawn command: `cc [-O2|-g] <srcs> -o <bin>`.
  * Differences:
    * Compiles and links everything in a single compiler invocation.
    * Completely ignores `deps`: transitive libraries (including `cc_library` deps) are not passed to the linker!
    * Ignores `copts`, `linkopts`, `includes`, `defines`, `linkshared`, `linkstatic`.
    * Uses host compiler `cc` directly instead of resolved C++ toolchain.

#### 4. `sh_binary`, `sh_test`, `py_binary`, `py_test`
* **Bazel**:
  * `sh_*`: Validates entry point script, prepares runfiles tree, creates launcher script with environment variables (`RUNFILES_DIR`, etc.).
  * `py_*`: Analyzes Python version (`PY3`), parses imports, collects transitive Python sources into `PyInfo`, creates executable Python bootstrap wrapper or zipapp, and configures python runtime toolchain.
* **Zel** ([`zel-rules/src/lib.rs:178-207`](file:///Users/pledger/Development/zel/crates/zel-rules/src/lib.rs#L178-L207)):
  * Attributes: Only reads `srcs`.
  * Actions: If `srcs` has at least one item, creates a `SpawnAction` running `/bin/cp -f <first_src> <bin_art>`. If `srcs` is empty, creates a `FileWriteAction` writing `#!/bin/sh\nexit 0\n`.
  * Differences:
    * `py_binary` and `py_test` are treated identically to shell scripts; no Python runtime or interpreter is invoked, and `main`, `imports`, and `python_version` are ignored.
    * No runfiles directory or manifest is created.
    * All `deps`, `data`, `args`, and `env` attributes are completely ignored.

#### 5. `alias`
* **Bazel**:
  * Attributes: `actual` (mandatory label).
  * Configured Target: Creates an `AliasConfiguredTarget` which transparently acts as an alias to the `actual` target, forwarding all providers, default info, output groups, and runfiles.
* **Zel** ([`zel-rules/src/lib.rs:124-128`](file:///Users/pledger/Development/zel/crates/zel-rules/src/lib.rs#L124-L128)):
  * In `zel-packages/src/lib.rs:163`, `actual` is extracted into `RuleInstance.deps`.
  * In `zel-rules/src/lib.rs:124-128`, `alias` is matched alongside `filegroup` and merely sets `out_artifacts = src_artifacts`. Since `alias` rules have no `srcs`, `out_artifacts` is empty.
  * Does not forward providers, output groups, or actions from `actual`.

#### 6. `test_suite`
* **Bazel**:
  * Attributes: `tests` (label list), `tags` (string list).
  * Expansion: Recursively expands other `test_suite` targets, filters test targets by positive and negative tags (`-manual`, `small`, etc.), and dynamically populates the test suite with resolved tests.
* **Zel** ([`zel-rules/src/lib.rs:124-128`](file:///Users/pledger/Development/zel/crates/zel-rules/src/lib.rs#L124-L128)):
  * Treated as `filegroup`. Does not resolve `tests`, does not filter by tags, and does not configure child test targets. Running `zel test //:my_test_suite` fails to execute the underlying test suite targets.

#### 7. Wildcard Fallback for All Other Rules (`java_*`, `proto_library`, etc.)
* **Zel** ([`zel-rules/src/lib.rs:332-353`](file:///Users/pledger/Development/zel/crates/zel-rules/src/lib.rs#L332-L353)):
  ```rust
  _ => {
      if out_artifacts.is_empty() {
          let default_out = pkg_frag.get_relative_str(rule.label.name());
          out_artifacts.push(Artifact::derived(
              &cfg_mnemonic,
              default_out,
              rule.label.clone(),
          ));
      }
      for out_art in &out_artifacts {
          actions.push(ActionAnalysisMetadata::FileWriteAction {
              owner: rule.label.clone(),
              output: out_art.clone(),
              content: Vec::new(),
              is_executable: rule.is_binary_rule(),
          });
      }
      if rule.is_binary_rule() {
          executable = out_artifacts.first().cloned();
      }
  }
  ```
  Every rule not explicitly matched in the `match` statement—including all `java_*` rules, `proto_library`, `platform`, `toolchain`, `constraint_*`, `label_flag`, and **any custom user-defined Starlark rule**—simply generates a 0-byte dummy file via `FileWriteAction`.

---


### Wave 2 status (analysis agent, supersedes the stub description above where they conflict)

The Starlark analysis engine exists and is wired into `build`/`test`/`run`/`aquery`/`cquery`
(`crates/zel-server/src/analysis_phase.rs`, `zel_rules::TargetResolver`). Done: dependency-aware analysis with
Bazel-format analysis errors (incl. Starlark tracebacks, `--keep_going`), option→configuration population
(`--cpu/-c/--define/--platforms/--host_platform/--extra_toolchains/--extra_execution_platforms/--features/--//pkg:flag=v`),
Bzlmod `register_toolchains`/`register_execution_platforms` + canonical `@platforms` name, real `ST-<hash>` output
directory names (exec config `...-opt-exec-ST-d57f47055a04`), aspects (`aspect()`, `attr_aspects`, `required_providers`,
`requires`, `--aspects`, `--aspects_parameters`, `ctx.rule`), split transitions + `ctx.split_attr`,
`analysis_test_transition` / `analysis_test=True`, exec groups with toolchains, `config.exec(exec_group=)`, `subrule`,
`attr.int_list`/`attr.string_keyed_label_dict`, late-bound defaults, `ctx.fragments`, `RunEnvironmentInfo`,
`InstrumentedFilesInfo`, `test_suite` expansion, tree-artifact `Args` expansion at execution, action-conflict detection.
Not done: `rule(parent/extendable/initializer)`, `Args.add_all(map_each=...)`/`add_joined` with directory expansion,
aspect-on-aspect via `required_aspect_providers`, `--aspects` on `exec` deps (`--exec_aspects`), `cc_*`/`java_*`/`py_*`/`proto_*`
native rules (still reported as analysis errors), aquery proto/jsonproto/textproto output, `--toolchain_resolution_debug`.

---

## Execution, Build/Test/Run/Coverage, Sandbox, Remote, Caching, Skyframe

### Overview

Parity Rating: **Minimal / Prototype Stub (15% Parity)**.

While Zel includes crates whose module docstrings claim to mirror Bazel's execution phase (`zel-exec`), sandboxing (`zel-sandbox`), remote execution (`zel-remote`), and Skyframe evaluation engine (`zel-skyframe`), the actual implementation consists largely of disconnected stubs, single-threaded in-memory loops, and non-functional mock behaviors. In particular:
- **Build & Action Execution:** Actions are executed single-threaded and sequentially in an imperative loop outside of Skyframe; `--jobs` is parsed but ignored; and failing child processes with non-zero exit codes are swallowed and reported as successful builds. If an action's inputs or host execroot are missing, Zel touches empty output files and returns success without executing anything.
- **Test Strategy:** Zel sets only 3 test environment variables (`TEST_SRCDIR`, `TEST_TMPDIR`, `XML_OUTPUT_FILE`) out of Bazel's 30+; tests run in the host `execroot` instead of the runfiles directory; no test wrapper (`test-setup.sh`) is used; test XML is hardcoded to a 4-line template; `--test_output`, `--test_filter`, and other test flags are parsed but ignored; non-test targets and targets without executables are falsely reported as `PASSED in 0.1s`; and tests missing their binary are mocked as passing.
- **Run Command:** `zel run` builds the target, but the Zel client completely ignores the server's `ExecRequest` and exits without executing the binary (`execvp` is never invoked, `BUILD_WORKSPACE_DIRECTORY` is never set).
- **Coverage Command:** `zel coverage` is an uninstrumented alias routing to `cmd_build_or_test` where tests are not even executed, producing no coverage data or LCOV reports.
- **Sandboxing & Workers:** macOS `sandbox-exec` profile generation and sandboxed directory execution exist as dead code and are never invoked; no Linux namespaces or cgroups exist; worker requests exist only as unused proto encoders with no worker process pool or daemon management.
- **Remote Execution & Caching:** There is no gRPC, REAPI client, or BES implementation in Zel; `--disk_cache` is parsed and instantiated into an unused variable (`_disk_cache`) that is never passed to action execution.
- **Skyframe:** Evaluation is a single-threaded recursive DFS with no change pruning, no filesystem watching (DiffAwareness), no `--discard_analysis_cache`, and no cycle formatting. Crucially, `zel-server` never invokes `evaluator.invalidate()` between commands, meaning the server daemon never detects file modifications. Skyframe is not used for action execution (`ActionExecutionFunction` is absent).
- **Runfiles Trees:** Runfiles trees (`<target>.runfiles/`), `MANIFEST`, and Bzlmod `_repo_mapping` files are never generated; `--enable_runfiles` is unrecognized.

---

### Execution phase status (second slice: strategies, process wrapper, sandbox, workers, resources, cache, coverage)

Implemented (and covered by unit + e2e tests through the `zel` binary):

- **Spawn strategy registry** (`zel-exec/src/strategy.rs`): `--spawn_strategy` (default `[remote,] worker, sandboxed, local`), `--strategy=Mnemonic=...`, `--genrule_strategy` (registered before `--strategy` so `--strategy=Genrule=` overrides), `--strategy_regexp` (last specified wins, `RegexFilter` +/- semantics, progress line `<description> with context [...]`), `standalone`/`local`, `sandboxed` → `darwin-sandbox`/`processwrapper-sandbox`/`linux-sandbox`, `worker`, `dynamic` (registered by `--internal_spawn_scheduler`; no remote branch exists, so only its local branch runs). `canExec` resolution and Bazel's error messages (`'x' was requested for mnemonic M but no strategy with that identifier was registered. Valid values are: [...]` in Java `HashMap` order, `NO_USABLE_STRATEGY_FOUND`), exit code 2.
- **process-wrapper** (`zel-sandbox/src/process_wrapper.rs` + `zel-process-wrapper` bin): getopt option parsing/usage errors, `setsid`+`umask(022)`, timeout → SIGTERM → `--kill_delay` → SIGKILL, death-by-SIGALRM semantics (exit 142), orphan killing, stdout/stderr redirect, `ExecutionStatistics` stats file via `wait4`.
- **Sandboxing** (`zel-sandbox/src/sandbox.rs`): `SymlinkedSandboxedSpawn` (execroot named after the workspace, symlinked inputs, moved outputs, tree-artifact output dirs pre-created), `processwrapper-sandbox`, `darwin-sandbox` (`sandbox-exec` profile exactly as `DarwinSandboxedSpawnRunner.writeConfig`, always-writable dirs, `TMPDIR`/`TEST_TMPDIR`/`--sandbox_writable_path`, `--sandbox_block_path`, network policy, `--sandbox_default_allow_network`, `--sandbox_add_mount_pair` parsing), `--sandbox_debug` (sandbox kept, failure shows sandbox command, hint suppression), `--sandbox_base`, cleanup after the build. `linux-sandbox`: only the command-line builder (`linux.rs`) is ported; the namespace helper itself is not.
- **Persistent workers** (`zel-sandbox/src/worker.rs`): `WorkerParser` (flagfile splitting, `--persistent_worker`, `--worker_extra_flag`, strict flagfiles, exact error messages), `WorkerKey`, per-key `WorkerPool` (`--worker_max_instances`, `--worker_max_multiplex_instances`), proto (varint-delimited) and JSON protocols, multiplex workers (request ids, shared process), `--worker_sandboxing`, worker logs in `<output_base>/bazel-workers`, `--worker_quit_after_build`, warm pools across commands. Not done: worker cancellation requests on interrupt, `WorkRequest.sandbox_dir` for multiplex sandboxing, `WorkerLifecycleManager` memory-based eviction.
- **Resources** (`zel-exec/src/resources.rs`): `ResourceConverter` (`auto`/`HOST_CPUS`/`HOST_RAM` with `-`/`*` operators and Bazel's error text), `--local_resources`, `--local_test_jobs`, `ResourceManager` (cpu/ram/extra resources, test slots, allow-one-when-unavailable).
- **Action cache** (`zel-exec/src/action_cache.rs`): per-primary-output entries (action key over mnemonic/command line/env/execution info/inputs/outputs/payload, client-env digest, metadata digest over declared **and discovered (`.d` depfile) inputs**, output file digests), `MustExecuteReason`, versioned checksummed binary persistence, unreadable/other-digest-function files discarded. `--digest_function` (startup option; SHA-1/256/384/512 and BLAKE3, all implemented in `digest.rs`; `MD5` is rejected exactly like Bazel) is used by the action cache, worker inputs and test cache keys. `--check_up_to_date`.
- **Coverage**: Bazel's `collect_coverage.sh` embedded verbatim (run via `test-setup.sh` with `COVERAGE_DIR`, `COVERAGE_MANIFEST`, `COVERAGE_OUTPUT_FILE`, `LCOV_MERGER`, `RUNTEST_PRESERVE_CWD`, `SPLIT_COVERAGE_POST_PROCESSING`, `IS_COVERAGE_SPAWN`), an LCOV merger/report generator (`resources/lcov_merger.sh`, CoverageOutputGenerator semantics: merge, `--filter_sources`, `--source_file_manifest`, `--sources_to_replace_file`, `LcovPrinter` order), per-shard/run merging into `coverage.dat`, `--instrumentation_filter`/`--instrument_test_targets`, `--combined_report=lcov` → `bazel-out/_coverage/_coverage_report.dat` with Bazel's INFO message. Real instrumentation of compilers is the rules' job (no cc/java rules exist).
- **Tests**: `test.cache_status` is now an exact proto2 `TestResultData` (incl. parsed `TestCase` tree from `test.xml`), `--test_lang_filters`, `--test_timeout_filters`, `--test_sharding_strategy` (`explicit|disabled|forced=k`), last-flag-wins tag/size filters, `RunEnvironmentInfo`.
- **Output base / execroot**: full `SymlinkForest` (one symlink per top-level workspace entry, `.bazelignore`, `external/<repo>` links, real `bazel-out`), execroot named after `_main` (Bzlmod) or the `WORKSPACE` name / `__main__` (`--noenable_bzlmod`), `command.log`, `install` symlink, `java.log` → per-server log file analog.

Known remaining gaps: remote execution/cache and `remote` strategy (no REAPI client), dynamic execution racing, `linux-sandbox` namespaces/cgroups, hermetic tmp, `--reuse_sandbox_directories`/stashes, test spawns are not routed through strategies (they run via the standalone test strategy), `--jobs=auto`/`HOST_CPUS*.5` are rejected by `zel-options`' integer converter before the exec phase sees them.

---

### Buildtool & Convenience Symlinks (`--symlink_prefix`)

- **[MISSING] Workspace Convenience Symlinks:**
  - **Bazel:** In `src/main/java/com/google/devtools/build/lib/buildtool/OutputDirectoryLinksUtils.java` (lines 48–110, 273–285), Bazel automatically creates convenience symlinks in the workspace root directory pointing into the output directory: `bazel-<workspace>` (pointing to execroot), `bazel-out` (pointing to output path), `bazel-bin` (pointing to target configuration bin directory), and `bazel-testlogs` (pointing to testlogs directory).
  - **Zel:** In `crates/zel-exec/src/lib.rs` (lines 87–120 in `plant_symlink_forest`) and `crates/zel-server/src/lib.rs` (lines 931–939 in `cmd_build_or_test`), Zel only creates nested directories on the filesystem (`execroot`, `bazel_out`, `bazel_bin`, `bazel_testlogs`). It **never creates any symlinks in the workspace root**.
- **[MISSING] `--symlink_prefix` Flag:**
  - **Bazel:** Supported in `src/main/java/com/google/devtools/build/lib/buildtool/BuildRequestOptions.java` (option `symlink_prefix`, default `null` which uses `<productName>-`, e.g. `bazel-`). Setting `--symlink_prefix=/` disables convenience symlinks.
  - **Zel:** `--symlink_prefix` is neither parsed nor present in `crates/zel-options/src/lib.rs`.
- **[MISSING] `--experimental_convenience_symlinks`:**
  - **Bazel:** Supports values `normal`, `clean`, `ignore`, `log_only` in `BuildRequestOptions.java`.
  - **Zel:** Absent from `crates/zel-options`.

---

### Action Execution, Spawn Strategies & Process Wrapper

- **[DIFFERENT] Non-Zero Action Exit Codes Swallowed as Successful Builds:**
  - **Bazel:** In `src/main/java/com/google/devtools/build/lib/skyframe/ActionExecutionFunction.java` and `src/main/java/com/google/devtools/build/lib/exec/AbstractSpawnStrategy.java`, if a spawn exits with non-zero exit code (e.g. compiler error), a `SpawnExecException` / `ActionExecutionException` is thrown, the target fails, and the build exits with `ExitCode::BUILD_FAILURE` (exit code 1).
  - **Zel:** In `crates/zel-exec/src/lib.rs` (lines 261–269), when `execute_spawn_local` returns `res.exit_code != 0`, `execute_action` wraps it in `Ok(ActionExecutionOutcome { exit_code: res.exit_code, ... })`. In `crates/zel-server/src/lib.rs` (lines 954–980), `cmd_build_or_test` matches on `Ok(outcome)` and only writes stdout/stderr, **without failing the target or checking `outcome.exit_code`**! It then posts `BepEvent::TargetComplete { success: true }`, prints `"INFO: Build completed successfully"`, and returns `ExitCode::SUCCESS` (0).
- **[STUB] Fake Output Creation on Missing Execroot or Inputs:**
  - **Bazel:** If an action's declared inputs are missing before execution, Bazel halts with input missing errors (`ActionExecutionException`).
  - **Zel:** In `crates/zel-sandbox/src/lib.rs` (lines 107–122), if `!host_execroot_exists` or `!host_inputs_exist`, `execute_spawn_local` iterates over all declared outputs, creates 0-byte empty files (`fs.write_file(&abs_out, b"")`), and returns `SpawnStatus::Success` with runner `"in-memory"` without ever running the command.
- **[STUB] Silent Output Creation on Action Missing Declared Outputs:**
  - **Bazel:** If a tool exits 0 but fails to produce its declared output files, Bazel fails the action with `ActionExecutionException` ("action failed to produce outputs: ...").
  - **Zel:** In `crates/zel-sandbox/src/lib.rs` (lines 136–141), if the command exits with code 0, Zel loops through `spawn.outputs` and, if any output file does not exist, silently writes an empty file `fs.write_file(&abs_out, b"")`.
- **[MISSING] Spawn Strategy Selection (`--spawn_strategy`, `--strategy`, `--genrule_strategy`):**
  - **Bazel:** Uses `SpawnStrategyRegistry` and `SpawnStrategyResolver` (`src/main/java/com/google/devtools/build/lib/exec/SpawnStrategyRegistry.java`). Allows fine-grained per-mnemonic and global strategy routing (e.g. `--strategy=Javac=worker`, `--strategy=CppCompile=remote`, `--spawn_strategy=sandboxed`, `--genrule_strategy=local`).
  - **Zel:** In `crates/zel-options/src/lib.rs` (lines 500, 653–659), `--spawn_strategy` is parsed into `opts.spawn_strategy` (defaulting to `"local"`), but this field is never accessed or inspected in `zel-exec`, `zel-sandbox`, or `zel-server`. All spawns unconditionally call `execute_spawn_local` in `crates/zel-exec/src/lib.rs:261`. The flags `--strategy` and `--genrule_strategy` are completely unhandled.
- **[MISSING] Process Wrapper:**
  - **Bazel:** Wraps spawn execution with `process-wrapper` (`src/main/tools/process-wrapper.cc`) to enforce action timeouts with graceful SIGTERM/SIGKILL escalation, redirect output streams, collect resource statistics, and kill orphaned process groups.
  - **Zel:** Executes commands directly via Rust standard library `std::process::Command::new` (`crates/zel-sandbox/src/lib.rs:124–129`). There is no timeout enforcement, no signal handling, and no orphaned child cleanup.
- **[MISSING] Dynamic Execution (`--dynamic_strategy`):**
  - **Bazel:** `DynamicSpawnStrategy.java` races local/sandboxed execution against remote execution, taking whichever finishes first.
  - **Zel:** Not implemented.
- **[MISSING] Parameter Files & Directives:**
  - **Bazel:** Supports `@params` files, `@param` file expansion, `--materialize_param_files`, and custom argument splitting in `SpawnInputExpander.java`.
  - **Zel:** No parameter file handling exists.

---

### Test Execution & Test Strategy (`StandaloneTestStrategy`, Environment, Flags, Exit Codes)

- **[MISSING] Standard Test Environment Variables (27 Missing Variables):**
  - **Bazel:** In `src/main/java/com/google/devtools/build/lib/analysis/test/TestRunnerAction.java` (lines 740–835) and `src/main/java/com/google/devtools/build/lib/exec/StandaloneTestStrategy.java` (lines 85–96), Bazel injects the complete Bazel Test Encyclopedia environment:
    `TEST_TARGET`, `TEST_SIZE`, `TEST_TIMEOUT`, `TEST_WORKSPACE`, `TEST_BINARY`, `TEST_SRCDIR`, `TEST_TMPDIR`, `HOME` (set to `TEST_TMPDIR`), `RUNFILES_DIR`, `TEST_RANDOM_SEED`, `TEST_RUN_NUMBER`, `TESTBRIDGE_TEST_ONLY`, `TESTBRIDGE_TEST_RUNNER_FAIL_FAST`, `TEST_WARNINGS_OUTPUT_FILE`, `TEST_UNUSED_RUNFILES_LOG_FILE`, `TEST_LOGSPLITTER_OUTPUT_FILE`, `TEST_UNDECLARED_OUTPUTS_ZIP`, `TEST_UNDECLARED_OUTPUTS_DIR`, `TEST_UNDECLARED_OUTPUTS_MANIFEST`, `TEST_UNDECLARED_OUTPUTS_ANNOTATIONS`, `TEST_UNDECLARED_OUTPUTS_ANNOTATIONS_DIR`, `TEST_PREMATURE_EXIT_FILE`, `TEST_INFRASTRUCTURE_FAILURE_FILE`, `TEST_SHARD_INDEX`, `TEST_TOTAL_SHARDS`, `TEST_SHARD_STATUS_FILE`, `XML_OUTPUT_FILE`, `RUNFILES_MANIFEST_ONLY`, `RUNTEST_PRESERVE_CWD`, `COVERAGE_MANIFEST`, `COVERAGE_DIR`, `COVERAGE_OUTPUT_FILE`, and `BAZEL_TEST=1`.
  - **Zel:** In `crates/zel-exec/src/lib.rs` (lines 305–311), Zel sets **only 3 environment variables**:
    1. `TEST_SRCDIR` (set incorrectly to `execroot` instead of the target's runfiles directory)
    2. `TEST_TMPDIR` (set to `testlogs_dir/<target>/tmp`)
    3. `XML_OUTPUT_FILE` (set to `testlogs_dir/<target>/test.xml`)
    All other 27 variables are completely missing.
- **[DIFFERENT] Working Directory During Test Execution:**
  - **Bazel:** Tests are executed with current working directory set to `TEST_SRCDIR` / the target's runfiles root (`<binary>.runfiles/<workspace>`), allowing tests to locate data files and runfiles via relative paths.
  - **Zel:** In `crates/zel-exec/src/lib.rs` (line 321) and `crates/zel-sandbox/src/lib.rs` (line 126), tests are executed with `current_dir` set to `execroot`.
- **[MISSING] Test Setup Wrapper Script (`test-setup.sh`):**
  - **Bazel:** Uses `tools/test/test-setup.sh` (447 lines) to redirect stderr to stdout, ensure paths are absolute, catch signals, enforce timeouts, handle premature exits via `TEST_PREMATURE_EXIT_FILE`, generate fallback JUnit XML, and zip undeclared outputs.
  - **Zel:** Executes the test binary directly with `std::process::Command`. No wrapper script exists.
- **[STUB] Fake Passing Tests on Missing Test Executables:**
  - **Bazel:** If a test target's executable is missing, the build and test fail.
  - **Zel:** In `crates/zel-exec/src/lib.rs` (lines 322–331), if `!fs.exists(&exe_abs, true)`, Zel synthesizes a passing `SpawnResult` with exit code 0, stdout `b"PASS\n"`, duration 1ms, and runner `"standalone-test"`.
- **[DIFFERENT] False Positive Pass Reports for Non-Test Targets:**
  - **Bazel:** `bazel test` filters out non-test targets or reports errors if a non-test target is tested without a test rule.
  - **Zel:** In `crates/zel-server/src/lib.rs` (lines 1000–1043), if a target has no executable (`ct.default_info.executable.is_none()`), or if `execute_test_target` returns an error, Zel writes `//target PASSED in 0.1s` to stdout and reports success!
- **[STUB] Toy JUnit XML Generation:**
  - **Bazel:** Validates and parses JUnit XML generated by the test runner via `TestXmlOutputParser.java`. If absent, `test-setup.sh` generates compliant XML with failure stacks and logs.
  - **Zel:** In `crates/zel-exec/src/lib.rs` (lines 337–344), Zel unconditionally overwrites `test.xml` with a hardcoded 4-line template:
    ```xml
    <?xml version="1.0" encoding="UTF-8"?>
    <testsuites>
      <testsuite name="{test_label}" tests="1" failures="{0|1}">
        <testcase name="{test_label}" status="run" duration="{ms}"/>
      </testsuite>
    </testsuites>
    ```
- **[STUB] Parsed But Ignored Test Flags:**
  - `--test_filter`: Parsed in `crates/zel-options/src/lib.rs:649` into `opts.test_filter`, but never passed to `execute_test_target` or set as `TESTBRIDGE_TEST_ONLY`.
  - `--test_output`: Parsed in `crates/zel-options/src/lib.rs:645` into `opts.test_output`, but never read. Zel always prints its single hardcoded line `//label {PASSED|FAILED} in Xs`. Supported Bazel modes (`summary`, `errors`, `all`, `streamed`, `no`) are not implemented.
- **[MISSING] Test Flags Absent in Zel:**
  - `--test_env`: Missing. Users cannot pass environment variables to tests.
  - `--test_arg`: Missing. Users cannot pass CLI arguments to test executables.
  - `--test_timeout`: Missing. Timeouts cannot be specified or overridden.
  - `--runs_per_test` & `--runs_per_test_detects_flakes`: Missing. No repeat execution.
  - `--flaky_test_attempts`: Missing. No retry attempts on test failure.
  - `--test_size_filters`, `--test_tag_filters`, `--test_lang_filters`, `--test_timeout_filters`: Missing. No target filtering by size, tag, language, or timeout.
  - `--test_sharding_strategy`, `TEST_TOTAL_SHARDS`, `TEST_SHARD_INDEX`: Missing. Sharded test execution is unsupported.
  - `--cache_test_results`: Missing. Tests are unconditionally executed on every run without caching.
- **[DIFFERENT] Test Exit Codes (Codes 3 & 4):**
  - **Bazel:** Returns exit code 3 (`TESTS_FAILED`) if tests fail; returns exit code 4 (`NO_TESTS_FOUND`) if no test targets match the invocation pattern (`src/main/java/com/google/devtools/build/lib/runtime/commands/TestCommand.java:204, 236`).
  - **Zel:** If no targets match, `cmd_build_or_test` returns exit code 0 (`SUCCESS`) (in `crates/zel-server/src/lib.rs:872`). Exit code 4 is never emitted. While `TESTS_FAILED` (3) is returned when a test fails, any compiler error during the preceding build phase is swallowed, causing build failures to masquerade as test failures or successes.

---

### Runfiles Trees (`MANIFEST`, `_repo_mapping`, `--enable_runfiles`)

- **[MISSING] Runfiles Tree Creation:**
  - **Bazel:** In `src/main/java/com/google/devtools/build/lib/analysis/RunfilesSupport.java` and `src/main/java/com/google/devtools/build/lib/exec/SymlinkTreeHelper.java`, Bazel creates a runfiles directory (`<binary>.runfiles/`) containing symlinks to all dependencies and data artifacts.
  - **Zel:** No runfiles tree is created anywhere. In `crates/zel-analysis/src/lib.rs` (lines 328–329), `DefaultInfo` holds `data_runfiles` and `default_runfiles` vectors, but they are never materialized on disk by `zel-exec` or `zel-server`.
- **[MISSING] Runfiles Manifest (`MANIFEST`):**
  - **Bazel:** Creates `<binary>.runfiles_manifest` and `<binary>.runfiles/MANIFEST` for environments without symlink support (Windows) or when `--build_runfile_manifests=true`.
  - **Zel:** No manifest file is generated.
- **[MISSING] Bzlmod Repository Mapping (`_repo_mapping`):**
  - **Bazel:** Writes `<binary>.runfiles/_repo_mapping` containing apparent-to-canonical repository name mappings so runfiles libraries can resolve cross-repo dependencies.
  - **Zel:** Even though `zel-bzlmod` computes repository mappings, it is never written into runfiles.
- **[MISSING] Runfiles Flags:**
  - `--enable_runfiles`, `--build_runfile_links`, `--build_runfile_manifests`: Absent from `crates/zel-options`.

---

### Sandboxing (Darwin Sandbox, Linux Sandbox, `--sandbox_*` Flags)

- **[STUB] macOS Darwin Sandbox (`generate_darwin_sandbox_profile`):**
  - **Bazel:** `src/main/java/com/google/devtools/build/lib/sandbox/DarwinSandboxedSpawnRunner.java` generates comprehensive `.sb` profiles incorporating allow/deny lists, writable paths, temporary dirs, and system frameworks, executing via `/usr/bin/sandbox-exec -f profile.sb`.
  - **Zel:** `crates/zel-sandbox/src/lib.rs` (lines 61–71) contains a 10-line string builder `generate_darwin_sandbox_profile`. However, **this function is dead code and is never invoked anywhere outside its own unit test** (`crates/zel-sandbox/src/lib.rs:255`).
- **[STUB] Sandboxed Execution (`execute_spawn_sandboxed`):**
  - **Bazel:** `AbstractSandboxSpawnRunner.java` isolates spawns with dedicated mount tables, user namespaces, chroots, and read-only source trees.
  - **Zel:** `crates/zel-sandbox/src/lib.rs` (lines 168–208) implements `execute_spawn_sandboxed`, which does not use any kernel sandboxing, but merely copies input files to a directory `$OUTPUT_BASE/sandbox/sandbox-<id>`, executes `execute_spawn_local`, and copies output files back. **Furthermore, `execute_spawn_sandboxed` is never called anywhere in Zel**; `zel-exec` unconditionally calls `execute_spawn_local`.
- **[MISSING] Linux Sandbox (`linux-sandbox`):**
  - **Bazel:** `src/main/tools/linux-sandbox.cc` implements a privileged/unprivileged C++ container runner utilizing Linux namespaces (CLONE_NEWUSER, CLONE_NEWNS, CLONE_NEWNET, CLONE_NEWPID, CLONE_NEWIPC, CLONE_NEWUTS), pivot_root, tmpfs mounts, and cgroups v1/v2 for memory limits.
  - **Zel:** No Linux namespace or container logic exists anywhere in Zel.
- **[MISSING] All `--sandbox_*` Flags:**
  - **Bazel:** Supports 25+ sandbox options in `src/main/java/com/google/devtools/build/lib/sandbox/SandboxOptions.java`, including:
    `--sandbox_debug`, `--sandbox_base`, `--sandbox_fake_hostname`, `--sandbox_fake_username`, `--sandbox_explicit_pseudoterminal`, `--sandbox_block_path`, `--sandbox_tmpfs_path`, `--sandbox_writable_path`, `--sandbox_add_mount_pair`, `--sandbox_default_allow_network`, `--reuse_sandbox_directories`, `--experimental_sandbox_memory_limit_mb`.
  - **Zel:** None of these flags exist or are parsed in `crates/zel-options`.

---

### Persistent Workers & Worker Protocol

- **[STUB] Worker Protocol Functions:**
  - **Bazel:** `src/main/java/com/google/devtools/build/lib/worker/` implements a full persistent worker subsystem: `WorkerPool`, `WorkerLifecycleManager`, `WorkerSpawnRunner`, multiplex workers (`--worker_multiplex`), worker sandboxing, process recycling, cancelation, and framing over protobuf / JSON streams.
  - **Zel:** In `crates/zel-sandbox/src/lib.rs` (lines 211–246), functions `build_worker_request` and `worker_response_to_spawn_result` exist, but **they are never called anywhere in Zel**.
- **[MISSING] Worker Process Management & Pool:**
  - **Bazel:** Spawns long-running worker daemon processes, manages standard I/O pipes, and routes compile spawns to warm compilers.
  - **Zel:** Zero worker process management exists. Persistent workers are completely unimplemented.
- **[MISSING] Worker Flags:**
  - `--worker_max_instances`, `--worker_multiplex`, `--worker_quit_after_build`, `--worker_verbose`, `--worker_sandboxing`: Missing from `crates/zel-options`.

---

### Remote Execution, Remote Caching & Disk Cache (REAPI, BES)

- **[MISSING] Remote Execution API (REAPI v2) Client:**
  - **Bazel:** Implements full REAPI v2 over gRPC in `src/main/java/com/google/devtools/build/lib/remote/` (`GrpcRemoteExecutor.java`, `RemoteExecutionService.java`). Connects to BuildBarn, BuildGrid, EngFlow, etc.
  - **Zel:** `crates/zel-remote` contains **no gRPC, HTTP, network client, or socket code whatsoever**. Its `Cargo.toml` dependencies are strictly `zel-proto`, `zel-util`, and `zel-vfs`. It cannot connect to any remote executor.
- **[MISSING] Remote Flags (`--remote_executor`, `--remote_cache`, `--remote_download_outputs`):**
  - **Bazel:** Supports `--remote_executor`, `--remote_cache`, `--remote_download_outputs` (all, toplevel, minimal for Build-without-the-Bytes), `--remote_header`, `--remote_timeout`, `--remote_upload_local_results`.
  - **Zel:** None of these flags exist in `crates/zel-options`.
- **[STUB] Disk Cache (`--disk_cache`):**
  - **Bazel:** In `src/main/java/com/google/devtools/build/lib/remote/disk/DiskCacheClient.java`, reads and writes REAPI action results and CAS blobs to local disk, sharing cache entries across workspaces.
  - **Zel:** While `crates/zel-remote/src/lib.rs` (lines 59–158) defines a `DiskCacheClient` struct, in `crates/zel-server/src/lib.rs` (lines 941–945), it is parsed and stored into an unused variable with a leading underscore:
    ```rust
    let _disk_cache = parsed
        .disk_cache
        .as_ref()
        .map(|root| DiskCacheClient::new(root.clone()));
    ```
    This `_disk_cache` is **never passed to `execute_action` or used anywhere in the build**. The `--disk_cache` flag in Zel is a complete no-op stub.
- **[MISSING] Build Event Service (BES):**
  - **Bazel:** Publishes build progress and event streams to BES servers via `--bes_backend=grpc://...`.
  - **Zel:** No BES client exists (only a local `--build_event_json_file` JSON dumper in `zel-events`).

---

### Action Cache (`CompactPersistentActionCache`)

- **[DIFFERENT] Keying Mechanism:**
  - **Bazel:** `src/main/java/com/google/devtools/build/lib/actions/cache/CompactPersistentActionCache.java` keys the action cache by the action's unique `actionKey` or action digest. It verifies all input digests, previous output file metadata (file size, timestamp / digest), and execution environment.
  - **Zel:** In `crates/zel-exec/src/lib.rs` (lines 28–83, 161–176, 273–278), `CompactPersistentActionCache` keys entries **by output execution path (`out_art.exec_path`)** instead of the action key or digest! It simply checks `action_cache.get(out_path) == Some(fingerprint)`.
- **[DIFFERENT] Storage Format & In-Memory Reading:**
  - **Bazel:** Stores a compact indexed binary format with corruption repair, generational versioning, and disk syncing (`.act` files).
  - **Zel:** Serializes in plaintext lines of `path=fingerprint\n` via standard string concatenation (`crates/zel-exec/src/lib.rs:59–67`).
- **[DIFFERENT] Action Execution Fingerprint Computation:**
  - **Bazel:** Uses Skyframe-cached digests of input artifacts (`ArtifactPathResolver`, `FileArtifactValue`).
  - **Zel:** In `crates/zel-exec/src/lib.rs` (lines 123–138), Zel reads **the entire content of every input file from disk into memory** synchronously on every action execution (`fs.read_file(&p)`) to compute SHA-256 hashes, creating severe I/O bottlenecks and memory pressure.
- **[MISSING] Action Cache Flags:**
  - `--use_action_cache`, `--bust_action_caches`: Missing from `crates/zel-options`. Action cache in Zel is hardcoded on.

---

### Concurrency, Scheduling & Execution Flags

- **[STUB] Parallel Execution (`--jobs` / `-j`):**
  - **Bazel:** Defaults `--jobs` to host CPU core count (`BuildRequestOptions.java:jobs`) and schedules actions concurrently across worker threads using `ParallelEvaluator` and `SimpleActionSchedulingEngine`.
  - **Zel:** In `crates/zel-options/src/lib.rs` (lines 673–692), `--jobs` is parsed into `opts.jobs`. In `crates/zel-server/src/lib.rs` (lines 951–985), actions are executed in an imperative, single-threaded sequential `for` loop (`for ct in &configured_targets { for action in &ct.actions { ... } }`). `opts.jobs` is **completely ignored**, and all builds are strictly single-threaded.
- **[PARTIAL] `--keep_going` (-k):**
  - **Bazel:** On error, continues executing independent branches in the action DAG, aggregates all errors, and returns `BUILD_FAILURE` (1) or `TESTS_FAILED` (3).
  - **Zel:** Only checks `if !parsed.keep_going { return ExitCode::BUILD_FAILURE; }` for I/O errors (`Err(e)`). Because non-zero process exits are treated as `Ok(outcome)`, `--keep_going` does not affect failed compiler commands.
- **[MISSING] Resource Scheduling Flags (`--local_*_resources`):**
  - `--local_ram_resources`, `--local_cpu_resources`, `--local_resources`: Absent from `crates/zel-options`.
- **[MISSING] Target & Output Filtering Flags:**
  - `--build_tests_only`: Missing.
  - `--output_groups`: Missing.
  - `--aspects`, `--aspects_parameters`: Missing.
  - `--nobuild`: Missing.
  - `--check_up_to_date`: Missing.

---

### Command: `run` (`--run_under`, `--script_path`, `BUILD_WORKSPACE_DIRECTORY`, Client Exec)

- **[STUB] Binary Execution by Client (`execvp` Never Called):**
  - **Bazel:** When `bazel run //pkg:bin` is invoked, the C++ client (`src/main/cpp/blaze.cc`) receives an `ExecRequest` in `RunResponse`, sets up environment variables, and invokes `execv` / `execvp` so that the client process is replaced by the user's binary.
  - **Zel:** In `crates/zel-server/src/lib.rs` (lines 1113–1121), the server builds the `ExecRequest`. However, in `crates/zel-client/src/lib.rs` (lines 280–354), **the client completely ignores `resp.exec_request` and simply exits with `resp.exit_code`**! The target executable is **never launched**.
- **[MISSING] `BUILD_WORKSPACE_DIRECTORY` & `BUILD_WORKING_DIRECTORY`:**
  - **Bazel:** `RunCommand.java` injects `BUILD_WORKSPACE_DIRECTORY` (pointing to the workspace root) and `BUILD_WORKING_DIRECTORY` (pointing to the directory where the user ran `bazel`).
  - **Zel:** In `crates/zel-server/src/lib.rs` (line 1116), `environment_variable` in `ExecRequest` is set to an empty vector `Vec::new()`.
- **[MISSING] `--run_under` & `--script_path`:**
  - `--run_under`: Missing. Cannot run binaries under debuggers or profilers (e.g. `--run_under=gdb`).
  - `--script_path`: Missing. `ExecRequest.script_path` is hardcoded to `None`.

---

### Command: `coverage` (Instrumentation, LCOV, `--combined_report`)

- **[STUB] `coverage` Command Implementation:**
  - **Bazel:** `CoverageCommand.java` runs tests with code coverage enabled (`--collect_code_coverage=true`), instruments source files according to `--instrumentation_filter`, passes `COVERAGE_DIR` and `COVERAGE_OUTPUT_FILE`, gathers `coverage.dat`, and generates combined LCOV reports via `lcov_merger`.
  - **Zel:** In `crates/zel-server/src/lib.rs` (lines 250–252), `"coverage"` is routed directly into `cmd_build_or_test("coverage", ...)`. Inside `cmd_build_or_test` (line 999), tests are only executed `if command == "test"`. Therefore, `zel coverage` **only runs the build phase, never executes any tests, performs zero coverage instrumentation, and outputs no coverage files**.
- **[MISSING] Coverage Flags:**
  - `--combined_report`, `--coverage_support`, `--instrumentation_filter`, `--split_coverage_postprocessing`: Missing from `crates/zel-options`.

---

### Skyframe Evaluation Engine & Semantics

- **[MISSING] Execution Phase in Skyframe (`ActionExecutionFunction`):**
  - **Bazel:** Skyframe is the execution engine. Every action is a `SkyKey` (`ActionLookupData`) evaluated by `ActionExecutionFunction.java`. This enables Skymeld (`--experimental_merged_skyframe_analysis_execution`, interleaving analysis and execution), action rewinding (`--rewind_lost_inputs`), and fine-grained action caching.
  - **Zel:** `SkyFunctionName::ACTION_EXECUTION` is declared in `crates/zel-skyframe/src/lib.rs:39`, but **no `ActionExecutionFunction` is implemented**. Execution is performed outside Skyframe via an imperative `for` loop in `zel-server`.
- **[MISSING] Incremental Invalidation in Server Daemon:**
  - **Bazel:** `DiffAwarenessManager` and filesystem watchers detect file edits and invalidate dirty `FileState` nodes in Skyframe before every build.
  - **Zel:** `crates/zel-server` **never calls `evaluator.invalidate()` or resets the evaluator between command runs** (`crates/zel-server/src/lib.rs:1134–1175`). If a file is modified on disk while the Zel server daemon is active, Zel serves stale packages from the memoized graph indefinitely.
- **[MISSING] Incremental Change Pruning:**
  - **Bazel:** Skyframe implements change pruning (`dirty` -> `checking-deps` -> `clean`). If a node's dependencies are rebuilt but evaluate to the exact same values, the node itself is not re-evaluated.
  - **Zel:** In `crates/zel-skyframe/src/lib.rs` (lines 400–407), if `entry.dirty` is true, the node is unconditionally re-evaluated. No dependency value comparison is performed.
- **[MISSING] Filesystem Watching (DiffAwareness):**
  - **Bazel:** Integrates with OS filesystem events (`fsevents` on macOS, `inotify` on Linux, Watchman) via `DiffAwarenessManager.java`.
  - **Zel:** No filesystem watching or diff awareness exists.
- **[MISSING] Memory Management (`--discard_analysis_cache`):**
  - **Bazel:** Drops configured targets and aspect values after analysis/execution to free JVM heap.
  - **Zel:** Unimplemented; graph nodes accumulate indefinitely in the server's `InMemoryMemoizingEvaluator`.
- **[PARTIAL] Cycle Detection & Reporting:**
  - **Bazel:** `CycleReporter.java` formats dependency cycles with target labels, configuration keys, attribute names, and file paths.
  - **Zel:** `crates/zel-skyframe/src/lib.rs` (lines 392–396) detects cycles during DFS by checking `visiting` and appends them to `result.cycles`. However, `zel-server` never inspects `result.cycles` and prints only generic failure strings.

---


## EVENTS, TERMINAL UI, BEP, PROFILER, EXIT CODES, PROTOS, FFI

### Parity Overview

Overall Parity Level: **LOW (15–20%)**

While Zel provides basic foundational structures for in-memory event reporting (`zel-events`), a subset of ExitCodes (`zel-util`), and rudimentary wire-format serialization for a small handful of protobuf messages (`zel-proto`), the parity gap across this subsystem is massive. Crucial components of Bazel's operational feedback and observability infrastructure are either completely absent or present only as minimal stubs. 

Specifically:
- **Terminal UI & Event Handling**: Zel possesses no `UiStateTracker`, dynamic progress bar, curses-based multi-line terminal redraw, ANSI colorization (`--color`), timestamping (`--show_timestamps`), or event filtering (`--ui_event_filters`). Build progress and status are printed via ad-hoc synchronous log writes to stderr upon command completion, rather than streamed concurrently via an event bus.
- **Build Event Protocol (BEP)**: Zel implements only 6 out of 25+ BuildEvent variants in `build_event_stream.proto`, omits the critical `children` DAG hierarchy entirely, serializes JSON using string formatting rather than standard protobuf JSON mapping, lacks `--build_event_binary_file` and `--build_event_text_file`, and has zero support for remote Build Event Service (BES) streaming (`--bes_backend`).
- **Profiler & Analysis**: Zel's `analyze-profile` command is a hardcoded no-op stub that prints a static message and exits with code 0 without reading any profile data. Its `--profile` output writes only two coarse synthetic phases (`Loading & Analysis` and `Execution`), completely omitting action-level profiling, Skyframe task counters, thread metadata, resource usage metrics, and critical path analysis.
- **Exit Codes & Failure Details**: Zel duplicates 20 exit codes from `ExitCode.java`, but lacks dynamic failure detail prioritization. In `failure_details.proto`, Zel defines typed enums for only 3 out of 65+ failure detail categories (`Interrupted`, `Command`, `GrpcServer`), demoting all other failure categories to untyped numeric fallbacks.
- **Protobuf Coverage**: `zel-proto` lacks more than 25 official Bazel protobuf schemas, including `action_cache.proto`, `spawn.proto` (execution logs), `invocation_policy.proto`, `command_line.proto`, and `test_status.proto`. Its REAPI v2 (`remote_execution.proto`) implementation covers only a minimal subset of action cache messages, omitting directory trees, CAS RPCs, and execution requests.
- **Hashing & FFI**: `--digest_function` is ignored, forcing hardcoded MD5 and SHA-256. `zel-ffi-bridge` is a custom in-tree bridge for an experimental Java-to-Rust delegation layer, with no correspondence to Bazel's JNI subsystems (`src/main/native`).


### Terminal UI & Event Handling

* **[MISSING] Real-time Action Progress State Machine (`UiStateTracker`)**: 
  - *Bazel*: `src/main/java/com/google/devtools/build/lib/runtime/UiStateTracker.java` (1,463 lines) implements an asynchronous, thread-safe state tracker for all phases of build execution. It monitors in-flight actions, action state transitions (`SCANNING`, `PREPARING`, `CACHING`, `UPLOADING`, `SCHEDULING`, `RUNNING`, `DOWNLOADING`), worker usage, and active test executions, computing dynamic execution elapsed times, completed/total action ratios, and strategy descriptions.
  - *Zel*: Completely missing. Zel has no action state machine, no concept of action phases, and does not monitor running actions while they execute.

* **[MISSING] Dynamic Curses Progress Bar (`--curses=yes|no|auto`)**:
  - *Bazel*: `src/main/java/com/google/devtools/build/lib/runtime/UiEventHandler.java:118` and `UiStateTracker.java:250-320` maintain a multi-line live terminal display via ANSI terminal cursor manipulation (`\033[A`, `\033[K`). It dynamically redraws progress lines at the bottom of the terminal, scrolling existing log lines cleanly above it while keeping current action descriptions visible.
  - *Zel*: No curses or terminal cursor positioning support exists. In `zel-options/src/lib.rs:693-700`, `--curses` is absorbed into `flag_map` and ignored. Zel only prints static, sequential log lines.

* **[MISSING] Terminal Color Output (`--color=yes|no|auto`)**:
  - *Bazel*: Supported via `UiOptions.color` (`src/main/java/com/google/devtools/build/lib/runtime/UiOptions.java:55`) and `AnsiTerminal.java`. Colorizes event prefixes (e.g., Red for `ERROR:`, Magenta for `WARNING:`, Green for `PASS:`, Red for `FAIL:`, Cyan for `INFO:`) based on tty detection or user flags.
  - *Zel*: Completely missing. `Event::format_console_line()` (`crates/zel-events/src/lib.rs:116-121`) emits unformatted ASCII strings without ANSI escape codes, and `--color` is ignored.

* **[MISSING] Progress Rate Limiting and Display Controls (`--show_progress`, `--show_progress_rate_limit`)**:
  - *Bazel*: Supported via `UiOptions.show_progress` (default true) and `UiOptions.show_progress_rate_limit` (default 0.2s for curses, 1.0s for non-curses; `UiEventHandler.java:100-104`). Allows users to disable progress output entirely or customize refresh intervals.
  - *Zel*: Completely missing. Zel does not implement progress rate limiting, and `--show_progress` / `--noshow_progress` flags are ignored.

* **[MISSING] Event Timestamping (`--show_timestamps`)**:
  - *Bazel*: Supported via `UiOptions.show_timestamps` (`UiOptions.java:80`, `UiEventHandler.java:106-107`). Prefixes console messages with timestamps formatted as `(HH:mm:ss) ` or calendar dates on day boundaries.
  - *Zel*: Completely missing. `Event` in `crates/zel-events/src/lib.rs:72-77` has no timestamp field and does not support timestamp prefixing.

* **[MISSING] Console Event Filtering (`--ui_event_filters`)**:
  - *Bazel*: Supported via `UiOptions.ui_event_filters` (`UiOptions.java:105`, `UiEventHandler.java:541-554`). Allows fine-grained filtering of console output by event kinds (e.g., `--ui_event_filters=+info,-stdout`).
  - *Zel*: Completely missing. All events collected during build execution are unconditionally dumped to stderr at the end of the build (`crates/zel-server/src/lib.rs:986-988`).

* **[MISSING] Target Result Reporting (`--show_result`)**:
  - *Bazel*: Supported via `BuildRequestOptions.show_result` (`src/main/java/com/google/devtools/build/lib/buildtool/BuildRequestOptions.java:253`). Reports up-to-date status and paths to output artifacts for successfully built targets (e.g., `Target //pkg:target up-to-date:\n  bazel-bin/pkg/target`).
  - *Zel*: Completely missing. Zel outputs only `INFO: Build completed successfully...` without enumerating target artifacts or honoring `--show_result=<n>`.

* **[STUB] Verbose Failure Diagnostics (`--verbose_failures`)**:
  - *Bazel*: `src/main/java/com/google/devtools/build/lib/exec/ExecutionOptions.java:194`. When set, prints the complete command line and environment variable assignments of any failed action.
  - *Zel*: `crates/zel-options/src/lib.rs:490, 589-601` parses `--verbose_failures` and `--noverbose_failures` into `opts.verbose_failures`. However, `opts.verbose_failures` is never read or referenced anywhere in `zel-exec`, `zel-sandbox`, or `zel-server`. Action execution failures simply print the error string without command lines or environment dumps (`crates/zel-server/src/lib.rs:974`).

* **[PARTIAL] Subcommand Announcements (`--subcommands`, `-s`)**:
  - *Bazel*: `ExecutionOptions.subcommands` (`ExecutionOptions.java:202`) supports `--subcommands` (shorthand `-s`) and `--subcommands=pretty_print`. Prints full action execution command lines, working directory, environment variables, and execution strategy headers.
  - *Zel*: `crates/zel-options/src/lib.rs:489, 581-587` parses `--subcommands` and `-s`. However, it only formats a truncated string `"# {} [action '{}']\n{}"` in `crates/zel-exec/src/lib.rs:245-250`. It omits environment variables, inputs/manifests, execution root paths, and does not support `=pretty_print`.

* **[MISSING] Build Explanation (`--explain`)**:
  - *Bazel*: Supported via `BuildRequestOptions.explain` (`src/main/java/com/google/devtools/build/lib/buildtool/BuildRequestOptions.java:131`). Generates an explanation file detailing why each action was executed (e.g. input file changed, action key mismatch, output file missing).
  - *Zel*: Completely missing for builds. While `zel mod explain` exists for Bzlmod inspection, the build flag `--explain=<file>` is unrecognized and ignored.

* **[DIFFERENT] Deferred Batch Printing vs Real-Time Event Dispatch**:
  - *Bazel*: Uses Google Guava `EventBus` (`UiEventHandler.java:120`) to dispatch events to listeners in real time. Standard output and error from running actions are streamed to the console while actions run.
  - *Zel*: `Reporter` (`crates/zel-events/src/lib.rs:126-146`) buffers all events into a `Mutex<Vec<Event>>`. In `zel-server/src/lib.rs:986-988`, all events are dumped to stderr in a tight loop only after ALL configured targets and actions have finished executing. There is zero streaming during action execution.

* **[DIFFERENT] EventKind Prefixes and Category Semantics**:
  - *Bazel*: `src/main/java/com/google/devtools/build/lib/events/EventKind.java` defines distinct behaviors: `Progress`, `Start`, `Finish`, `Pass`, `Fail`, `Timeout`, `DepChecker`, and `ServerLog`. `Progress`, `Start`, and `Finish` are consumed by UI progress trackers without emitting standalone `INFO: ` lines unless debugging is enabled.
  - *Zel*: `crates/zel-events/src/lib.rs:39-56` hardcodes `"INFO: "` prefix for `EventKind::Info`, `EventKind::Progress`, `EventKind::Start`, and `EventKind::Finish`. This produces repetitive, redundant `INFO:` logs for internal lifecycle transitions.


### Build Event Protocol (BEP)

* **[MISSING] 20+ Core BuildEvent ID and Payload Schemas**:
  - *Bazel*: Defined in `src/main/java/com/google/devtools/build/lib/buildeventstream/proto/build_event_stream.proto` (1,840 lines). Bazel defines 28 `BuildEventId` submessages and 25 payload messages.
  - *Zel*: `crates/zel-events/src/lib.rs:168-197` implements only 6 high-level event variants: `Started`, `Progress`, `TargetConfigured`, `TargetComplete`, `TestResult`, and `Finished`.
  - The following 20+ event types emitted by Bazel are completely **missing** from Zel:
    1. `WorkspaceConfigId` / `WorkspaceConfig` (workspace configuration and execution options)
    2. `UnstructuredCommandLineId` / `UnstructuredCommandLine` (raw command-line args)
    3. `StructuredCommandLineId` / `command_line.CommandLine` (canonical, parsed, and tool options)
    4. `OptionsParsedId` / `OptionsParsed` (startup and command options after parsing)
    5. `FetchId` / `Fetch` (external repository fetch operations and status)
    6. `WorkspaceStatusId` / `WorkspaceStatus` (build metadata keys from `workspace_status_command`)
    7. `BuildMetadataId` / `BuildMetadata` (custom build invocation metadata key-value pairs)
    8. `ConfigurationId` / `Configuration` (target platform, mnemonic, and configuration settings)
    9. `PatternExpandedId` / `PatternExpanded` (expansion of target patterns like `//...` or `:all`)
    10. `NamedSetOfFilesId` / `NamedSetOfFiles` (nested sets / depset artifacts; indispensable for downstream BEP consumers to locate outputs)
    11. `ActionCompletedId` / `ActionExecuted` (individual action execution results, exit codes, cache hits, stdout/stderr artifacts)
    12. `TestProgressId` / `TestProgress` (incremental test attempt logging)
    13. `TestSummaryId` / `TestSummary` (aggregated status, attempt counts, shard counts, total duration)
    14. `TargetSummaryId` / `TargetSummary` (aggregated target status across configurations)
    15. `BuildToolLogsId` / `BuildToolLogs` (server logs, garbage collection logs, profile file URIs)
    16. `BuildMetricsId` / `BuildMetrics` (detailed performance metrics: `ActionSummary`, `MemoryMetrics`, `TargetMetrics`, `PackageMetrics`, `TimingMetrics`, `CumulativeMetrics`, `ArtifactMetrics`, `BuildGraphMetrics`, `WorkerMetrics`, `NetworkMetrics`, `DynamicExecutionMetrics`, `RemoteAnalysisCacheStatistics`, `BzlMetrics`)
    17. `ConvenienceSymlinksIdentifiedId` / `ConvenienceSymlinksIdentified` (symlinks created in workspace root)
    18. `ExecRequestId` / `ExecRequestConstructed` (target execution details for `bazel run`)
    19. `SkyValueUploadedId` / `SkyValueUploaded` (remote analysis cache Skyframe node uploads)
    20. `Aborted` (announcement that an in-flight child event was aborted or canceled)

* **[MISSING] Event DAG Hierarchy (`children` array)**:
  - *Bazel*: `BuildEvent` in `build_event_stream.proto:1804` declares `repeated BuildEventId children = 2`. In Bazel's BEP contract, every event (starting with `BuildStarted`) must explicitly announce the `BuildEventId`s of all subsequent events that belong to it in the build DAG. Consumers (e.g. CI systems, ResultStore, BuildBuddy) rely on this tree structure to detect dropped events or unfinished builds.
  - *Zel*: `BepEvent::to_json_line()` (`crates/zel-events/src/lib.rs:200-248`) never emits a `children` field in any event JSON object. Every event is rendered as an isolated root-level object with empty `{}` id references.

* **[MISSING] Binary Protobuf Transport (`--build_event_binary_file`)**:
  - *Bazel*: Supported via `BuildEventStreamOptions.build_event_binary_file` (`src/main/java/com/google/devtools/build/lib/buildeventstream/BuildEventStreamOptions.java:42`). Streams binary varint-length-delimited `BuildEvent` protobuf messages.
  - *Zel*: Completely missing. The option is not recognized, and `zel-proto` has no binary encoder for `BuildEvent`.

* **[MISSING] Textproto Transport (`--build_event_text_file`)**:
  - *Bazel*: Supported via `BuildEventStreamOptions.build_event_text_file`. Streams human-readable Protobuf TextFormat lines.
  - *Zel*: Completely missing. Unrecognized option.

* **[MISSING] Build Event Service (BES) Remote Streaming (`--bes_backend`)**:
  - *Bazel*: Supports streaming build events live to remote gRPC backends (`google.devtools.build.v1.PublishBuildEvent`) via `--bes_backend`, `--bes_timeout`, `--bes_client_cert`, `--bes_client_key`, `--bes_header`, `--bes_instance_name`, `--bes_keywords`, `--bes_lifecycle_events`, `--bes_results_url`.
  - *Zel*: Completely missing. Zel has no gRPC client for BES, cannot stream events to remote endpoints, and ignores all `--bes_*` flags.

* **[DIFFERENT] Post-Build Single Write vs Real-Time Streaming**:
  - *Bazel*: Transports flush events incrementally to disk or network sockets as soon as the corresponding lifecycle events occur in the build pipeline.
  - *Zel*: Events are buffered in memory (`Reporter.bep_events`, `crates/zel-events/src/lib.rs:128`). Only when the entire build command completes (`zel-server/src/lib.rs:1063`) does Zel iterate over the buffered events and write them sequentially into `--build_event_json_file` using `fs.write_file()`. If Zel crashes, hangs, or is SIGKILLed mid-build, zero BEP data is written.

* **[DIFFERENT] Custom Non-Compliant JSON Formatting vs Proto3 JSON Mapping**:
  - *Bazel*: Emits JSON according to official proto3 JSON mapping rules (Google `JsonFormat`), correctly preserving field types, camelCase names, int64/uint64 strings, and enum values.
  - *Zel*: `BepEvent::to_json_line()` (`crates/zel-events/src/lib.rs:200-249`) uses raw `format!` strings with manual character escaping (`escape_json()`). This introduces functional divergences:
    - `TargetConfigured`: Zel formats `{"id":{"targetConfigured":{"label":"..."}},"configured":{"targetKind":"... rule"}}`. Bazel requires `TargetConfiguredId` to contain `aspect: ""` and `TargetConfigured` to contain `test_size` and `tag`.
    - `TargetCompleted`: Zel formats `{"id":{"targetCompleted":{"label":"..."}},"completed":{"success":...}}`. Bazel's `TargetCompletedId` requires `configuration: {id: "..."}` and `aspect: ""`.
    - `TestResult`: Zel serializes `duration_ms` as a quoted string inside `testAttemptDurationMillis: "{duration_ms}"`, while omitting `test_status` enum, `execution_info`, and `test_action_output` artifacts.
    - `BuildFinished`: Zel omits `overall_success` boolean and `finish_time_millis` timestamp.


### Profiler and Profile Analysis

* **[STUB] `analyze-profile` Command Implementation**:
  - *Bazel*: `docs/versions/8.3.1/advanced/performance/json-trace-profile.mdx:51-90` and `src/main/java/com/google/devtools/build/lib/profiler/JsonProfile.java:39-125`. `bazel analyze-profile <path>` parses JSON trace profiles, aggregates phase statistics (Launch, Init, Loading, Analysis, Preparation, Execution, Finish), calculates the action critical path with breakdown percentages, and prints task-level cumulative timing tables.
  - *Zel*: `analyze-profile` is an explicit no-op stub in `crates/zel-server/src/lib.rs:254-257`:
    ```rust
    "dump" | "config" | "analyze-profile" | "license" | "print_action" => {
        writeln_bytes(&mut stdout, &format!("INFO: Running '{command}'..."));
        ExitCode::SUCCESS
    }
    ```
    It performs no file I/O, does not read the profile argument, does not parse JSON trace events, and outputs no analytical data.

* **[PARTIAL] Chrome Trace Format JSON Generation (`--profile`)**:
  - *Bazel*: Implemented in `src/main/java/com/google/devtools/build/lib/profiler/JsonTraceFileWriter.java` and `Profiler.java`. Bazel records:
    - Thread metadata events (`ph: "M"`, `name: "thread_name"`, `thread_sort_index`).
    - Build phase marker events (`ph: "I"`, `ProfilePhase`: `LAUNCH`, `INIT`, `TARGET_PATTERN_EVAL`, `LOAD_AND_ANALYZE`, `PREPARATION`, `EXECUTION`, `FINISH`).
    - Over 100 granular task event types (`ProfilerTask`: `ACTION`, `ACTION_CHECK`, `ACTION_LOCK`, `ACTION_UPDATE`, `ACTION_COMPLETE`, `VFS_STAT`, `VFS_DIR`, `VFS_OPEN`, `VFS_READ`, `VFS_WRITE`, `SKYFRAME_EVAL`, `REMOTE_EXECUTION`, `REMOTE_DOWNLOAD`, `WORKER_BORROW`, etc.).
    - System performance counter tracks (`ph: "C"`, resident memory, CPU utilization percentage, active workers, network download/upload bytes).
    - Gzip compression (`.profile.gz`) by default.
  - *Zel*: `crates/zel-remote/src/lib.rs:170-211` defines a minimal `Profiler` struct. It records only duration events (`ph: "X"`) hardcoded with `pid: 1, tid: 1`. During a build, `zel-server` records exactly **two** events in total:
    1. `"Loading & Analysis"` (`crates/zel-server/src/lib.rs:895-900`)
    2. `"Execution"` (`crates/zel-server/src/lib.rs:991-996`)
    No individual actions, no file operations, no worker activities, and no system resource metrics are ever captured.

* **[MISSING] Gzip Compression for Trace Profiles**:
  - *Bazel*: By default, `--profile=<path>` writes a gzip-compressed trace file (`.profile.gz`), reducing multi-gigabyte traces into manageable files.
  - *Zel*: `crates/zel-remote/src/lib.rs:196-210` writes raw uncompressed JSON directly via `fs.write_file()`. If a path ending in `.gz` is passed to `--profile`, Zel writes plaintext JSON into the `.gz` file without compressing it.

* **[MISSING] Profiler Configuration and Collection Flags**:
  - *Bazel*: Supports comprehensive profiling controls:
    - `--record_full_profiler_data` (records all events without filtering)
    - `--experimental_slim_json_profile` (merges small events to limit trace file sizes)
    - `--profile_include_target_label` (annotates action events with their target labels)
    - `--always_profile_slow_operations` (forces recording for slow filesystem tasks)
    - `--experimental_collect_system_network_usage` (records system-level network throughput)
    - `--experimental_collect_resource_estimation` (records estimated CPU/RAM demand)
  - *Zel*: None of these flags are supported or parsed.


### Exit Codes and Failure Details

* **[DIFFERENT] Exit Code Naming and Constant Definitions (`ExitCode.java` vs `zel-util::ExitCode`)**:
  - *Bazel*: `src/main/java/com/google/devtools/build/lib/util/ExitCode.java:50-80` defines 19 primary registered exit codes and 2 unregistered aliases.
  - *Zel*: `crates/zel-util/src/lib.rs:29-50` defines 20 constants.
  - Differences in specific codes:
    - Code 38: In Bazel (`ExitCode.java:74-75`), the constant is named `TRANSIENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR`, with string name `"PUBLISH_ERROR"`. In Zel (`crates/zel-util/src/lib.rs:46`), the constant is named `PUBLISH_ERROR`.
    - Unregistered codes: In Bazel, `PARSING_FAILURE` (code 1) and `PARTIAL_ANALYSIS_FAILURE` (code 3) are created via `createUnregistered()` to avoid registry collisions with `BUILD_FAILURE` (code 1) and `TESTS_FAILED` (code 3). In Zel, both are exposed as normal constants without duplicate resolution logic.
    - Full list comparison:
      | Numeric Code | Bazel Constant Name | Zel Constant Name | Infrastructure Failure? |
      | :--- | :--- | :--- | :--- |
      | 0 | `SUCCESS` | `SUCCESS` | False |
      | 1 | `BUILD_FAILURE` | `BUILD_FAILURE` | False |
      | 1 | `PARSING_FAILURE` (unregistered) | `PARSING_FAILURE` | False |
      | 2 | `COMMAND_LINE_ERROR` | `COMMAND_LINE_ERROR` | False |
      | 3 | `TESTS_FAILED` | `TESTS_FAILED` | False |
      | 3 | `PARTIAL_ANALYSIS_FAILURE` (unregistered) | `PARTIAL_ANALYSIS_FAILURE` | False |
      | 4 | `NO_TESTS_FOUND` | `NO_TESTS_FOUND` | False |
      | 6 | `RUN_FAILURE` | `RUN_FAILURE` | False |
      | 7 | `ANALYSIS_FAILURE` | `ANALYSIS_FAILURE` | False |
      | 8 | `INTERRUPTED` | `INTERRUPTED` | False |
      | 9 | `LOCK_HELD_NOBLOCK_FOR_LOCK` | `LOCK_HELD_NOBLOCK_FOR_LOCK` | False |
      | 32 | `REMOTE_ENVIRONMENTAL_ERROR` | `REMOTE_ENVIRONMENTAL_ERROR` | True |
      | 33 | `OOM_ERROR` | `OOM_ERROR` | True |
      | 34 | `REMOTE_ERROR` | `REMOTE_ERROR` | True |
      | 36 | `LOCAL_ENVIRONMENTAL_ERROR` | `LOCAL_ENVIRONMENTAL_ERROR` | True |
      | 37 | `BLAZE_INTERNAL_ERROR` | `BLAZE_INTERNAL_ERROR` | True |
      | 38 | `TRANSIENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR` | `PUBLISH_ERROR` | True |
      | 39 | `REMOTE_CACHE_EVICTED` | `REMOTE_CACHE_EVICTED` | True |
      | 45 | `PERSISTENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR` | `PERSISTENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR` | False |
      | 48 | `EXTERNAL_DEPS_ERROR` | `EXTERNAL_DEPS_ERROR` | False |

* **[MISSING] Dynamic Exit Code Resolution & Prioritization (`DetailedExitCode`)**:
  - *Bazel*: `src/main/java/com/google/devtools/build/lib/util/DetailedExitCode.java:45-51, 184-210`. Bazel automatically determines the numeric exit code by inspecting the `FailureDetails.metadata.exit_code` custom protobuf option on the subcategory enum value inside `FailureDetail`. Furthermore, `DetailedExitCodeComparator` orders multiple failure details so that infrastructure failures override non-infrastructure failures and detailed exit codes override generic ones.
  - *Zel*: `crates/zel-util/src/lib.rs:72-103`. `DetailedExitCode` is a plain container of `ExitCode` and `Option<FailureDetail>`. It does not extract exit codes from proto metadata, and does not implement failure comparison or prioritization.

* **[PARTIAL] Failure Detail Subcategories Coverage (`failure_details.proto`)**:
  - *Bazel*: `src/main/protobuf/failure_details.proto` (1,444 lines) defines **65+** distinct submessage categories and hundreds of error enum values.
  - *Zel*: `crates/zel-proto/src/lib.rs:200-341` provides typed enum definitions for ONLY **3** categories:
    1. `Interrupted` (`InterruptedCode`: `InterruptedUnknown = 0`, `Interrupted = 4`, `Build = 5`). Note: Bazel defines additional codes such as `DEPREMITTED = 1`, `USER_INTERRUPTED = 2`, `SIGINT = 3`, `SIGTERM = 6`.
    2. `Command` (`CommandCode`: `CommandFailureUnknown = 0`, `CommandNotFound = 1`, `AnotherCommandRunning = 2`, `PreviouslyShutdown = 3`, `StarlarkCpuProfileFileInitializationFailure = 4`).
    3. `GrpcServer` (`GrpcServerCode`: `GrpcServerUnknown = 0`, `NotCompiledIn = 1`, `ServerBindFailure = 2`, `BadCookie = 3`, `NoClientDescription = 4`).
  - All other 62+ categories are untyped fallbacks in Zel (`Category::Submessage { field_number: u32, code: i32 }`), losing all error classification capability. Missing typed categories include:
    - Execution & Spawns: `Spawn`, `Execution`, `LocalExecution`, `RemoteExecution`, `DynamicExecution`, `Sandbox`, `Worker`
    - Caching & Dependencies: `ActionCache`, `RemoteAnalysisCaching`, `ExternalRepository`, `ExternalDeps`
    - Analysis & Skyframe: `Analysis`, `PackageLoading`, `Toolchain`, `StarlarkLoading`, `TargetPatterns`, `Skyfocus`
    - Commands: `Query`, `ActionQuery`, `ConfigurableQuery`, `FetchCommand`, `SyncCommand`, `CleanCommand`, `ConfigCommand`, `DumpCommand`, `HelpCommand`, `ProfileCommand`, `RunCommand`, `TestCommand`, `VersionCommand`, `ModCommand`
    - Actions: `JavaCompile`, `CppCompile`, `CppLink`, `StarlarkAction`, `NinjaAction`, `TestAction`, `LtoAction`, `SymlinkAction`
    - System & Environment: `Crash`, `ClientEnvironment`, `Filesystem`, `SymlinkForest`, `DiffAwareness`

* **[MISSING] Failure Detail Binary Output (`--failure_detail_out`)**:
  - *Bazel*: Supported via `--failure_detail_out=<path>` (`src/main/java/com/google/devtools/build/lib/runtime/BlazeServerStartupOptions.java`). Writes the serialized `FailureDetail` protobuf message to disk when a command fails.
  - *Zel*: Recognized in `StartupOptions` ignored flags list (`crates/zel-options/src/lib.rs:76`), but never parsed into a field or written to disk.


### Protobuf Wire Codecs and Protobuf Coverage

* **[DIFFERENT] Handwritten Wire-Format Primitives vs Code Generation**:
  - *Bazel*: Compiles official `.proto` files using Google's `protoc` compiler, generating strongly typed, tested Java and C++ protobuf classes with complete proto3 conformance, unknown field retention, map handling, packed repeated fields, and schema versioning.
  - *Zel*: `crates/zel-proto/src/lib.rs:32-164` implements handwritten manual byte-level encoders and decoders (`wire::encode_varint`, `wire::decode_varint`, `wire::encode_tag`, `wire::decode_tag`, `wire::encode_bytes`, `wire::skip_field`). This approach lacks support for packed scalar repeated fields, floating-point types (`fixed32`/`fixed64`/`double`/`float`), sint32/sint64 zigzag encoding, and map fields.

* **[PARTIAL] REAPI v2 (`remote_execution.proto`) Coverage**:
  - *Bazel*: Integrates full Remote Execution API v2 (`third_party/remoteapis/build/bazel/remote/execution/v2/remote_execution.proto`, 2,500+ lines), supporting remote execution, Content Addressable Storage (CAS), action cache services, and asset transfers.
  - *Zel*: `crates/zel-proto/src/lib.rs:1224-1483` only defines 6 structs: `Digest`, `CommandEnvironmentVariable`, `Command`, `Action`, `OutputFile`, and `ActionResult`.
  - Significant field and message omissions in Zel:
    - `Action`: Missing `timeout: Duration`, `salt: bytes`, and `platform: Platform`.
    - `Command`: Missing `output_paths: repeated string` (the modern REAPI standard replacing output_files/output_directories), `platform: Platform`, and `output_node_properties`.
    - `OutputFile`: Missing `contents: bytes` (inline output) and `node_properties`.
    - `ActionResult`: Missing `output_directories`, `output_symlinks`, `output_file_symlinks`, `output_directory_symlinks`, `stdout_digest`, `stderr_digest`, and `execution_metadata: ExecutedActionMetadata`.
    - Merkle Tree & CAS: Missing `Directory`, `FileNode`, `DirectoryNode`, `SymlinkNode`, and `Tree`.
    - RPC Services & Payloads: Missing `ExecuteRequest`, `ExecuteResponse`, `WaitExecutionRequest`, `GetActionResultRequest`, `UpdateActionResultRequest`, `FindMissingBlobsRequest/Response`, `BatchUpdateBlobsRequest/Response`, `BatchReadBlobsRequest/Response`, `GetTreeRequest/Response`, `ServerCapabilities`, `CacheCapabilities`, and `ExecutionCapabilities`.

* **[MISSING] Execution Log Proto (`spawn.proto`) & Flags**:
  - *Bazel*: `src/main/protobuf/spawn.proto` (455 lines) defines `SpawnExec`, `SpawnMetrics`, `File`, and `Digest`. These are emitted by `--execution_log_binary_file`, `--execution_log_json_file`, and `--execution_log_compact_file` to record every executed spawn's environment, command line, input file digests, output file digests, and execution timings for debugging cache misses.
  - *Zel*: Completely missing. `spawn.proto` is not implemented in `zel-proto`, and all `--execution_log_*` flags are unsupported.

* **[MISSING] Action Cache Proto (`action_cache.proto`)**:
  - *Bazel*: `src/main/protobuf/action_cache.proto` defines `ActionCacheStatistics` (hit counts, miss counts, miss reasons: `CORRUPTED_CACHE_ENTRY`, `NOT_CACHED`, `UNCONDITIONAL_EXECUTION`, `DIGEST_MISMATCH`, save/load timings). This message is reported in BEP and build summaries.
  - *Zel*: Completely missing. Zel implements an ad-hoc text-based key-value file (`CompactPersistentActionCache`, `crates/zel-exec/src/lib.rs:54-67`) without proto schema or miss diagnostic metrics.

* **[MISSING] Invocation Policy Proto (`invocation_policy.proto`) & Enforcement**:
  - *Bazel*: `src/main/protobuf/invocation_policy.proto` (246 lines) defines `InvocationPolicy`, `FlagPolicy`, `ConfigPolicy`, `SetValue`, `UseDefault`, `DisallowValues`, and `AllowValues`. The `--invocation_policy` flag enforces enterprise policies, preventing or forcing specific command flags across users.
  - *Zel*: While `RunRequest` in `crates/zel-proto/src/lib.rs:587` includes an `invocation_policy: String` field, it is passed as `String::new()` by `zel-client` (`crates/zel-client/src/lib.rs:271`) and completely ignored by `zel-server`. The protobuf schema is absent, and no policy parsing or enforcement exists.

* **[MISSING] Structured Command Line Proto (`command_line.proto`)**:
  - *Bazel*: `src/main/protobuf/command_line.proto` (108 lines) defines `CommandLine`, `CommandLineSection`, `ChunkList`, `OptionList`, and `Option`. Used in BEP to report structured canonical command lines, tool options, and startup options.
  - *Zel*: Completely missing from `zel-proto`.

* **[MISSING] Test Status Proto (`test_status.proto`)**:
  - *Bazel*: `src/main/protobuf/test_status.proto` (117 lines) defines `BlazeTestStatus` (`NO_STATUS`, `PASSED`, `FLAKY`, `TIMEOUT`, `FAILED`, `INCOMPLETE`, `REMOTE_FAILURE`, `FAILED_TO_BUILD`, `BLAZE_HALTED_BEFORE_TESTING`), `FailedTestCasesStatus`, `TestCase`, and `TestResultData`.
  - *Zel*: Completely missing from `zel-proto`. Test execution results in Zel are tracked only by an ad-hoc local struct without test case hierarchy or proto representations.

* **[MISSING] Additional Bazel Protobuf Schemas**:
  - The following 15+ Bazel protobuf definitions from `src/main/protobuf/` are absent from `zel-proto`:
    1. `analysis_v2.proto` (`aquery` action graph output container)
    2. `build.proto` (`query --output=proto` target and rule graph)
    3. `remote_execution_log.proto` (`LogEntry` for gRPC REAPI traffic dumps)
    4. `bazel_output_service.proto` and `bazel_output_service_rev2.proto` (lazy tree output service)
    5. `execution_graph.proto` and `execution_graph_writer.proto`
    6. `memory_pressure.proto` and `crash_debugging.proto`
    7. `crosstool_config.proto` and `xcode_config.proto`
    8. `option_filters.proto` (`OptionEffectTag`, `OptionMetadataTag`)
    9. `extra_actions_base.proto` (ExtraAction execution info)
    10. `file_invalidation_data.proto` (file system cache invalidation tracking)
    11. `strategy_policy.proto` (execution strategy rules)
    12. `project/*.proto` (BuildableUnit, Project enforcement)

* **[PARTIAL] Command Server Proto (`command_server.proto`) Coverage**:
  - *Bazel*: `src/main/protobuf/command_server.proto` defines the gRPC protocol between the Bazel client and server: `Run`, `Cancel`, `UpdateTerminalSize`, `Ping`.
  - *Zel*: `crates/zel-proto/src/lib.rs:344-1055` mirrors message structures (`RunRequest`, `RunResponse`, `CancelRequest`, `CancelResponse`, `TerminalSizeRequest`, `TerminalSizeResponse`, `PingRequest`, `PingResponse`, `ServerInfo`, `InfoResponse`). However:
    - `TerminalSizeRequest` is received by the server but completely ignored (terminal size is not applied to any UI handler).
    - `command_extensions` (field 8/10) are decoded into generic `Any` but never processed.
    - `block_for_lock_timeout_ms` (field 10) is parsed in `RunRequest` but not enforced by lock acquisition in `zel-server`.


### Hashing and Digest Functions

* **[MISSING] Configurable Digest Functions (`--digest_function`)**:
  - *Bazel*: Defined as a server startup option in `src/main/java/com/google/devtools/build/lib/runtime/BlazeServerStartupOptions.java:435`. Bazel's `DigestHashFunction` registry (`src/main/java/com/google/devtools/build/lib/vfs/DigestHashFunction.java:69-77`) supports `SHA-256`, `SHA-1`, `SHA-384`, `SHA-512`, and `BLAKE3`. Configuring `--digest_function` affects how all file status digests, CAS keys, action cache keys, and Skyframe nodes are hashed across the server lifecycle.
  - *Zel*: `--digest_function` is listed in an ignored startup options string array (`crates/zel-options/src/lib.rs:79`). It is never stored in `StartupOptions`, never parsed into an enum, and has zero effect on execution.

* **[DIFFERENT] Hardcoded Cryptographic Hashing Algorithms**:
  - *Bazel*: File and action hashing dynamically delegate to the configured `DigestHashFunction` instance. MD5 is isolated to legacy output base naming.
  - *Zel*: Cryptographic hashing is completely static and inflexible:
    - MD5 (`crates/zel-util/src/lib.rs:170-254`) is hardcoded to hash workspace directory paths for `$OUTPUT_BASE` resolution (`crates/zel-options/src/lib.rs:154`).
    - SHA-256 (`crates/zel-util/src/lib.rs:256-351`) is hardcoded for all `Fingerprint` calculations and CAS blob addressing.
    - No implementation or support exists for SHA-1, SHA-384, SHA-512, or BLAKE3.

* **[PARTIAL] Incremental `Fingerprint` Memory Allocation**:
  - *Bazel*: `src/main/java/com/google/devtools/build/lib/util/Fingerprint.java` streams bytes directly into an active, reusable `MessageDigest` instance, maintaining low memory overhead.
  - *Zel*: `Fingerprint` (`crates/zel-util/src/lib.rs:364-422`) buffers all input bytes into an unbounded in-memory `Vec<u8>` (`self.buffer`). It only computes the SHA-256 digest when `digest_and_reset()` is called (`sha256::digest(&self.buffer)`). For large actions with many input paths and environment variables, this results in significant repeated heap allocations and copy operations.

---

### FFI Bridge and Native Code Comparison

* **[DIFFERENT] Purpose and Nature of `zel-ffi-bridge`**:
  - *Bazel*: Contains no Rust FFI bridge and does not link against Rust crates.
  - *Zel*: `crates/zel-ffi-bridge/src/lib.rs` (122 lines) is an artifact of the Zel hybrid development strategy. It exposes 5 `extern "C"` functions across the C-ABI:
    1. `zel_normalize_path` (`src/lib.rs:29`): Wraps `zel_vfs::PathFragment::create`.
    2. `zel_canonicalize_label` (`src/lib.rs:46`): Wraps `zel_cmdline::Label::parse_canonical`.
    3. `zel_sha256_hex` (`src/lib.rs:64`): Computes hex SHA-256.
    4. `zel_md5_hex` (`src/lib.rs:79`): Computes hex MD5.
    5. `zel_free_string` (`src/lib.rs:94`): Frees `CString` pointers.
    As documented in `crates/zel-ffi-bridge/src/lib.rs:15-17`, this library exists solely to allow an experimental Java `ZelServer_deploy.jar` to incrementally delegate path normalization, label parsing, and hashing into Rust crates.

* **[MISSING] Bazel Native OS & Hardware Integrations (`src/main/native`)**:
  - *Bazel*: Implements native C/C++ JNI subsystems in `src/main/native/` across POSIX, macOS, and Windows:
    - `unix_jni.cc`: Native POSIX system calls for `lstat`, `chmod`, hard links, symlinks, and extended attributes (`getxattr`, `setxattr`).
    - `darwin/fsevents.cc`: Native macOS FSEvents file system monitoring for instant incremental change detection.
    - Hardware telemetry monitors:
      - `system_cpu_speed_monitor_jni.cc`: CPU frequency throttling detection.
      - `system_load_advisory_monitor_jni.cc`: OS scheduler load advisory monitoring.
      - `system_disk_space_monitor_jni.cc`: Disk space exhaustion monitoring.
      - `system_memory_pressure_jni.cc`: Kernel memory pressure signals.
      - `system_thermal_monitor_jni.cc`: Hardware thermal throttling.
      - `sleep_prevention_jni.cc`: Power management assertions (`IOPMAssertionCreateWithName`) to prevent system sleep during builds.
    - Windows native subsystem (`windows/*.cc`): Native directory junctions, short path resolution, Windows process creation with Job Objects, and memory limits.
    - `blake3_jni.cc`: Hardware-accelerated SIMD BLAKE3 implementation.
  - *Zel*: None of these native OS integrations exist in Zel. `zel-ffi-bridge` provides zero hardware monitoring, zero kernel-level sleep prevention, zero FSEvents integration, and zero extended attribute manipulation.

---

### Zel-Bazelisk Divergence Notes

* The `zel-bazelisk/` subdirectory contains an embedded fork of the Zel repo with additional bazelisk logic (`zel-bazelisk/crates/zel-client/src/bazelisk.rs`).
* There are no divergences in `zel-events`, `zel-proto`, `zel-util`, `zel-ffi-bridge`, or `zel-remote` between `crates/` and `zel-bazelisk/crates/`. The event handling, BEP formatting, exit code mappings, protobuf codecs, and profiler implementations are identical copies.

---


# Verification log (Phase 1 re-check, 2026-11-04)

Verified fixed against the real binary: `5|3 == 7`, chained comparison, `%` arg order, `list.append`, dict `.items()`, unknown flag exits 2, `-- //:g` builds the target, `cargo test --workspace` passes.

Still broken / suspicious after Phase 1:
- **[DIFFERENT]** `1 << 70` evaluates to `2` (expected 1180591620717411303424): bigint shift is wrong or truncated in the evaluator.
- **[DIFFERENT]** BUILD `print()` output is emitted twice (package is evaluated twice per build).
- **[DIFFERENT]** `zel build //:nonexistent` still prints "Build completed successfully" and exits 0 (loading phase not yet fixed).
- **[DIFFERENT]** Unknown-flag error lacks the "did you mean" suggestion that the agent claimed.
- Agent reports overstated: Starlark claimed 26 tests (actual 19); lib.rs shrank rather than grew.
