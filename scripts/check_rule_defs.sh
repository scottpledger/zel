#!/usr/bin/env bash
# Fails if crates/zel-packages/src/rule_defs_gen.rs is not exactly what scripts/gen_rule_defs.py
# derives from the Bazel source tree (default ~/Development/bazel; override with BAZEL_SRC).
set -euo pipefail
cd "$(dirname "$0")/.."
exec python3 scripts/gen_rule_defs.py --check --src "${BAZEL_SRC:-$HOME/Development/bazel}"
