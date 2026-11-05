# Zel

**Zel** is a fast, self-contained, single-binary Rust implementation of [Bazel](https://bazel.build) with zero JVM startup overhead and 1:1 drop-in compatibility with existing `MODULE.bazel`, `BUILD`, `.bzl`, and `bazelisk` workflows.

## Architecture

Zel is organized into a 19-crate zero-external-dependency Rust workspace under `crates/`:

| Crate | Responsibility |
| :--- | :--- |
| `zel-proto` | Wire protocol codecs (`CommandServer`, `FailureDetails`, Build Event Protocol, REAPI v2, `ActionCache`) |
| `zel-util` | Hashing (SHA-256, MD5), string interning (`Symbol`), exit codes, platform utilities |
| `zel-vfs` | `PathFragment`, `RootedPath`, `FileSystem` trait, `LocalFileSystem`, `InMemoryFileSystem` |
| `zel-events` | ANSI terminal progress reporter and Build Event Protocol (BEP) JSON/binary writer |
| `zel-cmdline` | `RepositoryName`, `PackageIdentifier`, `Label`, and `TargetPattern` parser |
| `zel-options` | `StartupOptions`, `ParsedCommandOptions`, `.zelrc`/`.bazelrc` parser (`import`, `try-import`, `--config`) |
| `zel-starlark` | Starlark lexer, parser, AST evaluator, `depset` (`NestedSet`), `select`, `glob`, and `.bzl` loader |
| `zel-packages` | `BUILD` evaluation, macro expansion, `Package`, and `Target` graph construction |
| `zel-bzlmod` | `MODULE.bazel` parser, Minimal Version Selection (MVS) solver, overrides, and `MODULE.bazel.lock` |
| `zel-query` | `zel query` / `cquery` / `aquery` engine (`deps`, `rdeps`, `somepath`, `allpaths`, `kind`, `filter`, `attr`) |
| `zel-analysis` | `BuildConfigurationKey`, `ConfiguredTarget`, `RuleContext`, providers, and action conflict detection |
| `zel-rules` | Built-in rules (`genrule`, `filegroup`, `alias`, `config_setting`, `sh_*`, `cc_*`, `rust_*`, `py_*`, `java_*`, `proto_library`) |
| `zel-sandbox` | Sandboxed process execution (`darwin-sandbox`, `linux-sandbox`, `process-wrapper`) and persistent workers |
| `zel-remote` | REAPI v2 Content-Addressable Storage (CAS), Action Cache, local disk cache, and JSON trace profiler |
| `zel-exec` | `SkyframeActionExecutor`, symlink forest planting, incremental `CompactPersistentActionCache`, and test runner |
| `zel-skyframe` | Incremental evaluation graph (`InMemoryMemoizingEvaluator`, `ParallelEvaluator`, dirty tracking, cycle detection) |
| `zel-server` | Persistent background daemon (`zel-server`), `BlazeDirectories`, `BlazeRuntime`, and command dispatcher |
| `zel-client` | `zel` CLI client, built-in Bazelisk version manager/launcher, workspace discovery, server lifecycle, and `--batch` execution |
| `zel-ffi-bridge` | C-ABI / FFI shared library bridge |

## Building & Testing

### With Cargo

```bash
# Build release binary
cargo build --release --bin zel

# Run all unit and integration tests
cargo test --workspace

# Run compatibility and self-hosting tests
./scripts/zel_compat_test.sh
```

### With Bazel (`@rules_rs`) or Zel (Self-Hosted)

```bash
# Build and test with Bazel
bazel build //:zel
bazel test //crates/...

# Self-hosted build using Zel itself
./target/release/zel --output_base=/tmp/zel-ob build //crates:zel
```

### Dependencies

Zel requires no external crates or runtime dependencies, with the following system utilities expected on the host:
- `curl` (used for standard network HTTP/HTTPS operations, such as accessing remote registries like `https://bcr.bazel.build` or downloading external dependencies).

### Development Helper (`scripts/zel_dev.sh`)

For local offline development and test fixtures relying on the vendored registry at `third_party/bcr`, use the documented helper script:
```bash
./scripts/zel_dev.sh build //...
```
This automatically supplies `--registry=file://<repo>/third_party/bcr` when no explicit `--registry` option is passed, preserving Bazel's default `https://bcr.bazel.build` network behavior when running `zel` directly.

## Version Enforcement & Bazelisk Compatibility

`zel` includes a built-in Bazelisk-compatible version manager and launcher:

- **Version Resolution Order:**
  1. `USE_ZEL_VERSION` or `USE_BAZEL_VERSION` environment variable (supports version strings, `latest`, `<owner>/<repo>/<version>`, or direct executable paths)
  2. `.zelversion` or `.bazelversion` file in the workspace root
  3. `USE_ZEL_FALLBACK_VERSION` or `USE_BAZEL_FALLBACK_VERSION` (`error`, `warn:<version>`, `silent:<version>`, or `<version>`)
- **Resolution & Auto-Download:** When the required version differs from the running `zel` binary, `zel` searches sibling binaries (`zel-<version>-<os>-<arch>`), the local cache (`$ZEL_HOME` / `$BAZELISK_HOME`), or automatically downloads and SHA-256 verifies the requested release from `$ZEL_BASE_URL` / `$BAZELISK_BASE_URL` (defaulting to GitHub Releases). Set `ZEL_NO_DOWNLOAD=1` (or `BAZELISK_NO_DOWNLOAD=1`) to enforce strict offline version checking.
- **Workspace Wrapper Delegation:** Automatically delegates to `tools/zel` or `tools/bazel` (or `$ZEL_WRAPPER` / `$BAZEL_WRAPPER`) with `ZEL_REAL` and `BAZEL_REAL` pointing to the resolved binary.

### Using with External Bazelisk

Use a published release directly via `bazelisk`:

```bash
BAZELISK_BASE_URL=https://github.com/scottpledger/zel/releases/download \
  USE_BAZEL_VERSION=0.0.1 \
  bazel info release
```

Or package Zel locally into Bazelisk-compatible release binaries (`zel-<version>-<os>-<arch>` and `bazel-<version>-<os>-<arch>`):

```bash
./scripts/package_zel.sh --version 0.0.1 --output-dir dist/release
USE_BAZEL_VERSION=$PWD/dist/release/bazel-0.0.1-$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m) bazel info release
```

## Deviations from Bazel (native implementation)

Zel is a native executable, not a JVM server. The following `bazel info` items cannot report JVM facts; they are
*computed* by the same templates as Bazel's `InfoItem`s from Zel's own runtime properties (never copied from a real
Bazel run):

| key | Bazel (`InfoItem`) | Zel |
|---|---|---|
| `java-runtime` | `java.runtime.name (build java.runtime.version) by java.vendor` | `Zel native runtime (build <crate version>) by The Zel Authors` |
| `java-vm` | `java.vm.name (build java.vm.version, java.vm.info) by java.vm.vendor` | `Zel native code (build <crate version>, <os>-<arch> compiled) by The Zel Authors` |
| `character-encoding` | `file.encoding`, `Charset.defaultCharset()`, `sun.jnu.encoding` | `UTF-8` (Rust strings), `UTF-8`, and the file-name encoding derived from `LC_ALL`/`LC_CTYPE`/`LANG` |
| `starlark-semantics` | `StarlarkSemantics.toString()` (non-default keys) | the same, generated from `BuildLanguageOptions.setFlags` (`scripts/extract_semantics_keys.py`) and the effective flags |
| `worker_metrics` | live persistent workers (`WorkerMetricsInfoItem`) | the live workers of Zel's own worker pool (`No persistent workers active.` when there are none; no per-worker memory stats) |
| `peak-heap-size` | peak post-GC heap, `unknown` if no full GC ran | always `unknown` (there is no garbage collector, so no full GC ever runs) |
| `gc-count` / `gc-time` | JVM GC counters | `0` / `0ms` (no garbage collector) |
| `used-heap-size`, `committed-heap-size`, `max-heap-size` | JVM heap | process resident memory; `max-heap-size` is the host RAM (the process has no `-Xmx`) |
| `server_log` | `java.log.<host>.<user>.log.java.<ts>.<pid>` (target of the `java.log` link) | the same naming scheme for the daemon's own log file |

Option parsing differences that are inherent to the implementation language:

* `SpellChecker` suggestions walk `OptionsParserImpl.getAllValidArgs()` in `OptionsData` order. Bazel builds that
  order from a `HashSet<Class<? extends OptionsBase>>` (`BlazeCommandUtils.getOptions`), i.e. it depends on the identity
  hash codes of the option classes in the running JVM; ties between equally distant candidates (`--nobogus` ->
  `--noemacs` in Bazel 9.3.2) therefore cannot be reproduced. Zel iterates in declaration order.
* `HttpConnector` retries (8 attempts, `100ms << n` jittered back-off, 1s..10s connect timeouts) are reproduced by
  shelling out to `curl` per attempt (there is no TLS stack without external crates).

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
