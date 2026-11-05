# Contributing to Zel

Thank you for your interest in contributing to Zel!

## Development Workflow

1. **Build and Test with Cargo**:
   ```bash
   cargo test --workspace
   ./scripts/zel_compat_test.sh
   ```
2. **Build and Test with Bazel**:
   ```bash
   bazel test //crates/...
   ```
3. **Self-Hosting Check**:
   Always use an isolated `--output_base` when testing a newly built `zel` binary against the workspace:
   ```bash
   ./target/debug/zel --output_base=/tmp/ob-dev build //crates:zel
   ```
