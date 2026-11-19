#!/usr/bin/env bash
# Serialize memory-heavy work (cargo builds/tests, differential harnesses) across ALL agents/shells,
# bound its resources, and clean up leaked servers afterwards.
#
#   scripts/heavy.sh <command...>
#
# - Only one heavy job runs at a time (mkdir lock, stale-lock stealing).
# - Refuses to start while free memory is low (waits up to 10 min).
# - Uses ONE shared cargo target dir and a capped number of build jobs unless overridden.
# - After the job, stops zel daemons and real-bazel servers whose output base lives under /tmp
#   (never touches servers whose output base is elsewhere, e.g. ~/Library/Caches).
set -u
LOCK=/tmp/zel-heavy.lock
MIN_FREE_PCT=${ZEL_MIN_FREE_PCT:-25}

free_pct() { memory_pressure 2>/dev/null | awk '/free percentage/ {gsub("%","",$NF); print $NF}'; }

cleanup_servers() {
  # zel daemons are always test servers started from /tmp/* target dirs or repo build dirs
  pkill -f internal_server_daemon 2>/dev/null
  for p in $(ps -axo pid,command | grep -E 'bazel\(' | grep -v grep | grep -E '/(private/)?tmp/' | awk '{print $1}'); do
    kill -9 "$p" 2>/dev/null
  done
}

# --- acquire lock
waited=0
while ! mkdir "$LOCK" 2>/dev/null; do
  owner=$(cat "$LOCK/pid" 2>/dev/null || echo "")
  if [ -n "$owner" ] && ! kill -0 "$owner" 2>/dev/null; then
    echo "heavy.sh: stealing stale lock from dead pid $owner" >&2
    rm -rf "$LOCK"; continue
  fi
  sleep 5; waited=$((waited+5))
  [ $((waited % 60)) -eq 0 ] && echo "heavy.sh: waiting for lock held by pid ${owner:-?} (${waited}s)" >&2
done
echo $$ > "$LOCK/pid"
trap 'cleanup_servers; rm -rf "$LOCK"' EXIT INT TERM

# --- wait for memory headroom
waited=0
while :; do
  f=$(free_pct); f=${f:-100}
  [ "$f" -ge "$MIN_FREE_PCT" ] && break
  echo "heavy.sh: only ${f}% memory free (< ${MIN_FREE_PCT}%), cleaning leaked servers and waiting" >&2
  cleanup_servers; sleep 20; waited=$((waited+20))
  if [ $waited -ge 600 ]; then echo "heavy.sh: memory never recovered; aborting" >&2; exit 75; fi
done

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/zel-target}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-3}"
export NW="${NW:-2}"          # harness worker count (h.py etc.)
export ZEL_MAX_IDLE_SECS=60   # harness scripts should pass --max_idle_secs=$ZEL_MAX_IDLE_SECS to zel and real bazel

"$@"
