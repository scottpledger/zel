# Bazel vs Zel compatibility: xml.bzl

## Metadata

| Field | Value |
| --- | --- |
| Repo | https://github.com/scottpledger/xml.bzl.git (`scottpledger/xml.bzl`) |
| Clone | `/tmp/bazel-vs-zel/repos/xml-bzl` |
| Tested commit | `d666e8a1830746bc8a53fda5cb4df84a28f8cb0d` (`main`, 2026-03-11, "Merge pull request #2 from scottpledger/feat/de-gazelle") |
| `.bazelversion` | `7.5.0` (first line; rest is a comment). Not 9.3.2. |
| Bazel under test | 9.3.2 via Bazelisk 1.30.0 (`USE_BAZEL_VERSION=9.3.2 /tmp/bazel-vs-zel/bin/bazel --host_jvm_args=-Xmx1g`) |
| Bazel pin check | 7.5.0 via the same Bazelisk, output base `/tmp/bazel-vs-zel/ob/xml-bzl-bazel75` |
| Zel | release label 9.3.2, built from `6297ed54b710bb023dfc7c7f275e3c2515d9761a` (`/tmp/bazel-vs-zel/bin/zel`). `version` build time Fri Nov 20 13:56:07 2026. |
| OS / arch | Ubuntu 24.04.5 LTS, Linux 6.12.113+ x86_64 |
| Date | 2026-11-20 |
| Output bases | Bazel 9 `/tmp/bazel-vs-zel/ob/xml-bzl-bazel`, Zel `/tmp/bazel-vs-zel/ob/xml-bzl-zel` |
| Nested workspaces | none. One root `MODULE.bazel`. `.bazelignore` lists `e2e/`, but that directory is not in the tree. No `examples/` or `tests/` module root. |

`shutdown` was run on the Bazel 9, Zel, and Bazel 7.5.0 output bases. No `xml-bzl` server was left running.

Zel was invoked with `USE_BAZEL_VERSION=9.3.2`, the same override Bazelisk uses. Without it, Zel refuses to start in this repo (see Startup).

Wall times were taken on a 4-core host that was also running other Bazel and Zel servers. Prefer exit codes, target sets, and server `Elapsed time` over wall clock.

## Executive verdict

**PARTIAL**

The XML rules and their Starlark unit tests match. `query` of every package Bazel 9 can load is identical, `//:all` + `//private/...` builds on both, and `test //tests/...` is 136/136 passed on Zel. Bazel 9.3.2 runs those same 136 tests only with `--ignore_dev_dependency` (136 `test.xml` files, `failures="0"`, 681 actions, the same action count Zel reported).

Workspace-wide `//...` does not match. Bazel 9.3.2 cannot load `//tools/gazelle` because gazelle 0.47.0 pulls rules_go 0.53.0, which still names the removed native `CcInfo`. `query //...`, `build //...`, and `test //...` all die there (query exit 7; build/test exit 1; 0 tests run). Zel still predeclares `CcInfo`, so it loads the same graph Bazel 7.5.0 loads (161 targets, set-equal) and then aborts `//...` later, in `bazel_lib` 3.0.0 `diff_test`, on Bazel 9's removal of `ctx.build_file_path`. Pinned Bazel 7.5.0 `test //...` passes 137/137, including `//tools:preset.update_test` and a built gazelle binary. Zel matches neither Bazel 9's load failure nor Bazel 7's full-suite success.

## Workspace and CI

Root module `xml.bzl` (`MODULE.bazel`, `version = ""`). Direct deps:

- `bazel_lib` 3.0.0
- `bazel_skylib` 1.8.2
- `package_metadata` 0.0.6
- `bazelrc-preset.bzl` 1.6.0
- dev: `gazelle` 0.47.0 (`repo_name = "bazel_gazelle"`), `bazel_skylib_gazelle_plugin` 1.8.2, `buildifier_prebuilt` 8.2.1

Root dev deps are active. gazelle 0.47.0 depends on rules_go 0.53.0 (`external/gazelle+/MODULE.bazel`). That module extension registers `@go_toolchains//:all`, so a broken rules_go toolchain poisons every analysis, not only `//tools/gazelle`.

`WORKSPACE.bazel` is only a marker comment. `REPO.bazel` sets `default_package_metadata = ["//:package_metadata"]`.

`.bazelrc` imports `%workspace%/tools/preset.bazelrc` and sets:

- `common --config=ruleset` (`--check_direct_dependencies=off` in the preset)
- `common --enable_bzlmod`
- `common --lockfile_mode=update`
- `try-import %workspace%/.bazelrc.user`

Committed `MODULE.bazel.lock` is `lockFileVersion` 13. Bazel 9.3.2 and Zel each rewrote it to version 28 during `query`; those two files were byte-identical (`diff -q`). The committed lock was restored before the Bazel 7.5.0 run. After that run the tree was clean again (`lockFileVersion` 13).

Packages: `//` (`bzl_library` `//:xml`, license / package metadata), `//private` (tokenizer, parser, dom, serializer, builder), `//tests` (eight `unittest.suite`s), `//tools` (`bazelrc_preset` → `//tools:preset.update_test`), `//tools/gazelle` (`gazelle_binary` + `gazelle`), `//.github/workflows` (`buildifier.check`).

`.github/workflows/ci.yaml` calls `bazel-contrib/.github/.github/workflows/bazel.yaml@v6` on folder `"."`, excluding `{bzlmodEnabled: false}` and `{bazelVersion: "6.4.0"}`. It does not pin 9.3.2. Other workflows: `buildifier.yaml` runs `bazel run --enable_bzlmod //.github/workflows:buildifier.check`; `ci.bazelrc` adds `--config=ci` and `--disk_cache=~/.cache/bazel`; `publish.yaml` / `release.yaml` / `tag.yaml` are BCR publish and tag jobs, not build/test. `.bcr/presubmit.yml` tests Bazel `7.x` and `8.x` on debian11, macos, ubuntu2204, and windows with:

```yaml
test_targets:
  - "//..."
  - "-//tools:preset.update_test"
```

BCR already drops `//tools:preset.update_test`. It does not list Bazel 9.

## Summary

| Workspace | Command | Bazel 9.3.2 | Zel 9.3.2 |
| --- | --- | --- | --- |
| root | `query //...` | exit 7, 0 targets, wall 4.610s. Load error in `tools/gazelle` (`CcInfo` removed). | exit 0, **161** targets, wall 37.387s (fetch + load). |
| root | `build //...` cold | exit 1, elapsed 1.241s, wall 1.404s, 0 processes. Same `CcInfo` load error in `rules_go` `binary.bzl`. | exit 1, elapsed 44.675s, wall 45.237s, 1 internal. Analysis abort on `//tools:preset.update_test` (`ctx.build_file_path`). 0 targets built. |
| root | `build //...` warm | exit 1, elapsed 0.407s, wall 0.438s. Same load error. | exit 1, elapsed 2.820s, wall 3.049s. Same `build_file_path` abort. |
| root | `test //... --test_output=errors` | exit 1, elapsed 0.949s, wall 1.042s. "Couldn't start the build. Unable to run tests." 0 tests. | exit 1, elapsed 6.830s, wall 7.422s. Same `preset.update_test` analysis abort. 0 tests. |
| root (extra) | `query //:all + //private/... + //tests/... + //tools:all + //.github/...` | exit 0, **158** targets, wall 1.304s | (from the 161) the same 158. Only Zel extras: `//tools/gazelle:gazelle`, `:gazelle-runner`, `:gazelle_bin`. |
| root (extra) | `query 'kind(test, //tests/...)'` | exit 0, **144** targets, wall 0.276s | exit 0, **144** targets, wall 0.136s. Sets equal. |
| root (extra) | `build //:all //private/...` | exit 0, 10 targets, elapsed 0.543s, wall 0.586s | exit 0, 10 targets, elapsed 0.018s, wall 0.347s |
| root (extra) | warm `build //:all //private/...` | exit 0, elapsed 0.544s, wall 0.587s, 1 action | exit 0, elapsed 0.008s, wall 0.318s, 1 action |
| root (extra) | `test //tests/... --test_output=errors` | exit 1, elapsed 1.057s, wall 1.145s. `//tests:builder_test_test_9` toolchain `@go_toolchains//:all` fails loading `cgo.bzl` (`CcInfo`). "No test targets were found, yet testing was requested." | exit 0, **136 / 136** tests, elapsed 2.324s, wall 6.924s, 681 actions (545 internal, 272 processwrapper-sandbox). 136 `test.xml` with `failures="0"` and `errors="0"`. |
| root (extra) | warm `test //tests/...` | not re-run (cold already failed) | exit 0, elapsed 0.154s, wall 3.918s, 1 action (cache hit) |
| root (extra) | `test --ignore_dev_dependency //tests/...` | exit 0, **136 / 136**, elapsed 5.490s, wall 5.597s, 681 actions. 136 `test.xml` clean. | not required; default `//tests/...` already passed |
| root (extra) | `build --ignore_dev_dependency //...` | exit 1, wall 0.946s. `//tools/gazelle` cannot see `@bazel_gazelle` once the dev dep is ignored. | not run |
| pin 7.5.0 | `query //...` | exit 0, **161** targets, wall 0.893s. Set **equal** to Zel. | — |
| pin 7.5.0 | `test //... --test_output=errors` | exit 0, **137 / 137** tests (136 under `//tests` plus `//tools:preset.update_test`), 15 non-test targets, elapsed 114.413s, wall 119.760s, 624 actions. 137 `test.xml` clean. Includes linking `//tools/gazelle:gazelle_bin`. | Zel `test //...` does not reach these tests. |

`//tests` is 136 `*_test_N` rules plus 8 `unittest.suite` targets (144 `kind(test, ...)` hits, because Bazel `kind()` is a regex and `test_suite` matches). The 137th test on Bazel 7.5.0 is `//tools:preset.update_test`.

## Detailed findings

### Startup: `.bazelversion` 7.5.0

Running the Zel binary with cwd in the repo and no version override exits before any command, including `help`:

```
ERROR: The project requires Zel version '7.5.0' (specified in /tmp/bazel-vs-zel/repos/xml-bzl/.bazelversion), but the running binary is version '9.3.2' and version '7.5.0' could not be found locally or downloaded from https://github.com/scottpledger/zel/releases/download.
```

`USE_BAZEL_VERSION=9.3.2` selects the running 9.3.2 binary (Zel implements the Bazelisk order in `crates/zel-client/src/bazelisk.rs`). All comparison commands below used that override. Bazelisk with `USE_BAZEL_VERSION=9.3.2` likewise ignores the pin. This is wrapper parity, not a graph bug. There is no published Zel 7.5.0 at that download URL, so a bare `zel` in this checkout cannot fall back the way Bazelisk falls back to Bazel 7.5.0.

### `query //...` — Bazel 9 fails, Zel returns the Bazel 7 graph

Bazel 9.3.2:

```
ERROR: /tmp/bazel-vs-zel/ob/xml-bzl-bazel/external/rules_go+/go/private/rules/cross.bzl:86:13: The CcInfo symbol has been removed, add the following to your BUILD/bzl file:

load("@rules_cc//cc/common:cc_info.bzl", "CcInfo")
ERROR: error loading package under directory '': error loading package 'tools/gazelle': at .../external/gazelle+/def.bzl:49:5: at .../gazelle+/internal/generationtest/generationtest.bzl:19:6: at .../rules_go+/go/def.bzl:47:5: compilation of module 'go/private/rules/cross.bzl' failed
```

Exit 7. Zero targets.

Zel exit 0, 161 labels. Diff against Bazel 7.5.0 `query //...`: empty both ways. Diff against the Bazel 9 query of every package that does not load gazelle (158 labels): Zel's only extras are the three `//tools/gazelle` targets.

### `build //...` and `test //...` — different fatal errors

Bazel 9 cold build (and the identical warm rebuild, and `test //...`) never gets a target configured. After the cross.bzl failure, build also reports the same removal inside `go/private/rules/binary.bzl` (lines 71, 108, 216) and:

```
ERROR: Skipping '//...': error loading package under directory '': error loading package 'tools/gazelle': at .../gazelle+/def.bzl:49:5: ... compilation of module 'go/private/rules/binary.bzl' failed
ERROR: Build did NOT complete successfully
```

`test` adds `ERROR: Couldn't start the build. Unable to run tests`.

Zel cold `build //...` fetches the module graph, runs rules_go's gazelle non-module Go tool build (`go install` of gazelle / fetch_repo / generate_repo_config with the downloaded Go 1.22.7 SDK), then aborts analysis:

```
ERROR: /tmp/bazel-vs-zel/repos/xml-bzl/tools/BUILD.bazel:3:15: in _diff_test rule //tools:preset.update_test: /tmp/bazel-vs-zel/ob/xml-bzl-zel/external/bazel_lib+/lib/private/diff_test.bzl:89:37: Deprecated API: ctx.build_file_path has been removed. Use ctx.label.package + '/BUILD' instead of ctx.build_file_path.
Use --noincompatible_stop_exporting_build_file_path to temporarily continue using it.
ERROR: Analysis of target '//tools:preset.update_test' failed; build aborted
```

`diff_test.bzl` line 89 is the template substitution `"{build_file_path}": ctx.build_file_path`. `//tools:preset` is a `bazelrc_preset` that instantiates that rule as `preset.update_test`. Warm `build //...` and `test //...` fail on the same line in about 3s and 7s. No test under `//tests` is executed, because Bazel and Zel both abort the build on the first analysis error unless `--keep_going` is set. `--keep_going` was not part of the requested `//...` commands.

Bazel 9 never reaches `ctx.build_file_path` for this target. Building `//tools:preset.update_test` alone still dies in toolchain resolution:

```
ERROR: .../rules_go+/go/private/rules/cgo.bzl:101:12: The CcInfo symbol has been removed, ...
ERROR: /tmp/bazel-vs-zel/repos/xml-bzl/tools/BUILD.bazel:3:15: While resolving toolchains for target //tools:preset.update_test (6b73009): invalid registered toolchain '@go_toolchains//:all': while parsing '@go_toolchains//:all': error loading package '@@rules_go++go_sdk+go_toolchains//': at .../go/private/go_toolchain.bzl:22:6: at .../go/private/actions/archive.bzl:36:5: compilation of module 'go/private/rules/cgo.bzl' failed
```

The same toolchain error is what kills `test //tests/...` and `build //:all //private/... //tests/... //tools:all //.github/...` on Bazel 9 (first victim in one run was `//tests:builder_test_test_11`, in another `//tests:builder_test_test_9`). The XML `bzl_library` targets themselves do not need a Go toolchain: `build //:all //private/...` succeeds on both tools (10 targets: `//:xml`, `//:xml.doc_extract`, `//:xml.docs-as-proto`, `//:license`, `//:package_metadata`, and the five `//private` libraries).

### The 136 XML tests do match

Zel `test //tests/... --test_output=errors`:

```
INFO: Analyzed 136 targets (1 packages loaded, 136 targets configured).
INFO: Found 136 test targets...
INFO: Elapsed time: 2.324s, Critical Path: 0.01s
INFO: 681 processes: 545 internal, 272 processwrapper-sandbox.
INFO: Build completed successfully, 681 total actions
```

Exit 0. Zel did not print Bazel's usual `Executed N out of N tests` line. Passes were checked from the 136 `test.xml` files under the Zel testlogs (`failures="0"`, `errors="0"`) and from the `test.log` header `Executing tests from //tests:...`. A second run was a cache hit (elapsed 0.154s, 1 action, exit 0).

Bazel 9.3.2 `test --ignore_dev_dependency //tests/...` drops the root dev deps (`gazelle`, `bazel_skylib_gazelle_plugin`, `buildifier_prebuilt`), so `@go_toolchains` is not registered:

```
INFO: Analyzed 136 targets (88 packages loaded, 1056 targets configured).
INFO: Found 136 test targets...
INFO: Elapsed time: 5.490s, Critical Path: 0.18s
INFO: Build completed successfully, 681 total actions
```

Exit 0, 136 clean `test.xml` files. Same action count as Zel. This is the apples-to-apples result for the ruleset. It is not the default `//...` result: `--ignore_dev_dependency //...` still fails, because `tools/gazelle/BUILD.bazel` loads `@bazel_gazelle//:def.bzl` and that repo is no longer visible. Excluding gazelle but keeping the pattern `//...` also trips `.github/workflows` loading `@buildifier_prebuilt`.

### Pinned Bazel 7.5.0 succeeds where both 9.3.2 runs of `//...` fail

`.bazelversion` and BCR (`7.x` / `8.x`) are the supported range. Bazel 9.3.2's failure is a repo/dependency problem (gazelle 0.47.0 / rules_go 0.53.0 / bazel_lib 3.0.0 are not Bazel 9-clean), not a Zel-only bug. Confirmed on 7.5.0 with the committed lock:

- `query //...` exit 0, 161 targets, identical to Zel.
- `test //... --test_output=errors` exit 0. `INFO: Found 15 targets and 137 test targets...` and `INFO: Build completed successfully, 624 total actions` in 114.413s. All 137 `test.xml` files are clean. The log shows the Go SDK stdlib build and `GoLink tools/gazelle/gazelle_bin_/gazelle_bin`, so gazelle actually builds on 7.5.0. `ctx.build_file_path` still exists on 7.5.0, so `//tools:preset.update_test` passes there. BCR excludes that target anyway; on Bazel 9 / Zel it cannot be analyzed against bazel_lib 3.0.0.

## Root cause and next Zel fixes

1. **Removed native `CcInfo` is still a `.bzl` global.** Bazel 9's Starlark compiler rejects any reference to the predeclared name `CcInfo` with `The CcInfo symbol has been removed` and tells the author to `load("@rules_cc//cc/common:cc_info.bzl", "CcInfo")`. Zel still lists `"CcInfo"` in `ANALYSIS_ONLY_GLOBALS` in `crates/zel-packages/src/loader.rs` (around the provider-name table that also includes `JavaInfo`, `PyInfo`, `CcToolchainConfigInfo`). There is no "symbol has been removed" diagnostic under `crates/zel-starlark`. Because the name resolves, `rules_go` 0.53.0 loads, `@go_toolchains//:all` registers, and Zel walks a Bazel 7-shaped graph. That is why `query //...` matches Bazel 7.5.0 and not Bazel 9.3.2, and why Zel will build Go repository rules that Bazel 9 refuses to load. Fix: stop exporting the Bazel 9-removed natives by default (a removed-symbol table, not only `CcInfo`), and emit the same load error with the `rules_cc` hint. Until that lands, any module that still names native `CcInfo` will look "more compatible" on Zel than on Bazel 9 and can hide a broken toolchain from later analysis.

2. **`ctx.build_file_path` already matches Bazel 9.** `crates/zel-analysis/src/starlark_rule_host.rs` returns the Bazel message when `incompatible_stop_exporting_build_file_path` is true (the default). The abort of `//tools:preset.update_test` is bazel_lib 3.0.0 `lib/private/diff_test.bzl:89`, which BCR already excludes. No Zel change is required for that flag. Bazel 9 would fail the same target if rules_go loaded. Zel's `//...` exit 1 is therefore a real Bazel 9 incompatibility in the repo, reached only because of gap (1). Do not "fix" it by weakening the flag default.

3. **No Zel change needed for the XML rules themselves.** Tokenizer / parser / DOM / serializer / builder and the eight `unittest` suites (`//tests:xml_parser_test` through `:builder_test`) analyze and pass 136/136 on Zel, and pass 136/136 on Bazel 9.3.2 once the gazelle dev dep is not registering a broken Go toolchain. Library-only `bzl_library` builds match (10 targets).

Not in scope for a Zel patch: bumping this repo's gazelle / rules_go / bazel_lib so that default `//...` works on Bazel 9. That is upstream of Zel. A rules_go that loads `CcInfo` from `rules_cc`, plus a bazel_lib `diff_test` that stops reading `ctx.build_file_path`, is what would make Bazel 9 and a CcInfo-correct Zel agree on `test //...`.
