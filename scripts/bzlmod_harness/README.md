# bzlmod differential harness (zel vs real bazel)
Merged from the round-5 verifier. Expects the `$ZEL_HARNESS_DIR (default $TMPDIR/zel-bzlmod-harness)` layout: run `python3 gen.py` (archives -> $ZEL_HARNESS_DIR (default $TMPDIR/zel-bzlmod-harness)/arch),
`python3 mkreg.py` (registry -> $ZEL_HARNESS_DIR (default $TMPDIR/zel-bzlmod-harness)/reg), `python3 server.py &` (www), then
`ZEL_BIN=<zel> python3 cases_ctx.py|cases_fetch.py|cases_mod.py [name-substr...]` and `drive.py` (codecs).
Full stdout/stderr (minus INFO/progress lines), exit codes, trees are compared. Do NOT set NORM2/NORM3
(they strip tracebacks and are kept only for debugging).

## Round 6D additions (repo rules run as the real Starlark)
* `setup_native.py` builds the extra fixtures (git repo with tags/branches/submodule, patches, netrc files, jars) under `$ZEL_HARNESS_DIR`.
* `cases_native.py` (~570 cases): http_archive/http_file/http_jar, git_repository/new_git_repository, local/new_local_repository, utils.bzl/cache.bzl/jvm.bzl/java.bzl helpers, repo-rule call validation (unknown attrs, type errors, labels), Starlark call-binding errors inside extensions. Local fixtures only.
* `ZEL_HARNESS_PORT` (default 18765) moves the python server (`server.py`) and every URL in the cases; per-case `--repository_cache` keeps caches isolated.
* zel runs the **9.3.2** `tools/build_defs/repo/*.bzl` (`crates/zel-bzlmod/tests/fixtures/bazel_9.3.2_repo_bzl`) so output is comparable; `ZEL_HARNESS_HEAD_BZL=1` runs the embedded HEAD files instead (diffs are then real: there is no skew allowlist).
* Cases where the reference bazel crashes (`FATAL: bazel crashed`) are reported as `SKIP`. There is no skew allowlist: the spec and the reference binary are both 9.3.2. Real-bazel nondeterminism is handled only through proven evidence files (`scripts/harness_common/compare.py`).
