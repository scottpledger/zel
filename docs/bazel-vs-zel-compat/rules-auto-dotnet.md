# Bazel vs Zel compatibility: `rules_auto_dotnet`

## Metadata

| Field | Value |
| --- | --- |
| Repo | https://github.com/scottpledger/rules_auto_dotnet.git |
| Clone | `/tmp/bazel-vs-zel/repos/rules-auto-dotnet` |
| Tested commit | `3d8b235147c538dc6524f9769e3ed9b6e25a6933` (`main`, 2026-03-17, "Merge pull request #8 from scottpledger/fix/bcr-failure") |
| `.bazelversion` | `8.5.0` (first line; comments follow). `MODULE.bazel` sets `bazel_compatibility = [">=8.0.0"]`. |
| Bazel | 9.3.2 via Bazelisk 1.30.0 (`USE_BAZEL_VERSION=9.3.2 /tmp/bazel-vs-zel/bin/bazel`), `--host_jvm_args=-Xmx1g` |
| Zel | `/tmp/bazel-vs-zel/bin/zel` built from `6297ed54b710bb023dfc7c7f275e3c2515d9761a`. `zel version` reports build label `9.3.2`, target `@@//crates/zel-client:zel`, build time Fri Nov 20 13:56:07 2026. Invoked with `USE_BAZEL_VERSION=9.3.2` so the embedded Bazelisk layer stays on this binary (see startup finding). |
| OS / arch | Linux 6.12.113+ x86_64 |
| Date | 2026-11-20 |
| Output bases | Bazel `/tmp/bazel-vs-zel/ob/rules-auto-dotnet-bazel` (+ `-e2e-smoke`, `-e2e-strict-failure`). Zel `/tmp/bazel-vs-zel/ob/rules-auto-dotnet-zel` (same suffixes). Optional Bazel 8.5.0 query used `/tmp/bazel-vs-zel/ob/rules-auto-dotnet-bazel85`. |
| Servers | `shutdown` was run on every output base above when finished. |

## Executive verdict

**PARTIAL**

`query //...` matches Bazel on every workspace that has targets (root 79/79, `e2e/smoke` 13/13). The BCR smoke module (`e2e/smoke`) builds and passes the same 3/3 tests under Zel. The root ruleset does not: with the repo's lockfile check bypassed so Bazel 9.3.2 can load it, Bazel `build //...` and `test //...` succeed (52/52 tests), and Zel aborts in analysis because `ctx.build_file_path` is treated as removed. The CI strict-diagnostics check also diverges: Bazel fails inside the `dotnet_projects` repo rule with the three expected diagnostic strings, while `zel build @dotnet_projects//:diagnostics.json` stops earlier with "No repository visible".

## What was compared

Workspaces (`.bazelignore` excludes `e2e/`, so the root `//...` does not include the nested modules):

- Root module `rules_auto_dotnet` (`MODULE.bazel`, `.bazelrc` imports `tools/preset.bazelrc`, `common --enable_bzlmod`, `common --lockfile_mode=error`, `common --config=ruleset`).
- `e2e/smoke` — BCR presubmit module (`bcr_test_module.module_path`). Own `MODULE.bazel` + empty `.bazelrc`. Local path override of `rules_auto_dotnet`.
- `e2e/strict_failure` — no `BUILD` file. CI runs `e2e/strict_failure/verify_strict_failure.sh`, which expects `bazel build @dotnet_projects//:diagnostics.json` to fail and to contain three diagnostic strings.

CI (`.github/workflows/ci.yaml`) runs bazel-contrib `bazel.yaml@v6` on folders `.` and `e2e/smoke` (bzlmod on; Bazel 6.4.0 excluded), plus `bazel test //auto_dotnet/tests:parser_test //auto_dotnet/tests:generator_test`, `bash e2e/strict_failure/verify_strict_failure.sh`, and `bash tools/phase0_scale_benchmark.sh 20`. Release/publish/tag workflows do not build the ruleset. This report covers `query`, `build`, `test`, and the strict-diagnostics build. The scale benchmark was not run.

Pinned Bazel 8.5.0 can `query //...` against the committed root lockfile (`lockFileVersion` 24): exit 0, 79 targets, wall 4.591s, same label set as Bazel 9.3.2 with `--lockfile_mode=off`. Bazel 9.3.2 itself rejects that lockfile (below).

## Summary

Wall times are end-to-end client times. Zel's own `INFO: Elapsed time` is in parentheses when it diverges a lot from wall time (module fetch and a memory-constrained host; the cgroup cap was ~14.6 GiB and was mostly occupied by other workloads).

| Workspace | Command | Bazel 9.3.2 | Zel | Match |
| --- | --- | --- | --- | --- |
| Root, repo rc (`--lockfile_mode=error`) | `query //...` | exit 48, 0 targets, 1.598s. `MODULE.bazel.lock` version is not supported | exit 48, 0 targets, 0.133s. Same error text | Yes (same failure) |
| Root, `--lockfile_mode=off` | `query //...` | exit 0, **79** targets, 5.403s | exit 0, **79** targets, 29.040s. Label sets identical | Yes |
| Root, `--lockfile_mode=off` | `build //...` cold | exit 0, 21.462s, analyzed **73** targets, 236 actions | exit **1**, 79.415s (elapsed 36.948s). Analysis abort on `//tools:preset.update_test` | No |
| Root, `--lockfile_mode=off` | `build //...` warm | exit 0, 2.118s, 1 action | exit **1**, 59.127s (elapsed 16.047s). Same analysis error | No |
| Root, `--lockfile_mode=off --test_output=errors` | `test //...` | exit 0, 0.945s, **52/52 passed** (21 non-test + 52 test targets) | exit **1**, 39.215s (elapsed 4.475s). 0 tests run. "Couldn't start the build. Unable to run tests" | No |
| `e2e/smoke` | `query //...` | exit 0, **13** targets, 4.915s | exit 0, **13** targets, 0.589s. Identical labels | Yes |
| `e2e/smoke` | `build //...` cold | exit 0, 5.709s, **5** targets, 42 actions (33 internal, 9 linux-sandbox) | exit 0, 35.247s (elapsed 1.788s), **5** targets, 42 actions (33 internal, 9 processwrapper-sandbox) | Yes (sandbox kind differs) |
| `e2e/smoke` | `build //...` warm | exit 0, 1.193s, 1 action | exit 0, 23.047s (elapsed 1.153s), 1 action | Yes |
| `e2e/smoke --test_output=errors` | `test //...` | exit 0, 6.593s, **3/3 passed** | exit 0, 25.096s (elapsed 3.602s), **3/3 passed** | Yes |
| `e2e/strict_failure` | `query //...` | exit 7, 0 targets, 9.529s. `no targets found beneath ''` | exit 7, 0 targets, 0.584s. Same error | Yes |
| `e2e/strict_failure` | `build //...` | exit 1, 1.153s. `no targets found beneath ''` | exit 1, 0.536s (elapsed 0.050s). Same error | Yes |
| `e2e/strict_failure --test_output=errors` | `test //...` | exit 1, 5.216s. No tests started | exit 1, 0.299s (elapsed 0.050s). No tests started | Yes |
| `e2e/strict_failure` | `build @dotnet_projects//:diagnostics.json` | exit 1, 1.200s. Fails in `dotnet_projects_repo` with all 3 expected diagnostics | exit 1, 0.816s. `No repository visible as '@rules_auto_dotnet++auto_dotnet+dotnet_projects'`. Diagnostics absent | No |

`build //...` reports 73 targets where `query //...` reports 79. The six query-only labels are `//auto_dotnet:defs`, `//auto_dotnet:extensions`, and the four `*.doc_extract` / `*.docs-as-proto` targets those `bzl_library` rules create. Both Bazel and Zel (once analysis got past `ctx.build_file_path`) analyzed 73.

Smoke tests, both tools:

- `//:dotnet_projects_files_exist_test`
- `//:integration_scan_projects_test`
- `//:smoke_test`

Bazel times were about 0.0–0.1s; Zel reported about 1.0s each.

## Detailed findings

### 1. Root lockfile is incompatible with Bazel 9.3.2 (and with Zel's matching check)

`.bazelrc` sets `common --lockfile_mode=error`. Root `MODULE.bazel.lock` is `lockFileVersion` 24, written for the pinned Bazel 8.5.0. Both tools stop before loading packages:

```
ERROR: Error computing the main repository mapping: The version of MODULE.bazel.lock is not supported by this version of Bazel. Please run `bazel mod deps --lockfile_mode=update` to update your lockfile.
```

Exit code 48. The repo's own `.bazelrc` says to use `--lockfile_mode=update` or `--lockfile_mode=off` when testing another Bazel version. Every root command below used `--lockfile_mode=off` on both tools so the lockfile on disk stayed unchanged. `e2e/smoke` and `e2e/strict_failure` lockfiles are version 26 and load under Bazel 9.3.2 without that override. Their `.bazelrc` does not set `lockfile_mode`, so the default update mode rewrote `e2e/smoke/MODULE.bazel.lock` from version 26 to 28 during the session (registry hash churn, `factsVersions`). That file was restored with `git checkout` after the runs. `e2e/strict_failure/MODULE.bazel.lock` was restored the same way.

### 2. Zel startup honors `.bazelversion` by trying to download another Zel

Without `USE_BAZEL_VERSION`, in the repo:

```
ERROR: The project requires Zel version '8.5.0' (specified in /tmp/bazel-vs-zel/repos/rules-auto-dotnet/.bazelversion), but the running binary is version '9.3.2' and version '8.5.0' could not be found locally or downloaded from https://github.com/scottpledger/zel/releases/download.
```

That is `zel-client` `bazelisk.rs` (`plan_invocation` / `resolve_version_binary`). Bazelisk, given `USE_BAZEL_VERSION=9.3.2`, overrides `.bazelversion` and runs Bazel 9.3.2. The same variable makes this Zel binary run in-process, because the requested version equals its build label. Comparisons in this report set `USE_BAZEL_VERSION=9.3.2` for both binaries. A user who points `zel` at this tree and expects the current binary to run, the way a copied `bazel` binary does when Bazelisk is not wrapping it, will get this error instead.

### 3. Root build and test: `ctx.build_file_path` removed while Bazel 9.3.2 still provides it

Zel cold `build //...` (and the warm rebuild, and `test //...`) abort on the first analysis failure:

```
ERROR: /tmp/bazel-vs-zel/repos/rules-auto-dotnet/tools/BUILD.bazel:5:15: in _diff_test rule //tools:preset.update_test: /tmp/bazel-vs-zel/ob/rules-auto-dotnet-zel/external/bazel_lib+/lib/private/diff_test.bzl:89:37: Deprecated API: ctx.build_file_path has been removed. Use ctx.label.package + '/BUILD' instead of ctx.build_file_path.
Use --noincompatible_stop_exporting_build_file_path to temporarily continue using it.
ERROR: Analysis of target '//tools:preset.update_test' failed; build aborted
```

`//tools:preset.update_test` is the `bazelrc_preset` diff test from `@bazelrc-preset.bzl`. It expands through `@bazel_lib` `diff_test`, which substitutes `ctx.build_file_path` at `lib/private/diff_test.bzl:89`. Bazel 9.3.2 analyzes that target and the test passes (`//tools:preset.update_test PASSED in 0.0s`).

The flag table in `crates/zel-options/src/flag_tables.rs` records the Bazel default as `false` (`incompatible_stop_exporting_build_file_path`, "If set to true, deprecated ctx.build_file_path will not be available"). Two other defaults disagree with that table:

- `crates/zel-starlark/src/semantics.rs` `StarlarkSemantics::default()` sets `incompatible_stop_exporting_build_file_path: true`.
- `crates/zel-analysis/src/starlark_rule_host.rs` reads `cfg.bool_option("incompatible_stop_exporting_build_file_path", true)` and the fallback is `true`.
- `crates/zel-analysis/src/build_options.rs` `find_option` does not see this flag (`build_options_table` has no entry), so `BuildConfiguration::option_str` returns nothing and the call-site fallback wins.

`crates/zel-bzlmod/src/semantics_fp.rs` already lists the flag as `false` for the semantics fingerprint. Passing `--noincompatible_stop_exporting_build_file_path` makes Zel `build //...` succeed: exit 0, 73 targets, 234 actions, engine elapsed 1.302s (wall 66.030s). Bazel reported 236 actions for the same graph. A follow-up `zel test //...` with that flag did not finish: the Zel server was OOM-killed (`Memory cgroup out of memory: Killed process ... (zel) ... anon-rss:4945724kB`, exit 37, `transport error: failed to fill whole buffer`). About 0.6 GiB was free in the 14.6 GiB cgroup at the next check, so that kill is host pressure, and it is not evidence the tests themselves fail. Root test behavior with the flag left at Bazel's default remains "analysis aborted, 0 tests executed".

### 4. `e2e/smoke` matches, with a sandbox and symlink caveat

Query labels are identical (13). Cold builds both report 5 targets and 42 actions and succeed. Tests are 3/3 on both sides. The generated project scan emits the same three `DEBUG` warnings from `dotnet_projects_repo.bzl:299` (missing `paket.references`, two `InternalsVisibleTo` warnings).

Bazel executed the genrule/runfiles work in `linux-sandbox`. Zel used `processwrapper-sandbox`. Tests still passed.

Both tools write `bazel-bin`, `bazel-out`, `bazel-testlogs`, and `bazel-smoke` in the workspace directory. After Zel's build those links pointed at the Zel output base. A later Bazel `//...` then walked `bazel-smoke/external/zlib+` and failed with an unknown `rules_license` repo. Bazel ignores its own convenience symlinks only when they point at the current output base. Deleting the links and re-running Bazel produced the successful test and warm build in the table. Alternating Bazel and Zel in one tree clobbers those links.

### 5. Strict diagnostics: Bazel fails in the repo rule; Zel `build` never fetches it

`e2e/strict_failure` has no packages. `query`, `build //...`, and `test //...` agree (exit 7 for query, exit 1 for build/test, message `no targets found beneath ''`).

The CI script builds `@dotnet_projects//:diagnostics.json` and requires a non-zero exit plus these substrings:

- `Paket strict mode diagnostic: Workspace uses Paket projects but paket.dependencies is set-to-strict-without-bang and must include `references: strict!`.`
- `Paket diagnostic: Project imports Paket restore targets but no paket.references was found.`
- `InternalsVisibleTo diagnostic: Project is referenced by src/tests/CoreTests.csproj`

Bazel 9.3.2 does that. The failure is `fail()` at `auto_dotnet/private/dotnet_projects_repo.bzl:904` while fetching `rules_auto_dotnet++auto_dotnet+dotnet_projects`.

Zel `build` of the same target exits 1 in ~0.05s of engine time and does not run the repo rule:

```
ERROR: No repository visible as '@rules_auto_dotnet++auto_dotnet+dotnet_projects' from main repository
```

Repeating the build after `zel fetch` still prints only that line. `zel mod show_repo @dotnet_projects` does show the extension repo (`use_repo` name `dotnet_projects`, canonical name `rules_auto_dotnet++auto_dotnet+dotnet_projects`, `paket_diagnostics = "strict"`). `zel fetch @dotnet_projects//:diagnostics.json` runs the repo rule and prints the same three diagnostics (client exit 2, and the log prefixes a fetch error with `ERROR: INFO: Repo ...`). So the module extension and the strict `fail()` work on the fetch path. The `build` path treats the extension repo as invisible and never fetches it.

`e2e/smoke` still builds, because its `BUILD` file labels (`@dotnet_projects//src/...`) pull the repo in during loading of `//...`. The gap is specifically a command-line target in an extension repo when the main workspace has no package that depends on it. The message is produced from unknown-repo handling in `crates/zel-server/src/loading.rs` (`unknown_repo_message`), with the same string in `crates/zel-packages/src/lookup.rs`, `crates/zel-packages/src/resolve.rs`, `crates/zel-skyframe/src/lib.rs`, and `crates/zel-bzlmod/src/session.rs`.

`verify_strict_failure.sh` calls whatever `bazel` is on `PATH`. It was not executed as-is. The `build @dotnet_projects//:diagnostics.json` invocation above is the check that script performs. On Zel that invocation would fail the script's string search.

## Root-cause assessment / next fixes in Zel

1. **Default `ctx.build_file_path` to available (flag false), matching Bazel 9.3.2 and `flag_tables.rs`.** Wire `incompatible_stop_exporting_build_file_path` into the configuration option table so `bool_option` does not fall back to `true`, and set `StarlarkSemantics::default()` to `false`. Until that lands, `@bazel_lib` `diff_test` (and therefore `bazelrc-preset.bzl` `preset.update_test`) cannot analyze. This is what blocks root `build //...` and `test //...` for this repo. Crates: `zel-analysis` (`starlark_rule_host.rs`, `build_options` / `build_options_table`), `zel-starlark` (`semantics.rs`).

2. **Fetch `use_repo` module-extension repositories when they are the build target.** `zel build @dotnet_projects//:diagnostics.json` should enter `dotnet_projects_repo` the way `zel fetch` and Bazel `build` do, and surface the Starlark `fail()` text. Crates: `zel-server` loading, `zel-bzlmod` repo mapping, `zel-packages` lookup/resolve.

3. **Bazelisk compatibility when `.bazelversion` is not the embedded label.** Document or align with "this binary is already the selected Bazel" when the user invoked a concrete `zel` path. Today a `.bazelversion` of `8.5.0` makes this 9.3.2 binary try to download `zel` 8.5.0 from GitHub releases and exit. Crate: `zel-client` `bazelisk.rs`.

4. **Sandbox.** Smoke tests passed under `processwrapper-sandbox`. Root tests on Bazel used `linux-sandbox`. If later rules need the Linux sandbox (network namespace, hermeticity), that difference will show up; it did not fail this repo's tests.

5. **Action-count drift.** With `--noincompatible_stop_exporting_build_file_path`, Zel built the root graph in 234 actions and Bazel in 236. Worth a look after the `build_file_path` default is fixed and root tests can run, to see which two actions are missing.

No Zel source was modified for this report.
