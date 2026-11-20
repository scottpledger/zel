# Bazel vs Zel: `rules_pylyglot`

## Metadata

| Field | Value |
| --- | --- |
| Repo | https://github.com/scottpledger/rules_pylyglot.git |
| Clone | `/tmp/bazel-vs-zel/repos/rules-pylyglot` |
| Tested commit | `fac6e0923c71916661c44785c286fb7798453470` (`main`) |
| `.bazelversion` | `9.2.0` (first line). Tested with Bazel **9.3.2** anyway. |
| Bazel | Bazelisk `/tmp/bazel-vs-zel/bin/bazel` (v1.30.0) with `USE_BAZEL_VERSION=9.3.2`. `bazel version`: Build label `9.3.2`, built Wed Nov 04 22:18:38 2026. |
| Zel | `/tmp/bazel-vs-zel/bin/zel`, supplied as built from `6297ed54b710bb023dfc7c7f275e3c2515d9761a`. `zel version` (with `USE_BAZEL_VERSION=9.3.2`): Build label `9.3.2`, target `@@//crates/zel-client:zel`, built Fri Nov 20 13:56:07 2026. |
| OS / arch | Ubuntu 24.04.5 LTS, `x86_64`, Linux 6.12.113+ |
| Date | 2026-11-20 |
| Output bases | Bazel `/tmp/bazel-vs-zel/ob/rules-pylyglot-bazel` and `...-bazel-e2e-smoke`. Zel `/tmp/bazel-vs-zel/ob/rules-pylyglot-zel` and `...-zel-e2e-smoke`. JVM heap `--host_jvm_args=-Xmx1g` on Bazel. |
| Workspaces | Root module `rules_pylyglot`. Nested Bzlmod workspace `e2e/smoke` (`local_path_override` of `rules_pylyglot` to `../..`). Root `.bazelignore` is `e2e/`, so the root `//...` pattern does not include the smoke test. No other `MODULE.bazel`. |

Both servers were shut down after the runs (`shutdown` on each output base). No `rules-pylyglot` daemons remained.

### How the repo is meant to be built

CI (`.github/workflows/ci.yaml`) runs `bazel test //...` on matrix folders `.` and `e2e/smoke`, OS `ubuntu-latest` / `macos-latest` / `windows-latest`, Bazel `8.x` and `9.x`. For the root folder it adds `--config=ci`. For Bazel versions that do not start with `9.` it adds `--lockfile_mode=update`.

Root `.bazelrc` imports `tools/preset.bazelrc` (bazelrc-preset) and sets `common --lockfile_mode=error` plus `--config=ruleset` (`--check_direct_dependencies=off`). The preset also sets `--nobuild_runfile_links`, `--incompatible_default_to_explicit_init_py`, `--nosandbox_default_allow_network`, and `--test_output=errors`. The rc comments say to use `--lockfile_mode=update` or `off` when testing a different Bazel version than the one that wrote the lockfile. `e2e/smoke/.bazelrc` is empty, so the smoke workspace does not force `--lockfile_mode=error`.

## Executive verdict

**PARTIAL**

`query //...` matches Bazel exactly in both workspaces (30 root targets, 5 smoke targets) once Zel is told to stay on 9.3.2. With the repo's committed lockfile and `--lockfile_mode=error`, Bazel 9.3.2 and Zel fail the same way (exit 48, missing `apple_support` 2.5.2 checksum). After `--lockfile_mode=update`, Bazel builds and tests the root module and `e2e/smoke` cleanly. Zel does not: every `pylyglot_*` bundle action fails because the rules_python venv interpreter symlink in the bundler runfiles tree is dangling, Gazelle targets fail analysis in `rules_go`, and `//tools:preset.update_test` fails because Zel has already removed `ctx.build_file_path`. The smoke workspace query matches, but its build and both tests fail on the same bundler problem plus a one-node action-graph cycle.

## Summary

Wall times are shell elapsed seconds. "Elapsed" is the tool's own `INFO: Elapsed time`. Root builds used `--keep_going` so one failure did not hide the rest. Root functional runs used `--lockfile_mode=update` for the cold build (required for Bazel 9.3.2 against the 9.2.0 lockfile) and `--lockfile_mode=error` for the warm build and tests after that update. Zel was invoked with `USE_BAZEL_VERSION=9.3.2`; without that, Zel tries to download Zel 9.2.0 and exits 36 before any command runs.

| Workspace | Command | Bazel 9.3.2 | Zel | Match |
| --- | --- | --- | --- | --- |
| root | version gate (no `USE_BAZEL_VERSION` on Zel) | `USE_BAZEL_VERSION=9.3.2` selects 9.3.2 | exit 36, wants Zel 9.2.0 from `.bazelversion` | no |
| root | `query //...` with rc `--lockfile_mode=error` | exit 48, 3s, 0 targets | exit 48, ~0s, 0 targets (same missing checksum) | yes (same failure) |
| root | `query //... --lockfile_mode=update` | exit 0, 5s, **30** targets | exit 0, 2s, **30** targets, identical list | yes |
| root | `build //...` cold | exit 0, wall 73s, elapsed 73.1s, 145 actions, **26** targets | exit 1, wall 337s, elapsed 4.3s, **18 of 30** reported succeeded | no |
| root | `build //...` warm | exit 0, wall 1s, elapsed 0.94s | exit 1, wall 160s, elapsed 3.5s, same 18/30 | no |
| root | `test //... --test_output=errors` | exit 0, wall 1s, **5/5 passed** | exit 1, wall 139s, **3 passed, 1 failed to build**, 5th test never analyzed | no |
| `e2e/smoke` | `query //...` | exit 0, 3s, **5** targets | exit 0, 1s, **5** targets, identical | yes |
| `e2e/smoke` | `build //...` cold | exit 0, wall 3s, elapsed 3.4s, 4 targets, 35 actions | exit 1, wall 34s, elapsed 1.3s, 1 of 12 succeeded | no |
| `e2e/smoke` | `build //...` warm | exit 0, wall <1s, elapsed 0.36s | exit 1, wall 30s, elapsed 1.5s | no |
| `e2e/smoke` | `test //... --test_output=errors` | exit 0, wall 1s, **2/2 passed** | exit 1, wall 29s, **0/2**, both failed to build | no |

Root query targets (both tools, sorted the same):

`//:gazelle`, `//:license`, `//:package_metadata`, `//pylyglot:defs`, `//pylyglot:defs.doc_extract`, `//pylyglot:defs.docs-as-proto`, `//pylyglot/private:bundler`, `//pylyglot/private:modules_aspect`, `//pylyglot/private:pylyglot_bundle`, `//pylyglot/tests:bundle_test`, `//pylyglot/tests:duplicate_modules`, `//pylyglot/tests:duplicate_modules_test`, `//pylyglot/tests:fixture_lib`, `//pylyglot/tests:generated_source`, `//pylyglot/tests:multiplat`, `//pylyglot/tests:namespace_package`, `//pylyglot/tests:namespace_package_test`, `//pylyglot/tests:nested_base`, `//pylyglot/tests:nested_wrapper`, `//pylyglot/tests:posix_first`, `//pylyglot/tests:source_less_bundle`, `//pylyglot/tests:source_less_bundle_test`, `//pylyglot/tests:source_less_library`, `//pylyglot/tests:windows_first`, `//tools:gazelle`, `//tools:gazelle-runner`, `//tools:gazelle_bin`, `//tools:preset`, `//tools:preset.update`, `//tools:preset.update_test`.

`e2e/smoke` query targets (identical): `//:message`, `//:smoke_build_test`, `//:smoke_build_test_0__deps`, `//:smoke_bundle`, `//:smoke_test`.

Bazel root tests (all passed): `//pylyglot/tests:bundle_test` (0.6s), `//pylyglot/tests:duplicate_modules_test` (0.0s), `//pylyglot/tests:namespace_package_test` (0.0s), `//pylyglot/tests:source_less_bundle_test` (0.0s), `//tools:preset.update_test` (0.1s).

Zel root tests: the three `failure_test` sh_tests passed (0.1s each). `//pylyglot/tests:bundle_test` failed to build. `//tools:preset.update_test` failed analysis and was not in the "4 test targets" Zel reported (`Executed 3 out of 4 tests: 3 tests pass and 1 fails to build`).

Bazel smoke tests: `//:smoke_build_test` PASSED, `//:smoke_test` PASSED. Zel: both `FAILED TO BUILD`.

`build //...` on the root analyzes 26 targets on both tools. `query //...` lists 30. Three of the extra query targets are `tags = ["manual"]` (`duplicate_modules`, `namespace_package`, `source_less_bundle`) and are excluded from `build //...`. Preset `--show_result=20` truncates the final target list, so the log does not print every successful label.

Zel wall time is much larger than its printed elapsed time. The command thread sat at high CPU (about 70%) with empty stdout/stderr before the summary appeared, so a lot of the wall clock is server work that the elapsed timer does not include. Bazel elapsed time matched wall time.

## Findings

### 1. `.bazelversion` 9.2.0 vs the 9.3.2 binary

Invoking Zel with no version override:

```
ERROR: The project requires Zel version '9.2.0' (specified in .../rules-pylyglot/.bazelversion), but the running binary is version '9.3.2' and version '9.2.0' could not be found locally or downloaded from https://github.com/scottpledger/zel/releases/download.
```

Exit 36. Zel's client (`crates/zel-client/src/bazelisk.rs`) resolves `.bazelversion` itself and tries to exec a downloaded Zel of that version. `USE_BAZEL_VERSION=9.3.2` matches the running binary and continues in-process. That is the same override Bazelisk needs so it does not download Bazel 9.2.0. All later Zel results used that override.

### 2. Lockfile is not valid for Bazel 9.3.2 under `--lockfile_mode=error`

Both tools, root workspace, committed `MODULE.bazel.lock`:

```
ERROR: Error computing the main repository mapping: ... Missing checksum for registry file https://bcr.bazel.build/modules/apple_support/2.5.2/MODULE.bazel not permitted with --lockfile_mode=error. Please run `bazel mod deps --lockfile_mode=update` to update your lockfile.
```

Exit 48 on both. This is Bazel 9.3.2 vs a lockfile written for the pinned 9.2.0, not a Zel divergence. `--lockfile_mode=update` on `query` produced **byte-identical** lockfiles from Bazel and from Zel (registry checksums swapped `apple_support` 1.24.2 for 2.5.2 and adjusted `rules_cc` entries). A full Bazel `build //... --lockfile_mode=update` then rewrote two extension digests (`rules_kotlin` `bzlTransitiveDigest`, `rules_python` `uv` `bzlTransitiveDigest`). Zel's build-time update did not match that file: it kept the pre-build `rules_kotlin` digest and recorded extra `pypi__*` `REPO_MAPPING` inputs under the `uv` extension. Query still matched, and the build failures below are not lockfile mismatches.

`e2e/smoke` has an empty `.bazelrc`, so Bazel 9.3.2's default lockfile mode applied. Query succeeded (exit 0) and rewrote the same class of registry checksums in `e2e/smoke/MODULE.bazel.lock`. The clone lockfiles were restored to `HEAD` after the run.

A first Bazel root build with `--lockfile_mode=error` on the query-updated lockfile (before the extension digests were refreshed) aborted during analysis:

```
ERROR: Analysis of target '//pylyglot/tests:namespace_package_test' failed; build aborted: MODULE.bazel.lock is no longer up-to-date because the implementation of the extension '@@rules_kotlin+//src/main/starlark/core/repositories:bzlmod_setup.bzl%rules_kotlin_extensions' ... has changed.
```

The cold build recorded above is the retry with `--lockfile_mode=update`, which Bazel completed successfully.

### 3. Bundler `py_binary` runfiles interpreter is dangling (root and smoke)

Bazel built `//pylyglot/private:bundler` and every `pylyglot_*` bundle. Zel failed `//pylyglot/tests:multiplat`, `:posix_first`, `:windows_first`, and `e2e/smoke` `//:smoke_bundle` with the same traceback. Excerpt:

```
ERROR: .../pylyglot/tests/BUILD.bazel:48:26: Bundling Python sources for //pylyglot/tests:multiplat failed: (Exit 1): bundler failed: error executing PylyglotBundle command
...
FileNotFoundError: [Errno 2] No such file or directory: '.../sandbox/processwrapper-sandbox/5/execroot/_main/bazel-out/k8-opt-exec/bin/pylyglot/private/bundler.runfiles/_main/pylyglot/private/_bundler.venv/bin/python3'
```

What is on disk after Zel's build:

- The venv created next to the binary has a **relative** interpreter link that is only valid inside a runfiles tree: `_bundler.venv/bin/python3` -> `../../../../../rules_python++python+python_3_11_x86_64-unknown-linux-gnu/bin/python3`. From `bazel-out/k8-opt-exec/bin/...` that target does not exist. `file` reports a broken symlink. The real interpreter is `external/rules_python++python+python_3_11_x86_64-unknown-linux-gnu/bin/python3`.
- Zel's `bundler.runfiles` entry for that `python3` is an **absolute** symlink back to the broken output path, so opening it fails.
- Bazel's sandbox stash for the same action keeps a **relative** symlink inside `bundler.runfiles/.../_bundler.venv/bin/python3`, and `os.path.exists` on that link is true because five `..` components land on `bundler.runfiles/rules_python++python+.../bin/python3`.

`crates/zel-exec/src/runfiles_tree.rs` `build_runfiles_tree` absolutizes every manifest value to `exec_root.join(v)` before creating the symlink tree. That loses the relative link rules_python emits so the venv interpreter resolves against the repo root **inside** the runfiles directory. `crates/zel-sandbox` `stage_runfiles_dir` then copies that absolute link into the process-wrapper sandbox. Zel ran these actions in `processwrapper-sandbox` (no `linux-sandbox` in the Zel install base). Bazel used `linux-sandbox` and the relative runfiles layout.

This is why `//pylyglot/tests:bundle_test` and both smoke tests fail to build: they depend on a bundle action that executes `//pylyglot/private:bundler`.

### 4. `rules_go` transition index is treated as a provider key

Zel analysis of the Gazelle targets:

```
ERROR: .../external/rules_go++go_sdk+go_default_sdk/BUILD.bazel:90:20: in non_go_reset_target rule @@rules_go++go_sdk+go_default_sdk//:builder_reset: .../rules_go+/go/private/rules/transition.bzl:288:21: unhashable or invalid provider key of type 'int'; expected a provider
WARNING: errors encountered while analyzing target '//:gazelle', it will not be built.
WARNING: errors encountered while analyzing target '//tools:gazelle-runner', it will not be built.
WARNING: errors encountered while analyzing target '//tools:gazelle_bin', it will not be built.
```

`transition.bzl` line 288 is the usual rules_go pattern for a transitioned label attribute:

```python
def _go_reset_target_impl(ctx):
    t = ctx.attr.dep[0]  # [0] seems to be necessary with the transition
```

`crates/zel-analysis/src/starlark_rule_host.rs` `host_index` for kind `"target"` only accepts a provider key (`provider_key_of`). An integer index becomes "unhashable or invalid provider key of type 'int'". Bazel built `//tools:gazelle`, `//tools:gazelle_bin`, and `//tools:gazelle-runner`.

### 5. `ctx.build_file_path` removed by Zel's default semantics

```
ERROR: .../tools/BUILD.bazel:4:15: in _diff_test rule //tools:preset.update_test: .../bazel_lib+/lib/private/diff_test.bzl:89:37: Deprecated API: ctx.build_file_path has been removed. Use ctx.label.package + '/BUILD' instead of ctx.build_file_path.
Use --noincompatible_stop_exporting_build_file_path to temporarily continue using it.
```

`diff_test.bzl` still passes `"{build_file_path}": ctx.build_file_path` into the test template. Bazel 9.3.2 analyzed and passed `//tools:preset.update_test`. Zel's `StarlarkSemantics` default sets `incompatible_stop_exporting_build_file_path: true` (`crates/zel-starlark/src/semantics.rs`), and `starlark_rule_host` refuses the attribute when that flag is on. Bazel 9.3.2 still exports the attribute under its default.

### 6. Smoke `build_test` hits a one-node action-graph cycle

After the bundler failure, Zel's smoke build and test also printed:

```
ERROR: cycle in the action graph:
    bazel-out/k8-fastbuild/bin/smoke_build_test.sh.runfiles_manifest
```

Only one node is listed (a self-cycle on the runfiles manifest of the bazel_skylib `build_test` shell wrapper). The message is emitted from `crates/zel-server/src/exec_phase.rs` when the scheduler records `sched.cycle`. Bazel built `//:smoke_build_test` and the test passed in 0.0s. The root workspace did not print this cycle; the failure tests there are `sh_test`s that do not wrap the bundle as a `build_test` data edge in the same way.

### What did match

- Root and smoke `query //...` label sets are identical.
- Strict lockfile rejection on the root module is the same error and exit code.
- Query-time `--lockfile_mode=update` rewrote the root lockfile the same way.
- Zel built the non-bundle root targets that do not need Gazelle or `ctx.build_file_path` (license / package metadata, `write_file`, the three analysis-failure tests, preset generation, the bundler `py_binary` output itself). The three `failure_test` targets passed because they only assert that a `manual` bundle fails analysis inside an `sh_test`; they do not run the bundler.
- Zel reports Build label 9.3.2 and accepts the preset flags that Bazel 9.3.2 accepted (`--nobuild_runfile_links`, `--incompatible_enforce_starlark_utf8=error`, `--test_output=errors`, and the rest of `tools/preset.bazelrc`).

## Root cause and next fixes in Zel

1. **Runfiles symlinks for rules_python venvs** (`zel-exec` `runfiles_tree.rs`, and the sandbox copy in `zel-sandbox` `stage_runfiles_dir`). Preserve relative symlink targets when the runfiles entry is itself a symlink whose relative target is meaningful only inside the runfiles tree. Absolutizing to the original `bazel-out` path makes `_bundler.venv/bin/python3` dangle. This blocks every `pylyglot_*` bundle and both smoke tests.

2. **Integer indexing of transitioned attributes** (`zel-analysis` `starlark_rule_host.rs` `host_index`, `"target"` arm). `ctx.attr.dep[0]` on a split/transition label must return the configured target, not a provider lookup. Until that works, `rules_go`'s `non_go_reset_target` cannot analyze, so Gazelle (`//tools:gazelle`, `:gazelle_bin`, `:gazelle-runner`, `//:gazelle`) cannot build.

3. **`incompatible_stop_exporting_build_file_path` default** (`zel-starlark` `StarlarkSemantics`). Bazel 9.3.2 still provides `ctx.build_file_path`. Zel's default of `true` breaks `bazel_lib`'s `diff_test` and therefore `//tools:preset.update_test`. Default it to match Bazel 9.3.2 (still exported), or only remove it when the user opts in.

4. **Runfiles-manifest action cycle** (`zel-server` exec scheduling, surfaced for `//:smoke_build_test`). A `*.sh.runfiles_manifest` action is recorded as depending on itself. Bazel has no such cycle. This is independent of the bundler crash and fails the smoke `build_test` even if the bundle artifact is fixed.

5. **Version launcher vs a prebuilt Zel** (`zel-client` bazelisk). Direct invocation honors `.bazelversion` and will not run a newer prebuilt binary. For parity tests against a pinned Bazel version, `USE_BAZEL_VERSION` (or `USE_ZEL_VERSION`) must be set. Worth keeping, but it is easy to mistake for a workspace load failure.

No Zel source was changed.
