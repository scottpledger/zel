#!/bin/bash
# show.sh ID : diff of bazel (baseline) vs zel for one command. QH_ROOT as in run.py
R=${QH_ROOT:-/tmp/cqh}; B=$(dirname "$0")/baseline
awk -F"\t" -v i=$1 '$1==i{print $3?$3:$2}' $(dirname "$0")/cq_cmds.tsv
diff <(cat $B/$1.b.out) <(cat $R/out/$1.z.out) | head -${N:-20}
echo "rc b=$(cat $B/$1.b.rc) z=$(cat $R/out/$1.z.rc)"
diff <(grep -v '^\(Loading\|Analyzing\|Computing\|INFO\|Starting\|\[\)' $B/$1.b.err) <(grep -v '^\(Loading\|Analyzing\|Computing\|INFO\|Starting\|\[\)' $R/out/$1.z.err) | head -${N:-20}
