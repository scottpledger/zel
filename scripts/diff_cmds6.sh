#!/bin/bash
# Differential harness for client/command behaviour: runs the real 9.3.2 binary (REAL_BAZEL, default /tmp/bazel-9.3.2; never bazelisk) and Zel with isolated output bases in the same directory and diffs
# stdout / stderr / exit code.
#
#   scripts/diff_cmds6.sh <label> <dir> [env KEY=VAL ...] -- <bazel args...>
#
# Env: ZEL_BIN (default target/debug/zel), WORK (default /tmp/zel-diff6), NORM=1 to normalise
# timings / pids / temp paths with sed (see `norm`).
# Prints SAME/DIFF and keeps outputs under $WORK/out/<label>.{b,z}.{out,err,rc}.
set -u
label=$1; dir=$2; shift 2
envs=()
while [ $# -gt 0 ] && [ "$1" != "--" ]; do envs+=("$1"); shift; done
[ "${1:-}" = "--" ] && shift
ZEL_BIN=${ZEL_BIN:-$(cd "$(dirname "$0")/.." && pwd)/target/debug/zel}
REAL=${REAL_BAZEL:-/tmp/bazel-9.3.2}
WORK=${WORK:-/tmp/zel-diff6}
O=$WORK/out
mkdir -p "$O"
norm() {
  sed -E \
    -e 's/[0-9]+\.[0-9]+s/Xs/g' \
    -e 's/pid[= ][0-9]+/pid=N/g' \
    -e 's/\[(FATAL|INFO|ERROR|WARNING) [0-9:.]+ /[\1 TS /' \
    -e "s#$WORK#WORK#g" \
    -e 's#/private/tmp#/tmp#g' \
    -e '/^Starting local Bazel server/d' \
    -e '/^Computing main repo mapping: *$/d' \
    -e 's/^(release|Build label:|\[bazel release) ?(0\.0\.1|9\.3\.2)/\1 VER/' \
    "$1"
}
cd "$dir" || exit 2
rm -rf "$WORK/ob_b_$label" "$WORK/ob_z_$label"
env -u USE_BAZEL_VERSION "${envs[@]+${envs[@]}}" timeout 300 $REAL --max_idle_secs=60 --output_base="$WORK/ob_b_$label" "$@" >"$O/$label.b.out" 2>"$O/$label.b.err"; echo $? >"$O/$label.b.rc"
env -u USE_BAZEL_VERSION "${envs[@]+${envs[@]}}" timeout 300 "$ZEL_BIN" --max_idle_secs=60 --output_base="$WORK/ob_z_$label" "$@" >"$O/$label.z.out" 2>"$O/$label.z.err"; echo $? >"$O/$label.z.rc"
for s in out err; do
  if [ "${NORM:-1}" = 1 ]; then
    norm "$O/$label.b.$s" >"$O/$label.b.$s.n"; norm "$O/$label.z.$s" >"$O/$label.z.$s.n"
  else
    cp "$O/$label.b.$s" "$O/$label.b.$s.n"; cp "$O/$label.z.$s" "$O/$label.z.$s.n"
  fi
done
if cmp -s "$O/$label.b.out.n" "$O/$label.z.out.n" && cmp -s "$O/$label.b.err.n" "$O/$label.z.err.n" && [ "$(cat "$O/$label.b.rc")" = "$(cat "$O/$label.z.rc")" ]; then
  echo "SAME $label"
else
  echo "DIFF $label (real rc=$(cat "$O/$label.b.rc") zel rc=$(cat "$O/$label.z.rc"); stdout diff lines $(diff "$O/$label.b.out.n" "$O/$label.z.out.n" | grep -c '^[<>]'), stderr diff lines $(diff "$O/$label.b.err.n" "$O/$label.z.err.n" | grep -c '^[<>]'))"
  [ "${VERBOSE:-0}" = 1 ] && { diff "$O/$label.b.err.n" "$O/$label.z.err.n" | head -20; diff "$O/$label.b.out.n" "$O/$label.z.out.n" | head -20; }
fi
exit 0
