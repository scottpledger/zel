#!/usr/bin/env bash
# Copyright 2026 The Zel Authors. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# End-to-end integration test verifying `zel` CLI, `package_zel.sh`,
# wrapper script discovery, self-hosting (`zel build //crates:zel`),
# and `bazelisk` (`USE_BAZEL_VERSION`) compatibility.

set -euo pipefail

# Isolate test execution from any ambient Bazelisk/Zel environment variables
unset USE_ZEL_VERSION USE_BAZEL_VERSION USE_ZEL_FALLBACK_VERSION USE_BAZEL_FALLBACK_VERSION
unset ZEL_SKIP_WRAPPER BAZELISK_SKIP_WRAPPER ZEL_BASE_URL BAZELISK_BASE_URL
unset ZEL_HOME BAZELISK_HOME ZEL_NO_DOWNLOAD BAZELISK_NO_DOWNLOAD
unset ZEL_VERIFY_SHA256 BAZELISK_VERIFY_SHA256 ZEL_WRAPPER BAZEL_WRAPPER ZEL_PRODUCT_PREFIX

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

TEST_TMP="$(mktemp -d "${TMPDIR:-/tmp}/zel-pkg-test.XXXXXX")"
trap 'rm -rf "${TEST_TMP}"' EXIT

READ_VERSION="$(cat "${WORKSPACE_ROOT}/.bazelversion" 2>/dev/null | tr -d '[:space:]' || echo "9.3.1")"
REL_DIR="${TEST_TMP}/release"
OB_DIR="${TEST_TMP}/ob"

# Build default (0.0.1) launcher binary first to test cross-version enforcement and auto-download
(cd "${WORKSPACE_ROOT}" && cargo build --bin zel)
LAUNCHER_DIR="${TEST_TMP}/launcher_bin"
mkdir -p "${LAUNCHER_DIR}"
cp "${WORKSPACE_ROOT}/target/debug/zel" "${LAUNCHER_DIR}/zel"
chmod +x "${LAUNCHER_DIR}/zel"

# 1. Package zel (${READ_VERSION}) using the dev profile for fast test execution
"${SCRIPT_DIR}/package_zel.sh"   --version="${READ_VERSION}"   --output_dir="${REL_DIR}"   --profile=dev

OS_NAME="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH_NAME="$(uname -m)"
case "${ARCH_NAME}" in
  x86_64|amd64) BAZELISK_ARCH="x86_64" ;;
  aarch64|arm64) BAZELISK_ARCH="arm64" ;;
  *) BAZELISK_ARCH="x86_64" ;;
esac

ZEL_BIN="${REL_DIR}/zel-${READ_VERSION}-${OS_NAME}-${BAZELISK_ARCH}"
BAZEL_BIN="${REL_DIR}/bazel-${READ_VERSION}-${OS_NAME}-${BAZELISK_ARCH}"

[[ -x "${ZEL_BIN}" ]] || { echo "FAIL: Missing executable ${ZEL_BIN}" >&2; exit 1; }
[[ -f "${ZEL_BIN}.sha256" ]] || { echo "FAIL: Missing sha256 ${ZEL_BIN}.sha256" >&2; exit 1; }
[[ -x "${BAZEL_BIN}" ]] || { echo "FAIL: Missing executable ${BAZEL_BIN}" >&2; exit 1; }
[[ -f "${BAZEL_BIN}.sha256" ]] || { echo "FAIL: Missing sha256 ${BAZEL_BIN}.sha256" >&2; exit 1; }

# 2. Verify native `zel --version` and `bazel --version` output formats
ZEL_VER_OUT="$("${ZEL_BIN}" --version)"
if [[ "${ZEL_VER_OUT}" != "zel ${READ_VERSION}" ]]; then
  echo "FAIL: Expected 'zel ${READ_VERSION}', got '${ZEL_VER_OUT}'" >&2
  exit 1
fi

BAZEL_VER_OUT="$("${BAZEL_BIN}" --version)"
if [[ "${BAZEL_VER_OUT}" != "bazel ${READ_VERSION}" ]]; then
  echo "FAIL: Expected 'bazel ${READ_VERSION}', got '${BAZEL_VER_OUT}'" >&2
  exit 1
fi

# 3. Verify wrapper script discovers and runs the packaged binary
WRAPPER_DIR="${TEST_TMP}/wrapper_bin"
mkdir -p "${WRAPPER_DIR}"
cp "${SCRIPT_DIR}/zel.sh" "${WRAPPER_DIR}/bazel"
chmod +x "${WRAPPER_DIR}/bazel"
cp "${BAZEL_BIN}" "${WRAPPER_DIR}/bazel-${READ_VERSION}-${OS_NAME}-${ARCH_NAME}"
chmod +x "${WRAPPER_DIR}/bazel-${READ_VERSION}-${OS_NAME}-${ARCH_NAME}"

WRAPPER_VER="$(cd "${WORKSPACE_ROOT}" && "${WRAPPER_DIR}/bazel" --version)"
if [[ "${WRAPPER_VER}" != "bazel ${READ_VERSION}" ]]; then
  echo "FAIL: Wrapper returned '${WRAPPER_VER}', expected 'bazel ${READ_VERSION}'" >&2
  exit 1
fi

# 4. Verify Bazelisk (`USE_BAZEL_VERSION=<path>`) works end-to-end if installed
if command -v bazel &>/dev/null; then
  BAZELISK_OUT="$(cd "${WORKSPACE_ROOT}" && USE_BAZEL_VERSION="${BAZEL_BIN}" bazel --output_base="${OB_DIR}" info release)"
  if [[ "${BAZELISK_OUT}" != *"zel"* ]]; then
    echo "FAIL: Bazelisk did not invoke zel: ${BAZELISK_OUT}" >&2
    exit 1
  fi
fi

# 5. Verify built-in Bazelisk version enforcement, auto-download, and tools/zel wrapper delegation
ENFORCE_WS="${TEST_TMP}/enforce_ws"
mkdir -p "${ENFORCE_WS}"
touch "${ENFORCE_WS}/MODULE.zel"
echo "${READ_VERSION}" > "${ENFORCE_WS}/.zelversion"

CACHE_HOME="${TEST_TMP}/zel_cache_home"

# 5a. Running 0.0.1 launcher in a workspace requiring ${READ_VERSION} with ZEL_NO_DOWNLOAD=1 must fail
if (cd "${ENFORCE_WS}" && ZEL_HOME="${CACHE_HOME}" ZEL_NO_DOWNLOAD=1 "${LAUNCHER_DIR}/zel" --version >/dev/null 2>&1); then
  echo "FAIL: Expected 0.0.1 launcher in ${READ_VERSION} workspace with ZEL_NO_DOWNLOAD=1 to fail" >&2
  exit 1
fi

# 5b. Auto-downloading ${READ_VERSION} from ZEL_BASE_URL into ZEL_HOME when running 0.0.1 launcher
DL_VER_OUT="$(
  cd "${ENFORCE_WS}" && \
  ZEL_BASE_URL="file://${REL_DIR}" \
  ZEL_HOME="${CACHE_HOME}" \
  "${LAUNCHER_DIR}/zel" --version
)"
if [[ "${DL_VER_OUT}" != "zel ${READ_VERSION}" ]]; then
  echo "FAIL: Expected 'zel ${READ_VERSION}' from launcher after auto-download, got '${DL_VER_OUT}'" >&2
  exit 1
fi

# 5c. Subsequent run with ZEL_NO_DOWNLOAD=1 succeeds using the cached binary in ZEL_HOME
CACHED_VER_OUT="$(
  cd "${ENFORCE_WS}" && \
  ZEL_HOME="${CACHE_HOME}" \
  ZEL_NO_DOWNLOAD=1 \
  "${LAUNCHER_DIR}/zel" --version
)"
if [[ "${CACHED_VER_OUT}" != "zel ${READ_VERSION}" ]]; then
  echo "FAIL: Expected 'zel ${READ_VERSION}' from cached binary, got '${CACHED_VER_OUT}'" >&2
  exit 1
fi

# 5d. Verify `tools/zel` workspace wrapper receives ZEL_REAL and BAZEL_REAL
mkdir -p "${ENFORCE_WS}/tools"
cat > "${ENFORCE_WS}/tools/zel" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
echo "wrapped:$("${ZEL_REAL}" --version)"
EOF
chmod +x "${ENFORCE_WS}/tools/zel"

WRAPPED_OUT="$(cd "${ENFORCE_WS}" && ZEL_HOME="${CACHE_HOME}" "${LAUNCHER_DIR}/zel" --version)"
if [[ "${WRAPPED_OUT}" != "wrapped:zel ${READ_VERSION}" ]]; then
  echo "FAIL: Expected 'wrapped:zel ${READ_VERSION}' from tools/zel wrapper, got '${WRAPPED_OUT}'" >&2
  exit 1
fi

# 5e. Verify executing `zel` via `bazelisk` using `.bazeliskrc`, `.bazelversion`, and shared BAZELISK_HOME cache
if command -v bazelisk &>/dev/null || command -v bazel &>/dev/null; then
  BAZELISK_CMD="bazel"
  if command -v bazelisk &>/dev/null; then
    BAZELISK_CMD="bazelisk"
  fi
  BZK_WS="${TEST_TMP}/bazelisk_ws"
  mkdir -p "${BZK_WS}"
  touch "${BZK_WS}/MODULE.bazel"
  echo "BAZELISK_BASE_URL=https://github.com/scottpledger/zel/releases/download" > "${BZK_WS}/.bazeliskrc"
  echo "${READ_VERSION}" > "${BZK_WS}/.bazelversion"

  BZK_VER_OUT="$(cd "${BZK_WS}" && BAZELISK_HOME="${CACHE_HOME}" "${BAZELISK_CMD}" --version)"
  if [[ "${BZK_VER_OUT}" != "bazel ${READ_VERSION}" ]]; then
    echo "FAIL: Expected 'bazel ${READ_VERSION}' when executing zel via bazelisk, got '${BZK_VER_OUT}'" >&2
    exit 1
  fi

  BZK_REL_OUT="$(cd "${BZK_WS}" && BAZELISK_HOME="${CACHE_HOME}" "${BAZELISK_CMD}" --output_base="${OB_DIR}" info release)"
  if [[ "${BZK_REL_OUT}" != *"zel"* ]]; then
    echo "FAIL: Expected zel release info from bazelisk, got '${BZK_REL_OUT}'" >&2
    exit 1
  fi
fi

# 6. Verify self-hosting query on `//crates:all`
QUERY_OUT="$(cd "${WORKSPACE_ROOT}" && "${ZEL_BIN}" --output_base="${OB_DIR}" query //crates:all)"
if [[ "${QUERY_OUT}" != *"//crates:zel"* ]]; then
  echo "FAIL: Self-hosted query did not find //crates:zel: ${QUERY_OUT}" >&2
  exit 1
fi

echo "PASS: zel packaging, wrapper, built-in Bazelisk version enforcement, self-hosted query, and Bazelisk compatibility verified."
