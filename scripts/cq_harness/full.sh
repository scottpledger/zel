#!/bin/bash
# full.sh [ROOT]: run the whole cquery/aquery matrix against baseline; prints per-kind OK counts.
export QH_ROOT=${1:-/tmp/cqhf}
python3 $(dirname "$0")/run.py $(dirname "$0")/cq_cmds.tsv
awk -F'\t' '{k=substr($1,1,1); t[k]++; if($2=="OK")o[k]++} END{for(k in t)print k, o[k]+0 "/" t[k]}' $QH_ROOT/summary.txt
