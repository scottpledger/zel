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
| `zel-client` | `zel` CLI client, workspace discovery, server lock/ping/spawn lifecycle, and `--batch` execution |
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

## Using with Bazelisk

Package Zel into Bazelisk-compatible release binaries (`zel-<version>-<os>-<arch>` and `bazel-<version>-<os>-<arch>`):

```bash
./scripts/package_zel.sh --version 9.3.1 --output-dir dist/release
USE_BAZEL_VERSION=$PWD/dist/release/bazel-9.3.1-$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m) bazel info release
```

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
