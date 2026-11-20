# Bazel 9.3.2 vs Zel Compatibility Benchmark (Real-World Repositories)

This directory records side-by-side compatibility and timing evaluations of **Bazel 9.3.2** versus **Zel** (built from `main` at `6297ed54b710bb023dfc7c7f275e3c2515d9761a`, build label `9.3.2`) across seven public Bazel ruleset repositories on Linux `x86_64` (Ubuntu 24.04.5 LTS, kernel `6.12.113+`) on `2026-11-20`.

## Methodology

- **Bazel reference**: Bazel `9.3.2` invoked via Bazelisk `v1.30.0` (`USE_BAZEL_VERSION=9.3.2`, `--host_jvm_args=-Xmx1g`). Where a repository's `.bazelversion` pinned an older Bazel release (`7.2.1`, `7.5.0`, `8.5.0`) and Bazel `9.3.2` itself failed due to upstream ruleset incompatibilities, the pinned Bazel version was also checked as a reference.
- **Zel binary**: Native release build of `zel` with isolated `--output_base` per repository and nested workspace. Because 6 of the 7 repos pin `.bazelversion` to an older Bazel version, `USE_BAZEL_VERSION=9.3.2` (or `USE_ZEL_VERSION=9.3.2` / `ZEL_SKIP_WRAPPER=true`) was passed so Zel's built-in Bazelisk version layer ran the native Zel engine in-process instead of attempting to download an older Zel release.
- **Commands tested**: `query //...`, cold `build //...`, warm `build //...`, and `test //... --test_output=errors` across root workspaces and nested `e2e/` / `tests/` / `examples/` workspaces where present.
- **Timing caveat**: All seven repository evaluations ran concurrently on a shared 4-core VM under heavy CPU and memory load. Compare exit codes, target sets, action counts, and tool-reported `INFO: Elapsed time` first; wall-clock times reflect contention as well as server overhead outside Zel's elapsed timer.

## Summary Matrix

| Repository | `.bazelversion` | Verdict | Query Parity (`//...`) | Cold Build (Bazel 9.3.2 vs Zel) | Warm Build (Bazel 9.3.2 vs Zel) | Tests (Bazel 9.3.2 vs Zel) | Primary Zel Blockers | Report |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `scottpledger/yaml.bzl` | `7.5.0` | **PARTIAL** | **Exact match** (`98` / `98` targets) | Bazel: PASS (`63.8s`, 94 targets)<br>Zel: FAIL (`0.08s` elapsed / `172s` wall, 91/94 configured) | Bazel: PASS (`2.85s`)<br>Zel: FAIL (`0.08s` elapsed / `81s` wall) | Bazel: `82/83` pass (preset diff fails on 9.3.2)<br>Zel: `82/82` YAML unit tests pass; preset test fails analysis | `ctx.build_file_path` default; `rules_go` non-split transition `ctx.attr.dep[0]` | [yaml-bzl.md](./yaml-bzl.md) |
| `scottpledger/xml.bzl` | `7.5.0` | **PARTIAL** | **Diverged** (Bazel 9 fails on `CcInfo`; Zel matches Bazel 7.5.0 `161` targets; non-gazelle `158` exact match) | Bazel 9: FAIL (`1.24s`, `CcInfo`)<br>Zel: FAIL (`44.7s`, `ctx.build_file_path`)<br>Library `//:all //private/...`: both PASS (`0.54s` vs `0.02s`) | Bazel 9: FAIL (`0.41s`)<br>Zel: FAIL (`2.82s`) | `//tests/...`: Bazel 9 w/ `--ignore_dev_dependency` `136/136` (`5.49s`); Zel `136/136` (`2.32s` elapsed / `6.92s` wall; warm `0.15s`) | Still predeclares removed `CcInfo` (diverges from Bazel 9 load failure); `ctx.build_file_path` default | [xml-bzl.md](./xml-bzl.md) |
| `scottpledger/rules_auto_dotnet` | `8.5.0` | **PARTIAL** | **Exact match** (root `79`/`79` w/ `--lockfile_mode=off`; `e2e/smoke` `13`/`13`; `e2e/strict_failure` both exit 7) | Root: Bazel PASS (`21.5s`, 73 targets); Zel FAIL (`36.9s` elapsed / `79.4s` wall)<br>`e2e/smoke`: both PASS (`5.7s` vs `1.79s` elapsed / `35.2s` wall, 42 actions each) | Root: Bazel PASS (`2.12s`); Zel FAIL<br>`e2e/smoke`: both PASS (`1.19s` vs `1.15s` elapsed / `23.0s` wall) | Root: Bazel `52/52` pass; Zel `0` (analysis abort)<br>`e2e/smoke`: **both `3/3` pass** | `ctx.build_file_path` default (root passes `build //...` with flag); `build @dotnet_projects//:diagnostics.json` returns "No repository visible" instead of fetching | [rules-auto-dotnet.md](./rules-auto-dotnet.md) |
| `scottpledger/rules_polyglot` | `7.2.1` | **PARTIAL** | Root: both exit 7 (different error)<br>`e2e/smoke`: **Exact match** (`6`/`6`)<br>`//polyglot/...`: **Exact match** (`11`/`11`) | Root: both FAIL (Bazel 9 `CcInfo`; Zel `native.sh_binary`; Bazel 7.2.1 PASS `54.7s`)<br>`e2e/smoke`: **both PASS** (`1.63s` vs `0.04s` elapsed / `4.24s` wall) | Root: both FAIL<br>`e2e/smoke`: **both PASS** (`0.14s` vs `0.005s` elapsed / `0.37s` wall) | Root: both `0` (Bazel 7.2.1 `2/2`)<br>`e2e/smoke`: **both `1/1` pass** | Still predeclares `CcInfo`; missing `--incompatible_autoload_externally` / `native.sh_binary`; toolchain init drops traceback | [rules-polyglot.md](./rules-polyglot.md) |
| `scottpledger/rules_multirun` | none | **PARTIAL** | **Exact match** (`//...` both exit 7; `--keep_going` `3`/`3`; `//internal:all` `3`/`3`) | `//...`: both FAIL (missing committed `internal/runfiles_enabled.bzl` + bare `sh_*`/`py_*`)<br>`//internal:all`: Bazel analyzes all 3, fails on missing src; Zel fails toolchain load (`1.07s`) | Both FAIL (`0.46s` vs `0.05s` elapsed / `32.3s` wall) | Both `0` (loading fails on repo HEAD) | `rule()` rejects `incompatible_use_toolchain_transition` (`aspect_bazel_lib` 2.x); nested `.bzl` errors collapsed into `initialization of module 'defs.bzl' failed` | [rules-multirun.md](./rules-multirun.md) |
| `scottpledger/rules_pylyglot` | `9.2.0` | **PARTIAL** | **Exact match** (lockfile error exit 48 on both; updated root `30`/`30`; `e2e/smoke` `5`/`5`) | Root: Bazel PASS (`73.1s`, 145 actions); Zel FAIL (`4.3s` elapsed / `337s` wall, 18/30)<br>`e2e/smoke`: Bazel PASS (`3.4s`); Zel FAIL (`1.3s` elapsed / `34s` wall) | Root: Bazel PASS (`0.94s`); Zel FAIL (`3.5s` elapsed / `160s` wall)<br>`e2e/smoke`: Bazel PASS (`0.36s`); Zel FAIL (`1.5s` elapsed / `30s` wall) | Root: Bazel `5/5` pass; Zel `3` pass, `1` fail to build, `1` analysis abort<br>`e2e/smoke`: Bazel `2/2` pass; Zel `0/2` | `rules_python` venv relative `python3` symlink absolutized & dangling in runfiles; `rules_go` transition `ctx.attr.dep[0]`; `ctx.build_file_path` default; 1-node `runfiles_manifest` action cycle | [rules-pylyglot.md](./rules-pylyglot.md) |
| `scottpledger/rules_itest` | `9.0.0` | **FAIL** | Root: Zel exit 2 on `common --@rules_go//go/config:pure` (exact `24`/`24` if removed)<br>`tests/`: Bazel `143`, Zel exit 7<br>`examples/`: Bazel `32`, Zel exit 2 | Root: Bazel PASS (`118.6s`, 580 actions); Zel FAIL (`4.4s`)<br>`tests/`: Bazel PASS (`94.0s`, 1135 actions); Zel FAIL (`0.7s`)<br>`examples/`: Bazel PASS (`556s`, 3782 actions); Zel FAIL (`254s`) | Root: Bazel `4.2s`, Zel FAIL (`35.0s`)<br>`tests/`: Bazel `1.6s`, Zel FAIL (`0.5s`)<br>`examples/`: Bazel `1.4s`, Zel FAIL (`203s`) | Root: Bazel `1/1`, Zel `0`<br>`tests/`: Bazel `42/42` (`4.5s`), Zel `0`<br>`examples/`: Bazel `6/6`, Zel `0` | Starlark flags in `common` rejected by `query`/`shutdown`; missing `native.bazel_version` in `zel-packages`; nested `module_extension()` rejected while extension active; `+@rules_cc` autoload ignored; Gazelle duplicate self-repo visibility | [rules-itest.md](./rules-itest.md) |

## What Already Works Well

- **Bzlmod & `query //...` graph parity**: Once Starlark rc flags and Bazel 9 symbol removals align, Zel produces identical `query //...` target sets across `yaml.bzl` (98), `xml.bzl` (158 non-gazelle / 161 Bazel-7 graph), `rules_auto_dotnet` (79 root + 13 smoke), `rules_polyglot` (6 smoke + 11 `//polyglot/...`), `rules_multirun` (3), `rules_pylyglot` (30 root + 5 smoke), and `rules_itest` (24 root).
- **Lockfile diagnostics & updates**: Strict `--lockfile_mode=error` exit code `48` and error messages match Bazel 9.3.2 on `rules_auto_dotnet` and `rules_pylyglot`; query-time `--lockfile_mode=update` generated byte-identical lockfiles on `yaml.bzl`, `xml.bzl`, and `rules_pylyglot`.
- **Starlark rulesets & e2e suites**: Zel executed and passed `82/82` Starlark unit tests in `yaml.bzl`, `136/136` XML unit tests in `xml.bzl`, `3/3` e2e tests in `rules_auto_dotnet/e2e/smoke`, `1/1` e2e test in `rules_polyglot/e2e/smoke`, and `3/3` `failure_test` targets in `rules_pylyglot`.

## Prioritized Fix Backlog (Ranked by Real-World Impact)

1. **Fix `incompatible_stop_exporting_build_file_path` default to `false` (4 repos: `yaml.bzl`, `xml.bzl`, `rules_auto_dotnet`, `rules_pylyglot`)**
   - Bazel 9.3.2 still exports `ctx.build_file_path` by default, and `bazel_lib` 3.0.0's `diff_test` (`//tools:preset.update_test`) relies on it.
   - `crates/zel-options/src/flag_tables.rs` and `crates/zel-bzlmod/src/semantics_fp.rs` already record `false`, but `StarlarkSemantics::default()` (`crates/zel-starlark/src/semantics.rs`) and `cfg.bool_option(..., true)` (`crates/zel-analysis/src/starlark_rule_host.rs`) default to `true`, and `build_options_table` lacks the flag.
   - Passing `--noincompatible_stop_exporting_build_file_path` already unblocked `rules_auto_dotnet` root `build //...` (73/73 targets) and matched Bazel's `preset.update_test` execution on `yaml.bzl`.

2. **List-wrap `ctx.attr.<label>` for non-split Starlark transitions (2 repos: `yaml.bzl`, `rules_pylyglot`)**
   - `rules_go`'s `non_go_reset_target` (`transition.bzl:288`) indexes `ctx.attr.dep[0]` on a 1:1 Starlark transition.
   - `attr_to_value` in `crates/zel-analysis/src/starlark_rule_driver.rs` unwraps single non-split transitioned attributes into a bare target, causing `host_index` (`crates/zel-analysis/src/starlark_rule_host.rs`) to fail with `unhashable or invalid provider key of type 'int'`. Fixing this unblocks `rules_go` and Gazelle builds.

3. **Allow `module_extension()` during `.bzl` initialization even while another extension is active (`rules_itest`)**
   - `make_module_extension` in `crates/zel-bzlmod/src/rt.rs` rejects top-level `module_extension()` calls if an outer module extension context is active on the stack. This blocks `gazelle`'s `go_deps` (loading `non_module_deps.bzl`) and `llvm` (loading `tar.bzl`'s `extensions.bzl`).

4. **Accept Starlark flags (`--//`, `--@`, `--no//`, `--no@`) on non-analysis commands (`rules_itest`)**
   - `crates/zel-options/src/parser.rs` only skips Starlark flags when `command_analyzes()` is true. Any workspace with `common --@repo//...` in `.bazelrc` immediately fails `zel query` and `zel shutdown` with exit code 2.

5. **Expose `native.bazel_version` in `zel-packages` `.bzl` evaluation (`rules_itest`)**
   - `zel-bzlmod` exposes `native.bazel_version`, but `zel-packages`'s `native` struct does not. This breaks `aspect_tools_telemetry` (pulled in by `aspect_rules_js`) during package loading.

6. **Preserve relative symlinks inside `rules_python` venv runfiles + fix `runfiles_manifest` self-cycle (`rules_pylyglot`)**
   - `build_runfiles_tree` (`crates/zel-exec/src/runfiles_tree.rs`) absolutizes manifest symlink targets via `exec_root.join(v)`, breaking relative `rules_python` venv interpreter symlinks (`_bundler.venv/bin/python3`) that only resolve relative to the runfiles tree.
   - Independently, `//:smoke_build_test` in `rules_pylyglot/e2e/smoke` triggers a one-node `cycle in the action graph` on `smoke_build_test.sh.runfiles_manifest` (`crates/zel-server/src/exec_phase.rs`).

7. **Align Bazel 9 symbol removals and `--incompatible_autoload_externally` (3 repos: `xml.bzl`, `rules_polyglot`, `rules_itest`)**
   - Stop unconditionally predeclaring removed Bazel 9 symbols (`CcInfo`, `JavaInfo`, `PyInfo`, `ProtoInfo`) in `ANALYSIS_ONLY_GLOBALS` (`crates/zel-packages/src/loader.rs`) and emit Bazel's removal diagnostic by default.
   - Honor `--incompatible_autoload_externally=+@rules_cc` (and `+sh_*`, etc.) instead of always routing to the removed-rule failure stub in `exports.bzl`.

8. **Fetch `use_repo` extension repositories when targeted directly on the command line (`rules_auto_dotnet`)**
   - `zel build @dotnet_projects//:diagnostics.json` in a workspace with no root packages fails with `No repository visible as '@rules_auto_dotnet++auto_dotnet+dotnet_projects'`, whereas `zel fetch` and `zel mod show_repo` resolve and execute the repo rule properly (`crates/zel-server/src/loading.rs`).

9. **Accept `rule(incompatible_use_toolchain_transition=...)`, preserve `.bzl` load tracebacks, and allow Gazelle's duplicate self-repo visibility (`rules_multirun`, `rules_polyglot`, `rules_itest`)**
   - Preserve nested Starlark tracebacks instead of collapsing errors into `initialization of module 'defs.bzl' failed` (`crates/zel-packages/src/loader.rs`).
   - Match Bazel 9's behavior on `aspect_bazel_lib` 2.x `rule()` kwargs and Gazelle's duplicate `//:__subpackages__` + `@self//:__subpackages__` visibility lists (`crates/zel-packages/src/loader_host.rs`).

10. **Server telemetry, overhead, and Bazelisk fallback ergonomics (cross-cutting)**
    - **Elapsed timer accounting & latency**: Several Zel commands reported `<1s` `INFO: Elapsed time` while taking `30s–160s` wall time (including warm builds in `yaml.bzl`, `rules_auto_dotnet`, and `rules_pylyglot`), indicating expensive work outside the measured phase and/or missing incremental caching on failed/repeated builds.
    - **Linux sandbox**: Zel consistently fell back to `processwrapper-sandbox` where Bazel 9 used `linux-sandbox`.
    - **`.bazelversion` fallback**: When a workspace pins an older Bazel version (e.g. `7.5.0`, `8.5.0`, `9.0.0`, `9.2.0`), running `zel` directly exits 36 trying to download a matching Zel release unless `USE_BAZEL_VERSION` / `USE_ZEL_VERSION` / `ZEL_SKIP_WRAPPER=true` is set.
