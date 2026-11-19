#!/bin/bash
echo "hello args=$* RULE=${RULE_ENV:-} pwd-base=$(basename "$PWD")"
cat "$(dirname "$0")/data.txt" 2>/dev/null || cat "$RUNFILES_DIR/_main/data.txt" 2>/dev/null
