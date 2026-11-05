#!/bin/bash
key=$(echo "$TEST_TMPDIR" | grep -o 'w[0-9]*/ob[bz]/' | tr -d /)
f=/tmp/vb5-flaky-$key-$(echo "$TEST_TARGET" | tr -c 'a-z' _)
n=$(cat $f 2>/dev/null || echo 0); n=$((n+1)); echo $n > $f
echo "run count $n"
[ $n -lt 2 ] && exit 1
exit 0
