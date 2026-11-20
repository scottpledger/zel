# Bazel vs Zel compatibility: rules_multirun

## Metadata

| Field | Value |
| --- | --- |
| Repo | https://github.com/scottpledger/rules_multirun.git (`scottpledger/rules_multirun`) |
| Clone | `/tmp/bazel-vs-zel/repos/rules-multirun` |
| Tested commit | `acd8e82bc159bac194e42d1fc2d6e3c521d39e70` (`main`, 2025-05-06, "Improve windows support by using the same technique to run the generated shell script that rules_shell uses.") |
| `.bazelversion` | none |
| Bazel | 9.3.2 via Bazelisk 1.30.0 (`USE_BAZEL_VERSION=9.3.2 /tmp/bazel-vs-zel/bin/bazel --host_jvm_args=-Xmx1g`) |
| Zel | release label 9.3.2, built from `6297ed54b710bb023dfc7c7f275e3c2515d9761a` (`/tmp/bazel-vs-zel/bin/zel`) |
| OS / arch | Linux 6.12.113+ x86_64 |
| Date | 2026-11-20 |
| Output bases | Bazel `/tmp/bazel-vs-zel/ob/rules-multirun-bazel`, Zel `/tmp/bazel-vs-zel/ob/rules-multirun-zel` |
| Nested workspaces | none (`MODULE.bazel` only at the repo root; no `e2e/`, `examples/`, or other module roots) |

Both servers were shut down after the runs (`shutdown` exit 0). No actions were executed, so the repo `.bazelrc` remote cache (`grpcs://remote.buildbuddy.io`) was never used.

## Executive verdict

**PARTIAL**

`query //...`, `build //...`, and `test //...` fail on both Bazel 9.3.2 and Zel, with the same exit codes and the same undefined-rule diagnostics for bare `sh_binary` / `py_binary` / `sh_test`. The workspace at this commit is already unloadable on Bazel 9: `internal/runfiles_enabled.bzl` is loaded by `multirun.bzl` and listed as a `bzl_library` source, and the file is not in the tree. Zel matches that failure outcome and, with `--keep_going`, returns the same three `//internal` targets.

Zel diverges once analysis starts. Bazel configures `//internal:all` and stops on the missing source file. Zel aborts `//internal:multirun` while loading a registered `aspect_bazel_lib` toolchain, because `rule()` rejects `incompatible_use_toolchain_transition`, and it replaces the real load error with `initialization of module 'defs.bzl' failed`. No test ran on either tool (the only test is `//tests:test`).

## Workspace and CI

Single Bzlmod module `rules_multirun` (`MODULE.bazel` version `"0"`). Direct deps: `bazel_features` 1.18.0, `rules_shell` 0.4.0, `bazel_skylib` 1.4.2, `rules_python` 0.36.0, `platforms` 0.0.10, `aspect_bazel_lib` 2.15.3, dev dep `stardoc` 0.7.1 (`repo_name = "io_bazel_stardoc"`). A legacy `WORKSPACE` still vendors older `bazel_skylib` 1.3.0 and `rules_python` 0.27.1. Bazel 9 and Zel both used Bzlmod (the default).

`.bazelrc`:

- `common --lockfile_mode=off`
- `build --remote_cache=grpcs://remote.buildbuddy.io` and `--remote_timeout=3600`
- protobuf/zlib `--per_file_copt` warning suppressions (for stardoc)
- `test --test_output=errors`

Packages: `//` (`BUILD`), `//doc`, `//internal`, `//tests`. The product rules are Starlark (`command`, `multirun`) wrapping `sh_binary` / `py_binary`. The only test rule is `//tests:test` (`sh_test`).

`.github/workflows/main.yml` (CI, not re-run here):

- macOS and Ubuntu: `USE_BAZEL_VERSION=6.x bazelisk test //tests/...`; `USE_BAZEL_VERSION=7.x bazelisk test //tests/... --enable_bzlmod=false`; `bazelisk test //...`; `USE_BAZEL_VERSION=rolling bazelisk test //...`; `USE_BAZEL_VERSION=last_green bazelisk test //...`
- Windows: `bazelisk test //tests/...`, plus `rolling` and `last_green`

`.github/workflows/create-release.yml` is a manual `gh release create` packaging job. `.bcr/presubmit.yml` tests `@rules_multirun//tests/...` on Bazel `6.x`, `7.x`, and `rolling`.

There is no pin to 9.3.2. Bazel 9.3.2 itself fails on this commit (see below), which matches the missing source file and the removal of native shell/Python rules from the main repo.

Resolved direct versions (both tools, `--check_direct_dependencies` default warning) were identical:

| Module | Requested | Resolved |
| --- | --- | --- |
| bazel_features | 1.18.0 | 1.42.1 |
| rules_shell | 0.4.0 | 0.6.1 |
| bazel_skylib | 1.4.2 | 1.8.2 |
| rules_python | 0.36.0 | 1.7.0 |
| platforms | 0.0.10 | 1.0.0 |
| stardoc (`io_bazel_stardoc`) | 0.7.1 | 0.7.2 |

## Summary

Wall times were taken on a 4-core host with load average about 15 and many other Bazel/Zel servers. Zel wall clock is much higher than the server-reported `Elapsed time` (often ~0.05–1.1s). Treat wall clock as contaminated. Exit codes and target sets are the comparison that matters.

| Workspace | Command | Bazel 9.3.2 | Zel |
| --- | --- | --- | --- |
| root | `query //...` | exit 7, 0 targets, wall 3.33s (includes server start) | exit 7, 0 targets, wall 28.2s |
| root | `build //...` cold | exit 1, elapsed 0.830s, wall 1.01s, 0 processes | exit 1, elapsed 0.050s, wall 46.8s, 0 processes |
| root | `build //...` warm | exit 1, elapsed 0.462s, wall 0.51s, 0 processes | exit 1, elapsed 0.050s, wall 32.3s, 0 processes |
| root | `test //... --test_output=errors` | exit 1, 0 tests run, elapsed 0.438s | exit 1, 0 tests run, elapsed 0.050s, wall 41.6s |
| root (extra) | `query //... --keep_going` | exit 3, 3 targets | exit 3, 3 targets, same set |
| root (extra) | `query //internal:all` | exit 0, 3 targets, wall 0.65s | exit 0, 3 targets, same set, wall 33.9s |
| root (extra) | `build //internal:all` | exit 1 after analysis (3 targets configured); missing `runfiles_enabled.bzl` | exit 1 during analysis of `//internal:multirun` (toolchain `rule()` rejected) |

`query //... --keep_going` targets, identical on both:

```
bzl_library rule //internal:constants
bzl_library rule //internal:runfiles_enabled
py_binary rule //internal:multirun
```

No targets were missing or extra in that set. `//`, `//doc`, and `//tests` do not load, so they contribute no targets.

## Detailed findings

### 1. `//...` does not load on Bazel 9.3.2

Root `BUILD` loads `//:defs.bzl`. `defs.bzl` loads `multirun.bzl`. `multirun.bzl` line 15 is:

```
load("//internal:runfiles_enabled.bzl", "RunfilesEnabledProvider")
```

`git cat-file -e HEAD:internal/runfiles_enabled.bzl` fails. `internal/BUILD` also lists that path as a `bzl_library` src. The file is absent from the commit, not from a shallow-clone omission (`git ls-tree -r HEAD` has no such path).

`tests/BUILD` and `doc/BUILD` call `sh_binary`, `py_binary`, and `sh_test` with no `load()`. Bazel 9 does not provide those names in the main repo (autoloads are off for the main repository). Both tools report the same 15 undefined-name errors, including the suggestion `did you mean 'cc_binary'?` / `cc_test`.

Bazel `query //...` (exit 7):

```
ERROR: .../tests/BUILD:4:1: name 'sh_binary' is not defined (did you mean 'cc_binary'?)
ERROR: .../tests/BUILD:37:1: name 'py_binary' is not defined (did you mean 'cc_binary'?)
ERROR: .../doc/BUILD:36:1: name 'sh_binary' is not defined (did you mean 'cc_binary'?)
ERROR: .../tests/BUILD:251:1: name 'sh_test' is not defined (did you mean 'cc_test'?)
ERROR: error loading package under directory '': error loading package '': cannot load '//internal:runfiles_enabled.bzl': no such file
```

Bazel `build //...` and `test //...` (exit 1) repeat that and add:

```
ERROR: package contains errors: tests
ERROR: package contains errors: doc
WARNING: Target pattern parsing failed.
ERROR: Skipping '//...': error loading package under directory '': error loading package '': cannot load '//internal:runfiles_enabled.bzl': no such file
ERROR: Build did NOT complete successfully
ERROR: Couldn't start the build. Unable to run tests
```

With `--keep_going`, Bazel names the load chain:

```
ERROR: error loading package '': at .../defs.bzl:8:6: at .../multirun.bzl:15:6: cannot load '//internal:runfiles_enabled.bzl': no such file
```

Zel `query` / `build` / `test` emit the same 15 undefined-name lines and the same `package contains errors` lines for `tests` and `doc`. The root-package failure is rewritten:

```
ERROR: Skipping '//...': error loading package '': initialization of module 'defs.bzl' failed
ERROR: error loading package '': initialization of module 'defs.bzl' failed
ERROR: Build did NOT complete successfully
ERROR: Couldn't start the build. Unable to run tests
```

Zel `query //...` also omits the phrase `under directory ''` (`error loading package ''` only). Zel `query` did not print the direct-dependency warnings or Bazel's `compatibility_level` no-op warning; Zel `build` and `test` did print the same six direct-dependency warnings as Bazel. Zel never printed the `compatibility_level` no-op warning (`zel-bzlmod` module-file evaluation).

Warm `build //...` repeats the same load errors. Nothing is cached into an action graph.

### 2. `//internal:all` analyzes on Bazel and dies in toolchain loading on Zel

`//internal/BUILD` loads `py_binary` from `@rules_python//python:defs.bzl`, so the package itself loads.

Bazel `build //internal:all` (exit 1, elapsed 0.347s cold / 0.093s warm):

```
Analyzing: 3 targets (4 packages loaded, 6 targets configured)
ERROR: .../internal/BUILD:20:12: //internal:runfiles_enabled: missing input file '//internal:runfiles_enabled.bzl'
ERROR: .../internal/BUILD:20:12: 1 input file(s) do not exist
Target //internal:runfiles_enabled failed to build
INFO: 1 process: 1 internal.
ERROR: Build did NOT complete successfully
```

All three targets were configured. The failure is the missing source on `//internal:runfiles_enabled`.

Zel `build //internal:all` (exit 1, elapsed 1.071s cold / 0.305s warm):

```
ERROR: invalid registered toolchain '@@aspect_bazel_lib++toolchains+copy_directory_toolchains//:all': error loading package '': initialization of module 'defs.bzl' failed
ERROR: .../internal/BUILD:4:10: Analysis of target '//internal:multirun' (config: b6aa069) failed
ERROR: Analysis of target '//internal:multirun' failed; build aborted
INFO: 1 process: 1 internal.
ERROR: Build did NOT complete successfully
```

The toolchain repo is generated by `aspect_bazel_lib` 2.15.3. Its `BUILD.bazel` loads `:defs.bzl`, and that file defines:

```
resolved_toolchain = rule(
    implementation = _resolved_toolchain_impl,
    toolchains = ["@aspect_bazel_lib//lib:copy_directory_toolchain_type"],
    incompatible_use_toolchain_transition = True,
)
```

Zel `rule()` (`crates/zel-packages/src/loader_host.rs`, `define_rule`) allows a fixed keyword set and does not include `incompatible_use_toolchain_transition`. `check_kwargs` in `crates/zel-packages/src/loader.rs` returns `rule() got unexpected keyword argument 'incompatible_use_toolchain_transition'`. That Starlark error is not what the command prints. `eval_bzl_file` substitutes `initialization of module 'defs.bzl' failed` (`loader.rs` around the `fatal_msg` fallback). The label in the message is the toolchain repo's `defs.bzl`, which has an empty package fragment, so it looks like the main-repo `defs.bzl` failure from finding 1.

`rules_python`'s `py_binary` triggers registered-toolchain validation. `aspect_bazel_lib` registers `//:all` of that generated repo. Bazel accepts the legacy `rule()` argument and continues. Zel aborts analysis of `//internal:multirun` before it can report the missing `runfiles_enabled.bzl` input.

### 3. Tests

`test //...` never reaches `//tests:test` on either tool. Pass count 0, fail count 0, not-run because loading failed. Exit 1 on both, with `Couldn't start the build. Unable to run tests`.

## Root cause and next Zel fixes

1. **Preserve nested `.bzl` load failures** (`zel-packages` loader, `eval_bzl_file` / `install_load_hook`). A missing `load()` currently surfaces as `initialization of module '<file>' failed` and drops Bazel's `at <file>:<line>:<col>: cannot load '<label>': no such file` chain. `query`, `build`, and toolchain errors in this repo all hit that wrapper. The fallback should keep the child message and location the way Bazel prints it under `--keep_going`.

2. **Accept `rule(incompatible_use_toolchain_transition=...)`** (`zel-packages` `define_rule` allowed keywords). `aspect_bazel_lib` 2.x still emits that argument on generated toolchain aliases. Bazel 9 loads those files. Until Zel accepts (or ignores, matching Bazel) the argument, any workspace that registers those toolchains fails analysis of unrelated targets such as `//internal:multirun`, and the diagnostic from fix 1 is required to see the keyword error.

3. **Main-repo shell/Python rules.** Undefined `sh_binary` / `py_binary` / `sh_test` matches Bazel 9.3.2 here. No Zel change is required for this repo's `//...` load failure. A future repo that depends on main-repo autoloads (`--incompatible_autoload_externally` with autoloads enabled in the main repo) would need Zel's default of `incompatible_autoload_externally = []` (`crates/zel-bzlmod/src/semantics_fp.rs`) checked against Bazel 9.3.2's default. This workspace does not exercise that path.

4. **`compatibility_level` warning.** Bazel warns that `module(compatibility_level=...)` is a no-op. Zel does not. Low priority (`zel-bzlmod` module-file parser).

The repository itself needs `internal/runfiles_enabled.bzl` restored (or the loads and `bzl_library` src removed) before either tool can build or test `//...` on this commit. CI on Bazel 6/7 does not prove the file exists at `acd8e82`; the git tree does not contain it.
