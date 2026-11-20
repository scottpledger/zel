# Bazel vs Zel compatibility: yaml.bzl

## Metadata

| Field | Value |
| --- | --- |
| Repo | https://github.com/scottpledger/yaml.bzl.git (`scottpledger/yaml.bzl`) |
| Clone | `/tmp/bazel-vs-zel/repos/yaml-bzl` |
| Tested commit | `5c99625b87ba0ffa24c77eef5b6d1ef28b020d1d` (`main`, 2026-04-17, "Merge pull request #3 from scottpledger/fix/force-bump") |
| `.bazelversion` | `7.5.0` (first line; the rest of the file is comments). Bazel was still invoked as 9.3.2. Bazel 7.5.0 was not re-run: 9.3.2 builds the repo, and the only Bazel test failure is the preset diff CI already excludes on non-canonical versions. |
| Bazel | 9.3.2 via Bazelisk 1.30.0 (`USE_BAZEL_VERSION=9.3.2 /tmp/bazel-vs-zel/bin/bazel --output_base=/tmp/bazel-vs-zel/ob/yaml-bzl-bazel --host_jvm_args=-Xmx1g`) |
| Zel | release label 9.3.2, built from `6297ed54b710bb023dfc7c7f275e3c2515d9761a` (`/tmp/bazel-vs-zel/bin/zel --output_base=/tmp/bazel-vs-zel/ob/yaml-bzl-zel`). Same `USE_BAZEL_VERSION=9.3.2` so the provided binary runs in-process instead of trying to fetch Zel 7.5.0. |
| OS / arch | Ubuntu 24.04.5, Linux 6.12.113+ x86_64 |
| Date | 2026-11-20 |
| Output bases | Bazel `/tmp/bazel-vs-zel/ob/yaml-bzl-bazel`, Zel `/tmp/bazel-vs-zel/ob/yaml-bzl-zel` |
| Nested workspaces | none. Only root `MODULE.bazel` and `WORKSPACE.bazel`. `.bazelignore` lists `e2e/`, and that directory is not in the tree. |

Both output bases were shut down afterward (`shutdown` exit 0). No `yaml-bzl` server processes remained. `build` and `test` were passed `--keep_going` so every analysis error was collected. `test` also used `--test_output=errors`.

## Executive verdict

**PARTIAL**

`query //...` matches: both tools list the same 98 rules. All 82 Starlark `unittest` targets under `//tests` pass on both Bazel 9.3.2 and Zel. Those are the product tests (parser, serializer, errors).

`build //...` is green on Bazel (exit 0, 94 targets) and red on Zel (exit 1, 91 of 94 configured). Zel cannot analyze `//tools:preset.update_test` because `ctx.build_file_path` is treated as removed, and it cannot analyze the two non-manual Gazelle targets because a non-split Starlark transition does not make `ctx.attr.dep` a list. `test //...` is exit 3 on Bazel (82 pass, 1 fail: the preset diff, which CI excludes) and exit 1 on Zel (those 82 pass, and the preset test never runs because analysis fails). With `--noincompatible_stop_exporting_build_file_path`, Zel runs the preset test and fails it the same way Bazel does.

## Workspace and CI

Single Bzlmod module `yaml.bzl` (`version = ""`). Direct deps: `bazel_lib` 3.0.0, `bazel_skylib` 1.8.2, `package_metadata` 0.0.6, `bazelrc-preset.bzl` 1.6.0. Dev deps: `gazelle` 0.50.0 (`repo_name = "bazel_gazelle"`), `bazel_skylib_gazelle_plugin` 1.8.2, `buildifier_prebuilt` 8.2.1.

Packages: `//` (`bzl_library` + package metadata), `//private`, `//tests` (three `unittest` suites), `//tools` (`bazelrc_preset`), `//tools/gazelle` (`gazelle` / `gazelle_binary`), `//.github/workflows` (`buildifier`).

`.bazelrc` imports `tools/preset.bazelrc`, then sets `common --config=ruleset`, `common --enable_bzlmod`, and `common --lockfile_mode=update`. The committed lock (54,756 bytes, 484 lines) was therefore rewritten during these runs. After both `query` invocations the on-disk locks were identical (36,948 bytes). A later build grew the file again (61,080 bytes at the end). That is `--lockfile_mode=update` moving a Bazel 7 lock forward, not a query-result mismatch.

`.github/workflows/ci.yaml` calls `bazel-contrib/.github` `bazel.yaml@v7.4.0` with `bazel_versions: ["9.x", "8.x", "7.x"]`, folder `.`, and:

```text
bazel test //... -- -//tools:preset.update_test
```

WORKSPACE mode and Bazel 6.4.0 are excluded. A separate job runs `bazel test //tools:preset.update_test` with Bazelisk and the repo `.bazelversion` (7.5.0), because the generated preset depends on the running Bazel version and cannot match every matrix cell. Other workflows (`buildifier.yaml`, `release.yaml`, `publish.yaml`, `tag.yaml`, `conventional-commits.yaml`) are lint/release jobs, not `bazel test //...`.

Launcher note: the first Zel invocation in this tree, without `USE_BAZEL_VERSION`, exited 36:

```text
ERROR: The project requires Zel version '7.5.0' (specified in /tmp/bazel-vs-zel/repos/yaml-bzl/.bazelversion), but the running binary is version '9.3.2' and version '7.5.0' could not be found locally or downloaded from https://github.com/scottpledger/zel/releases/download.
```

That is Zel's Bazelisk-compatible resolver (`crates/zel-client/src/bazelisk.rs`: `USE_ZEL_VERSION`, then `USE_BAZEL_VERSION`, then `.zelversion`, then `.bazelversion`). `USE_BAZEL_VERSION=9.3.2` selects the provided binary, matching how Bazelisk was pinned to 9.3.2. Every result below uses that override.

## Summary

Wall times are from a 4-core VM that was also running other Bazel and Zel servers (load was high enough that a Zel `build //...` whose own log says `Elapsed time: 0.08s` and `1 process` still took 81–153s of wall clock). Compare exit codes, target sets, and the tool-reported elapsed times. Wall clock is contaminated.

`query //...` returns 98 rules. `build` / `test //...` analyze 94: Bazel omits the three `test_suite` rules (`//tests:yaml_{parser,serializer,error}_test`) and the `manual`-tagged `//tools/gazelle:gazelle`. Zel reports the same 94-target pattern.

| Workspace | Command | Bazel 9.3.2 | Zel |
| --- | --- | --- | --- |
| root | `query //...` | exit 0, 98 targets, reported ~3.7s | exit 0, 98 targets, same set, wall 17.6s |
| root | `build //...` cold | exit 0, 94 analyzed, 418 actions, elapsed 63.8s, wall 64.0s | exit 1, 94 analyzed / 91 configured, 340 internal actions, elapsed 0.078s, wall 172s |
| root | `build //...` warm | exit 0, 1 internal action, critical path 0.01s, elapsed 2.85s, wall 2.95s | exit 1, same 3 analysis errors, elapsed 0.08s (repeat 0.34s), wall 81s then 153s |
| root | `test //... --test_output=errors` | exit 3, 83 tests run, 82 pass, 1 fail, elapsed 2.48s, wall 2.66s | exit 1, 82 tests run, 82 pass, 0 executed failures, elapsed 4.14s, wall 167s |
| root (diagnostic) | `test //tools:preset.update_test --noincompatible_stop_exporting_build_file_path` | not required (Bazel default already exports `ctx.build_file_path`) | exit 3, same preset diff failure as Bazel, elapsed 0.24s, wall 1.9s |

Bazel test summary: `Found 11 targets and 83 test targets`. Zel test summary: `Found 9 targets and 82 test targets` plus `Build succeeded for only 91 of 94 top-level targets`. The three missing targets are exactly the analysis failures below (two non-test Gazelle targets and one test).

## Detailed findings

### 1. Query parity

Sorted `query //...` labels are identical (98). The set is the three `bzl_library` / metadata targets, 82 `unittest` rules, 3 `test_suite`s, stardoc extract + docs filegroup, buildifier, the preset generate/update/diff trio, and the three Gazelle rules (`gazelle`, `gazelle-runner`, `gazelle_bin`).

### 2. YAML unit tests pass on both

`//tests:yaml_parser_test_test_{0..38}` (39), `//tests:yaml_error_test_test_{0..25}` (26), and `//tests:yaml_serializer_test_test_{0..16}` (17) are skylib `unittest` rules. Bazel: 82 PASSED. Zel: 82 PASSED. No assertion diffs. This is the yaml.bzl library itself (parse, dump, errors), loaded from `//:yaml` and `//private:yaml_impl`.

### 3. Bazel `test //...` fails only `//tools:preset.update_test`

Exit 3. The other 82 tests passed. The failure is a content diff, not an analysis error. Excerpt from the Bazel test log:

```text
FAIL: files "tools/_preset.bazelrc" and "tools/preset.bazelrc" differ. The bazelrc preset has changed. Run 'bazel run //tools:preset.update' to update it.
```

The diff is the Bazel 9 preset versus the file committed for the 7.5.0 pin (extra `coverage --build_runfile_links`, downloader/cache retry flags, `incompatible_disallow_empty_glob` in place of `incompatible_enforce_starlark_utf8`, `nolegacy_external_runfiles` in place of `module_mirrors`, and similar). CI runs `bazel test //... -- -//tools:preset.update_test` on the 7/8/9 matrix for this reason, and checks the preset only with `.bazelversion`. Treat this Bazel failure as expected on 9.3.2, not as a Zel gap.

`build //...` on Bazel still succeeds (exit 0), including `//tools/gazelle:gazelle_bin` and `//tools/gazelle:gazelle-runner` (rules_go / Go SDK compile and link, 53 linux-sandbox actions, critical path 46s).

### 4. Zel drops `ctx.build_file_path` by default

`//tools:preset.update_test` is a `bazel_lib` `diff_test`. Analysis fails before the test runs:

```text
ERROR: /tmp/bazel-vs-zel/repos/yaml-bzl/tools/BUILD.bazel:3:15: in _diff_test rule //tools:preset.update_test:
  .../external/bazel_lib+/lib/private/diff_test.bzl:89:37:
  Deprecated API: ctx.build_file_path has been removed. Use ctx.label.package + '/BUILD' instead of ctx.build_file_path.
Use --noincompatible_stop_exporting_build_file_path to temporarily continue using it.
ERROR: Analysis of target '//tools:preset.update_test' failed
```

`diff_test.bzl` writes the path into the test script:

```starlark
"{build_file_path}": ctx.build_file_path,
```

Bazel 9.3.2 still provides that field: the same target analyzes, executes, and fails only on the file diff.

Repeating the one target on Zel with the flag clears analysis and reproduces Bazel's failure (exit 3, same `tools/_preset.bazelrc` vs `tools/preset.bazelrc` message). The diff logic is fine. The default is not.

### 5. Zel rejects `ctx.attr.dep[0]` on rules_go's non-split transition

The two Gazelle targets that `build //...` includes fail while analyzing a `non_go_reset_target`:

```text
ERROR: .../external/rules_go++go_sdk+go_default_sdk/BUILD.bazel:90:20:
  in non_go_reset_target rule @@rules_go++go_sdk+go_default_sdk//:builder_reset:
  .../external/rules_go+/go/private/rules/transition.bzl:288:21:
  unhashable or invalid provider key of type 'int'; expected a provider
ERROR: Analysis of target '@@rules_go++go_sdk+go_default_sdk//:builder_reset' failed
WARNING: errors encountered while analyzing target '//tools/gazelle:gazelle-runner', it will not be built.
WARNING: errors encountered while analyzing target '//tools/gazelle:gazelle_bin', it will not be built.
```

`//tools/gazelle:gazelle` itself is tagged `manual`, so neither tool builds it as part of `//...`. The Go toolchain never compiled under Zel (340 internal actions, critical path 0.03s, no sandbox compiles). Bazel did compile it.

Line 288 of `transition.bzl` is the transition indexing, not the provider comprehension on the next line:

```starlark
t = ctx.attr.dep[0]  # [0] seems to be necessary with the transition
providers = [t[p] for p in [GoInfo, GoArchive] if p in t]
```

`dep` is `attr.label(cfg = non_go_tool_transition)` (and the same impl is used for `go_tool_transition`). Bazel makes `ctx.attr.<label>` a list when the attribute has a Starlark transition, including a 1:1 transition. Zel indexes the target with `0` and then rejects the int because target indexing only accepts providers (`host_index` in `crates/zel-analysis/src/starlark_rule_host.rs`).

### 6. Timing caveat

Zel's reported elapsed time for the cold `build //...` is 0.078s with 340 internal actions, while wall clock was 172s. Warm repeats stayed under 0.4s of reported elapsed and over a minute of wall clock. Bazel cold build elapsed (63.8s) matched wall clock (64.0s) and included the Go compile. Do not read the Zel wall times as the action graph. The functional gap is the three analysis failures, not a slow no-op of the YAML tests (those 82 tests ran in a reported 4.1s).

## Root cause and next fixes

1. **Default `incompatible_stop_exporting_build_file_path` to false, and honor that default.** `ctx.build_file_path` in `crates/zel-analysis/src/starlark_rule_host.rs` uses `cfg.bool_option("incompatible_stop_exporting_build_file_path", true)`. The fallback is `true`. `StarlarkSemantics::default` in `crates/zel-starlark/src/semantics.rs` also sets the field to `true`. `crates/zel-options/src/flag_tables.rs` already documents the Bazel default as `false`. Bazel 9.3.2 still exports the field. Until the live default matches, `bazel_lib` `diff_test` (and anything else still reading `ctx.build_file_path`) fails analysis on Zel and passes analysis on Bazel. Workaround confirmed: `--noincompatible_stop_exporting_build_file_path`.

2. **List-wrap label attributes that use a Starlark transition, including non-split transitions.** `attr_to_value` in `crates/zel-analysis/src/starlark_rule_driver.rs` returns the raw value when `cfgs.len() == 1 && cfgs[0].0.is_none()`. `apply_transition_split_with_exec_ctx` in `crates/zel-analysis/src/transition.rs` documents that a non-split transition uses key `None`. Bazel still exposes `ctx.attr.dep` as a one-element list, which is why rules_go writes `ctx.attr.dep[0]`. That mismatch is what turns the subscript into "provider key of type 'int'" and blocks `//tools/gazelle:gazelle_bin` and `//tools/gazelle:gazelle-runner`. Fix the `ctx.attr` shape for transitioned `attr.label` (and keep `ctx.split_attr` keyed). No Zel source was changed in this run.

No other command divergence showed up. Module resolution for `query` produced the same 98 targets and, at that point, the same lockfile. There is no nested workspace to compare.
