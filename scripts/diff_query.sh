#!/bin/bash
set -eo pipefail
# Reproducible differential harness script for query / cquery / aquery
# Compares zel against real bazel (9.3.2) on the fixture workspace.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WS="${1:-/tmp/vq3/ws}"
if [ ! -d "$WS" ]; then
    WS="$SCRIPT_DIR/../crates/zel-query/tests/fixtures/query-matrix"
fi
CMD_LIST="${2:-/tmp/vq3/all.txt}"
if [ ! -f "$CMD_LIST" ]; then
    CMD_LIST="$SCRIPT_DIR/../crates/zel-query/tests/fixtures/all.txt"
fi
ZEL_BIN="${ZEL_BIN:-/tmp/zel-target/release/zel}"
BAZEL_BIN="${BAZEL_BIN:-/tmp/bazel-9.3.2}"  # real 9.3.2 binary, never bazelisk
BOB="${BOB:-/tmp/diff_bob}"
ZOB="${ZOB:-/tmp/diff_zob}"

mkdir -p "$BOB" "$ZOB"
cd "$WS"

norm() {
    # Normalizes only absolute paths, timestamps, timing lines and hashes
    sed -E \
        -e 's/\(([0-9a-f]{7})\)/(H7)/g' \
        -e 's/[0-9a-f]{64}/H64/g' \
        -e 's/\b[0-9a-f]{7}\b/H7/g' \
        -e 's/\/private\/tmp\/diff_[a-z]+/OUTPUT_BASE/g' \
        -e 's/\/tmp\/diff_[a-z]+/OUTPUT_BASE/g' \
        -e 's/\/private\/tmp/\/tmp/g' \
        -e 's/\/Users\/[^/]+/USER/g' \
        "$1" | grep -vE '^(INFO: (Elapsed|Analyzed|Build completed|Found|Invocation|Streaming|Current date|Repository|Reading|Computing)|Loading:|Analyzing:|\(|DEBUG:| no actions running|Starting local|WARNING: Build option|INFO: Empty)'
}

errn() {
    tr '\r' '\n' < "$1" | sed -E \
        -e 's/\x1b\[[0-9;]*m//g' \
        -e 's/\(([0-9a-f]{7})\)/(H7)/g' \
        -e 's/[0-9a-f]{64}/H64/g' \
        -e 's/\b[0-9a-f]{7}\b/H7/g' \
        -e 's/\/private\/tmp\/diff_[a-z]+/OUTPUT_BASE/g' \
        -e 's/\/tmp\/diff_[a-z]+/OUTPUT_BASE/g' \
        -e 's/\/private\/tmp/\/tmp/g' \
        -e 's/\/Users\/[^/]+/USER/g' \
        | grep -E '^(ERROR|FATAL|WARNING: --)'
}

passed=0
failed=0
total=0

while IFS= read -r line || [ -n "$line" ]; do
    [[ "$line" =~ ^[[:space:]]*# ]] && continue
    [[ -z "${line// }" ]] && continue
    total=$((total + 1))
    
    eval "timeout 60 $BAZEL_BIN --output_base=\"$BOB\" $line" >/tmp/b.out 2>/tmp/b.err || be=$?
    be=${be:-0}
    eval "timeout 60 $ZEL_BIN --output_base=\"$ZOB\" $line" >/tmp/z.out 2>/tmp/z.err || ze=$?
    ze=${ze:-0}

    st=""
    if [ "$be" != "$ze" ]; then
        st="EXIT b=$be z=$ze"
    fi
    if ! diff -q <(norm /tmp/b.out) <(norm /tmp/z.out) >/dev/null 2>&1; then
        st="$st STDOUT"
    fi
    if ! diff -q <(errn /tmp/b.err) <(errn /tmp/z.err) >/dev/null 2>&1; then
        st="$st STDERR"
    fi

    if [ -z "$st" ]; then
        passed=$((passed + 1))
        echo "[OK] $line"
    else
        failed=$((failed + 1))
        echo "[$st] $line"
        if [ -n "${VERB:-}" ] && [ "${VERB}" -gt 0 ] 2>/dev/null; then
            echo "--- STDOUT DIFF ---"
            diff -u <(norm /tmp/b.out) <(norm /tmp/z.out) | head -n "${VERB}" || true
            echo "--- STDERR DIFF ---"
            diff -u <(errn /tmp/b.err) <(errn /tmp/z.err) | head -n "${VERB}" || true
        fi
    fi
    unset be ze
done < "$CMD_LIST"

echo "=== Summary: $passed / $total passed ($failed failed) ==="
