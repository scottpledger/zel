#!/bin/bash
echo "args: $@ env=$BIN_ENV"; cat data.txt 2>/dev/null || cat "$0.runfiles/_main/data.txt" 2>/dev/null; true
