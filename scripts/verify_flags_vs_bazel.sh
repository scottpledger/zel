#!/bin/bash
# scripts/verify_flags_vs_bazel.sh
#
# Genuinely verifies `zel help flags-as-proto` against the real Bazel binary:
#   1. both `help flags-as-proto` outputs (base64 FlagCollection) are decoded - with `protoc --decode` when it is
#      installed, otherwise with `zel-flags-dump` (zel-proto's schema driven decoder);
#   2. scripts/flags_proto_diff.py compares every field of every flag (name, has_negative_flag, documentation,
#      commands, abbreviation, allows_multiple, effect_tags, metadata_tags, documentation_category,
#      requires_value, default_value, old_name, deprecation_warning, option_expansions, type_converter,
#      enum_values), the set of flags and the flag order;
#   3. there is NO skew tolerance: the tables are generated from the Bazel 9.3.2 tree, which is the source of the
#      real binary, so every difference is a failure.
#
# Exit status: 0 only if there is no non-allowlisted difference. A missing real bazel binary is an error
# (set ZEL_VERIFY_SKIP_NO_BAZEL=1 to turn that into an explicit, loudly reported skip).
#
# Environment: REAL_BAZEL (default: bazel on PATH / /tmp/bazel-9.3.2), ZEL (zel binary; default: built
# from this workspace into $CARGO_TARGET_DIR or ./target), BAZEL_SRC_DIR (default ~/Development/bazel).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REAL_BAZEL="${REAL_BAZEL:-$(command -v bazel || echo /tmp/bazel-9.3.2)}"
BAZEL_SRC="${BAZEL_SRC_DIR:-$HOME/Development/bazel}"

# The reference must be the real 9.3.2 binary (use the direct binary, never bazelisk: cwd-dependent .bazelversion).
case "$("$REAL_BAZEL" --version 2>/dev/null)" in
    "bazel 9.3.2") ;;
    *) echo "ERROR: REAL_BAZEL must be a direct Bazel 9.3.2 binary (got: $("$REAL_BAZEL" --version 2>&1 | head -1))" >&2; exit 1;;
esac
if [ ! -x "$REAL_BAZEL" ]; then
    if [ "${ZEL_VERIFY_SKIP_NO_BAZEL:-0}" = "1" ]; then
        echo "SKIPPED: real bazel not found at $REAL_BAZEL (ZEL_VERIFY_SKIP_NO_BAZEL=1)" >&2
        exit 0
    fi
    echo "ERROR: real bazel not found at $REAL_BAZEL; set REAL_BAZEL" >&2
    exit 1
fi

TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
ZEL="${ZEL:-$TARGET_DIR/debug/zel}"
if [ ! -x "$ZEL" ]; then
    (cd "$REPO_ROOT" && cargo build --bin zel) >&2
fi

TMP="$(mktemp -d /tmp/zel_verify_flags.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT
# Run both binaries from a neutral empty directory (zel honours .bazelversion like bazelisk, which would hand the
# invocation to a real Bazel when run from a checkout that has one).
mkdir -p "$TMP/cwd"
cd "$TMP/cwd"

decode() { # base64 file -> text file
    if command -v protoc >/dev/null 2>&1 && [ -f "$BAZEL_SRC/src/main/protobuf/bazel_flags.proto" ]; then
        base64 -d < "$1" 2>/dev/null | protoc --proto_path="$BAZEL_SRC/src/main/protobuf" \
            --decode=bazel_flags.FlagCollection bazel_flags.proto > "$2" \
            || base64 -D < "$1" | protoc --proto_path="$BAZEL_SRC/src/main/protobuf" \
            --decode=bazel_flags.FlagCollection bazel_flags.proto > "$2"
    else
        DUMP="${ZEL_FLAGS_DUMP:-$TARGET_DIR/debug/zel-flags-dump}"
        [ -x "$DUMP" ] || (cd "$REPO_ROOT" && cargo build -p zel-proto --bin zel-flags-dump) >&2
        "$DUMP" < "$1" > "$2"
    fi
}

"$REAL_BAZEL" --output_base="$TMP/real_ob" help flags-as-proto > "$TMP/real.b64" 2> "$TMP/real.err" \
    || { cat "$TMP/real.err" >&2; echo "ERROR: real bazel help flags-as-proto failed" >&2; exit 1; }
"$ZEL" --output_base="$TMP/zel_ob" help flags-as-proto > "$TMP/zel.b64" \
    || { echo "ERROR: zel help flags-as-proto failed" >&2; exit 1; }
decode "$TMP/real.b64" "$TMP/real.txt"
decode "$TMP/zel.b64" "$TMP/zel.txt"

python3 "$REPO_ROOT/scripts/flags_proto_diff.py" "$TMP/real.txt" "$TMP/zel.txt"
