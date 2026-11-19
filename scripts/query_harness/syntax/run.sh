#!/bin/bash
# Compare zel vs bazel for invalid-syntax BUILD snippets (s*.txt in this dir).
# Bazel results are cached in $WORK/bcache (BAZEL_REFRESH=1 to redo).
# env: ZEL_BIN, BAZEL_BIN (default bazel), ONLY=glob prefix, VERBOSE=1,
#      QARGS=--keep_going (extra query flags; use a separate WORK dir per flag set),
#      WORK=/tmp/qh5p (scratch + Bazel result cache),
#      MODE=bzl (put the snippet in a loaded e/x.bzl instead of e/BUILD)
export LC_ALL=C
D="$(cd "$(dirname "$0")" && pwd)"
ZEL_BIN=${ZEL_BIN:?}; BAZEL_BIN=${BAZEL_BIN:-bazel}
W=${WORK:-/tmp/qh5p}; mkdir -p $W/ws/e; cd $W/ws
echo 'module(name="w")' > MODULE.bazel; : > WORKSPACE
echo 'x = 1' > e/ok.bzl
same=0; diff=0; list=""
for f in $D/${ONLY:-s}*.txt; do
  n=$(basename $f .txt); if [ "$MODE" = bzl ]; then cp $f e/x.bzl; printf 'load("//e:x.bzl", "y")\nfilegroup(name = "a")\n' > e/BUILD; else cp $f e/BUILD; rm -f e/x.bzl; fi
  norm(){ sed -e "s#/private$W#$W#g" -e "s#$W/ws#<WS>#g" -e '/^ *no actions running/d'  -e "s#$W/[a-z]*ob[^ :']*#<OB>#g" -e '/^Loading:/d;/^INFO: Invocation/d;/^Starting local/d;/^INFO: Elapsed/d;/^INFO: Found/d;/^INFO: Analy/d;/^INFO: Build/d;/^Computing main/d;/^WARNING: Build Event/d;/^Analyzing/d;/^\[/d' | tr -d '\r'; }
  mkdir -p $W/bcache${MODE:+_$MODE}
  if [ -n "$BAZEL_REFRESH" ] || [ ! -f $W/bcache${MODE:+_$MODE}/$n.cmp ] || ! cmp -s $f $W/bcache${MODE:+_$MODE}/$n.src; then
    (cd $W/ws; $BAZEL_BIN --output_base=$W/ob query $QARGS //e:all >$W/b.out 2>$W/b.err; echo $? >$W/b.rc)
    { norm <$W/b.out; echo "--err"; norm <$W/b.err; echo "rc=$(cat $W/b.rc)"; } > $W/bcache${MODE:+_$MODE}/$n.cmp; cp $f $W/bcache${MODE:+_$MODE}/$n.src
  fi
  (cd $W/ws; $ZEL_BIN --output_base=$W/zob query $QARGS //e:all >$W/z.out 2>$W/z.err; echo $? >$W/z.rc)
  cp $W/bcache${MODE:+_$MODE}/$n.cmp $W/b.all
  { norm <$W/z.out; echo "--err"; norm <$W/z.err; echo "rc=$(cat $W/z.rc)"; } > $W/z.all
  if cmp -s $W/b.all $W/z.all; then same=$((same+1)); else diff=$((diff+1)); list="$list $n"
    if [ -n "$VERBOSE" ]; then echo "=== $n: $(head -c 200 $f|head -3)"; diff $W/b.all $W/z.all; fi; fi
done
echo "identical=$same different=$diff"; echo "diffs:$list"
