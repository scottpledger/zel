#!/bin/bash
# scripts/zel_dev.sh
# Development helper that invokes Zel with the local vendored registry if --registry is not specified.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Find or build zel binary
ZEL_BIN="${ZEL_BIN:-}"
if [ -z "$ZEL_BIN" ]; then
    if [ -x "$REPO_ROOT/target/release/zel" ]; then
        ZEL_BIN="$REPO_ROOT/target/release/zel"
    elif [ -x "$REPO_ROOT/target/debug/zel" ]; then
        ZEL_BIN="$REPO_ROOT/target/debug/zel"
    elif [ -x "/tmp/zel-target-fix-cmds4/release/zel" ]; then
        ZEL_BIN="/tmp/zel-target-fix-cmds4/release/zel"
    elif [ -x "/tmp/zel-target-fix-cmds4/debug/zel" ]; then
        ZEL_BIN="/tmp/zel-target-fix-cmds4/debug/zel"
    else
        ZEL_BIN="zel"
    fi
fi

# Ensure local vendored registry exists
REG_DIR="/tmp/zel-registry"
if [ ! -f "$REG_DIR/bazel_registry.json" ]; then
    python3 "$REPO_ROOT/scripts/make_vendored_registry.py" "$REG_DIR" >/dev/null 2>&1 || true
fi

HAS_REGISTRY=false
for arg in "$@"; do
    if [[ "$arg" == --registry=* || "$arg" == "--registry" ]]; then
        HAS_REGISTRY=true
        break
    fi
done

if [ "$HAS_REGISTRY" = false ] && [ -d "$REG_DIR" ]; then
    exec "$ZEL_BIN" "$@" "--registry=file://$REG_DIR"
else
    exec "$ZEL_BIN" "$@"
fi
