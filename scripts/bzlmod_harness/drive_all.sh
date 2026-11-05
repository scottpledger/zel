#!/bin/bash
# usage: drive_all.sh cases_file.py total [name-substr...]   -- batches of 50, 2 workers, shutdown+cleanup per batch
# run through scripts/heavy.sh. Needs `python3 server.py` running on $ZEL_HARNESS_PORT.
H=$(cd "$(dirname "$0")" && pwd)
export ZEL_HARNESS_DIR=${ZEL_HARNESS_DIR:-/tmp/zbh-v7} ZEL_HARNESS_PORT=${ZEL_HARNESS_PORT:-18877} ZEL_BIN=${ZEL_BIN:-/tmp/zel-target/release/zel}
f=$1; n=$2; shift 2; tag=$(basename $f .py); out=${OUT:-/tmp/zbh-out}; mkdir -p $out
for ((i=0;i<n;i+=50)); do
  BATCH=$i:$((i+50)) python3 $H/run.py $f "$@" > $out/${tag}_$i.txt 2>&1
  /tmp/bazel-9.3.2 shutdown >/dev/null 2>&1 </dev/null
  bash $H/../cleanup_servers.sh >/dev/null 2>&1
done
grep -h "^TOTAL" $out/${tag}_*.txt
