# Bazel vs Zel compatibility: rules_polyglot

## Metadata

| Field | Value |
| --- | --- |
| Repo | https://github.com/scottpledger/rules_polyglot.git (`scottpledger/rules_polyglot`) |
| Tested commit | `32559d363ca11d1ffb031dab9bb89a0f33d2ffb1` (branch `main`, commit subject `Initial changes`, 2025-02-13) |
| Clone | `/tmp/bazel-vs-zel/repos/rules-polyglot` (shallow clone) |
| Bazel under test | 9.3.2 via Bazelisk v1.30.0 (`USE_BAZEL_VERSION=9.3.2 /tmp/bazel-vs-zel/bin/bazel --host_jvm_args=-Xmx1g`) |
| Pinned Bazel (reference only) | 7.2.1 from `.bazelversion` |
| Zel | native binary `/tmp/bazel-vs-zel/bin/zel`, build label 9.3.2, build target `@@//crates/zel-client:zel`, build time 2026-11-20 13:56:07 UTC, source commit `6297ed54b710bb023dfc7c7f275e3c2515d9761a` |
| OS / arch | Linux 6.12.113+ x86_64 |
| Date | 2026-11-20 |

Output bases:

- Bazel 9.3.2 root: `/tmp/bazel-vs-zel/ob/rules-polyglot-bazel`
- Zel root: `/tmp/bazel-vs-zel/ob/rules-polyglot-zel`
- Bazel 7.2.1 root (reference): `/tmp/bazel-vs-zel/ob/rules-polyglot-bazel721`
- Nested `e2e/smoke`: the same paths with suffix `-e2e-smoke`

Native Zel was forced with `USE_ZEL_VERSION=9.3.2` and `ZEL_SKIP_WRAPPER=true`. Without that, Zel’s built-in Bazelisk layer (`crates/zel-client/src/bazelisk.rs`) reads `.bazelversion` (`7.2.1`) and `exec`s real Bazel 7.2.1. A first unforced `query //...` did exactly that (Java server install label `7.2.1` on the Zel output base) and returned the same 21 targets as pinned Bazel. That run was discarded, the output base wiped, and every number below is from the native server (`zel --internal_server_daemon`, install md5 `e8c80ebd2ea524ddf4abedf435d244ef`).

## Executive verdict

**PARTIAL**

Native Zel matches Bazel 9.3.2 on the CI end-to-end workspace `e2e/smoke`: identical `query //...` target set (6), successful cold `build //...`, `test //...` (`//:smoke_test` passed, 1/1), and a warm no-op build. The root workspace `//...` fails on both Bazel 9.3.2 and native Zel, so there is no Bazel 9 success for Zel to miss, but the failures are not the same. Bazel 9 stops while loading Gazelle’s `rules_go` 0.44.0 because bare `CcInfo` was removed; Zel still predeclares `CcInfo`, keeps going, and then dies in `gazelle` 0.35.0 on `native.sh_binary`. The version this repo actually supports, Bazel 7.2.1, builds and tests the root workspace cleanly (2/2 tests).

## Workspace layout and CI

Single Bzlmod module `rules_polyglot` (`MODULE.bazel`, `compatibility_level = 1`). `WORKSPACE.bazel` is only a marker. `.bazelignore` is `e2e/`, so the root `//...` does not include the nested workspace.

Direct deps: `bazel_skylib` 1.4.1, `platforms` 0.0.5. Dev deps: `gazelle` 0.35.0 (repo name `bazel_gazelle`), `bazel_skylib_gazelle_plugin` 1.4.1, `aspect_bazel_lib` 1.32.1, `buildifier_prebuilt` 6.1.2. Root `BUILD.bazel` loads Gazelle. `docs/` loads `@aspect_bazel_lib//lib:docs.bzl`. `polyglot/tests` is one skylib unittest (`versions_test`).

`.bazelrc`: `common --enable_bzlmod` and `common --check_direct_dependencies=off`.

`.bazelversion` first line is `7.2.1` (comment says CI uses that version).

Nested workspace: `e2e/smoke` has its own `MODULE.bazel` (no `module()` name; `local_path_override` of `rules_polyglot` to `../..`), empty `.bazelrc`, and targets `//:empty`, `//:smoke_test` (`build_test`), plus `//tools/formatters:{buildozer,buildozer.bin,format}`.

GitHub workflows (`.github/workflows`):

- `ci.yaml` calls `bazel-contrib/.github` reusable `bazel.yaml@v6` with folders `"."` and `"e2e/smoke"`, excluding `{folder: ".", bzlmodEnabled: false}` (Bzlmod only for the root). Also a pre-commit job.
- `buildifier.yaml`: `bazel run --enable_bzlmod //.github/workflows:buildifier.check`
- `ci.bazelrc` (CI only, not used here): `--announce_rc`, `--disk_cache=~/.cache/bazel`, `--test_output=errors`, `--test_env=XDG_CACHE_HOME`
- `release.yml` / `tag.yaml`: release tagging, not a build.

Commands actually run (plus `--test_output=errors` on every `test`):

- Root: `query //...`, cold `build //...`, `test //...`, warm `build //...`
- `e2e/smoke`: the same four
- Diagnostic, not CI: `query|build|test //polyglot/...` on the root, and a tiny scratch workspace for `native.sh_binary`

Both tools rewrite `MODULE.bazel.lock` by default (`lockfile_mode=update`). Lockfiles were restored in the clone after the runs. Convenience symlinks (`bazel-bin`, `bazel-out`, `bazel-testlogs`, `bazel-rules-polyglot` / `bazel-smoke`) were removed between tools. The first `e2e` test/warm attempts after Zel had pointed `bazel-smoke` at the Zel execroot made Bazel try to load that symlink as a package (`error loading package 'bazel-smoke/external/rules_shell++...'`). Isolated reruns all passed. Those contaminated attempts are not in the table.

Servers were shut down at the end.

## Summary

Wall times are client-side. “Elapsed” is the tool’s own `INFO: Elapsed time` when it printed one.

### Root workspace (`/tmp/bazel-vs-zel/repos/rules-polyglot`)

| Command | Bazel 9.3.2 | Zel native (9.3.2 label) | Bazel 7.2.1 (pinned, reference) |
| --- | --- | --- | --- |
| `query //...` | FAIL exit 7, 6.865s, 0 targets | FAIL exit 7, 25.142s, 0 targets | PASS exit 0, 8.333s, **21** targets |
| `build //...` cold | FAIL exit 1, 1.250s (elapsed 1.109s), 0 actions | FAIL exit 1, 1.267s (elapsed 0.050s), 0 actions | PASS exit 0, 54.733s (elapsed 54.576s), **14** targets, 91 actions |
| `test //...` | FAIL exit 1, 1.219s, 0 tests | FAIL exit 1, 0.138s, 0 tests | PASS exit 0, 1.664s, **2/2** passed |
| `build //...` warm | FAIL exit 1, 0.455s | FAIL exit 1, 0.312s | PASS exit 0, 0.765s, 1 internal action |

Bazel 7.2.1 tests: `//docs:update_test` PASSED, `//polyglot/tests:versions_test_test_0` PASSED.

`query` on Bazel 7 lists 21 targets; `build //...` reports 14 because manual targets (including `//polyglot:resolved_toolchain`, `tags = ["manual"]`) and test-suite wrappers are not built as the same set. Zel never got a target list.

### `e2e/smoke`

| Command | Bazel 9.3.2 | Zel native | Bazel 7.2.1 |
| --- | --- | --- | --- |
| `query //...` | PASS exit 0, 3.342s, **6** targets | PASS exit 0, 0.308s, **6** targets | PASS exit 0, 2.720s, **6** targets |
| `build //...` cold | PASS exit 0, 1.673s (elapsed 1.627s), 4 targets, 8 actions, linux-sandbox | PASS exit 0, 4.239s (elapsed 0.037s), 4 targets, 12 actions, processwrapper-sandbox | PASS exit 0, 1.531s (elapsed 1.469s), 4 targets, 7 actions |
| `test //...` | PASS exit 0, 0.586s, **1/1** (`//:smoke_test`) | PASS exit 0, 2.625s (elapsed 0.066s), **1/1** executed | PASS exit 0, 0.376s, **1/1** |
| `build //...` warm | PASS exit 0, 0.139s, 1 internal action | PASS exit 0, 0.365s (elapsed 0.005s), 1 internal action | PASS exit 0, 0.198s, 1 internal action |

Query target sets are identical:

```
//:empty
//:smoke_test
//:smoke_test_0__deps
//tools/formatters:buildozer
//tools/formatters:buildozer.bin
//tools/formatters:format
```

Both Bazel 9 and Zel warn on this workspace (its `.bazelrc` is empty, so the root `--check_direct_dependencies=off` does not apply):

```
WARNING: For repository 'bazel_skylib', the root module requires module version bazel_skylib@1.7.1, but got bazel_skylib@1.8.2 in the resolved dependency graph.
```

Zel’s “targets configured” counter is much smaller (6 vs Bazel 9’s 874 on the same cold build) even though both report “Analyzed 4 targets” and the test passes. That is a progress-metric difference, not a missing top-level target. Zel also sandboxes with `processwrapper-sandbox` where Bazel 9 uses `linux-sandbox`.

### Diagnostic: `//polyglot/...` (root, Gazelle not loaded)

| Command | Bazel 9.3.2 | Zel native | Bazel 7.2.1 |
| --- | --- | --- | --- |
| `query //polyglot/...` | PASS, 2.293s, **11** targets | PASS, 0.148s, **11** targets | PASS, 1.084s, **11** targets |
| `build //polyglot/...` | FAIL exit 1, 0.717s | FAIL exit 1, 0.267s | (covered by root `//...` PASS) |
| `test //polyglot/...` | FAIL exit 1, 0.279s, 0 tests | FAIL exit 1, 0.175s, 0 tests | (root test PASS) |

The 11 query labels match exactly, including `//polyglot/tests:versions_test` and `//polyglot/tests:versions_test_test_0`.

## Detailed findings

### 1. Root `//...` — Bazel 9 fails on removed `CcInfo`; Zel fails later on `native.sh_binary`

Bazel 9.3.2, `query` / `build` / `test` / warm build, exit 7 then 1:

```
WARNING: .../MODULE.bazel:3:7: The attribute 'compatibility_level' in module() is a no-op and will be removed in a future Bazel release.
ERROR: .../external/rules_go+/go/private/rules/cross.bzl:91:13: The CcInfo symbol has been removed, add the following to your BUILD/bzl file:

load("@rules_cc//cc/common:cc_info.bzl", "CcInfo")
ERROR: error loading package '': at .../external/gazelle+/def.bzl:37:5:
  at .../gazelle+/internal/generationtest/generationtest.bzl:19:6:
  at .../rules_go+/go/def.bzl:73:5:
  compilation of module 'go/private/rules/cross.bzl' failed
```

Chain: root `BUILD.bazel` loads `@bazel_gazelle//:def.bzl`, which loads `//internal/generationtest:generationtest.bzl`, which loads `@rules_go//go:def.bzl`. Fetched modules are `gazelle` 0.35.0 and `rules_go` 0.44.0. `cross.bzl` names bare `CcInfo` in a provider list. Bazel 9 removed that prelude symbol.

Zel native, same commands:

```
ERROR: .../external/gazelle+/def.bzl:188:21: type 'struct' has no method 'sh_binary'
ERROR: Error evaluating '//...': error loading package '': Package '' contains errors
```

`def.bzl` line 188 is inside the `gazelle()` macro, which the root `BUILD.bazel` calls:

```python
native.sh_binary(
    name = name,
    srcs = [runner_name],
    ...
)
```

Zel loaded `rules_go`’s `cross.bzl` without the `CcInfo` removal error, then evaluated the macro and missed `native.sh_binary`.

Zel does not print Bazel’s `compatibility_level` no-op warning.

This is an upstream ruleset problem for Bazel 9 either way. Gazelle 0.35 / rules_go 0.44 are from the Bazel 7 era this repo pins. A Zel that matched Bazel 9 loading rules would still fail `//...` until those deps move. The compatibility gap is that Zel fails differently, so it does not yet reproduce Bazel 9’s diagnosis.

Scratch check (`/tmp/bazel-vs-zel/scratch/shnative`, a `MODULE.bazel` with no `rules_shell` dep, macro calls `native.sh_binary`):

- Bazel 9.3.2 exit 7: `Error: no native function or rule 'sh_binary'` (available list still contains `cc_binary` / `java_binary`, not `sh_*`).
- Zel exit 7: `type 'struct' has no method 'sh_binary'`.

So Bazel 9.3.2 has also deleted plain `native.sh_binary`. On this repo Bazel never reached that call. Zel did, because `CcInfo` was still visible.

### 2. `//polyglot/...` build/test — `rule()` keyword Bazel 9 and Zel both reject

Query matches. Build dies while resolving toolchains for `//polyglot/tests:versions_test_test_0`, because dev-dependency `aspect_bazel_lib` 1.32.1 registers toolchains whose generated `defs.bzl` is:

```python
resolved_toolchain = rule(
    implementation = _resolved_toolchain_impl,
    toolchains = ["@aspect_bazel_lib//lib:jq_toolchain_type"],
    incompatible_use_toolchain_transition = True,
)
```

Bazel 9.3.2:

```
ERROR: Traceback (most recent call last):
	File ".../external/aspect_bazel_lib++ext+jq_toolchains/defs.bzl", line 15, column 20, in <toplevel>
		resolved_toolchain = rule(
Error in rule: rule() got unexpected keyword argument 'incompatible_use_toolchain_transition'
ERROR: .../polyglot/tests/BUILD.bazel:3:20: While resolving toolchains for target //polyglot/tests:versions_test_test_0: invalid registered toolchain '@jq_toolchains//:all'
```

Zel:

```
ERROR: invalid registered toolchain '@@aspect_bazel_lib++ext+copy_directory_toolchains//:all': error loading package '': initialization of module 'defs.bzl' failed
ERROR: .../polyglot/tests/BUILD.bazel:3:20: Analysis of target '//polyglot/tests:versions_test_test_0' failed
```

Same keyword, weaker message. Zel’s `rule()` allowlist does not include `incompatible_use_toolchain_transition`, and the toolchain-resolution path drops the Starlark traceback. Bazel 7.2.1 still accepts the keyword, which is why the root build there compiles stardoc and the unittest.

### 3. `e2e/smoke` matches

The module extension `//polyglot:extensions.bzl` (`polyglot.formatter(name = "buildifier")`), `register_toolchains("@polyglot//:all")`, and `//tools/formatters:format` (`polyglot_format` / `polyglot_formatter` against `@buildifier_prebuilt`) load, analyze, build, and test the same way on Bazel 9.3.2 and native Zel. The only behavioral nits are the configured-target counter, sandbox name, and a larger client wall time than server elapsed time (module/lockfile work outside the reported elapsed time).

### 4. Default `zel` is Bazelisk, not the native engine

`.bazelversion` is `7.2.1`. `plan_invocation` in `crates/zel-client/src/bazelisk.rs` prefers `USE_ZEL_VERSION`, then `USE_BAZEL_VERSION`, then `.zelversion`, then `.bazelversion`, and `ExecExternal`s when the resolved version is not the running binary’s label (`9.3.2`). Running `zel` with no env override in this repo downloads and runs Bazel 7.2.1, which does pass root `query` / `build` / `test`. That is Bazelisk-compatible and hides the native gaps above. `ZEL_SKIP_WRAPPER=true` forces in-process even when the version file disagrees.

## Root cause and next Zel fixes

1. **Removed prelude symbols are still predeclared.** `CcInfo` (and `JavaInfo`, `PyInfo`, `ProtoInfo`, `cc_common`, …) are listed in `ANALYSIS_ONLY_GLOBALS` in `crates/zel-packages/src/loader.rs`. Bazel 9 treats bare `CcInfo` as removed and points at `load("@rules_cc//cc/common:cc_info.bzl", "CcInfo")`. Zel should stop injecting those names, or redirect them the way Bazel 9’s `AutoloadSymbols` does (`CcInfo` → `@rules_cc//cc/common:cc_info.bzl`). Until then, old `rules_go` loads on Zel and fails later with a misleading `sh_binary` error.

2. **`native.sh_*` is gone and not autoloaded.** `crates/zel-packages/src/rule_class.rs` correctly drops native `sh_binary` / `sh_test` / `sh_library` for Bazel 9 (they live in `@rules_shell`). Bazel 9 still autoloads them onto `native` when `--incompatible_autoload_externally` includes `+sh_binary` (`@rules_shell//shell:sh_binary.bzl` in `AutoloadSymbols`). Zel’s semantics fingerprint hardcodes that flag as `"[]"` in `crates/zel-bzlmod/src/semantics_fp.rs`, and `BUILD_FUNCTIONS` / `loader_host.rs` have no `sh_binary` redirect. Gazelle 0.35’s `gazelle()` macro calls `native.sh_binary` and cannot load. Implement the Bazel 9 autoload list (at least `sh_binary`, `sh_test`, `sh_library`, and the `cc_*` / `java_*` / `py_*` / `proto_*` symbols) and the `+foo` / `-foo` flag behavior.

3. **Toolchain `.bzl` init errors drop the cause.** `define_rule` in `crates/zel-packages/src/loader_host.rs` rejects unknown `rule()` kwargs (`incompatible_use_toolchain_transition` is not in the allowlist, which matches Bazel 9). The message that reaches the user is only `initialization of module 'defs.bzl' failed`. Include the underlying `rule() got unexpected keyword argument '...'` and a traceback, as Bazel does.

4. **Not a Zel code change:** root `//...` cannot pass on Bazel 9.3.2 until `rules_polyglot` bumps `gazelle` / `rules_go` and `aspect_bazel_lib` past the `CcInfo` removal and the `incompatible_use_toolchain_transition` `rule()` argument. Native Zel should be judged against that Bazel 9 failure mode, not against Bazel 7.2.1 success, unless the product decision is to keep emulating the `.bazelversion` toolchain rather than the binary’s 9.3.2 label.

`e2e/smoke` does not need a Zel fix. It is already green against Bazel 9.3.2.
