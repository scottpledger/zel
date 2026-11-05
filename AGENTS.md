# Zel Codebase Guide for AI Agents

Welcome to the **Zel** repository — a native Rust implementation of Bazel.

## Building & Testing

* **Fast Cargo Build:** `cargo build --bin zel`
* **Release Build:** `cargo build --release --bin zel`
* **Run All Workspace Tests:** `cargo test --workspace`
* **Run Packaging & Bazelisk Compatibility Test:** `./scripts/zel_compat_test.sh`
* **Bazel Build (`@rules_rs`):** `bazel build //:zel`
* **Bazel Test (`@rules_rs`):** `bazel test //crates/...`

## Iterative Self-Hosting Workflow

When running `zel` against this workspace, always specify an isolated `--output_base`:

```bash
./target/debug/zel --output_base=/tmp/zel-ob build //crates:zel
```

## Repository Structure

All 19 Rust crates live in `crates/zel-*` and are wired into both `Cargo.toml` and `crates/BUILD.bazel`.
