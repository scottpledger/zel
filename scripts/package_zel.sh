#!/usr/bin/env bash
# Copyright 2026 The Zel Authors. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Builds `zel` and packages single-file release binaries and SHA-256 checksums
# for both native `zel` consumers and `bazelisk` (`BAZELISK_BASE_URL`):
#   <output_dir>/zel-<version>-<os>-<arch>[.exe][.sha256]
#   <output_dir>/bazel-<version>-<os>-<arch>[.exe][.sha256]
#   <output_dir>/bazel_nojdk-<version>-<os>-<arch>[.exe][.sha256]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

VERSION=""
OUTPUT_DIR="${WORKSPACE_ROOT}/dist/release"
TARGET_TRIPLE=""
PROFILE="release"
OS_OVERRIDE=""
ARCH_OVERRIDE=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version=*)
      VERSION="${1#*=}"
      shift
      ;;
    --version)
      VERSION="$2"
      shift 2
      ;;
    --output-dir=*|--output_dir=*)
      OUTPUT_DIR="${1#*=}"
      shift
      ;;
    --output-dir|--output_dir)
      OUTPUT_DIR="$2"
      shift 2
      ;;
    --target=*)
      TARGET_TRIPLE="${1#*=}"
      shift
      ;;
    --target)
      TARGET_TRIPLE="$2"
      shift 2
      ;;
    --os=*)
      OS_OVERRIDE="${1#*=}"
      shift
      ;;
    --os)
      OS_OVERRIDE="$2"
      shift 2
      ;;
    --arch=*)
      ARCH_OVERRIDE="${1#*=}"
      shift
      ;;
    --arch)
      ARCH_OVERRIDE="$2"
      shift 2
      ;;
    --debug|--dev)
      PROFILE="dev"
      shift
      ;;
    --profile=*)
      PROFILE="${1#*=}"
      shift
      ;;
    *)
      echo "Unknown option: $1" >&2
      exit 1
      ;;
  esac
done

if [[ -z "${VERSION}" ]]; then
  if [[ -n "${ZEL_EMBED_LABEL:-}" ]]; then
    VERSION="${ZEL_EMBED_LABEL}"
  elif [[ -n "${BAZEL_EMBED_LABEL:-}" ]]; then
    VERSION="${BAZEL_EMBED_LABEL}"
  elif [[ -f "${WORKSPACE_ROOT}/.bazelversion" ]]; then
    VERSION="$(head -n 1 "${WORKSPACE_ROOT}/.bazelversion" | tr -d '[:space:]')"
  else
    VERSION="0.1.0"
  fi
fi

if [[ -n "${OS_OVERRIDE}" ]]; then
  BAZELISK_OS="${OS_OVERRIDE}"
else
  UNAME_S="$(uname -s | tr '[:upper:]' '[:lower:]')"
  case "${UNAME_S}" in
    linux*) BAZELISK_OS="linux" ;;
    darwin*) BAZELISK_OS="darwin" ;;
    msys*|mingw*|cygwin*|windows*) BAZELISK_OS="windows" ;;
    *) BAZELISK_OS="${UNAME_S}" ;;
  esac
fi

if [[ -n "${ARCH_OVERRIDE}" ]]; then
  BAZELISK_ARCH="${ARCH_OVERRIDE}"
else
  UNAME_M="$(uname -m | tr '[:upper:]' '[:lower:]')"
  case "${UNAME_M}" in
    x86_64|amd64) BAZELISK_ARCH="x86_64" ;;
    aarch64|arm64) BAZELISK_ARCH="arm64" ;;
    *) BAZELISK_ARCH="${UNAME_M}" ;;
  esac
fi

EXE_EXT=""
if [[ "${BAZELISK_OS}" == "windows" ]]; then
  EXE_EXT=".exe"
fi

CARGO_ARGS=(--bin zel)
if [[ "${PROFILE}" == "release" ]]; then
  CARGO_ARGS+=(--release)
fi
if [[ -n "${TARGET_TRIPLE}" ]]; then
  CARGO_ARGS+=(--target "${TARGET_TRIPLE}")
fi

echo "+++ Building zel (version=${VERSION}, os=${BAZELISK_OS}, arch=${BAZELISK_ARCH}, profile=${PROFILE})"
(
  cd "${WORKSPACE_ROOT}"
  ZEL_EMBED_LABEL="${VERSION}" cargo build "${CARGO_ARGS[@]}"
)

TARGET_DIR_NAME="${PROFILE}"
if [[ "${PROFILE}" == "dev" ]]; then
  TARGET_DIR_NAME="debug"
fi

if [[ -n "${TARGET_TRIPLE}" ]]; then
  BUILT_BIN="${WORKSPACE_ROOT}/target/${TARGET_TRIPLE}/${TARGET_DIR_NAME}/zel${EXE_EXT}"
else
  BUILT_BIN="${WORKSPACE_ROOT}/target/${TARGET_DIR_NAME}/zel${EXE_EXT}"
fi

mkdir -p "${OUTPUT_DIR}"

ZEL_ARTIFACT="zel-${VERSION}-${BAZELISK_OS}-${BAZELISK_ARCH}${EXE_EXT}"
BAZEL_ARTIFACT="bazel-${VERSION}-${BAZELISK_OS}-${BAZELISK_ARCH}${EXE_EXT}"
NOJDK_ARTIFACT="bazel_nojdk-${VERSION}-${BAZELISK_OS}-${BAZELISK_ARCH}${EXE_EXT}"

for artifact in "${ZEL_ARTIFACT}" "${BAZEL_ARTIFACT}" "${NOJDK_ARTIFACT}"; do
  cp -f "${BUILT_BIN}" "${OUTPUT_DIR}/${artifact}"
  chmod 0755 "${OUTPUT_DIR}/${artifact}"
done

compute_sha256() {
  local file="$1"
  local dir
  dir="$(dirname "${file}")"
  local base
  base="$(basename "${file}")"
  (
    cd "${dir}"
    if command -v sha256sum &>/dev/null; then
      sha256sum "${base}" > "${base}.sha256"
    elif command -v shasum &>/dev/null; then
      shasum -a 256 "${base}" > "${base}.sha256"
    fi
  )
}

compute_sha256 "${OUTPUT_DIR}/${ZEL_ARTIFACT}"
compute_sha256 "${OUTPUT_DIR}/${BAZEL_ARTIFACT}"
compute_sha256 "${OUTPUT_DIR}/${NOJDK_ARTIFACT}"

echo "+++ Packaged Zel & Bazelisk release artifacts in ${OUTPUT_DIR}:"
echo "  - ${OUTPUT_DIR}/${ZEL_ARTIFACT}"
echo "  - ${OUTPUT_DIR}/${BAZEL_ARTIFACT}"
echo "  - ${OUTPUT_DIR}/${NOJDK_ARTIFACT}"
