#!/usr/bin/env bash
# Differential test of configuration ids: for every flag combination in
# crates/zel-server/tests/fixtures/config_ws/combos.txt build the fixture workspace with real
# bazel and with zel (fresh output bases), then compare
#   * `config`           (ids + display mnemonics)
#   * `config <id>`      for every id real bazel lists (full fragment dumps, i.e. every option value)
# Usage: scripts/diff_config_ids.sh [-j N] [-b /path/to/bazel] [combos-file]
# Env:   ZEL=<zel binary> (default: $CARGO_TARGET_DIR/debug/zel), OUT=/tmp/cfgdiff
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
JOBS=4
BAZEL="$(command -v bazel)"
while getopts "j:b:" o; do case $o in j) JOBS=$OPTARG;; b) BAZEL=$OPTARG;; esac; done
shift $((OPTIND-1))
COMBOS="${1:-$ROOT/crates/zel-server/tests/fixtures/config_ws/combos.txt}"
ZEL="${ZEL:-${CARGO_TARGET_DIR:-$ROOT/target}/debug/zel}"
OUT="${OUT:-/tmp/cfgdiff}"
FIXTURE="$ROOT/crates/zel-server/tests/fixtures/config_ws"
chmod -R u+w "$OUT" 2>/dev/null; rm -rf "$OUT"; mkdir -p "$OUT/out"
export BAZEL ZEL OUT FIXTURE

run_one() {
  idx="$1"; flags="$2"
  d="$OUT/w$idx"; mkdir -p "$d"
  for side in real zel; do
    ws="$d/ws-$side"; rm -rf "$ws"; mkdir -p "$ws"; cp -R "$FIXTURE"/. "$ws"/
    if [ $side = real ]; then bin="$BAZEL"; else bin="$ZEL"; fi
    ob="$d/ob-$side"
    ( cd "$ws" && "$bin" --output_base="$ob" build $flags //:u //:t >"$d/$side.build.out" 2>&1; echo "rc=$?" >>"$d/$side.build.out"
      "$bin" --output_base="$ob" config >"$d/$side.list" 2>&1
      "$bin" --output_base="$ob" shutdown >/dev/null 2>&1 ) &
  done
  wait
  status=PASS
  grep -q "^rc=0" "$d/real.build.out" || status="REAL-BUILD-FAILED"
  grep -q "^rc=0" "$d/zel.build.out" || { [ $status = PASS ] && status="ZEL-BUILD-FAILED"; }
  if [ $status = PASS ]; then
    if ! diff <(grep -v '^INFO\|^Fetching\|^Loading\|^Analyz\|^Starting local' "$d/real.list") <(grep -v '^INFO\|^Fetching\|^Loading\|^Analyz\|^Starting local' "$d/zel.list") >"$d/list.diff"; then status=LIST-DIFF; fi
    # Dump every config real lists, from both sides (zel may lack an id: then its dump is an error).
    for id in $(grep -Eo '^[0-9a-f]{64}' "$d/real.list"); do
      for side in real zel; do
        if [ $side = real ]; then bin="$BAZEL"; else bin="$ZEL"; fi
        ( cd "$d/ws-$side" && "$bin" --output_base="$d/ob-$side" config "${id:0:8}" 2>&1 | grep -v '^INFO\|^Fetching\|^Starting local' >"$d/$side.dump.${id:0:8}" )
      done
      if ! diff "$d/real.dump.${id:0:8}" "$d/zel.dump.${id:0:8}" >"$d/dump.${id:0:8}.diff"; then [ $status = PASS ] && status="DUMP-DIFF"; fi
    done
    ( cd "$d/ws-real" && "$BAZEL" --output_base="$d/ob-real" shutdown >/dev/null 2>&1; cd "$d/ws-zel" && "$ZEL" --output_base="$d/ob-zel" shutdown >/dev/null 2>&1 )
  fi
  echo "$status [$idx] $flags" | tee "$OUT/out/$idx.status"
}
export -f run_one
i=0
grep -v '^#' "$COMBOS" | grep -v '^$' | while IFS= read -r flags; do i=$((i+1)); printf '%s\0%s\0' "$i" "$flags"; done \
  | xargs -0 -n2 -P "$JOBS" bash -c 'run_one "$0" "$1"'
total=$(cat "$OUT"/out/*.status | wc -l); pass=$(grep -c '^PASS' "$OUT"/out/*.status | awk -F: '{s+=$2} END {print s}')
echo "== $pass/$total combinations identical (details in $OUT/w*/)"
[ "$pass" = "$total" ]
