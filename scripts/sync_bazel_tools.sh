#!/bin/bash
# Regenerates crates/zel-bazel-tools/resources from a Bazel source checkout's //tools,
# mirroring how Bazel assembles the embedded @bazel_tools repository:
#   BUILD.tools -> BUILD, BUILD.repo -> BUILD (build_defs/repo), otherwise the dev BUILD is kept.
set -euo pipefail
SRC="${1:-$HOME/Development/bazel}/tools"
DST="$(cd "$(dirname "$0")/.." && pwd)/crates/zel-bazel-tools/resources/tools"
ROOT="$(dirname "$DST")"
BZL="${1:-$HOME/Development/bazel}"
rm -rf "$ROOT"; mkdir -p "$DST"
cp "$BZL/src/BUILD.tools" "$ROOT/BUILD"
cp "$BZL/src/MODULE.tools" "$ROOT/MODULE.bazel"
(cd "$SRC" && find . -type f \
  ! -path './intellij/*' ! -path './distributions/*' ! -path './test/CoverageOutputGenerator/javatests/*' \
  ! -name 'mirror_external_deps.py' ! -name 'upload_bazel_mirror.sh' ! -name '*.md.tmpl' \
  ! -name '*_test' ! -name '*_test.*' ! -name '*_tests.*' ! -name '.DS_Store') | sort | while read -r f; do
  f="${f#./}"; d="$(dirname "$f")"; b="$(basename "$f")"
  case "$b" in
    BUILD)
      [ -e "$SRC/$d/BUILD.tools" ] && continue
      [ -e "$SRC/$d/BUILD.repo" ] && continue
      out="$d/BUILD";;
    BUILD.tools|BUILD.repo) out="$d/BUILD";;
    BUILD.java_tools*|BUILD.tools) out="$f";;
    *) out="$f";;
  esac
  mkdir -p "$DST/$(dirname "$out")"; cp "$SRC/$f" "$DST/$out"
done
# Bazel's Starlark builtins (`@_builtins`): exports.bzl + common/ + bazel/.
BI="$(dirname "$ROOT")/builtins"
rm -rf "$BI"; mkdir -p "$BI"
(cd "$BZL/src/main/starlark/builtins_bzl" && find . -type f \( -name '*.bzl' -o -name 'BUILD' \) | while read -r f; do
  mkdir -p "$BI/$(dirname "$f")"; cp "$f" "$BI/$f"; done)
# `BUILD` in builtins_bzl only marks the package; its stardoc loads are never evaluated.
echo "synced builtins: $(find "$BI" -type f | wc -l) files"
echo "synced $(find "$DST" -type f | wc -l) files"

# Other source trees that Bazel's //src:embedded_tools_*_srcs puts into @bazel_tools
# (see src/BUILD and src/create_embedded_tools.py): conditions, cpp/util, native, launcher,
# def_parser, protobuf, and the ijar/zipper sources. BUILD.tools -> BUILD.
copy_tree() { # <src dir rel to $BZL> <dest dir rel to $ROOT> <find args...>
  local s="$1" d="$2"; shift 2
  [ -d "$BZL/$s" ] || return 0
  (cd "$BZL/$s" && find . -type f "$@" | sort) | while read -r f; do
    f="${f#./}"; out="$f"
    case "$(basename "$f")" in
      BUILD) [ -e "$BZL/$s/$(dirname "$f")/BUILD.tools" ] && continue;;
      BUILD.tools) out="$(dirname "$f")/BUILD";;
    esac
    mkdir -p "$ROOT/$d/$(dirname "$out")"; cp "$BZL/$s/$f" "$ROOT/$d/$out"
  done
}
copy_tree src/conditions src/conditions
copy_tree src/main/cpp/util src/main/cpp/util \( -name '*.cc' -o -name '*.h' -o -name BUILD \) ! -path '*/testing/*' ! -name '*_test.cc'
copy_tree src/main/native src/main/native -name BUILD -maxdepth 1
copy_tree src/tools/launcher src/tools/launcher
copy_tree third_party/def_parser third_party/def_parser
copy_tree src/main/protobuf src/main/protobuf
for f in BUILD common.h mapped_file.h mapped_file_unix.cc mapped_file_windows.cc platform_utils.cc platform_utils.h zip.cc zip.h zip_main.cc zlib_client.cc zlib_client.h; do
  mkdir -p "$ROOT/third_party/ijar"; cp "$BZL/third_party/ijar/$f" "$ROOT/third_party/ijar/$f"
done
echo "synced total $(find "$ROOT" -type f | wc -l) files (resources)"

# zel has no prebuilt tools/zip/zipper binary (Bazel's distribution embeds one built from
# //third_party/ijar:zipper): always build the zipper from the embedded ijar sources.
sed -i.bak 's|":matches_host_constraints": glob(\["zipper/\*"\]),|":matches_host_constraints": ["//third_party/ijar:zipper"],|' "$ROOT/tools/zip/BUILD" && rm -f "$ROOT/tools/zip/BUILD.bak"
