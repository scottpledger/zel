#!/bin/bash
# Honest bazel-vs-zel comparison for build/test/run/coverage: stdout and stderr are compared
# SEPARATELY and byte-for-byte (plus exit code); only the small documented normalizer in
# build_harness/h.py is applied and cases proven nondeterministic in
# real 9.3.2 (scripts/build_harness/nondet/*.json) are compared per harness_common/compare.py. Usage: diff_build.sh <casefile> <outdir> [seq]
# Env: ZEL_BIN, BAZEL_BIN, FX (fixture dir), NW (workers).
exec python3 "$(dirname "$0")/build_harness/h.py" "$@"
