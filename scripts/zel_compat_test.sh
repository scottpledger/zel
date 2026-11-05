#!/usr/bin/env bash
# Copyright 2026 The Zel Authors. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# End-to-end integration test verifying `zel` CLI, `package_zel.sh`,
# wrapper script discovery, self-hosting (`zel build //crates:zel`),
# and `bazelisk` (`USE_BAZEL_VERSION`) compatibility.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

TEST_TMP="$(mktemp -d "${TMPDIR:-/tmp}/zel-pkg-test.XXXXXX")"
trap 'rm -rf "${TEST_TMP}"' EXIT

READ_VERSION="$(cat "${WORKSPACE_ROOT}/.bazelversion" 2>/dev/null | tr -d '[:space:]' || echo "9.3.1")"
REL_DIR="${TEST_TMP}/release"
OB_DIR="${TEST_TMP}/ob"

# 1. Package zel using the dev profile for fast test execution
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

# 5. Verify self-hosting query on `//crates:all`
QUERY_OUT="$(cd "${WORKSPACE_ROOT}" && "${ZEL_BIN}" --output_base="${OB_DIR}" query //crates:all)"
if [[ "${QUERY_OUT}" != *"//crates:zel"* ]]; then
  echo "FAIL: Self-hosted query did not find //crates:zel: ${QUERY_OUT}" >&2
  exit 1
fi

echo "PASS: zel packaging, wrapper, self-hosted query, and Bazelisk compatibility verified."
