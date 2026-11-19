#!/bin/bash
# cluster.sh [c|a]: group the non-identical commands by first differing stdout/stderr line.
R=${QH_ROOT:-/tmp/cqhf}; B=$(dirname "$0")/baseline; K=${1:-c}
for i in $(awk -F'\t' -v k=$K '$2!="OK" && (substr($1,1,1)==k || (substr($1,1,1)=="n" && $3 ~ "^"(k=="c"?"cquery":"aquery"))) {print $1}' $R/summary.txt); do
  d=$(diff <(tr -d '\r' <$B/$i.b.out) <(tr -d '\r' <$R/out/$i.z.out) | grep '^[<>]' | head -2 | sed -E 's/[0-9a-f]{64}/H/g; s/(.{90}).*/\1/' | tr '\n' '|')
  e=$(diff <(grep -v '^\(Loading\|Analyzing\|Computing\|INFO\|Starting\|\[\)' $B/$i.b.err| tr -d '\r') <(grep -v '^\(Loading\|Analyzing\|Computing\|INFO\|Starting\|\[\)' $R/out/$i.z.err| tr -d '\r') | grep '^[<>]' | head -1 | sed -E 's/@WS@[^ ]*/LOC/; s/(.{80}).*/\1/')
  echo "$d || $e"
done | sort | uniq -c | sort -rn | head -${N:-25}
