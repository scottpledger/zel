#!/bin/bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMP_OUT="$(mktemp /tmp/flag_tables_check.XXXXXX.rs)"
trap 'rm -f "$TEMP_OUT"' EXIT

BAZEL_SRC="${BAZEL_SRC_DIR:-$HOME/Development/bazel}"
if [ ! -d "$BAZEL_SRC" ]; then
    echo "Error: Bazel source tree not found at $BAZEL_SRC" >&2
    exit 1
fi

echo "Running flag table generator against $BAZEL_SRC..."
python3 "$REPO_ROOT/scripts/extract_bazel_options.py" --bazel-dir="$BAZEL_SRC" --out="$TEMP_OUT"

COMMITTED="$REPO_ROOT/crates/zel-options/src/flag_tables.rs"

if cmp -s "$COMMITTED" "$TEMP_OUT"; then
    echo "SUCCESS: flag_tables.rs is up-to-date and matches generator output exactly."
    exit 0
else
    echo "ERROR: flag_tables.rs differs from generator output:" >&2
    diff -u "$COMMITTED" "$TEMP_OUT" | head -n 50 >&2
    exit 1
fi
