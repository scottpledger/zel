# BUILD / .bzl syntax-error parity harness

Compares `zel query //e:all` with real Bazel (`bazel query //e:all`) on a corpus of invalid
BUILD/Starlark snippets (`s*.txt`, one snippet per file): stdout, stderr (absolute paths
normalised) and exit code.

```sh
export ZEL_BIN=/path/to/zel          # e.g. target/release/zel
export BAZEL_BIN=bazel               # optional, default `bazel`
./run.sh                             # BUILD mode: the snippet is e/BUILD
VERBOSE=1 ONLY=s02 ./run.sh          # only s02*.txt, print diffs
MODE=bzl ./run.sh                    # snippet is e/x.bzl, BUILD loads it
WORK=/tmp/qh5p_kg QARGS=--keep_going ./run.sh   # extra query flags (own WORK dir!)
```

* Scratch workspace, output bases (`$WORK/ob` for Bazel, `$WORK/zob` for zel) and the cache of
  Bazel's results (`$WORK/bcache[_bzl]`) live in `$WORK` (default `/tmp/qh5p`). Bazel results are
  cached per snippet; `BAZEL_REFRESH=1` re-runs Bazel.
* Do not run two instances with the same `WORK` concurrently.
* `cmp_parse.py [-v]` is the unit-level counterpart: it compares only the scanner/parser error
  lines of the cached Bazel results with `zel_starlark::syntax::bazel_syntax::parse_errors`
  (build the `syntax_errors` example of `zel-starlark` first; `EXAMPLE=` overrides its path).
* `gen_unit_tests.py` regenerates
  `crates/zel-starlark/src/syntax/bazel_syntax_corpus_tests.rs` from the cached Bazel results.

Known differences (not covered by the port) are listed in the final report of the commit that
introduced the harness: non-ASCII bytes in `invalid character` messages (Bazel writes the raw
byte, zel a UTF-8 encoded char), `cannot assign to '<expr>'` text, `True = 1`, and runtime
error wording (`1..a`).
