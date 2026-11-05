#!/bin/bash
# d.sh ID : show diff baseline(bazel) vs zel for a command
H=$(cd "$(dirname "$0")" && pwd); R=${QH_ROOT:-/tmp/qh}
grep -P "^$1\t" $H/query_cmds.tsv $H/extra_cmds.tsv 2>/dev/null | head -1
diff <(cat $H/baseline/$1.b.out) <(cat $R/out/$1.z.out) | head -${2:-20}
echo "-- rc b=$(cat $H/baseline/$1.b.rc) z=$(cat $R/out/$1.z.rc); stderr:"
diff <(grep -v '^\(Loading\|Analyzing\|INFO: \(Elapsed\|Analyzed\|Build comp\|Found\)\|Computing\|Starting\)' $H/baseline/$1.b.err) <(grep -v '^\(Loading\|Analyzing\|INFO: \(Elapsed\|Analyzed\|Build comp\|Found\)\)' $R/out/$1.z.err) | head -${2:-20}
