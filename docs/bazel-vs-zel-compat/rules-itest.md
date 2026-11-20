# Bazel vs Zel: `rules_itest`

## Metadata

| Field | Value |
| --- | --- |
| Repo | https://github.com/scottpledger/rules_itest.git |
| Clone | `/tmp/bazel-vs-zel/repos/rules-itest` |
| Tested commit | `6b94fd2e513d1a964d2bd0b1ff423cb6183cbb31` (`master`) |
| `.bazelversion` | `9.0.0` (root, `tests/`, and `examples/`) |
| Bazel | 9.3.2 via Bazelisk 1.30.0 (`USE_BAZEL_VERSION=9.3.2`), `--host_jvm_args=-Xmx1g` |
| Zel | label `9.3.2`, built from `6297ed54b710bb023dfc7c7f275e3c2515d9761a` |
| OS / arch | Linux 6.12.113+ x86_64 |
| Date | 2026-11-20 |

Output bases: `/tmp/bazel-vs-zel/ob/rules-itest-bazel` and `...-zel`, with `-tests` and `-examples` suffixes for the nested workspaces. Both sides were shut down afterward. Zel `shutdown` only succeeds with `--noworkspace_rc`, because `common --@rules_go//go/config:pure` is rejected on non-analysis commands (see below).

## Executive verdict

**FAIL**

Bazel 9.3.2 builds this ruleset and passes its CI suite. Zel does not complete `query`, `build`, or `test` on the repo’s own `.bazelrc` in any of the three workspaces. With `USE_BAZEL_VERSION=9.3.2` (required; see version gate), the root `query //...` target set matches Bazel only after the Starlark flag is removed from the rc file. Every build dies in Bzlmod/Starlark loading: Gazelle and `llvm`/`tar.bzl` cannot define a `module_extension` while another extension is active, `tests/` cannot load `aspect_rules_js` because `native.bazel_version` is missing, and `examples/` also rejects bare `cc_*` rules and Gazelle’s duplicate `//:__subpackages__` visibility.

`.bazelversion` pins 9.0.0. Bazel 9.3.2 itself succeeds on the root module and on `tests/` (the workflow command). The pin was not the reason Bazel worked.

## What was tested

CI (`.github/workflows/run-tests.yml`) checks out the repo and runs `bazel test //...` with `working-directory: tests` on `ubuntu-latest` and `windows-latest`. Release/publish workflows only cut a BCR release; they do not build.

Workspaces, each with its own `MODULE.bazel`:

- **Root** `rules_itest` — rules, Go libraries, a `cc_binary`/`cc_test`, dev deps on Stardoc, Gazelle, and `llvm`.
- **`tests/`** — integration suite (`local_path_override` of the parent). Adds `aspect_rules_js`, `rules_go`, `rules_shell`. This is the CI workspace.
- **`examples/`** — Redis, MySQL 8.4, DynamoDB local, and an OpenTelemetry collector. Adds `rules_java`, `rules_jvm_external`, and large `go_deps`.

Shared `.bazelrc` (imported by the nested workspaces) sets `--enable_bzlmod`, `--watchfs`, `--nolegacy_external_runfiles`, `--incompatible_autoload_externally=` then `+@rules_cc`, `--@rules_go//go/config:pure`, and `BAZEL_DO_NOT_DETECT_CPP_TOOLCHAIN`.

Zel runs below used `USE_BAZEL_VERSION=9.3.2` so the binary under test actually executed. Without that variable, every command in these trees exits before startup:

```
ERROR: The project requires Zel version '9.0.0' (specified in .../.bazelversion), but the running binary is version '9.3.2' and version '9.0.0' could not be found locally or downloaded from https://github.com/scottpledger/zel/releases/download.
```

That is `zel-client` Bazelisk handling (`crates/zel-client/src/bazelisk.rs`). Bazelisk with `USE_BAZEL_VERSION=9.3.2` ignores the pin; Zel does too when the same variable is set.

## Summary

| Workspace | Command | Bazel 9.3.2 | Zel 9.3.2 |
| --- | --- | --- | --- |
| root | `query //...` | exit 0, **24** targets, ~5s cold / 3.5s warm | exit 2, 0 targets. `Unrecognized option: --@rules_go//go/config:pure` |
| root | `query` with that one rc line removed | — | exit 0, **24** targets, 1.3s, identical to Bazel |
| root | `build //...` cold | exit 0, **23** targets, 580 actions, **118.6s** | exit 1, **4.4s**, Gazelle `module_extension()` |
| root | `build //...` warm | exit 0, **4.2s** | exit 1, **35.0s**, same class of error via `tar.bzl` / `llvm` |
| root | `test //...` | exit 0, **1/1** pass (`//:exit0_test`), **2.2s** | exit 1, **0.2s**, analysis aborted (`@@llvm+//toolchain:all`) |
| `tests/` | `query //...` | exit 0, **143** targets, **10.3s** | exit 2, same unrecognized `--@rules_go//go/config:pure` |
| `tests/` | `query` with Starlark flags stripped | — | exit 7, 0 targets. `js/defs.bzl` failed to initialize |
| `tests/` | `build //...` cold | exit 0, **107** targets, 1135 actions, **94.0s** | exit 1, **0.7s**, package `js_binary` |
| `tests/` | `build //...` warm | exit 0, **1.6s** | exit 1, **0.5s**, same |
| `tests/` | `test //...` | exit 0, **42/42** pass, **4.5s** | exit 1, **0.8s**, no tests started |
| `examples/` | `query //...` | exit 0, **32** targets, **4.4s** | exit 2, unrecognized `--@rules_go//go/config:pure` |
| `examples/` | `build //...` | exit 0, **31** targets, 3782 actions, **556s** after host libs (see note) | exit 1, **254s**, `cc_*` autoload + duplicate visibility + `tar.bzl` |
| `examples/` | `build //...` warm | exit 0, **1.4s** | exit 1, **203s**, `llvm` / `tar.bzl` `module_extension()` |
| `examples/` | `test //...` | **6/6** pass once `libncurses.so.6` is installed (see note) | exit 1, **0.8s**, no tests started |

`build //...` counts are Bazel’s “Found N targets” (manual targets stay in `query` and drop out of the build). Root: query 24 vs build 23 (22 + 1 test). `tests/`: query 143 vs build 107 (65 + 42 tests). `examples/`: query 32 vs build 31 (25 + 6 tests).

### `examples/` environment (Bazel, not Zel)

The first `examples` build died in Gazelle `fetch_repo` with `net/http: TLS handshake timeout` against `proxy.golang.org` for `github.com/ClickHouse/clickhouse-go/v2@v2.40.3` (exit 1, 103s). The retry got past that and failed in `//mysql:vanilla` because the sandbox could not load `libaio.so.1` (exit 1, 133s). The following `test //...` skipped all 3 tests it tried to run.

After `libaio1t64` plus a `libaio.so.1` symlink, `build //...` succeeded (31 targets, 556s) and a warm build was 1.4s. `test //...` then ran 6 tests: DynamoDB and both Redis hygiene tests passed; both MySQL hygiene tests and `//service_group:service_group_hygiene_test` hit the 300s timeout because `mysql_cli` could not load `libncurses.so.6` (mysqld itself became ready). After `libncurses6`, those three targets **PASSED in ~2.2s**. Count the Bazel examples suite as **6/6** on this host once those two libraries exist. Zel never reached MySQL.

## Detailed findings

### 1. Starlark flags in `common` are illegal for `query` and `shutdown`

`.bazelrc` contains `common --@rules_go//go/config:pure` (no `=true`). Bazel accepts it for `query`, `version`, and `shutdown`. Zel returns:

```
ERROR: --@rules_go//go/config:pure :: Unrecognized option: --@rules_go//go/config:pure
```

Exit code 2. `build` and `test` accept the flag and move on.

`command_analyzes()` in `crates/zel-options/src/parser.rs` is only `build`, `test`, `coverage`, `run`, `cquery`, `aquery`, `fetch`, `vendor`, `print_action`, and `mobile-install`. Starlark prefixes (`--//`, `--@`, `--no//`, `--no@`) are skipped only when that is true. Bazel still parses those options from `common` lines for commands that do not analyze.

A copy of the rc with that line (and the unused `enable-reload` config) removed makes root `query //...` match Bazel’s 24 labels exactly.

### 2. `tests/` never loads `//js_binary` — `native.bazel_version`

Stock `build //...` / `test //...` in `tests/`:

```
ERROR: Skipping '//...': error loading package 'js_binary': initialization of module 'js/defs.bzl' failed
ERROR: Build did NOT complete successfully
```

The summary drops the cause. A build of `//js_binary:all` prints it:

```
ERROR: .../external/aspect_rules_js+/js/defs.bzl:3:6: Unable to find package for
  @@aspect_tools_telemetry++telemetry+aspect_tools_telemetry_report//:defs.bzl
...
File ".../aspect_tools_telemetry+/collectors/bazel.bzl", line 38, column 18, in _bazel_version
    return native.bazel_version
Error: 'struct' value has no field or method 'bazel_version'
Available attributes: action_listener, alias, cc_binary, ... package_name, ... toolchain_type
```

`aspect_rules_js` loads `@aspect_tools_telemetry_report//:defs.bzl`. That repo is produced by a `tel_repository` rule whose collector calls `native.bazel_version`. The `.bzl` was evaluated by the package loader, whose `native` struct is the BUILD-rule struct (`StructValue::new` in `zel-packages`). It does not include `bazel_version`. `zel-bzlmod`’s own `.bzl` context (`crates/zel-bzlmod/src/rt.rs`, `new_context`) does insert `native.bazel_version`, but this file was not loaded through that path. The fetch fails, the repo is invisible, and every `tests/` package that loads `js/defs.bzl` is unloadable. That blocks the whole CI `//...` pattern before any `itest` target is analyzed.

### 3. `module_extension()` while an extension is already active

Root `build //...` (4.4s, 0 actions):

```
ERROR: .../external/gazelle+/internal/go_repository.bzl:130:32: An error occurred during the fetch of repository 'gazelle++go_deps+org_golang_x_sys':
   Traceback (most recent call last):
    File ".../gazelle+/internal/bzlmod/non_module_deps.bzl", line 59, column 35, in <toplevel>
        non_module_deps = module_extension(
Error: module_extension() can only be used while initializing a .bzl file
```

The same string is raised for `@@tar.bzl+//tar:extensions.bzl` while fetching `llvm++llvm_source+llvm-raw` (root warm build, and `examples/`):

```
File ".../external/tar.bzl+/tar/extensions.bzl", line 15, column 30, in <toplevel>
    toolchains = module_extension(
Error: module_extension() can only be used while initializing a .bzl file
ERROR: Error loading '@@llvm+//extensions:llvm.bzl' for module extensions, requested by
  https://bcr.bazel.build/modules/llvm/0.8.4/MODULE.bazel:96:21
```

`make_module_extension` in `crates/zel-bzlmod/src/rt.rs` requires both a module label and **no** active extension context (`ctxs` all `None`). Gazelle’s `go_repository.bzl` loads `non_module_deps.bzl`, which calls `module_extension()` at top level, while `go_deps` is still running. `llvm`’s `http_bsdtar_archive` loads `tar.bzl`’s `extensions.bzl` the same way. Bazel treats that top-level call as ordinary `.bzl` initialization and returns the extension object. Zel aborts the load, so the registered `@@llvm+//toolchain:all` toolchain never materializes and C++ targets (`//:exit0`, Gazelle’s runner) fail analysis:

```
ERROR: invalid registered toolchain '@@llvm+//toolchain:all': error loading package 'toolchain':
  initialization of module 'toolchain/bootstrap/declare_toolchains.bzl' failed
ERROR: Analysis of target '//:exit0' failed; build aborted
```

On `examples/`, Gazelle’s `non_module_deps` extension happened to be evaluated first (the `fetch_repo` tool ran and downloaded modules). The `tar.bzl` failure still aborted the build, together with the two issues below.

### 4. `--incompatible_autoload_externally=+@rules_cc` does not keep `cc_*` defined

`examples/` Redis and sqlite4java BUILD files call `cc_library` / `cc_binary` with no `load()`. The rc flag is what makes that legal on Bazel 9. Zel still routes them through the removed-rule stub:

```
File ".../external/+http_archive+com_github_redis_redis/BUILD.bazel", line 3, column 11, in <toplevel>
    cc_library(
File "/tmp/zel-builtins-.../bazel/exports.bzl", line 40, column 9, in _removed_rule_failure
    fail("""
Error in fail:
         This rule has been removed from Bazel. Please add a `load()` statement for it.
```

The flag is parsed (`zel-options` flag table) but the builtins path still installs the failure stub. Bazel 9.3.2 compiled those `cc_library` targets (libcxx actions appear in the successful examples build).

### 5. Duplicate visibility after canonicalizing Gazelle’s self-repo labels

`examples/` `build` also reports dozens of package errors (62 lines in the cold log), for example:

```
ERROR: .../gazelle++go_deps+com_github_aws_aws_sdk_go_v2_internal_configsources/BUILD.bazel:3:11:
  Label '@@gazelle++go_deps+com_github_aws_aws_sdk_go_v2_internal_configsources//:__subpackages__'
  is duplicated in the 'visibility' attribute of rule 'configsources'
```

Gazelle writes both `//:__subpackages__` and `@com_github_aws_aws_sdk_go_v2_internal_configsources//:__subpackages__`. Inside that repo the apparent name is the repo itself, so both canonicalize to one label. `zel-packages` `loader_host.rs` rejects duplicate canonical labels on every label-list attribute, including `visibility`. Bazel loaded these BUILD files and compiled the OpenTelemetry collector (the successful 31-target build). The check is stricter than Bazel for this Gazelle pattern.

Direct-dependency version warnings (`bazel_lib` 3.0.0 vs 3.2.2, `rules_cc` 0.1.5 vs 0.2.18, `gazelle` 0.41.0 vs 0.45.0, `platforms` 0.0.11 vs 1.1.0, and the Go `golang.org/x/sys` bump) are printed by both tools and are not failures.

## Root cause and next fixes

All of these are in Zel. No `rules_itest` change is required for Bazel 9.3.2.

1. **`zel-client` Bazelisk** — A workspace `.bazelversion` of `9.0.0` refuses to run a 9.3.2 binary and then fails the download from `scottpledger/zel` releases. Match Bazelisk: `USE_BAZEL_VERSION` already overrides the file; the default path should not hard-fail the only installed binary when the requested release artifact does not exist, or should document the override as mandatory. As tested, a stock `zel query` in this repo never starts.

2. **`zel-options`** — Recognize `--//` / `--@` / `--no//` / `--no@` on every command that reads rc files, including `query`, `info`, `version`, and `shutdown`. `common` lines in real rulesets use them. Today `query //...` and `shutdown` are dead on this repo’s rc.

3. **`zel-packages` `native`** — Expose `native.bazel_version` (string, Bazel’s value) on `.bzl` modules loaded for packages and for repository rules, not only on the `zel-bzlmod` eval context. `aspect_tools_telemetry` is pulled in by `aspect_rules_js` 2.8.2 and is what stops `tests/`.

4. **`zel-bzlmod` `make_module_extension`** — Allow `module_extension()` at top level of a `.bzl` file that is initializing, even if another extension context is on the stack. The `in_active_ctx` guard rejects Gazelle `non_module_deps.bzl` and `tar.bzl` `extensions.bzl`. That blocks `go_deps` fetches and the `llvm` toolchain this repo registers (`dev_dependency` on the root module, a normal dep in `tests/` and `examples/`).

5. **Autoload `+@rules_cc`** — Honor `--incompatible_autoload_externally=+@rules_cc` so external BUILD files can call `cc_library` / `cc_binary` without a `load()`, which is what Bazel 9 does here. The removed-rule stub in the builtins `exports.bzl` currently fires anyway.

6. **`zel-packages` duplicate label check** — Do not fail the package when `visibility` lists `//:__subpackages__` and the repo’s own apparent name for the same canonical label. Gazelle emits that pair; Bazel accepts it. Treating it as a hard error fails `examples/` `go_deps` packages even after the fetch succeeds.

Fix order that unblocks CI: (2) so `query` runs, (3) so `tests/` can load `js_binary`, (4) so Go and `llvm` repos fetch, then (5) and (6) for `examples/`. (1) is what a user hits before any of that.
