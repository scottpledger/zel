#!/usr/bin/env bash
# Stop leaked test servers (zel daemons; real-bazel servers whose output base is under /tmp).
pkill -f internal_server_daemon 2>/dev/null
for p in $(ps -axo pid,command | grep -E 'bazel\(' | grep -v grep | grep -E '/(private/)?tmp/' | awk '{print $1}'); do kill -9 "$p" 2>/dev/null; done
echo "zel daemons: $(pgrep -f internal_server_daemon | wc -l), bazel servers: $(ps -axo command | grep -E 'bazel\(' | grep -v grep | wc -l)"
