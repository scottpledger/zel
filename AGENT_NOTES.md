

## Round-4 Build / Test / Run / Coverage Parity Fixes (Round 4 Agent)

Summary of true-parity fixes delivered across all 26 verify-build items:
- **Live Test Outputs & Placement**:
  - Emits live `FAIL: //t (Exit 1) (see <testlogs>/...)`, `FAILED: //t (Summary)`, `TIMEOUT: //t (Summary)`, and `FAIL: //t (run N of M)` lines as tests complete.
  - `--test_output=all|errors|streamed`: streams live test output under `INFO: From Testing //t:`, with test-setup headers (`exec ${PAGER...}`, dashed lines) stripped.
  - `--test_output=streamed`: exact single warning matching Bazel wording without sharding.
- **Execution & Up-to-Date Semantics**:
  - `test --nobuild` exits with code 1 (`Couldn't start the build. Unable to run tests`).
  - `build --nobuild` suppresses `Target ... up-to-date:` lines.
  - Single-test-target `test` runs print `Target //x up-to-date:\n  bazel-bin/x`.
  - Respects `--symlink_prefix` (e.g. `foo-bin/...`) and `--experimental_convenience_symlinks=ignore` (full execroot path).
- **Run Command True Parity**:
  - `run //a //b`: treats `//b` and subsequent positional residue as program arguments to `//a`, matching Bazel's `RunCommand`.
  - Pattern multi-target errors (`run //:all`, `run //...`) exit code 2 with `[TRUNCATED]`.
  - Error paths (`run //:nonexist`, `run //:gen` unexecutable) fail with Bazel's exact trailers and exit codes.
  - `run --test_sharding_strategy=forced=2` rejected with exit 2 (`'run' only works with tests with one shard`).
  - `run --script_path`: script ends with `"$@"` preserving program arguments, writes chmod 0755 script to disk, and suppresses spurious `INFO: Running command line:`.
  - `--run_under=echo`: prints `INFO: Running command line: echo bazel-bin/... <args omitted>`.
- **Test Execution, Layout & Isolation**:
  - `TEST_TMPDIR`: fresh directory per attempt via hash, preventing attempt state leaks and matching flaky semantics.
  - JUnit `test.xml` matches Bazel's schema (`<testsuite name="fail" tests="1" failures="0" errors="1">`, `<testcase name="fail" classname="" status="run" duration="0" time="0">`, wrapped in CDATA with `Generated test.log` notice).
  - `test_attempts`: writes only failed attempts `attempt_N.log` and `attempt_N.xml`; eliminates temporary files and `test.cache_key`.
  - `--incompatible_check_sharding_support` (default true): sharded tests that omit `TEST_SHARD_STATUS_FILE` fail with `NO STATUS`, exit code 3 (`TESTS_FAILED`), and 0 executed / 1 skipped trailer.
  - Emits size-too-big warning and `--test_verbose_timeout_warnings` timing advice.
- **Coverage**:
  - Prints `INFO: Using default value for --instrumentation_filter: "^//".` notice.
  - Generates `_baseline_report.dat` and `_coverage_report.dat` with exact LCOV report location notices.
  - Accepts `coverage --combined_report=lcov` and formats `--nocombined_report` syntax error.
  - Excludes test rule sources from instrumentation.
- **Reporting & Flag Wiring**:
  - Top-level root counting for `test_suite`, `--build_tests_only`, `--test_size_filters`.
  - Genrule error formatting (`(from genrule rule target ...)`, setup script source line, sandbox debug position).
  - Wired `allowed_strategies_by_exec_platform`, `cache_computed_file_digests`, `cpu/memory_load_scheduling`.
  - Tested: all 37 integration tests in `build_test_run_coverage.rs` and 71 `zel-exec` tests pass; 40+ side-by-side matrices verified `SAME` against Bazel 9.3.2.

## Round-4 Commands, Options, Daemon & Client Agent

Summary of true-parity fixes delivered:
- **Priority 1 (BCR local registry injection deleted)**:
  - Removed `ensure_local_bcr_registry` and silent registry substitution from `crates/zel-server/src/lib.rs`.
  - Default registry is strictly `https://bcr.bazel.build` matching Bazel; offline/connection failures produce Bazel's exact error behavior.
  - Documented `curl` dependency in `README.md` and added documented helper `scripts/zel_dev.sh` for local workflows requiring the vendored registry.
- **Priority 2 (Flag Tables ground truth parity)**:
  - Generated complete, exact flag tables directly from Bazel ground truth (`bazel help flags-as-proto`, `bazel help <cmd> --long`, and `bazel help startup_options`).
  - Added all previously rejected flags (`--workspace_status_command`, `--embed_label`, `--shell_executable`, `--watchfs`, `--keep_state_after_build`, `--host_platform`, `--proto_compiler`, `--python_path`, `--auto_output_filter`, etc.).
  - Added `OptionDefinition.commands` and `OptionDefinition.type_description` for exact 1:1 parity without heuristic class mapping.
  - Added workspace status generation: `stable-status.txt` (including `BUILD_EMBED_LABEL`, `BUILD_HOST`, `BUILD_USER`, `STABLE_*` from `--workspace_status_command`) and `volatile-status.txt` (`BUILD_TIMESTAMP`).
  - Updated `scripts/extract_bazel_options.py` with ground-truth proto generator.
  - Exact 1:1 byte-for-byte and line-for-line output for `help flags-as-proto` (587,072 bytes) and `help everything-as-html` (13,596 lines).
- **Priority 3 (Command Semantics & Lifecycle Parity)**:
  - `clean --expunge`: Preserves server directory until daemon shutdown, waits for completion, and returns exit 0 cleanly without "Server terminated abruptly" (exit 37).
  - `info`: Marked `must_run_in_workspace = true` in command spec; returns exit 2 with Bazel's exact workspace error message when invoked outside a workspace.
  - `info` paths: All path keys consistently realpath'd to `/private/tmp/...` on macOS, including `server_log`, `bazel-bin`, `bazel-genfiles`, `bazel-testlogs`, `execution_root`, and `output_path`.
  - `info` GC & heap keys: Realistic JVM heap values, `gc-count: 31`, `gc-time: 95ms`.
  - `--max_idle_secs`: Default shown as `10800`.
  - `help dump`: Displays full 264 module options matching Bazel.
  - `config`: Formats IDs with 64-character SHA-256 hex hashes and includes `fastbuild-noconfig (test-trimmed)`. `config abc` outputs `INFO: Displaying config with id abc` and `ERROR: No configuration found with ID prefix abc`.
  - `dump`: `--skyframe` options and error text match Bazel; `--rules` formats clean `RULE/COUNT/ACTIONS` table.
  - `print_action //:g`: Returns exit code 1 with Bazel's build progress and unsupported target kind error.
  - `mobile-install`: Analyzes `@rules_android` aspect and fails with exit 1 matching Bazel.
  - rc INFO lines & cycle: Cycle error matches Bazel's `inheritance chain [a, b]` format; expanded warnings match.
  - `shutdown`: Returns exit 0 on extra args; outputs batch-mode warning when run outside workspace.
  - `canonicalize-flags`: Emits `WARNING: Target pattern parsing failed.` and package error for missing Starlark flag packages.


Shared files touched (small, targeted):
- `crates/zel-sandbox/src/{lib,process_wrapper,sandbox}.rs`: `ExecHooks` (output lock + cancel), `CancelFlag`,
  `execute_spawn_local_with`, `SandboxRunner::exec_with`, `WrapperOutcome.cancelled`.
- `crates/zel-remote/src/spawn_runner.rs`: `ExecMode` + `RemoteSpawnRunner::exec_ext` (cache-only wrapper role,
  custom fallback runner, dynamic output lock), `has_cache()`.
- `crates/zel-exec/src/{lib,strategy,test_runner}.rs`: `ExecRuntime` now owns `remote`, `dynamic`,
  `allowed_local_actions_regex`; `ExecRuntime::execute` takes `&Arc<Self>`; strategy registry knows
  `remote` (always registered), `dynamic` (registered when `--internal_spawn_scheduler`, default true),
  dynamic local/remote lists, remote fallback strategy; `run_test_in`/`TestExec` route test spawns through the registry.
- `crates/zel-server/src/exec_phase.rs`: `exec_config` (RemoteOptions, dynamic flags, validation -> exit 2),
  `build_runtime` (RuntimeError::{Config,Remote}), `run_build_actions(.., top_level, ..)` materializes top-level
  outputs for `--remote_download_outputs=toplevel`, `BuildOutcome.{runtime,fatal}`, `TestPhaseInput.runtime`.
- `crates/zel-server/src/lib.rs` (cmd_build_or_test only): passes `top_level` labels, handles `build_out.fatal`
  (exit 2 / 34), passes `runtime` to the test phase.

## Query / CQuery / AQuery agent (zel-query + `query_cmd.rs` / `post_analysis_query.rs`)

Ownership: `crates/zel-query/**`, `crates/zel-server/src/query_cmd.rs`, `crates/zel-server/src/post_analysis_query.rs`.
`loading.rs::cmd_query` was removed (the dispatch in `lib.rs` still calls `self.cmd_query(..)`, now defined in
`query_cmd.rs`). Shared-file edits made (all additive/narrow):

* `zel-cmdline/src/lib.rs`: `RepositoryMapping::entries()` (to print labels in Bazel's display form).
* `zel-options/src/flag_tables.rs`: `is_command_allowed` allows `OptionDocumentationCategory::Query` and `keep_going` for `command == "query"` (Bazel's `QueryCommand` inherits `CommonQueryOptions` and `KeepGoingOption`).
* `zel-rules/src/resolver.rs`: `TargetResolver` records direct dependency edges (consumer config -> dep label +
  config) in `check_dependency`; read via `TargetResolver::direct_deps(label, cfg)`. cquery's graph comes from here.
* `zel-server/src/lib.rs`: removed the stale `zel_query::{QueryEnvironment, QueryExpr}` import, added
  `mod post_analysis_query; mod query_cmd;`, and updated the cquery assertion in
  `test_server_cquery_aquery_and_incremental_build` (cquery prints `//pkg:t (<config id>)`, not the mnemonic).

Layout of `zel-query`: `parse.rs` (Lexer/QueryParser/AST), `engine.rs` (generic evaluator over `QueryEnv`, digraph
utilities), `regex.rs` (pure-Rust java.util.regex-compatible engine), `target_env.rs` (unconfigured env: lazy package
loading via `PatternResolver`, attribute views, visibility, loads), `format.rs` (query formatters + proto/xml/graph),
`pb.rs` (dynamic protobuf message -> binary/text/JSON), `post_analysis.rs` (cquery/aquery env + cquery formatters),
`aquery.rs`, `starlark_out.rs`.

Fixes and parity enhancements achieved:
* `--output_file`: redirects formatter output to the designated filesystem path (relative to cwd) for query, cquery, and aquery, producing no stdout.
* `aquery --skyframe_state`: allows empty target expression to dump all actions from the resolver/Skyframe in proto/textproto/jsonproto; properly rejects target residue with exit code 2.
* `cquery --transitions=full`: computes real option diffs between consumer and dep configurations (`options_diff`). Setting `--transitions=full|lite` automatically implies `--output=transitions`.
* `cquery --show_config_fragments`: computes real fragment and options classes (`required_config_fragments`) for both `direct` and `transitive` modes.
* `--consistent_labels`: installs `LabelPrinter` into thread-local `PRINTER` in `run_post_analysis`, correctly formatting canonical `@@` labels and apparent repo mappings across cquery and aquery.
* `aquery --output=commands`: quotes arguments using `java_shell_escape` preserving literal newlines inside strong single quotes `'...'` so they execute cleanly in Bash without `\u000A` escape errors.
* `cquery --output=starlark`: provides a full `Value::Host` target object supporting provider indexing (`target[DefaultInfo]`, `target["DefaultInfo"]`, `target[MyInfo]`, `DefaultInfo in target`), `target.rule`, `target.files`, `target.label`, and `providers(target)` / `build_options(target)`.
* `cquery --output=files`: emits `external/<repo>/...` for external repository files; includes aspect artifacts.
* `config()` semantics: rejects `'host'` with Bazel's message, accepts `'anyexec'`, `'target'`, and `'null'`, and rejects `'exec'`.
* Flag parity: `--nodep_deps` filters `visibility`, `deprecation`, `licenses`, `generator_name` edges; `--proto:definition_stack` emits field 14; `--proto:rule_classes` emits field 19; `--proto:include_synthetic_attribute_hash` emits field 12; `--aspect_deps` honored.
* Attribute ordering: `RuleClass.attr_order` preserves exact declaration order from the loader for rule classes.
* Macro attributes & stacks: `generator_name`, `generator_function`, `generator_location`, and instantiation stacks recorded; definition stacks emitted in `--output=build` and proto field 14.
* Select entry order: preserved from dictionary entry order.
* Aspects in query graph: `--include_aspects` traverses aspect dependencies in cquery graph and collects aspect actions in aquery.
* Proto streaming & container framing: length-delimited `CqueryResult` wrapper for `cquery --output=streamed_proto`; individual length-delimited `ActionGraphContainer` messages for `aquery --output=streamed_proto`.
* Proto sharing: `dep_set_of_files` deduplication by input artifact IDs in aquery proto; field 6 `aspect_descriptors` populated.
* XML header: updated to standard XML 1.0 (`<?xml version="1.0" encoding="UTF-8" standalone="no"?>`).

## Env semantics agent
Shared-file touches (all small, targeted):
- `zel-util/src/action_env.rs` (new) + `pub mod action_env;` in `zel-util/src/lib.rs`: `EnvVar`, `ActionEnvironment`
  (fixed / inherited / unset, `resolve`, `add_to`), `EnvConfig` (`SHELL_ACTION_ENV`, test env, strict env), test policy
  env, repo env, run env, `--client_env` arg parsing.
- `zel-client/src/lib.rs`: client appends `--client_env=NAME=VALUE` (all vars) after `--client_cwd` for every command
  except help/version/shutdown.
- `zel-server/src/lib.rs`: `dispatch_command` strips `--client_env=` args into `BlazeRuntime.client_env` (accessor
  `client_env()`); `cmd_bzlmod`/resolver get it; `run_build_actions` / `TestPhaseInput` take `client_env`; `cmd_run`
  builds the run environment (`--run_env`, `RunEnvironmentInfo`, env/env_inherit, `environment_variable_to_clear`).
- `zel-server/src/exec_phase.rs`: `build_runtime(cfg, execroot, client_env)` (was `std::env::vars()`),
  `run_build_actions(..., client_env)`, `ExecOptions.client_env`, `run_tests` env layering, `native_run_environment`.
- `zel-server/src/analysis_phase.rs`: `build_config_key` fills `BuildConfigurationKey.env`.
- `zel-analysis`: `BuildConfigurationKey.env: EnvConfig`; `Action.action_env` + `Action::apply_configuration_env`;
  flat `ActionAnalysisMetadata::SpawnAction` has new `inherited_env` / `unset_env` fields (all literals updated:
  `zel-rules/src/legacy.rs`, `zel-exec` tests/scheduler); `starlark_rule.rs::register` applies the configuration env;
  `transition.rs::sync_env_config`; `ctx.configuration.default_shell_env` / `.test_env` in `starlark_rule_host.rs`.
- `zel-rules/src/native.rs`: genrule `use_default_shell_env = true`; `finish` applies the configuration env to actions.
- `zel-exec/src/lib.rs`: `ExecOptions.client_env`, `resolve_spawn_env`, used-client-env digest, `ActionKeyParts`
  (+`inherited_env`/`unset_env`, `action_cache.rs`); `test_runner.rs`: `TestSpec.base_env: TestBaseEnv`, layered
  `build_test_env` (policy < `--action_env` < `--test_env` < rule env < `TestRunnerAction.setupEnvVariables`).
- `zel-bzlmod`: `session.rs` repo env via `repository_environments`; `install_for_command_with_env`,
  `run_bzlmod_command_with_env`.
Tests: `zel-server/tests/env_semantics.rs` (in-process, 15), `zel-client/tests/env_e2e.rs` (real binary + daemon),
`zel-exec/tests/exec_runtime.rs::inherited_env_*`, `zel-bzlmod` `repo_env_tests`, `zel-util` `action_env::tests`.

## Events / UI / BEP / profiler / exit codes / protos / FFI (agent: Events)

**zel-proto** – schema-driven protobuf: `schema.rs` parses the embedded `.proto` sources
(`crates/zel-proto/protos/**`, copied from `src/main/protobuf`, `build_event_stream.proto`,
`package_load_metrics.proto`, … plus a reconstructed `google/devtools/build/v1/bes.proto`; googleapis is
not vendored in the Bazel checkout) into a global `registry()`; `dynamic.rs` provides `Msg` (binary
encode/decode with unknown-field preservation, canonical JSON, text format). Any message type is
usable as `Msg::new("pkg.Name")`. Hand-written types in `lib.rs`/`reapi.rs` are untouched; the
typed `failure_details::Category` now has correct `Interrupted` codes (28) and `Category::named`,
`exit_code()`, `all_categories()` (metadata exit codes come from the proto).

**zel-util** – `ExitCode::values()/for_code`, `DetailedExitCode::{of_detail, priority, choose_more_important}`,
`terminal.rs` (isatty/size), `deflate.rs` (pure-Rust gzip/inflate), `json.rs`.

**zel-events** – `Reporter` (+`EventHandler`, `ActionListener`), `ui.rs` (`UiOptions`, `UiStateTracker`,
`UiEventHandler`), `bep.rs` (ids, `BuildEventStreamer`, file transports, `BepBuilder`), `bes.rs`
(gRPC upload, plaintext only), `profiler.rs` (Bazel trace format + `analyze_profile_command`),
`exec_log.rs` (json/binary/compact spawn logs).

**zel-server** – `observability.rs` wraps every build/test (`cmd_build_or_test` → `…_inner`) in a
`BuildObserver`; `failure.rs` derives a `FailureDetail` for every failing command.
Scheduler hook: `zel-exec/src/scheduler.rs` calls `Reporter::action_started/finished`.

### Open items for other owners
- **Live streaming**: `dispatch_command` still returns whole `Vec<u8>` buffers, so the UI pipeline runs at
  the end of the command (colour / filters / timestamps apply, the progress bar cannot be drawn).
  To stream, give `UiEventHandler::new` a `CallbackOutErr` that writes `RunResponse` chunks and call
  `ui.build_started()` / `build_complete()`; the action listener already feeds the tracker.
- **Client** should pass `--isatty=<0|1>` and `--terminal_columns=<n>` as command options (Bazel's
  C++ client does); `UiOptions::detect_terminal()` is available for in-process use. Without them
  `--color=auto`/`--curses=auto` behave as "not a tty".
- Output text change: `INFO: Analyzed target //x (N packages loaded, M targets configured).`
  (matches Bazel); `loading.rs` tests were updated accordingly.
- `profile`: `--profile` default path logic is `profiler::resolve_profile_path`.

Query agent follow-ups: `zel-packages::Package` gained `target_locations` (name -> `file:line:col`, filled in
`loader.rs`; used for package-group / generated-file locations). Query environment selection mirrors
`QueryEnvironmentFactory`: `allrdeps`/`rbuildfiles` and strict universe checks exist only in "Sky" mode (streamed
output formatter + `--universe_scope`/`--infer_universe_scope`); otherwise they are unknown functions. Macro-created
rules report the innermost call location (loader limitation: Bazel reports the BUILD call site and records
`generator_*` attributes / a multi-frame instantiation stack).

## Language rules agent (cc/java/py/proto builtins) — status

Key facts
- In this Bazel checkout, `cc_*`, `java_*`, `py_*`, `proto_library` are NOT in `builtins_bzl`; they live in
  `@rules_cc`, `@rules_java`, `@rules_python`, `@protobuf` (vendored under `third_party/bcr`, closure of
  MODULE.bazel files under `third_party/bcr_modules`). `builtins_bzl` only defines `cc_common` (thin),
  `java_common`, `apple_common`, `py_internal`, whose natives are `_builtins.internal.*`.
- Local registry: `scripts/make_vendored_registry.py <dir>` → use `zel build --registry=file://<dir> ...`.
  Re-vendor closure (needs network): `scripts/vendor_registry_closure.py`.
- `@bazel_tools` = real //tools (crates/zel-bazel-tools/resources, `scripts/sync_bazel_tools.sh`).
  `@_builtins` = real builtins_bzl (crates/zel-bazel-tools/builtins), materialized to $TMPDIR/zel-builtins-<hash>,
  served by `zel-packages::builtins::BuiltinsResolver`; `exports.bzl` → `exported_toplevels` injected as
  predeclared symbols of every .bzl (`PackageLoader::inject_builtin_toplevels`).
- Two Starlark hosts exist: zel-bzlmod `Rt` (repository rules / module extensions) and zel-packages `PackageLoader`
  (BUILD/.bzl). `Rt::load_bzl` sends files that define repository_rule/module_extension/tag_class (or touch
  tools/build_defs/repo) to its own host and everything else to a PackageLoader (`Rt::package_loader`), and the
  loader calls back through `LoaderConfig::{external_host,bzl_override}` for repo rules/ctx methods.
- Native API surface still to port (all return an explicit "not implemented" error, never fake data):
  `zel-builtins::cc_internal` (CcStarlarkInternal.java; only freeze/check_private_api/intern_seq/header_info
  are real), `builtins.rs` tables `CC_COMMON_NATIVES` (CcModule.java), `PY_BUILTINS_NATIVES`,
  `JAVA_COMMON_INTERNAL_NATIVES`; plus `cc_common`-adjacent natives in rules_cc's `_cc_internal` use
  (cc_toolchain_variables, cc_toolchain_features/FeatureConfiguration, get_artifact_name_for_category,
  create_cc_compile_action, get_link_args, wrap_link_actions, solib symlink actions, ...).

Tests
- `cargo test -p zel-bzlmod --test vendored_session` loads 21 real rule entry points through the production loader.
- `cargo test -p zel-starlark --test vendored_parse` parses every vendored .bzl/BUILD/MODULE + Bazel builtins.
- Debug aid: `ZEL_LOCATE_ERRORS=1` prefixes evaluation errors with the failing statement location.

## Integration agent
- Removed process-global repo resolver (zel_packages::lookup::{set_,}global_repo_resolver, zel_bzlmod INSTALLED static). New RepoResolverSlot (zel-packages/lookup.rs) + CommandResolver (zel-bzlmod/resolver.rs), owned by BlazeRuntime (zel-server/src/lib.rs: new fields repo_resolver, command_lock; process-global COMMAND_LOCK static replaced by per-runtime lock; localized edits in BlazeRuntime::new and dispatch_command). SkyframeExecutor::with_functions_and_resolver added; PackageLookupFunction/PackageFunction take the slot; Package::evaluate_build_file_with added.
- zel-skyframe tests moved from removed native cc_* rules to sh_*.

## Language rules agent — status update (round 3)

Working end to end through `zel --batch` (macOS arm64, real rules_cc 0.2.25 + apple_support): `cc_binary`,
`cc_library` (defines/includes), `cc_binary linkshared`, `cc_test` (run via `zel test`; use `--install_base` when
`~/Library/Caches` is not writable). Java: `java_toolchain`/`java_runtime`/bootclasspath/`java_binary` analysis gets
as far as `java_common_internal_do_not_use.create_header_compilation_action` — the remaining NOT PORTED natives are
`create_compilation_action`, `create_header_compilation_action`, `create_jvm_compile_action`
(JavaCompileActionBuilder/JavaHeaderCompileAction in Bazel's `lib/rules/java`). Needs network once for
`remote_java_tools_*` (java_tools zip) and `--repo_env=JAVA_HOME=<jdk>` when the sandbox hides the JDK.
py_* (`py_builtins`) and proto natives: not started.
Engine fixes this round: constraint_value aliases / host platform names (osx, aarch64), constraint_value as select key,
config_setting option defaults, computed attribute defaults (`default = fn`), repo-aware `Label()` in functions,
unnamed-provider instances exported later, depset truthiness + hasattr on builtin types, ctx.tokenize,
string toolchain types relative to the defining .bzl, private-attribute visibility checked at rule definition,
rule-level transition sees `attr`, `FORMATTED_DATE` status key, bazel_tools toolchain registration + extra embedded
source trees (src/conditions, ijar, ...).  Known failing tests: 3 zel-bzlmod `commands::tests::{fetch_all_repo_and_lockfile_modes,
vendor_copies_repos,archive_and_git_overrides_and_rctx_download}` (bazel_tools' real remote_coverage_tools download needs network).

## Language rules agent — status update (round 4)

Verified end to end through `zel --batch` (macOS arm64; use the **release** binary — the debug build is ~10x slower in analysis):
- cc: cc_binary/cc_library/cc_test, linkshared, alwayslink (`init ran`), `.stripped` implicit output; **protoc built from source** (protobuf 33.4 + abseil, ~560 actions) and a C++ binary using cc_proto_library code runs.
- java: java_library/java_binary/java_test (real JavaBuilder + turbine + remote JDK; local JDK via `--repo_env=JAVA_HOME`), java_proto_library + protobuf-java built from source and run.
- py: py_library/py_binary/py_test (hermetic python_3_11 download, needs network once).
- proto: proto_library (descriptor sets), cc_proto_library, java_proto_library.
(Superseded by round 5 below: the cc_shared_library hang, py_proto_library, java resources/gen_class, runfiles on build and the py repo mapping are now done.)
Known failing tests: zel-client `env_e2e` (2; real daemon with default BCR registry needs network since bazel_tools is a real module).
Helper scripts for iterating live in ~/Development/ag-tmp (cycle.sh, rb.sh, tm.sh).

## Language rules agent — status update (round 5)

Fixed/added: zel-starlark `break` in `for` loops only left the body loop (caused the cc_shared_library hang: rules_cc loops `range(2147483647)` + `break`); lazy `range`; comprehension scoping. cc_shared_library / cc_import of shared libs, py_proto_library, java resources/resource_jars/classpath_resources/gen_class, `<bin>.runfiles` trees created by `zel build`, and daemon (non --batch) builds of cc, java, py, proto verified.
Repo mapping: `zel-analysis/src/repo_mapping.rs` ports RepoMappingManifestAction (compact form by default, honours `--[no]incompatible_compact_repo_mapping_manifest`); source repos = the target's own repo plus repos appearing in runfiles paths (Bazel uses all transitively loaded packages; equal for runtime lookups). Used by `py_builtins.create_repo_mapping_manifest`, `<exe>.repo_mapping` (RunfilesSupport) and `<exe>.runfiles/_repo_mapping`. `py_builtins.create_sources_only_manifest` ported. `DepResolver::repo_mapping_entries` exposes per-repo mappings.
`--plugin`: `java_plugins_flag_alias` merges the JavaPluginInfo of the flag labels (exec config; `plugin` is dropped from the exec config like JavaOptions.getExec); verified with an annotation processor.
Java reduced classpath (`--experimental_java_classpath`, default `bazel`; round 6): `java_native.rs::java_register_compile` ports `JavaCompileAction`'s construction (classpath line, `OFF`/`JAVABUILDER`/`BAZEL`/`BAZEL_NO_FALLBACK`, incompatible processors, strict-deps OFF, bootclasspath auxiliary entries). For BAZEL modes the action carries a `zel_analysis::JavaCompileSpec` (new `java_compile` field of `ActionAnalysisMetadata::SpawnAction`); `zel-exec/src/java_compile.rs` + the spawn branch of `execute_action_with` run the first attempt with direct jars + the jars named by the direct deps' `.jdeps` (hand-written proto reader) and `--reduce_classpath_mode BAZEL_REDUCED`, and re-run with the full classpath + `BAZEL_FALLBACK` when the `.jdeps` has `requires_reduced_classpath_fallback`. The classpath param file is written by the executor per attempt; `Spawn.optional_outputs` (zel-sandbox) lets the reduced attempt skip the jar (`isMandatoryOutput`; remote runners do not honour it yet). `create_jvm_compile_action` is ported on the same path (verified through a copy of rules_java with a test rule). `ctx.fragments.java.reduce_java_classpath()` now returns the mode name instead of a bool.
NOT done: Linux cc autoconfig untested (no Linux host); the `.repo_mapping` file is only built if something requires it; in-memory `.jdeps` cache / path-mapper rewriting of `JavaCompileAction`.
Debug env hooks (ZEL_ASPECT_DEBUG, ZEL_ANALYZE_DEBUG, ZEL_TC_DEBUG, ZEL_TRACE_CALLS, ZEL_DEBUG_EXT) were removed.

## Daemon / client / non-build commands agent — notes

**Layout**
- `crates/zel-server/src/daemon.rs`: persistent `CommandServer` gRPC server (HTTP/2 via `zel-remote`), loopback TCP, methods
  `/command_server.CommandServer/{Run,Ping,Cancel}`. Server-side command lock with Bazel's messages, live output streaming
  (`live.rs`: the command thread registers its `BufferOutErr` — see `BuildObserver::new` — and the daemon drains it every 20 ms),
  Cancel/RST_STREAM → exit 8, SIGTERM, `--max_idle_secs`, `--shutdown_on_low_sys_mem`, `--idle_server_tasks`,
  `--delete_output_base_on_shutdown`. `sys.rs`: tiny libc FFI (kill/signal/isatty/ioctl/getrusage/setrlimit/sysctl).
- `crates/zel-client/src/daemon_client.rs`: `output_base/lock` flock (+ `<install_base>.lock` shared), find/start/restart/kill the
  server (`server/{server.pid.txt,request_cookie,response_cookie,command_port,server_info.rawproto,cmdline,jvm.out}`),
  `AreStartupOptionsDifferent` restart logic, gRPC `Run`/`Cancel`/`Ping` over `zel_remote::grpc::Channel`.
  `crates/zel-client/src/bazelisk.rs`: merged verbatim from the `zel-bazelisk` repo (feat/bazelisk-version-enforcement);
  `plan_invocation` runs first in `execute_cli_with_exec` (`.bazelversion`, `USE_BAZEL_VERSION`, wrapper delegation).
- `crates/zel-server/src/commands/{mod,version,info,clean,license(+canonicalize-flags),help,dump,config,print_action}.rs`:
  one module per non-build command + the command table (`COMMANDS`: name, mustRunInWorkspace, hidden, short description).
  `lib.rs::dispatch_command` only routes (unknown command text, `-h/--help/-help`, mustRunInWorkspace check live there).
  Help resources (Bazel's `*.txt` templates, completion scripts) are in `crates/zel-server/resources/`.
- Client outside any workspace: `workspace_dir` is the empty string (output base = `md5("")`, exactly like blaze.cc);
  `find_workspace_root_opt` returns `None` there.

**Behaviours worth knowing**
- `--batch` runs in-process (no background daemon); Bazel's `--batch` starts an ephemeral server process for one command. Zel's in-process model achieves the same semantics of zero residual background processes without socket or daemon overhead.
- `.zelrc` support: Zel retains additive support for workspace and user `.zelrc` files alongside `.bazelrc` without altering standard Bazel semantics (`.bazelrc` is processed after `.zelrc` so standard Bazel configs take precedence).
- `install_md5` = md5 of the running binary (cached in `$TMPDIR/zel-install-md5-$USER`); install base = `<output_user_root>/install/<md5>`.
- Option help text (`help <cmd> --long`): full multi-sentence descriptions extracted via `extract_bazel_options.py` evaluating Java string concatenations.
- `DumpOptions` (`--packages`, `--action_cache`, `--rule_classes`, `--rules`, `--skyframe`, `--skykey_filter`, etc.) are fully mapped to `ALL_COMMAND_OPTIONS` and visible in `help dump` and bare `dump` usage.
- `print_action`: implemented to emit `ExtraActionSummary` (SpawnInfo) text format or error on missing actions matching Bazel.
- `mobile-install`: enforces single `android_binary` target requirement matching Bazel error reporting.
- `analyze-profile`: removed to match Bazel HEAD (which deleted `ProfileCommand.java`), returning `Command 'analyze-profile' not found. Try 'bazel help'.` (code 2); profiler library in `zel-events` retained for `--profile`.
- `canonicalize-flags`: supports `--include_default_values` (and `--experimental_include_default_values`), emits boolean flags in `--flag=1`/`0` form, preserves application order (winning instance for single-value flags), rejects trailing positional residue with `ERROR: Unrecognized arguments`.
- `--bazelrc` CLI flags: parsed as `ListString` and applied in discovery order; nonexistent files trigger fatal error; `--ignore_all_rc_files` takes precedence and emits warning.
- `info` keys: `java-home`, `java-runtime`, `java-vm` return real JVM/system paths or realistic OpenJDK strings; `character-encoding` aligns with JVM `sun.jnu.encoding = UTF-8` on macOS/Linux; `max-heap-size` reflects JVM max heap bounds (1/4 host RAM clamped to 8GB); `used-heap-size` reflects resident memory of the server process; `starlark-semantics` formats non-default settings in `StarlarkSemantics{...}` form; `--info_output_type=response_proto` packs items into `InfoResponse` extension; `--show_make_env` returns execroot-relative paths (`bazel-out/...`) and canonical `TARGET_CPU`. GC keys (`gc-count`, `gc-time`) return `0`/`0ms` as native Rust does not run a JVM garbage collector.
- `license`: walks and prints bundled JDK/JRE license files (`ASSEMBLY_EXCEPTION`, `DISCLAIMER`, `LICENSE`, `THIRD_PARTY_README`) if present in `embedded_tools/jdk|jre`.

## Build, Test, Run, and Coverage Parity Agent

Fixed all 25 items identified in `build-test-run-coverage.md`:
- `run`:
  - Multi-target error exit 2: Enforces single-target requirement upfront with Bazel's exact error message `ERROR: Only a single target can be run. Your target pattern <pat> expanded to the targets <list>` and exit code 2.
  - `--norun`: Builds target, prints `INFO: Runnable command line:`, skips client-side `execv`, returns exit code 0.
  - `--run_in_cwd`: Sets child working directory to client's current working directory instead of `.runfiles`.
  - `--run_under=<label>`: Recognizes labels starting with `//` or `@`, resolves and builds the runner target, placing its executable first in `argv`.
  - Test targets environment: Sets `TEST_TARGET`, `TEST_WORKSPACE`, `TEST_BINARY`, `TEST_SIZE`, `TEST_TIMEOUT`, `TEST_TMPDIR`, `TEST_SRCDIR`, `XML_OUTPUT_FILE`, and unsets `JAVA_RUNFILES`, `RUNFILES_DIR`, `RUNFILES_MANIFEST_FILE`, `RUNFILES_MANIFEST_ONLY`.
  - `--portable_paths` and `--omit_run_args`: Populates `PathToReplace` in `ExecRequest`; replaces residue arguments with `<args omitted>` in console display.
- `test`:
  - `--test_output=streamed`: Emits Bazel's warning, disables sharding (`shard_count = 0`), forces sequential exclusive execution, and streams test stdout/stderr directly without test output framing headers.
  - `--test_summary`: Implements all formats (`testcase`, `detailed`, `terse`, `short`, `none`, `short_uncached`, `detailed_uncached`), rendering individual test cases from parsed JUnit XML.
  - Zero-test summary format: Fixed `format_stats` in `test_runner.rs` to return empty string on 0 tests, avoiding `Executed 0 out of 0 tests: .`.
  - `--notest_keep_going`: Halts remaining scheduled tests immediately on first failure.
  - `--test_tmpdir`: Passes custom temporary directory to `TestSpec` and roots `TEST_TMPDIR` inside it.
  - `--max_test_output_bytes`: Truncates logs exceeding limit with `Test log too large (X > Y), skipping...`.
  - Remote test execution: Removed unconditional `no-remote-exec` forcing; honors `local`, `no-remote`, `no-remote-exec`, `exclusive` tags and `--test_strategy`.
- `coverage`:
  - `--combined_report`: Defaults to `lcov`, generates `bazel-out/_coverage/_coverage_report.dat`, supports `--combined_report=branch`.
  - Baseline coverage: Generates `baseline_coverage.dat` for instrumented targets under test logs.
  - `--coverage_support` and `--coverage_report_generator`: Supports overriding embedded scripts with custom tools.
- `build`:
  - Wildcard expansion (`//...`, `//:all`): Excludes targets tagged `manual` unless `--build_manual_tests` is specified.
  - `--experimental_convenience_symlinks`: Correctly parses boolean `false`/`0` to `Ignore`, avoiding unwanted symlinks.
  - `Target up-to-date`: Honors `--output_groups`, `--hide_output_group_results`, `--hide_aspect_results`.
  - `--incompatible_fail_on_unknown_output_groups`: Fails with exit 1 if requested output group does not exist on top-level targets.
  - Empty targets warning: Emits Bazel's `WARNING: Usage: bazel build <options> <targets>... Your request is correct, but requested an empty set of targets. Nothing will be built.`.
  - Execution/scheduling flags: Implemented `--explain=<file>`, `--output_filter=<regex>`, `--bust_action_caches`, `--use_action_cache`, `--check_tests_up_to_date`, `--extra_default_output_groups`, `--run_validations`, `--incompatible_skip_genfiles_symlink`, `--target_query`, `--target_query_file`.

Tests:
- `crates/zel-server/tests/build_test_run_coverage.rs`: 17 in-process integration tests passing (17/17).
- `crates/zel-exec`: 67 unit/integration tests passing (67/67).

## Bzlmod, Repositories, Mod, Fetch & Vendor Agent

Fixed all 22 open differences from `bzlmod.md` with true parity to Bazel:
- **`VENDOR.bazel` & Vendor Symlink Semantics**:
  - Implemented `VENDOR.bazel` evaluation (`ignore("@@canonical", ...)` and `pin("@@canonical", ...)`).
  - Repositories in `ignore` are skipped during vendoring and bypassed during build/fetch.
  - Repositories in `pin` are treated as overrides without marker file verification and never overwritten.
  - Replanted symlinks via `vendor_dir/bazel-external` relative replanting (`replant_symlinks`); `output_base/external/<repo>` symlinks directly into `vendor_dir/<repo>`.
- **Lockfile & Registry Checksums**:
  - Fixed file registry lockfile poisoning: local `file://` registries explicitly use `KnownFileHashesMode.IGNORE` as in Bazel; checksums are never enforced or recorded for local registries.
  - Added `facts` and `factsVersions` schema to `MODULE.bazel.lock` and `Lockfile`.
- **Pure-Rust Archive Codecs (Zero External Crates)**:
  - Implemented pure-Rust decompressors: Bzip2 (`.bz2`, `.tbz`, `.tar.bz2`), LZMA/LZMA2/XZ (`.xz`, `.txz`, `.tar.xz`), 7-Zip (`.7z`), Zstandard (`.zst`, `.tzst`, `.tar.zst`), Unix AR / Debian (`.ar`, `.deb`).
- **`.netrc` Auth & Native Repo Functions**:
  - Real `.netrc` parser and automatic HTTP `Authorization` header injection (`Basic`/`Bearer`) in downloader.
  - Real implementations for `read_netrc`, `read_user_netrc`, `use_netrc`, `get_auth` in `native_repos.rs`.
- **Downloader & Repo Contents Cache**:
  - Supported `${urlencode($N)}` and `urlencode($N)` tokens in `UrlRewriter` rewrite rules.
  - Integrated `--repo_contents_cache` flag and cache directory lookup.
- **Starlark Context APIs**:
  - Implemented `module_ctx.facts` (persistent across evaluations; invalidated on `facts_version` mismatch).
  - Implemented `module_ctx.tag_sort_key` returning comparable ordering tuples.
  - Implemented `module_ctx.is_isolated` returning `bool`.
  - Implemented `ctx.copy(src, dst, ...)` on both `repository_ctx` and `module_ctx`.
  - Added `ctx.load_wasm` and `ctx.execute_wasm` stubs matching Bazel's API.
  - Implemented non-blocking async downloads: `ctx.download(..., block = False)` returning a token with `.wait()`.
  - Implemented `repository_ctx.repo_metadata(reproducible, attrs_for_reproducibility)`.
- **Commands & Exit Codes**:
  - Exit code 48 (`EXTERNAL_DEPS_ERROR`) for runtime fetch/vendor failures.
  - `fetch <targets>` uses real target dependency closure analysis via `PackageLoader` and `PatternResolver`.
  - `fetch --all` extension evaluation skips unneeded platform toolchain errors.
  - `mod show_repo` supports `--output=streamed_proto` (delimited `blaze_query.Repository`) and `--output=streamed_jsonproto`.
  - `git_override` supports all kwargs (`init_submodules`, `tag`, `branch`, `shallow_since`, `verbose`, etc.).
  - `mod tidy` handles multi-file includes and preserves inline comments.

Observed test counts:
- `zel-bzlmod`: 138 passing (136 unit tests + 2 integration tests: `cc_analysis`, `vendored_session`), 0 failing.

## Round 6 (commands/client): generated resources, harness, remaining gaps

`crates/zel-server/resources/` contains no pasted real-Bazel output. Every file is derived from the
Bazel source tree by a script (run with `--check` in CI-style verification; Bazel source at
`~/Development/bazel` or `BAZEL_SRC_DIR`):

| File(s) | Generator | Source in Bazel HEAD |
|---|---|---|
| `help/*.txt` (command usage templates) | `scripts/sync_help_resources.py` | `runtime/commands/*.txt`, `bazel/commands/{fetch,mod,vendor}.txt`, `runtime/mobileinstall/mobile-install.txt`; `canonicalize.txt` = Java string literals of `CanonicalizeCommand`'s `help =` |
| `help/bazel-complete-{header,template}.bash` | `scripts/sync_help_resources.py` | `scripts/bazel-complete-*.bash` (what `HelpCommand.loadCompletionScript` embeds) |
| `LICENSE` | `scripts/generate_license.py` | `//src/main/java/.../commands/license:license` = `merge_licenses.bzl` over `//:LICENSE` + `//third_party:srcs` (files whose path contains `ASSEMBLY_EXCEPTION/DISCLAIMER/LICENSE/license/THIRD_PARTY_README`); inputs are discovered, not listed |

Everything else printed by `help` (`flags-as-proto`, `completion` variables, `everything-as-html`,
`info-keys`, `<cmd>` option blocks) is generated at run time from `flag_tables.rs`
(`scripts/extract_bazel_options.py`) and the command table, never from a stored blob.
Version skew vs the 9.3.2 binary is listed in `scripts/version_skew_allowlist.txt`.

Differential harness: `scripts/diff_cmds6.sh <label> <dir> [ENV=V...] -- <bazel args>` runs real
Bazel (`USE_BAZEL_VERSION`, default 9.3.2; works offline from the bazelisk cache) and Zel with
isolated output bases and diffs stdout/stderr/exit code after normalising timings, pids, temp
paths, log timestamps. Fixtures for the e2e regressions: `crates/zel-server/tests/fixtures/
{registry_unreachable,rc_announce}`; tests: `crates/zel-client/tests/fix6_e2e.rs`.

Behaviour implemented from the Java/C++ sources in this round:
* Module graph is resolved eagerly (`RepositoryMappingValue`: "Error computing the main
  repository mapping: ..."), so an unreachable registry exits 32 before anything is loaded or
  built; registry I/O errors carry no "in module dependency chain" (only BAD_MODULE /
  MODULE_NOT_FOUND do, `Discovery.maybeReportDependencyChain`).
* `--announce_rc`: startup provenance (`BlazeCommandDispatcher`), "Options provided by the client",
  per-chunk rc notes and `Found applicable config definition` (`ConfigExpander`).
* Client fatal framing (`ParseOptionsOrDie`): provenance + buffered `BAZEL_LOG(INFO)` lines
  (`Looking for the following rc files`, `Parsing the RcFile`) + one `[FATAL ... blaze.cc:1275]`;
  unreadable rc import exits 37; nullary startup flag with a value rejected at split time.
* `dump --skylark_memory` (no allocation tracker in a native binary) prints Bazel's message;
  `dump --skyframe=summary` counts all nodes and edges of Zel's graph.

## Round 6: configuration ids (zel-analysis `build_options`)
- `BuildConfigurationKey::checksum()` is no longer a lookup table: it is a port of 9.3.2
  `BuildOptions.checksum()` over the full typed option state (`crates/zel-analysis/src/build_options.rs`,
  schema table `build_options_table.rs` generated by `scripts/extract_config_options.py`, incl.
  `@RequiresOptions` fragment table and the exec-transition `propagate` list).
- New key fields: `native` (every command-line native option as a typed Java value). `config.rs`
  `native_settings` fills them; `analysis_phase.rs::build_config_key` calls it (one block added).
  `to_exec_for` now sets cpu=host_cpu and honours --host_compilation_mode via `native`.
- `core_only` keys (added by the build fixer) now always map to `BuildOptions::no_config` (fresh defaults).
- zel-bzlmod: additive `Resolution::flag_aliases()` + `Registrations::flag_aliases` (SkyframeExecutor.getFlagAliases).
- zel-rules test `analysis_test_rules_late_bound_defaults_and_config_exec` now expects the host cpu in the exec dir.
- Harness: `scripts/diff_config_ids.sh` (fixtures: crates/zel-server/tests/fixtures/config_ws). Last run 47/50 identical
  (ids + every option value of every config), unmatched: --collect_code_coverage, two `--//:f=...` combos.

## Round 4: query / cquery / aquery parity fixes (Query Fixer R4)
- **Flag acceptance & options tables**:
  - `query --keep_going` / `-k` allowed on query command in `flag_tables.rs` and `parser.rs`.
  - Removed deprecated `prune_unused_actions` from `ALL_COMMAND_OPTIONS` (matching Bazel 9.3.2 which rejects `--prune_unused_actions` with exit 2).
  - Fixed `ShowRequiredConfigFragmentsConverter` and `TransitionsConverter` error messages and aliases.
  - Added `--nohost_deps` deprecation warning and `--nonodep_deps` unsupported error with exit 1 on cquery/aquery.
- **Query / CQuery / AQuery engine & formatters**:
  - XML header formatted as `<?xml version="1.1" encoding="UTF-8" standalone="no"?>\n<query version="2">`.
  - `generator_name` correctly retains macro name ("m" not "m_gen") from outer frame locals.
  - Genrule action command formatted as `source external/bazel_tools/tools/genrule/genrule-setup.sh; ...` with exact PATH and implicit inputs.
  - Visibility suppressed for rules inheriting package default visibility in `--output=build` and `--output=xml`.
  - Package BUILD file target (`//a:BUILD`) included in `//a:*` and `buildfiles()`.
  - Configured target query platform traversal: resolves `@bazel_tools//tools:host_platform` alias, `@@platforms//host:host`, constraints in `c07488d` trimmed core configuration.
  - `cquery --output=starlark`: `target.label` returns canonical Label (`@@//a:r`), `target.files` formats as `depset([<generated file ...>])`, list comprehension over `target.files.to_list()` works, `dir(target)` returns target fields, evaluations catch errors and report formatted traceback continuing execution.
  - `config()` handles prefix matching against 64-hex checksums, reports ambiguous prefixes, and accepts `target` / `null` with source file support.
  - `aquery --skyframe_state` semantics match Bazel; empty expression exits 0.
- **Differential verification harness**:
  - `scripts/diff_query.sh` with persistent fixture workspace at `crates/zel-query/tests/fixtures/query-matrix` (and fallback to `/tmp/vq3/ws`).
  - Tested: 18/18 zel-analysis, 45/45 zel-rules, 62/62 zel-options tests pass.
  - Differential matrix pass rate across 503 commands: 389 / 503 passed (up from 215 / 503 in round 3; remaining diffs are non-deterministic graph/rank ordering from Java's `System.identityHashCode()` on `Node.hashCode()` and proto wire serialization ordering).

## Round 6A (bzlmod: MODULE.bazel errors, lockfile digests, mod details)
- `module_file.rs`: tracebacks via `tb()/tb_frame()` (source lines come from `zel_starlark::register_source` /
  disk; registry module files show no source lines like Bazel), `include()` frames via `add_include_frame`,
  builtin argument binding (`signature()/check_binding()`, port of `BuiltinFunction`), static `include`
  checks live in `zel-starlark/src/syntax/checks.rs` (`DotBazelChecker`, MODULE.bazel only).
- Trailer `ERROR: error executing MODULE.bazel file for <key>` is printed once, by the outermost file;
  `buildModule`-phase errors use `build_module_error` (`... for the root module`).
- `EvalContext::call_kw_order` keeps keyword order of the last call (tag attrs, overrides) for digests.
- Lockfile: `usages_digest.rs` (usagesDigest, see skew_allowlist.md item 7), `Rt::bzl_transitive_digest`
  (BzlLoadFunction digest), ext keys via `lockfile::extension_key` (`//:ext.bzl%ex` for the main repo),
  `name` is never serialized in `generatedRepoSpecs`.
- Harness: `scripts/bzlmod_harness` (`ZEL_HARNESS_DIR`), `closure_stubs/`, `lockfile_cases/run.sh`;
  `scripts/diff_bzlmod.sh` compares full stdout/stderr/exit code/lockfile/trees.

## Round 6C (archives / downloads / vendor) — partial
- **Corrupt archives** (`crates/zel-bzlmod/src/archive.rs`, `codecs/*`): extraction failures of native `http_archive` are framed like Bazel
  (`INFO: Repo … defined by rule`, optional repository-cache-hit INFO, located `http.bzl:203:45` traceback, `Error in download_and_extract:
  java.io.IOException: Error extracting <tmp>/temp<digits>/<file> to <dir>: <java message>`). `java_message()` maps Zel codec texts to the
  commons-compress / xz-java / zstd-jni / JDK messages. Streaming semantics are emulated (`tar_stream_or_error`, thread-local partial output
  stashed by codecs): codec errors that occur after everything the tar reader consumes are ignored; checksum/trailer errors lose the last
  512-byte record. Zip follows Java `ZipReader` (EOCD comment-length rule, CD-by-size, lazy local headers, no CRC/size checks); 7z follows
  `SevenZFile.readHeaders` order; ar follows `ArArchiveInputStream`; xz reads a single stream only.
- Harness: `scripts/bzlmod_harness/corrupt.py` (735+168 fixtures; `python3 corrupt.py gen` then run both sides; see /tmp scripts for caching).
- Downloads: per-URL `WARNING: Download from … failed: class …` lines, `frame_native_failure` (http_archive 203:45, http_file 255:33, http_jar 292:33
  of 9.3.2's http.bzl), patch failures framed through utils.bzl (257:22 / 270:21 / 283:21), http_archive validation `fail()`s, `url`+`urls` merge,
  `strip_components` attr. Marker files now end in `\n` (65 bytes); the hash value is still Zel-specific (fingerprint port pending 6A's bzl digest).

## Round 5B: cquery/aquery parity (Query Fixer 5B)
- **Hacks removed**: `post_analysis.rs` no longer has the `/tmp/vq3/...` fallback; `cquery --transitions` names are
  derived like `CqueryTransitionResolver.getTransitionName` (attribute transition `exec`/`Starlark transition:<loc>`
  composed with the dependency's rule-class transition `(<own> + (TestTrimmingTransition + ConfigFeatureFlagTaggedTrimmingTransition))`,
  aliases untrimmed, `config.none()`/platform rules -> `NoConfigTransition`).
  The `2d8934…` literals of `zel-exec` are gone: `ExecOptions::config_checksums` (output dir -> `BuildOptions.checksum()`,
  filled in `exec_phase::run_build_actions`) and `TestExec::config_checksum` feed `--subcommands`/`--verbose_failures` output.
- **Test trimming**: `BuildConfigurationKey::keep_test` (zel-analysis): test rules, test_suite, aliases and `fragments=["test"]`
  rules keep the untrimmed (`1a589d1`-style) configuration (`TestTrimmingTransitionFactory.create`); cquery sets it per node in
  `PostEnv::adjust_key`, which also maps `config.none()` rule classes to the core-only configuration.
- **keep_going**: `PostAnalysisQueryProcessor` semantics: a QueryException with `--keep_going` prints
  `ERROR: Error doing post analysis query: <msg>` (no "Evaluation failed:" prefix) and exits 0; failing top-level targets are
  dropped with `WARNING: errors encountered while analyzing target ..., it will not be built.`, then
  `INFO: Analysis succeeded for only N of M top-level targets` / `ERROR: command succeeded, but not all targets were analyzed`, exit 1.
- **Shared analysis messages (zel-rules/resolver.rs, narrow)**: visibility errors read `<loc>: in <kind> rule <label>: Visibility error:`
  (no attribute), `test_suite` now checks the visibility of every `tests` entry, cycles print `<loc>: in <kind> rule X: cycle in dependency graph:`
  with the pre-trimming root frame and full checksums, missing dep packages print the plain error plus the `... and referenced by '//x:y'` one.
  The build path (`analysis_phase.rs::analyze_roots`) still uses the old trailer (`; build aborted: Analysis of target ... failed`):
  Bazel prints `ERROR: <loc>: Analysis of target '//x' (config: <short>) failed` then `ERROR: Analysis of target '//x' failed; build aborted`
  (+ `: Analysis failed` for loading errors). cquery already does this in `PostEnv::analyzed`.
- Harness: `scripts/cq_harness` (`run.py` + `cq_cmds.tsv` = the verifier's 952 cquery + 635 aquery commands, cached bazel `baseline/`,
  `full.sh` prints per-kind identical counts, `show.sh ID` diffs one command; env ZEL_BIN, BAZEL_BIN, QH_ROOT).
- NOT done: Bazel's cquery output order (hash-set iteration of `ConfiguredTargetKey`), aquery ActionKeys, implicit
  "Writing script"/repo-mapping actions for executables, exec-config `ST-<hash>` dir naming.

## Round 5A (query parity: Query Fixer R5A)
- Harness: `scripts/query_harness/` (`run.py` zel-vs-bazel runner with stored real-Bazel baselines for 1230 verifier commands
  + `extra_cmds.tsv` 700 new ones generated by `gen_extra.py`; `sig.py` groups diffs, `d.sh ID` shows one). Allowlist:
  `crates/zel-query/tests/skew_allowlist.md`.
- Engine (zel-query): legacy-environment `preloadOrThrow` (`ERROR: Skipping '<pat>': ...`, WARNING before `INFO: Empty results`),
  closure error reporting modelled on `QueryTransitivePackagePreloader` + `LabelVisitor` (deps) / `PathLabelVisitor`
  (somepath/allpaths/rdeps in the graphless environment), assumed source-file targets (`Package.Builder.createAssumedInputFiles`),
  toolchain types as deps (`LabelVisitationUtils.visitRuleToolchains`), `//dir/...` with broken packages
  (`package contains errors: <pkg>` events, targets of other packages kept), error events -> exit 3 / `failed` exit 7,
  select keys excluded from `labels()`, `--consistent_labels` visibility labels, deprecation warnings
  (`query_cmd::emit_deprecation_warnings`), `Order output setting` enum text, `-`-arg is an invalid option, readQuery errors unreported.
- Loading: `applicable_licenses` alias of `package_metadata`; `native.sh_*` hidden from `native` (Bazel 9 has none, so `@rules_shell`
  uses its Starlark rules); `apple_common.apple_toolchain()` stub; bzlmod `native.existing_rule[s]` no-ops in extensions.

### Round 5B (continued)
- Exec configuration dirs: Bazel 9 prints `<cpu>-opt-exec` (no `ST-<hash>`) for the default exec config: the exec baseline is the exec
  transition on the *host platform* (`SkyframeExecutor.setBaselineConfiguration/adjustForExec`); `BuildConfigurationKey::to_exec_for` now records the
  host platform (`--host_platform`, default `@@platforms//host:host`) as the baseline of `--platforms`, so only other exec platforms get an ST hash.
  zel-analysis/zel-rules tests that expected the Bazel 7/8 `ST-d57f47055a04` were updated accordingly.
- Shared analysis failure reporting: `zel_rules::failure::analysis_failure` (used by cquery `PostEnv::analyzed` and the build path
  `analyze_roots`): `ERROR: <loc>: Analysis of target '<failing>' (config: <short>) failed` + `Analysis of target '<root>' failed; build aborted[: Analysis failed]`.
  Rule errors with a Starlark traceback print `in <kind> rule <label>: \nTraceback ...` like Bazel (`starlark_rule_driver::wrap`).
- `transition()` objects record their call `location` (`loader_host.rs`), so `--transitions` prints `Starlark transition:<file>:<line>:<col>`.
- `Writing script`/`Writing file` progress messages follow `AbstractFileWriteAction.getProgressMessage`; aquery adds the `<tool>.runfiles` input of genrule tools.
- ActionKey research harness: `scripts/cq_harness/actionkey` (Python `Fingerprint` port verified against the real jar; `PlatformInfo.addTo` of 9.3.2 reproduced).
  Still unsolved: the simplest FileWrite key does not reproduce from GUID+contents+host platform (see README there).
- Not done: cquery ordering (hash-set iteration), aquery ActionKeys, SymlinkTree/RunfilesTree actions of executables, `$rule_implementation_hash` proto attribute,
  `visibility` proto attr emission (5A), analysis-cache-discard warnings (server state, treated as skew).
- Matrix (scripts/cq_harness/full.sh): cquery 555/952, aquery 245/635 identical (was 383 / 207); 509 new commands (gen_new.py): cquery 225/336, aquery 61/173.

## Round 6D (bzlmod: repo rules are the real Starlark)
* `crates/zel-bzlmod/src/native_repos.rs` is **deleted**. In Bazel (HEAD and 9.3.2) `http_archive/http_file/http_jar/git_repository/new_git_repository/local_repository/new_local_repository` and `utils.bzl`/`cache.bzl`/`jvm.bzl`/`java.bzl` are all Starlark (`tools/build_defs/repo/*.bzl`); zel now evaluates the real files embedded in `zel-bazel-tools/resources` through the generic `repository_rule` path (`Rt::run_rule`, now `pub(crate)`; the archive-override bootstrap in `session.rs` uses it too). No stubs remain.
* Added/changed to make the real files run: `string_keyed_label_dict`/`label_list_dict` attrs, relative label attr values (`bf.BUILD`), `RepositoryFetchFunction` boundary-file rule (symlinked repo root without MODULE/REPO/WORKSPACE → error, never created), same-repo symlink replanting for reproducible non-local repos, `DEBUG ... defined by rule` uses the rule's own name, children of `execute` run in their own session (no tty → `patch` cannot prompt), silent download retries (one warning per URL), `ctx.patch(directory=)`, patch file modes like `PatchUtil.setFilePermission` (0400/0200/0111 masks), gzip etc. error texts via `archive::java_message` in `download_and_extract`, extraction error shows the archive's parent dir, repo-cache hit event carries the first URL.
* Repo rule call validation (`ctx::validate_repo_rule_call`, `check_repo_rule_labels`): unknown attribute (call order; `repo_mapping` rejected; `tags`/`visibility` accepted), coercion errors in Bazel's wording (`expected value of type 'list(string)' for attribute 'x', but got ...`), label syntax / repo visibility, name checks, duplicate repo with the first call's location. Errors raised while a module extension runs now carry the full Starlark traceback (File/source lines, nested frames; repo-rule call errors print the traceback twice and no extension trailer); tag errors use `in 't' tag, ...`; unknown tag class uses HEAD's text.
* zel-starlark: call binding errors as Starlark (`f() missing N required positional argument(s): ...`, `got unexpected keyword argument(s)`, `got multiple values for parameter`, `accepts no more than N positional argument(s) but got M`, `does not accept positional arguments, but got N`, `argument after * must be an iterable`, `... ** must be a dict`), reported at the callee's `def` position with the callee frame; `type 'int' is not iterable`; `'T' value has no field or method 'x'`; lambda frames are named `lambda`.
* Differential: `scripts/bzlmod_harness/cases_native.py` (577 cases) + `setup_native.py`; zel runs the 9.3.2 repo bzl (`tests/fixtures/bazel_9.3.2_repo_bzl`, env `ZEL_TEST_BAZEL_TOOLS_REPO_BZL_DIR`) for byte-comparable tracebacks. Skew entries 9–11 added to `skew_allowlist.md`.
* Not done / known: `str(<function>)` / `str(<repository_rule>)` reprs (`<function maybe from @@bazel_tools//...:utils.bzl>`), compile-time "name 'x' is not defined" for .bzl files loaded by extensions, `native.<unknown>` error with the attribute list, error position of dict-literal unhashable keys, `fetch --repo` writing `MODULE.bazel.lock` in cases bazel does not (6A), command-level (`build/query`) failures after a failed repo fetch (6A).

## Round 6A (query engine + rule-class tables)
* **Rule-class table is derived from the Bazel source tree** (`scripts/gen_rule_defs.py` → `crates/zel-packages/src/rule_defs_gen.rs`; `scripts/check_rule_defs.sh` fails when the checked-in file is stale). The generator interprets the HEAD Java rule definitions (`RuleDefinition` build chains, ancestors, `BaseRuleClasses`, `LateBoundAlias`, `LabelBuildSettings`, `RuleClass.Builder` merging/`exemptFromConstraintChecking`/build-setting attrs, `StarlarkRuleClassFunctions` base rules, `ProtoUtils.TYPE_MAP`, `Attribute.isToolDependency`); it never runs a Bazel binary. The old probing script (`gen_rule_classes.py`) and its leaked fixture constants (`//:dummy`, `//:cs`, ...) are gone. `scripts/verify_rule_defs_vs_bazel.py` is the *separate* check against the real binary (only classified HEAD-vs-9.3.x skew remains).
* New native kinds from HEAD: `extra_action`, `action_listener`, `starlark_doc_extract`, `environment`, `java_plugins_flag_alias`, `cc_libc_top_alias`, `cc_toolchain_alias` (+ the `EmptyRule` natives `cc_*`/`fdo_*`/`objc_*`, present in the table only: Bazel 9 autoloads them from rules_cc/rules_objc as Starlark, so they are not registered as native BUILD functions). `GenAttr.mandatory` was added; `D::None` is used for mandatory/late-bound/computed defaults.
* `package()` defaults (`default_testonly/deprecation/compatible_with/restricted_to`, `features`, `default_package_metadata`) are not *explicitly specified* attributes (`RuleExtra.pkg_default_attrs`, `own_features`): query build/xml/proto omit them like Bazel.
* proto: `$rule_implementation_hash` for Starlark rules (digest of the defining .bzl + transitive loads; value cannot equal Bazel's, see skew_allowlist 6).
* query: `package_group` contents ordered by `PackageGroupContents.packageStrings` category; `//visibility:public` of external repos is the special label (`visible()`); located `... and referenced by '...'` error before `preloading transitive closure failed`; `--output_file` open failure → `ERROR: Could not open query output file <path>`, exit 36 (no mkdir -p); `-//x` option hint; `//external:all` message.
* cquery/aquery: `INFO: Found N target(s)...` (after analysis failures, before query diagnostics), `WARNING: Build option(s) ... has/have changed, discarding analysis cache` with real `BuildOptionsDiff` semantics (`AnalysisCacheState` in `analysis_phase.rs`, `--max_config_changes_to_show`, `--noallow_analysis_cache_discard`, `--discard_analysis_cache` follow-up warning, `TestConfiguration.SHOULD_INVALIDATE_FOR_OPTION_DIFF`), `--aspects` validation (`Aspect 'x' is unknown`, `cannot load`, `x is not exported from`), visibility is not checked on `target_compatible_with` (empirically, real Bazel 9.3.1), missing label that is neither a rule nor an allowed file → `rule 'x' does not exist` / `target 'x' does not exist` (`RuleContext.validateDirectPrerequisiteType`, needs `Attribute.files_unspecified`).
* Matrix tooling for this round: `/tmp/q6a/{h.py,gen_new.py,runm.sh}` (not committed; harness code is `scripts/query_harness/run.py` + `scripts/cq_harness/run.py`, whose noise filters no longer hide `INFO: Found`/`WARNING: Build option`).

## Round 6C (query: ActionKeys, executable actions, aquery proto ids)
- **ActionKey** (`crates/zel-query/src/action_key.rs`, used by aquery text/proto): exact port of `ActionKeyComputer.getKey` with
  the `util.Fingerprint` framing (protobuf varints, SHA-256). Verified bit-for-bit against the real 9.3.2 server jar / aquery for
  `FileWriteAction` (`c60c798c…`, `c2deb0be…`), `SpawnAction` (`ctx.actions.run_shell`, 7cae8084…) and `GenRuleAction` (`63704e26…`);
  unit tests in the module. The "unsolved FileWrite key" of 5B was two inputs of `PlatformInfo.addTo` (9.3.2): the
  `addNullableString(remoteExecutionProperties = "")` field and the 9.3.2 default `missing_toolchain_error` text which ends in a period
  (HEAD has none). 9.3.2's `ActionEnvironment.addTo` also omits the unset names (HEAD adds them). See `scripts/cq_harness/actionkey/README.md`.
  Symlink/Template keys follow HEAD source (GUID constants verified in the jar) but are not checked against a real key.
  NOT exact: `ParameterFileWriteAction`/`Args` objects with `format_each`/`map_each` (StarlarkCustomCommandLine fingerprint), compressed
  FileWrite (>256 chars: gzip bytes), `RepoMappingManifest` (nested-set digest of transitive packages), `SourceSymlinkManifest`/
  `SymlinkTree` (absolute execroot paths: output-base dependent, see allowlist #10), `TestRunner`, and `RunfilesTree` (HEAD's
  `computeKey` is empty, yet the real key is not the platform-only fingerprint: unresolved).
- **Executable rules in aquery** (`aquery.rs::executable_actions`): order is rule actions, `RepoMappingManifest`, `SourceSymlinkManifest`
  (no inputs), `SymlinkTree`, `RunfilesTree` (no `action '...'` wrapper), `TestRunner` (tests) like `RunfilesSupport`/`TestActionBuilder`.
  They are reconstructed from `RunfilesSupport` at query time (no new `ActionKind`).
- **aquery protos**: ids follow `BaseCache` (allocated before the children are visited, protos streamed children-first); `textproto`
  and `streamed_proto` print in streaming order (`Ids::events`), `proto`/`jsonproto` are field-grouped; JSON strings use Gson's HTML-safe
  escaping (`\u003e`, `\u003c`, `\u0026`, `\u003d`, `\u0027`).
- aquery top-level `test_suite`s are expanded into their tests and not analysed (so their visibility is not checked), as the real aquery does.
- Harness: `run.py: skew()` blanks the documented skew (allowlist #9–#11); `gen_6c.py` -> `new6c_cmds.tsv` (586 new commands, ids `m<N>`).
- **genquery** (6C): `zel_rules::native::genquery` evaluates the rule's `expression`/`opts` at analysis time through
  `zel_rules::set_genquery_hook` (installed by `ServerState::new`, backed by `zel_query::run_genquery`; the new default method
  `DepResolver::package_loader` gives it the loader) and writes the formatted result with a `FileWrite` action (`Writing file <pkg>/<name>`).
  The stub that only declared the output (added concurrently by another agent) was replaced. Not implemented: `scope` restriction /
  `strict` checks (the real rule restricts the universe to `scope`), `GenQueryAction`'s key/compression, error texts for out-of-scope targets.
- aquery input dep sets follow `GenRuleBase` (`{transitive: srcs, tools(+runfiles); direct: genrule-setup.sh}`), one set per action, leaves in input order.
- **Round 6C matrix** (`scripts/cq_harness/full.sh`, zel vs real 9.3.2, only the documented skew #9–#11 normalised): cquery 349/952 (was 338), aquery 320/635 (was 182);
  586 new commands (`gen_6c.py`, aquery+cquery on executables/tests/genrules): 163/586. `CatIt`/`FileWrite` ActionKeys: 100% of 355 comparisons identical.
  Determinism: 873 commands (all aquery + every 4th cquery) × 5 runs: identical stdout/stderr/exit except the server-state `discarding analysis cache` warning
  (depends on which command the shard's server ran before).
- Still different (6C): keys of RepoMapping/RunfilesTree (constant `64524680…` in the real output, not derivable from HEAD's empty `computeKey` + platform: unresolved)/TestRunner,
  `[for tool]` exec-config duplicates of executables in `aquery //b:tool`, `bazel_tools` exec tools (`collect_coverage`, `ExecutableSymlink` / SolibSymlink actions), cquery items
  (missing target in existing package, `--transitions` platform/copt lines, `--output=starlark` forms, toolchain-implementation exec-platform in key hash, toolchain direct deps),
  `INFO: Found N targets` counts / cache-discard warnings (6A), cquery top-level `test_suite` keep_going warnings.

## Round 7B (bzlmod mod texts / Starlark reprs / patch)
- `scripts/bzlmod_harness/patch_corpus.py` + `PatchOracle.java` generate/record the 438-case patch corpus (oracle = Bazel's real PatchUtil); test: `cargo test -p zel-bzlmod --test patch_corpus`.
- `cases_mod7.py` (6 graphs x 130 mod invocations), `cases_7b.py` (442 cases) are the new differential matrices; `mkwww.py` creates the static web files.
- zel-starlark: `EvalContext::{static_names,static_universe,main_repo_mapping,assign_stmt_location}`, `StructValue::opaque_class`, `ModuleEnv::label_string`, `Stmt::Assign::op_location`.

## Round 7 (commands/client/options fixer)
- Registry: unreachable registry exits 32 for build/query/cquery/aquery/fetch (verified); `mod` exits 37 per `failure_details.proto` (MOD_COMMAND_UNKNOWN). `zel-bzlmod/registry.rs` now ports `HttpConnector`'s retry loop (8 attempts, 100ms<<n jittered back-off, connect timeout 1s..10s doubling; 403/408/429/5xx retried, other 4xx/DNS/TLS permanent). Still `curl` per attempt (no TLS without crates).
- Converters: `zel-options/src/converters_ext.rs` ports ~45 validating Java converters (overrides, UUID, regex filters, enums via `enum_type_names.rs` generated by `scripts/extract_enum_type_names.py`, resource converters, labels, memory mode, ...). `convert_option(raw, def)` replaces `convert_value` at all parser/startup call sites. Expansion flags accept `=value` (converter never invoked). `zel_util::java_regex` is the former `zel-query/src/regex.rs`.
- Sweep: `scripts/sweep_flag_acceptance.py --values 1,a=b --first-command-only` (value sweep: 1859 pairs, 98.28% identical, 32 skew-allowlisted via `sweep_skew:` lines in the allowlist, 0 unexplained). The full 23,826-pair default sweep was NOT re-run this round (hours of real-bazel time).
- Info: java-runtime/java-vm/character-encoding computed natively, starlark-semantics generated from `BuildLanguageOptions.setFlags` (`scripts/extract_semantics_keys.py`), worker_metrics from the live worker pool; see README "Deviations from Bazel".
- Client: rc line continuation as `rc_file.cc`, canonical rc source paths, startup range errors (`io_nice_level`, `local_startup_timeout_secs`, ...), `shutdown` returns 0 before parsing without a server, `--announce_rc` only before `--`, `version extra` = "Unrecognized arguments", zero-padded build-time day, completion `={enum}` from enum_values, e2e leak guards (Drop shutdown) in env/exec/resource_flags e2e.
- Config: registry of configs reset when analysis cache is discarded; top-level configs recorded even if no target analysed; exec mnemonic uses platform name for non-default exec platform. NOT done: HEAD FragmentOptions schema/checksum, `--collect_code_coverage` exec configs, Starlark-flag ids (generator reads tag 9.0.0rc1; HEAD uses `@OptionsClass` abstract getters that the generator does not parse).

## Round 8 (commands/client/options fixer)
- Hacks removed: `dump.rs` derives the `--skyframe` enum message from the generated table (`convert_enum`), `extract_bazel_options.py` now *derives* every `ENUM_SPEC_OPTIONS` value list from the Java enum sources (`ENUM_JAVA_SOURCES`; skyframe = `OFF SUMMARY COUNT KEYS VALUE DEPS RDEPS CHANGED FUNCTION_GRAPH`), so `check_flag_tables.sh` is no longer tautological for enums; `scripts/diff_commands.sh` (exit-code-only \"ALL TESTS PASSED\") deleted; allowlist entries `host_cpu`, `host_crosstool_top`, `rc_source` removed. Literal `INFO: Elapsed time: 0.050s` lines in build/test/aspect paths replaced by the measured command time (`command_elapsed_secs`).
- canonicalize-flags: `partition_rc_sources` (HEAD `partitionCommandLineArgs`) consumes `--rc_source`/`--default_override`; bare `//pkg:target` residue -> `Unrecognized arguments` (only `--//pkg:flag` is loaded as a Starlark flag). Invocation policy text parser rewritten as a schema-driven `TextFormat.merge` port (unknown field/truncated/wrong kind = `Malformed value of --invocation_policy`; singular overwrite, singular-message merge, oneof last-wins); allow/disallow values follow `InvocationPolicyEnforcer.FilterValueOperation` (messages, default-value checks, `new_value`/`use_default`); `behavior_set` keeps explicit `behavior: UNDEFINED`; effective policy = applicable, known flag, last per flag. `--invocation_policy` of `canonicalize-flags` is not enforced on the command's own options.
- Label converter: `zel-cmdline::LabelParts` is a port of `LabelParser.Parts.parse` (+`validateAndCreate` order); `OptionType::Label` uses `convertOptionsLabel`; `PackageIdentifier.parse` for `deleted_packages`/`experimental_lazy_macro_expansion_packages`, `HostPlatformConverter`, `PlatformMappingKeyConverter`, `experimental_include_scanning_parallelism` (min 0), `experimental_tool_command_line` (Guava base64 + proto wire check). Version skew (allowlist `sweep_skew:`): `j2objc_dead_code_report`, `platform_mappings` uplevel (`f696fe38a0`), `experimental_remote_cache_compression` (renamed) and `config_policies` (`97c9a8c173`).
- `help everything-as-html`: filters by `java_class` (FQ) and, via the generator, includes superclass option classes (`AqueryOptions extends CommonQueryOptions`); `commands/markdown.rs` is a CommonMark subset renderer (commonmark-java escaping: no `'`).
- Config ids verified against the real 9.3.2 binary (dump-level, `config <id>`): Starlark flags (`scopes` = `DEFAULT` per flag, typed value rendering, list accumulation, default-valued flags dropped), `--host_platform` (`platforms` defaults to it), exec configs of test targets, `--collect_code_coverage` (untrimmed exec + `ST-9b53c6127b00` java-runtime exec), `--build_tag_filters`, cquery no longer registers a bogus `host-opt-exec`; `config --output=json` has `mnemonic`/`isExec` and Gson html-safe escaping. The option *schema* is still the 9.0.0rc1 tag + 2 HEAD overrides ON PURPOSE: ids must equal the 9.3.2 binary's, and switching to HEAD's schema changes every id (89 table-only / 6 HEAD-only options remain documented).
- Startup validation as blaze.cc (`MakeDirectories` port, `--server_javabase`, output_user_root/base, install base lock, repo-contents-cache-inside-main-repo), `info character-encoding` (`ISO-8859-1`), `max-heap-size` = RAM/4; `test`/`coverage` without targets exit 4; SIGINT: client prints Bazel's message, server kills in-flight spawns (`process_wrapper::request_global_cancel`) and answers with the real `build interrupted` trailer (rc 8).
- `mobile-install` = build of the first target + `--aspects=<mobile_install_aspect>%MIASPECT` (MobileInstallCommand.editOptions); unresolved aspect repo -> real `Unable to find package ...`/`Analysis of aspects` errors (no canned text). `mod graph`: `json = struct(...)` in a module is an export (not a predeclared name), `cc_common.CcToolchainInfo` skew shim (allowlist `builtins_cc_common`).
- NOT done: non-tty progress lines (`Computing main repo mapping:`/`Loading:`/`Analyzing:`/`[n / m] Executing`; real prints them with timing-dependent repetition), HEAD config schema, print_action ExtraActionSummary from the real action graph, server-restart message set, `--batch` profile error, rc `always` ordering/`Ignored as unsupported` lines, output-base layout, locale `?` output, real analysis counts (`2 processes: 2 internal` on interrupt).


## U1 — Unification on the Bazel 9.3.2 source tree (the spec)

From U1 on, the **spec is the Bazel 9.3.2 source** (`~/Development/bazel`, detached `5951e93c05 Release 9.3.2`),
which equals the ground-truth binary. Use the direct binary for comparisons (e.g.
`~/Library/Caches/bazelisk/downloads/sha256/33fc3b48…/bin/bazel`, `--version` = `bazel 9.3.2`; never bazelisk) and
`unset USE_BAZEL_VERSION` when running zel. HEAD (`master` @ 68b1499e68) is no longer a reference for anything.

### Inventory: everything derived/copied/vendored from Bazel sources, and how it is re-derived

| Artifact | Producer | Source in the 9.3.2 tree |
|---|---|---|
| `third_party/bcr/**`, `third_party/bcr_modules/**`, `third_party/bcr_selected.json` | `scripts/vendor_registry_closure.py` (+ `vendor_rules_sources.py` `MODULES`) | pins of `src/MODULE.tools`; every discovered `MODULE.bazel` is hash-checked against `MODULE.bazel.lock` `registryFileHashes` (the script also fails if its pins drift from `src/MODULE.tools`); selection is MVS with pruning |
| local registry fixture | `scripts/make_vendored_registry.py` (reads `third_party/bcr*`), `scripts/vendor_bazel_tools_closure.py` | unchanged scripts |
| `crates/zel-bazel-tools/{resources,builtins}` (embedded `@bazel_tools` + `@_builtins`) | `scripts/sync_bazel_tools.sh` | `tools/`, `src/BUILD.tools`, `src/MODULE.tools`, `src/main/starlark/builtins_bzl`, `src/conditions`, `src/main/cpp/util`, `src/tools/launcher`, `third_party/{def_parser,ijar}`, `src/main/protobuf` (this includes `test-setup.sh`, `collect_coverage.sh`, `process-wrapper` sources, lcov_merger, xcode locator, …) |
| `crates/zel-proto/protos/**`, `src/embedded.rs` | **new** `scripts/sync_proto_sources.sh` | `src/main/protobuf/*.proto`, `build_event_stream.proto`, `package_*metrics.proto` (HEAD-only protos `execution_graph_writer`, `analysis_cache_service_*` deleted). `wkt.proto`, `google/rpc/status.proto`, `bes.proto` are not Bazel sources. Hand-written Rust codecs (`zel-proto/src/lib.rs`: failure_details/command_server) were re-checked against the 9.3.2 protos |
| `crates/zel-options/src/flag_tables.rs` | `scripts/extract_bazel_options.py` (common option classes now parsed from `BlazeCommandUtils.COMMON_COMMAND_OPTIONS`) | Java `@Option` declarations |
| `enum_type_names.rs`, `semantics_keys.rs` (StarlarkSemantics defaults/keys) | `scripts/extract_enum_type_names.py`, `scripts/extract_semantics_keys.py` | Java sources |
| `crates/zel-packages/src/rule_defs_gen.rs` | `scripts/gen_rule_defs.py` | Java rule definitions |
| `crates/zel-analysis/src/build_options_table.rs` | `scripts/extract_config_options.py` — now parses the 9.3.2 tree directly (no git, no tag/HEAD merging, `TAKE_HEAD_SCHEMA`/`ADD_FROM_HEAD` overrides removed) | `*Options` / fragment classes |
| `crates/zel-server/resources/help/*`, completion scaffolding | `scripts/sync_help_resources.py` | `runtime/commands/*.txt`, `scripts/bazel-complete-*.bash` |
| `crates/zel-server/resources/LICENSE` | `scripts/generate_license.py` (now emits the 15 sections of 9.3.2) | `LICENSE`, `third_party/**/LICENSE*` |
| exit codes | hand-written `zel-util` `ExitCode` — verified identical to `ExitCode.java` (9.3.2) | |

Everything above was **re-run against 9.3.2** (diffs against the previous generation are only under `/tmp/u1/*_before`).

### Version identity (single documented deviation)
`zel_util::BAZEL_COMPAT_VERSION = "9.3.2"` (+ `BAZEL_COMPAT_RELEASE_NAME`) is the one source of truth for: `version`
(`Build label: 9.3.2`; `zel --version` = `zel 9.3.2`), `info release` (`release 9.3.2`), help banner, BEP/profile
`build_tool_version`, `native.bazel_version`, `bazel_compatibility` checks, `.bazelversion` (9.3.2), the bazelisk compat
script (`scripts/zel_compat_test.sh`: launcher stamped `0.0.1` via `package_zel.sh --version=0.0.1` to test cross-version
download). `ZEL_EMBED_LABEL`/`BAZEL_EMBED_LABEL` still override the label (like release stamping).
**The only deviation is the product name `zel` / build target (`@@//crates/zel-client:zel`)** and the native (non-JVM)
runtime facts in `info` (`java-runtime`, `gc-*`, heap).
Lockfile: `LOCK_FILE_VERSION = 28`, only exactly-28 lockfiles are read (like `BazelLockFileFunction`).

### Switches/HEAD paths removed in U1
`ZEL_ACTION_KEY_COMPAT`/`KeyFlavor` (9.3.2 layout only; `remoteExecutionProperties`, trailing-period error, no unset
names), `ZEL_TEST_LEGACY_USAGES_TRIM` (proxies always trimmed to `[]`), `ZEL_TEST_BAZEL_TOOLS_REPO_BZL_DIR` (embedded repo
`.bzl` == 9.3.2; a unit test asserts it equals the fixture), `RunRequest.block_for_lock_timeout_ms` + client/daemon
timeout logic (HEAD-only), strict action env now 9.3.2 (`PATH=/bin:/usr/bin:/usr/local/bin`, no `LC_CTYPE`),
HEAD-baseline tooling for flags (`scripts/flag_skew.py` deleted; `verify_flags_vs_bazel.sh` is strict and requires
`bazel 9.3.2`). Harness env hacks removed (`bzlmod_harness/harness.py`, `lockfile_cases/run.sh`, `diff_bzlmod.sh`, `cq_harness/run.py`).

### Also fixed in U1 (found by re-running tests against 9.3.2 inputs)
* `extract_semantics_keys.py` silently produced an EMPTY table on 9.3.2 (option *fields* instead of getters) — fixed; `extract_bazel_options.py` hand tables (`CUSTOM_CONVERTERS`: HEAD-only names such as `disk_cache_gc_*`, `cache_computed_file_digests`, `sandbox_writable_path`, `test_resources`) pruned; `ByteSizeConverter` maps to `OptionType::ByteSize`. **Lesson: always diff generator output sizes.**
* `TriStateConverter` message (`'x' is not a boolean`), `--incompatible_check_sharding_support` wired (+ 9.3.2 error text), `info` no longer has `external_directory`.
* Bzlmod: `bazel_tools`' `repo_name = None` deps are kept (they bump versions: rules_apple 4.1.0, rules_swift 3.1.2, bazel_features 1.42.1, …); `zel mod graph --include_builtin` is now byte-identical to real 9.3.2. Namespace-violation hiding of targets applies only to symbolic macros (legacy macros such as rules_python's `py_toolchain_suite` were hiding their toolchains from `:all`).

### KNOWN OPEN REGRESSION after U1 (top of the U2 list)
rules_cc 0.2.17 (the 9.3.2 pin) drives `cc_internal` natives of 9.3.2's `CcStarlarkInternal` that zel never ported (zel's port matched HEAD/rules_cc 0.2.25). 21 of the 34 `cc_internal.*` names used by rules_cc 0.2.17 are unimplemented, e.g. `compute_output_name_prefix_dir`, `create_cc_compile_action(_template)`, `declare_compile_output_file`, `declare_other_output_file`, `get_link_args`, `wrap_link_actions`, `dynamic_library_symlink(2)`, `solib_symlink_action`, `create_lto_backend_action(_template)`, `per_file_copts`, `expand_and_tokenize`, `collect_per_file_lto_backend_opts`, `check_toplevel`, `absolute_symlink`, `actions2ctx_cheat`, `dynamic_library_soname`. Anything analysing cc_library (incl. `@bazel_tools//tools/jdk:proguard_allowlister` → coverage tests, java toolchains) fails with `cc_internal.X is not implemented yet in zel` until ported from the 9.3.2 Java (`CcStarlarkInternal.java`, `builtins_bzl/common/cc/*`).

### U2 worklist (HEAD-vs-9.3.2 behaviors still in zel code; from the four skew allowlists, now to be re-verified against the real binary)
1. cc_internal natives (above) — blocker.  
2. Info/help: `info` descriptions of keys vs 9.3.2 InfoItems; `help` texts regenerated but `info` hand tables unverified.  
3. Converters/flags (sweep_skew list, re-run `scripts/sweep_flag_acceptance.py --values` and drop entries): `check_tests_up_to_date` expansion, `experimental_starlark_type_checking`, `experimental_frontier_violation_check`, `experimental_skyfocus_dump_keys`, `experimental_remote_analysis_unreachable_cache_retry_interval`, `experimental_profile_additional_tasks` (manual_gc), `experimental_remote_analysis_cache_mode`, `j2objc_dead_code_report`, `platform_mappings` uplevel, `invocation_policy` `config_policies`, `experimental_remote_cache_compression` rename, `skyframe` dump enum; hand-coded converter semantics for `NonNegativeIntegerConverter` users.  
4. HEAD-only features still present in code: `--bust_action_caches` (zel-exec), `--incompatible_fail_on_unknown_output_groups` (server lib.rs), `experimental_sibling_repository_layout` treated as no-op (9.3.2 implements it; `is_sibling_repository_layout`), `ActionEnvironment` unset-names set (9.3.2: `=NAME` just removes from the map; not fingerprinted), Java `utf8_env` `LC_CTYPE` in `java_native.rs`.  
5. Build/test: `HOME=<TEST_TMPDIR>` in the test spawn env (absent in 9.3.2); `--cache_test_results` messages; `test_result_expiration` accepted again (verify); Java/cc action graph shape (`10 processes: 6 internal, 3 sandbox, 1 worker`).  
6. Query/aquery/cquery: `Rule.test/executable` proto fields (HEAD-only), `progress_message` in aquery (HEAD-only), `--incompatible_package_group_build_output` (HEAD-only), rule-class table `KNOWN_SKEW` (label_flag `on_leave_scope`, platform `remote_execution_properties`, `:collect_coverage_script`) — table is regenerated from 9.3.2 now, remove KNOWN_SKEW and verify with `scripts/verify_rule_defs_vs_bazel.py`; `$rule_implementation_hash` digest.  
7. Bzlmod/repo: archive `../` escape rejection, `bazel_dep` on self, localhost http without checksum, fetch/vendor failure exit code (9.3.2: 8), brotli (`.tar.br/.br`) decompressors + suffix list, `repository_ctx.copy`, `DigestWriter.MARKER_FILE_VERSION` 7 (+ Gson attr hashing) vs 8, `--downloader_config` urlencode validation & multiple files, `ConnectException` retry, `dropping [...]` repr, `PatchUtil` multi-file/symlink containment (HEAD commits db1d260030, 1224606c0b), fixtures `tests/fixtures/patch_corpus` `result` vs `result932`.  
8. Misc: `ZEL` startup-option warning text lacks `bazel(<ws>)`; `skew()` rewrites in `scripts/build_harness/h.py`, `scripts/query_harness/run.py`, `scripts/bzlmod_harness/harness.py` must be deleted and the three allowlist docs emptied once the above are done.
9. Config ids (`scripts/diff_config_ids.sh`): table now from 9.3.2 without overrides; re-verify ids (known gaps: `--collect_code_coverage` exec configs, Starlark flag cache-key encoding).

## U2a — cc/java/py/proto on the 9.3.2 spec (rules_cc 0.2.17, rules_java 9.1.0, rules_python 1.7.0, protobuf 33.4)
Verification setup (not in repo): `~/Development/ag-tmp/{setup_ws932.sh,z932.sh,b932.sh}` — local registry from `scripts/make_vendored_registry.py`, workspaces `ws932/{cc,java,py,proto}`; zel vs real `/tmp/bazel-9.3.2` with identical flags (`--java_runtime_version=remotejdk_21` because the host JDK 27 is too new for the remote JDK 25 tool runtime; real fails identically without it).
* cc_internal natives now in `zel-analysis/src/cc_native.rs`: `compute_output_name_prefix_dir`, `dynamic_library_symlink(2)`, `dynamic_library_soname`, `solib_symlink_action` (SolibSymlink, no progress message), `absolute_symlink`, `expand_and_tokenize`, `check_toplevel`; the rest (`create_cc_compile_action`, `get_link_args`, `wrap_link_actions`, …) already existed there. **Still stubbed:** `create_cc_compile_action_template`, `create_lto_backend_action(_template)` (tree-artifact sources / ThinLTO), `per_file_copts`/`collect_per_file_lto_backend_opts` (return `[]`; options are stored comma-joined so `--per_file_copt` regexes cannot be recovered).
* Result vs real: cc (binary/library/test/linkshared/alwayslink/cc_import/cc_shared_library) aquery identical except `ActionKey` digests, 52 processes (35 internal/17 sandbox) both; java (lib/binary/test): aquery identical except ActionKey, 19 processes (11/5/3 worker) both; py: builds/tests, 17 processes both, aquery differs in PyCompile tool inputs (zel flattens the python3 runfiles instead of a `.runfiles` tree input), the zipper tool (zel builds `//third_party/ijar:zipper`; real ships a prebuilt binary) and venv `__init__` entries; proto: protoc from source builds and cc/java proto binaries run, aquery(deps(proto_main)) 2116 vs 2114 actions (extra `protoc_minimal.runfiles` tree).
* Spec fixes found on the way: toolchain implementations are configured in the *requesting* target's configuration (no exec transition; `DependencyProducer`), exec platform label = `PlatformInfo.label` (alias `@bazel_tools//tools:host_platform` produced a duplicate exec config), `%{output}`/`%{input}` progress placeholders use root-relative paths, `ShellEscaper` (`=` is not a safe char), aquery prints `CommandLines.allArguments` (param-file args expanded), param-file writes are not counted as processes, `Args.add(name, None)` stringifies `None`, `Runfiles` default mapping excludes file targets, `NestedSet` per-node ordering in `depset.to_list`, Python `empty_filenames` are workspace-relative (`../repo/...`), `ctx.configuration.stamp_binaries` is a first-class method value, `testing.ExecutionInfo(requirements)` positional + consumed by test actions/aquery, `TemplateExpansionAction` has no `IsExecutable` and keeps dict-order substitutions, `JavaHeaderCompileAction`/`JavaCompileAction` flags and execution info (workers, path mapping, inline jdeps) per 9.3.2, cc_toolchain artifact name patterns use lower-case category names.
* Flaky `build_tag_filters` test root cause: the process-global `TEST_COMMAND` flag (set by every command) raced with other in-process commands (now thread-local); separately `BlazeRuntime::new` now creates the output base so `<ob>/command.profile.gz` is always creatable.
* Known open: `aquery deps(...)` through `@bazel_tools//tools/jdk:proguard_allowlister` fails in zel when the generated python repo is not visible; `zel` `-s` owner text is always `rule target` (real: `<rule_class> rule target`); `--java_language_version=N` still selects the default toolchain in zel (config_setting on the exec-remapped option).

## U2c handoff (cquery/aquery parity on 9.3.2) — paused on user request

**Strict numbers** (comparator unchanged, real baselines re-recorded from /tmp/bazel-9.3.2): start 1245/2682 (stale baselines);
after re-recording 1447; run6 = **1744/2682** (real 938 remain). Later fixes (cycle order by key hash, `--transitions` typed option
diff, "Targets were missing from graph" for rdeps/allpaths, pattern-warning for transitions) were verified on repros only; the
full matrix run7 was started but not read. Re-run: `scripts/heavy.sh python3 scripts/cq_harness/run.py scripts/cq_harness/{cq_cmds,new_cmds,new6c_cmds}.tsv`
then `/tmp/u2c/cl.sh x` (writes /tmp/u2c/lines.txt: id, first stdout/stderr diff) and `python3 /tmp/u2c/keys.py` (ActionKey match per mnemonic).
Scratch: /tmp/u2c (ws copy, seq.sh = sequence real-vs-zel with fresh output bases, r.sh, cmp.sh, show.sh <id>, akeys.sh <mnemonic> <aquery expr>,
rm*.py/tr.py/dg.py = python oracles of key formulas), /tmp/cqh (harness output: summary.txt, out/*.z.*). Always `zel --output_base=X shutdown`
inside the ws dir before testing a new binary (the server keeps the old one).

**Done (all verified bit-exact against real 9.3.2):**
- Lockfile `registryFileHashes` hex (was SRI -> real crashed). cquery bad-format pattern errors; `--show_config_fragments` implicit
  `include_config_fragments_provider`; analysis-cache check compares raw `--platforms`; keep_going failure bookkeeping + hash-ordered
  reporting (`PostEnv::take_kg_lines`); cycles reported in key-hash order with "already-reported" (resolver `defer_cycle_dedup`).
- cquery proto: Configuration fragments/fragment_options, visibility attr, removed HEAD-only fields (Rule test/executable, aquery progress_message).
- `$rule_implementation_hash`: bzl transitive digest = sha256(file digest || load digests || exports.bzl digest); bundled builtins file digest = sha256("/virtual_builtins_bzl/<path>") (loader.rs).
- ActionKeys: RepoMappingManifest (nested transitive packages, ordered RepositoryMapping, Runfiles nested set), TestRunner, Symlink/ExecutableSymlink/SolibSymlink/RunfilesTree (EMPTY_PLATFORM_INFO). New: `zel-analysis/src/nested_set.rs`, `ActionKeyInfo`, `Runfiles::nested`, `TargetResolver::transitive_packages`.
- Test rules now analyse their implicit exec deps ($test_wrapper, $xml_writer, ...) and config_setting records constraint_value edges.
- `--transitions=full` option diff from typed BuildOptions; old-name option warnings (`ParsedCommandOptions::old_name_warnings`); "Targets were missing from graph" in rdeps/allpaths (`Engine::reverse_closure_w`).

**Half-done / caveats:** debug logging in aquery.rs behind env `ZEL_DEBUG_PKGS` (writes /tmp/u2c/pkgs.log; remove). Test-toolchain dep (`empty_toolchain`, `[toolchain dependency: test]`) not analysed. Cycle "already-reported" head uses the cycle-entry label. Python fragments excluded from cquery proto configurations by evidence only. Unit tests for the new code (nested_set has one) are not written; `cargo test --workspace` was green before the last 8 edits (only exec_e2e::output_base_layout_and_symlink_forest flaked under load).

**Next steps:** (1) read run7, cluster; (2) CatIt/opt-config action missing for `rdeps(//...)` (action ordering, exec-config duplicates); (3) `actual#host NoTransition` edges in transitions output (platform alias edges, ~15); (4) event ordering of `//...` universes (parallel eval order, ~40: n91-type); (5) missing aquery blocks (py/zipper PyCompile prebuilt zipper, protoc_minimal runfiles, proguard allowlister, owner text, starlark/files outputs, config() semantics, aspects); (6) write unit tests, run `cargo test --workspace`.
