#!/bin/bash
# Lockfile differential cases: for every fixture workspace in fx/ run `mod deps --lockfile_mode=update`
# in real Bazel and in Zel and compare the full MODULE.bazel.lock `moduleExtensions` section
# (bzlTransitiveDigest, usagesDigest, recordedInputs, generatedRepoSpecs, moduleExtensionMetadata),
# stdout and exit codes; then run `--lockfile_mode=error` without a lockfile (must fail like Bazel).
#
# Env: ZEL_BIN, BAZEL_BIN (default bazel), ZEL_HARNESS_DIR (registry from `python3 mkreg.py`),
#      LOCKCASES_DIR (scratch), ONLY="e03 e05" to select fixtures.
# Only `lockFileVersion`/`facts`/`factsVersions` (HEAD format 30 vs 9.3.2's 28) and
# `registryFileHashes` IS compared (file:// registries must write none); `selectedYankedVersions` is not. Zel emulates 9.3.2, so the
# usages digest and lockfile layout are expected to match without any switch.
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ZEL_BIN="${ZEL_BIN:?set ZEL_BIN}"
BAZEL_BIN="${BAZEL_BIN:-bazel}"
HARNESS_DIR="${ZEL_HARNESS_DIR:-${TMPDIR:-/tmp}/zel-bzlmod-harness}"
REG="file://$HARNESS_DIR/reg"
SCRATCH="${LOCKCASES_DIR:-${TMPDIR:-/tmp}/zel-lockfile-cases}"
export ZEL_E11_VAR=hello
ok=0; bad=0
compare_locks() {
python3 - "$1" "$2" <<'P'
import json, sys
def load(p):
    try:
        return json.load(open(p))
    except Exception:
        return None
a, b = load(sys.argv[1]), load(sys.argv[2])
if a is None or b is None:
    print("MISSING lockfile (bazel=%s zel=%s)" % (a is not None, b is not None)); sys.exit(1)
if a.get("registryFileHashes", {}) != b.get("registryFileHashes", {}):
    print("DIFF registryFileHashes"); print("  bazel:", json.dumps(a.get("registryFileHashes"), sort_keys=True)[:600]); print("  zel  :", json.dumps(b.get("registryFileHashes"), sort_keys=True)[:600]); sys.exit(1)
ea, eb = a.get("moduleExtensions", {}), b.get("moduleExtensions", {})
if ea != eb:
    for k in sorted(set(ea) | set(eb)):
        if ea.get(k) != eb.get(k):
            print("DIFF", k); print("  bazel:", json.dumps(ea.get(k), sort_keys=True)[:600]); print("  zel  :", json.dumps(eb.get(k), sort_keys=True)[:600])
    sys.exit(1)
P
}
for fx in "$HERE"/fx/*/; do
    n=$(basename "$fx")
    [ -n "$ONLY" ] && [[ " $ONLY " != *" $n "* ]] && continue
    for k in b z; do
        d="$SCRATCH/$n/$k"; rm -rf "$d"; mkdir -p "$d"; cp -R "$fx" "$d/w"; (cd "$d/w"; [ -f BUILD.bazel ] || touch BUILD.bazel)
        if [ $k = b ]; then bin="$BAZEL_BIN"; else bin="$ZEL_BIN"; fi
        (cd "$d/w" && "$bin" --output_base="$d/ob" mod deps --enable_workspace=false --lockfile_mode=update --registry="$REG" >"$d/out" 2>"$d/err"; echo "rc=$?" >>"$d/out")
        "$bin" --output_base="$d/ob" shutdown >/dev/null 2>&1
    done
    msg=$(compare_locks "$SCRATCH/$n/b/w/MODULE.bazel.lock" "$SCRATCH/$n/z/w/MODULE.bazel.lock")
    rcs=$(diff <(sed "s#$SCRATCH/$n/[bz]/##g" "$SCRATCH/$n/b/out") <(sed "s#$SCRATCH/$n/[bz]/##g" "$SCRATCH/$n/z/out") >/dev/null && echo same || echo differ)
    if [ -z "$msg" ] && [ "$rcs" = same ]; then ok=$((ok+1)); echo "  [OK] lockfile $n"; else bad=$((bad+1)); echo "  [DIFF] lockfile $n ($rcs stdout/rc)"; echo "$msg"; fi
    # --lockfile_mode=error with no lockfile: same exit code (fetch --all) and stderr.
    for k in b z; do
        d="$SCRATCH/$n/e$k"; rm -rf "$d"; mkdir -p "$d"; cp -R "$fx" "$d/w"; (cd "$d/w"; [ -f BUILD.bazel ] || touch BUILD.bazel)
        if [ $k = b ]; then bin="$BAZEL_BIN"; else bin="$ZEL_BIN"; fi
        (cd "$d/w" && "$bin" --output_base="$d/ob" fetch --all --enable_workspace=false --lockfile_mode=error --registry="$REG" >"$d/out" 2>"$d/err"; echo "rc=$?" >>"$d/out")
        "$bin" --output_base="$d/ob" shutdown >/dev/null 2>&1
        grep -E '^(ERROR|WARNING)' "$d/err" | sed "s#$d#X#g" >"$d/err.n"
    done
    brc=$(tail -1 "$SCRATCH/$n/eb/out")
    if diff "$SCRATCH/$n/eb/err.n" "$SCRATCH/$n/ez/err.n" >/dev/null && [ "$brc" = "$(tail -1 "$SCRATCH/$n/ez/out")" ]; then ok=$((ok+1)); echo "  [OK] lockfile_mode=error $n"; else bad=$((bad+1)); echo "  [DIFF] lockfile_mode=error $n ($(tail -1 "$SCRATCH/$n/eb/out") vs $(tail -1 "$SCRATCH/$n/ez/out"))"; fi
done
echo "lockfile cases: $ok ok, $bad differ"
[ "$bad" -eq 0 ]
