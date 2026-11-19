#!/bin/bash

# Copyright 2019 The Bazel Authors. All rights reserved.
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#    http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

set -eu

# This is a script which can be installed as your "bazel" binary instead of the
# real Bazel binary. When called, it tries to determine and run the correct
# Bazel version for a given project and forwards all arguments to it.
#
# You can specify which Bazel version to use using these methods:
# 1. Set $USE_BAZEL_VERSION to a version number
#    (e.g. export USE_BAZEL_VERSION=1.0.0).
# 2. Add a .bazelversion file that contains a version number next to your
#    WORKSPACE[.bazel] file.
# 3. Otherwise, the latest Bazel version will be used.
#
# This wrapper only recognizes Bazel versions installed next to itself, thus
# if you install this wrapper as /usr/bin/bazel, you'll have to install binaries
# for individual Bazel binaries as e.g. /usr/bin/bazel-1.0.0.
#
# In addition, if an executable called "tools/bazel" is found in the current
# workspace, this script will not directly execute Bazel, but instead store
# the path to the real Bazel executable in the environment variable BAZEL_REAL
# and then execute the "tools/bazel" wrapper script. The location of the wrapper
# script relative to the workspace can be changed with the $BAZEL_WRAPPER
# environment variable.
#
# In contrast to Bazelisk, this script does not download anything from the
# internet and instead relies on the local system to provide Bazel binaries.

function color() {
      # Usage: color "31;5" "string"
      # Some valid values for color:
      # - 5 blink, 1 strong, 4 underlined
      # - fg: 31 red,  32 green, 33 yellow, 34 blue, 35 purple, 36 cyan, 37 white
      # - bg: 40 black, 41 red, 44 blue, 45 purple
      printf '\033[%sm%s\033[0m\n' "$@"
}

# `readlink -f` that works on OSX too.
function get_realpath() {
    if [ "$(uname -s)" == "Darwin" ]; then
        local queue="$1"
        if [[ "${queue}" != /* ]] ; then
            # Make sure we start with an absolute path.
            queue="${PWD}/${queue}"
        fi
        local current=""
        while [ -n "${queue}" ]; do
            # Removing a trailing /.
            queue="${queue#/}"
            # Pull the first path segment off of queue.
            local segment="${queue%%/*}"
            # If this is the last segment.
            if [[ "${queue}" != */* ]] ; then
                segment="${queue}"
                queue=""
            else
                # Remove that first segment.
                queue="${queue#*/}"
            fi
            local link="${current}/${segment}"
            if [ -h "${link}" ] ; then
                link="$(readlink "${link}")"
                queue="${link}/${queue}"
                if [[ "${link}" == /* ]] ; then
                    current=""
                fi
            else
                current="${link}"
            fi
        done

        echo "${current}"
    else
        readlink -f "$1"
    fi
}

function get_workspace_root() {
  workspace_dir="${PWD}"
  while [[ "${workspace_dir}" != / ]]; do
    if [[ -e "${workspace_dir}/MODULE.zel" || -e "${workspace_dir}/MODULE.bazel" || -e "${workspace_dir}/REPO.bazel" || -e "${workspace_dir}/WORKSPACE.zel" || -e "${workspace_dir}/WORKSPACE.bazel" || -e "${workspace_dir}/WORKSPACE" ]]; then
      readonly workspace_dir
      return
    fi
    workspace_dir="$(dirname "${workspace_dir}")"
  done
  readonly workspace_dir=""
}

get_workspace_root

readonly wrapper_dir="$(dirname "$(get_realpath "${BASH_SOURCE[0]}")")"
readonly os_arch_suffix="$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)"

function get_bazel_version() {
  bazel_version=""

  if [[ -n ${USE_ZEL_VERSION:-} ]]; then
    reason="specified in \$USE_ZEL_VERSION"
    bazel_version="${USE_ZEL_VERSION}"
  elif [[ -n ${USE_BAZEL_VERSION:-} ]]; then
    reason="specified in \$USE_BAZEL_VERSION"
    bazel_version="${USE_BAZEL_VERSION}"
  elif [[ -n "${workspace_dir}" && -e "${workspace_dir}/.zelversion" ]]; then
    reason="specified in ${workspace_dir}/.zelversion"
    read -r bazel_version < "${workspace_dir}/.zelversion" || true
  elif [[ -n "${workspace_dir}" && -e "${workspace_dir}/.bazelversion" ]]; then
    reason="specified in ${workspace_dir}/.bazelversion"
    # The "read" can fail if the .bazelversion file does not contain a final
    # newline character. It will still correctly assign the read data to the
    # variable. Of course it can also fail due to real I/O errors. Because we do
    # validation of the read data later, we can just ignore the error here.
    # Alternatives like 'bazel_version=$(head -1 ...)' are not better either,
    # because bash does not care about the exit code of processes used in
    # command substitution.
    read -r bazel_version < "${workspace_dir}/.bazelversion" || true
  fi

  # Strip optional leading 'v' if followed by a digit.
  if [[ "${bazel_version}" =~ ^v[0-9] ]]; then
    bazel_version="${bazel_version#v}"
  fi

  # If we read an empty string or the magic word "latest" from .bazelversion or
  # $USE_BAZEL_VERSION, then we should use the latest available stable version.
  # This mimics the behavior of Bazelisk.
  if [[ -z $bazel_version || $bazel_version == "latest" ]]; then
    if [[ -x "${wrapper_dir}/zel-real" || -x "${wrapper_dir}/bazel-real" ]]; then
      reason="${reason:-"automatically selected real binary"}"
      bazel_version="real"
    else
      # Find the latest Zel or Bazel version installed on the system.
      reason="${reason:-"automatically selected latest available version"}"
      bazel_version="$(basename "$(find -H "${wrapper_dir}" -maxdepth 1 \( -name "zel-[0-9]*-${os_arch_suffix}" -o -name "bazel-[0-9]*-${os_arch_suffix}" \) -type f | sort -V | tail -n 1)")"
      if [[ -z $bazel_version ]]; then
        bazel_version="$(basename "$(find -H "${wrapper_dir}" -maxdepth 1 \( -name 'zel-[0-9]*' -o -name 'bazel-[0-9]*' \) -type f | sort -V | tail -n 1)")"
      fi
      # Remove the "zel-" or "bazel-" prefix and optional os_arch_suffix from the file name.
      bazel_version="${bazel_version#"zel-"}"
      bazel_version="${bazel_version#"bazel-"}"
      bazel_version="${bazel_version%"-${os_arch_suffix}"}"
    fi
  fi

  readonly reason
  readonly bazel_version
}

get_bazel_version

BAZEL_REAL=""
for candidate in \
  "${wrapper_dir}/zel-${bazel_version}-${os_arch_suffix}" \
  "${wrapper_dir}/zel-${bazel_version}" \
  "${wrapper_dir}/bazel-${bazel_version}-${os_arch_suffix}" \
  "${wrapper_dir}/bazel-${bazel_version}"; do
  if [[ -x "${candidate}" ]]; then
    BAZEL_REAL="${candidate}"
    break
  fi
done

# Last try: Maybe `zel-real` or `bazel-real` is actually the requested correct version?
for real_cand in "${wrapper_dir}/zel-real" "${wrapper_dir}/bazel-real"; do
  if [[ ! -x "${BAZEL_REAL}" && -x "${real_cand}" ]]; then
    real_ver="$("${real_cand}" --version | awk 'NR==1 {print $2}')"
    if [[ "${real_ver}" == "${bazel_version}" || "${bazel_version}" == "real" ]]; then
      BAZEL_REAL="${real_cand}"
      break
    fi
  fi
done
ZEL_REAL="${BAZEL_REAL}"

# If the repository contains a checked-in executable set by $ZEL_WRAPPER / $BAZEL_WRAPPER
# (defaults to tools/zel or tools/bazel), we assume that they know what they're doing and
# have their own way of versioning Zel/Bazel.
if [[ -n "${workspace_dir}" ]]; then
  for wrapper_rel in "${ZEL_WRAPPER:-}" "${BAZEL_WRAPPER:-}" "tools/zel" "tools/bazel"; do
    [[ -n "${wrapper_rel}" ]] || continue
    wrapper="${workspace_dir}/${wrapper_rel}"
    if [[ -x "${wrapper}" && -f "${wrapper}" ]]; then
      export BAZEL_REAL ZEL_REAL
      exec -a "$0" "${wrapper}" "$@"
    fi
  done
fi

if [[ -z $bazel_version ]]; then
  color "31" "ERROR: No installed Bazel version found, cannot continue."
  (echo ""
  echo "Bazel binaries have to be installed in ${wrapper_dir}, but none were found.") 2>&1
  exit 1
fi

if [[ ! -x $BAZEL_REAL ]]; then
  color "31" "ERROR: The project you're trying to build requires Bazel ${bazel_version} (${reason}), but it wasn't found in ${wrapper_dir}."

  long_binary_name="bazel-${bazel_version}-${os_arch_suffix}"

  if [[ -x $(command -v apt-get) && $wrapper_dir == "/usr/bin" ]]; then
    (echo ""
    echo "You can install the required Bazel version via apt:"
    echo "  sudo apt update && sudo apt install bazel-${bazel_version}"
    echo ""
    echo "If this doesn't work, check Bazel's installation instructions for help:"
    echo "  https://bazel.build/install/ubuntu") 2>&1
  else
    (echo ""
    echo "Bazel binaries for all official releases can be downloaded from here:"
    echo "  https://github.com/bazelbuild/bazel/releases") 2>&1

    if [[ -x $(command -v curl) && -w $wrapper_dir ]]; then
      (echo ""
      echo "You can download the required version directly using this command:"
      echo "  (cd \"${wrapper_dir}\" && curl -fLO https://releases.bazel.build/${bazel_version}/release/${long_binary_name} && chmod +x ${long_binary_name} && cd - )") 2>&1
    elif [[ -x $(command -v wget) && -w $wrapper_dir ]]; then
      (echo ""
      echo "You can download the required version directly using this command:"
      echo "  (cd \"${wrapper_dir}\" && wget https://releases.bazel.build/${bazel_version}/release/${long_binary_name} && chmod +x ${long_binary_name} && cd - )") 2>&1
    else
      (echo ""
      echo "Please put the downloaded Bazel binary into this location:"
      echo "  ${wrapper_dir}/${long_binary_name}") 2>&1
    fi
  fi
  exit 1
fi

exec -a "$0" "${BAZEL_REAL}" "$@"
