#!/bin/bash
# Differential check of the generated help topics (completion, everything-as-html, license,
# info-keys, target-syntax, startup_options) against real Bazel: prints SAME/DIFF per topic and
# the number of differing lines; keeps both outputs under $WORK/out for `diff`.
#
#   scripts/diff_help_topics.sh [workspace-dir]
#
# No skew tolerance: the script normalises only the release label.
set -u
here=$(cd "$(dirname "$0")" && pwd)
ws=${1:-$(mktemp -d)}
[ -f "$ws/MODULE.bazel" ] || echo 'module(name="m")' >"$ws/MODULE.bazel"
export WORK=${WORK:-/tmp/zel-diff-help}
for t in completion everything-as-html license info-keys target-syntax startup_options; do
  "$here/diff_cmds6.sh" "help_$t" "$ws" -- help "$t"
done
"$here/diff_cmds6.sh" help_completion_bash "$ws" -- help completion bash
"$here/diff_cmds6.sh" license "$ws" -- license
for t in completion everything-as-html license; do
  f="$WORK/out/help_$t"
  printf '%s lines: real %s zel %s\n' "$t" "$(wc -l <"$f.b.out")" "$(wc -l <"$f.z.out")"
done
