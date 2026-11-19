#!/usr/bin/env python3
"""Unit-level comparison: zel-starlark's bazel_syntax port vs the cached real-bazel query output.

usage: cmp_parse.py [-v]   (env EXAMPLE=path to the `syntax_errors` example binary, WORK=/tmp/qh5p)
Compares only the `BUILD:L:C: msg` error lines emitted before `package contains errors`.
"""
import os, re, subprocess, sys, glob
W = os.environ.get("WORK", "/tmp/qh5p")
EX = os.environ.get("EXAMPLE", "/tmp/zel-target-fix-parser5a/release/examples/syntax_errors")
D = os.path.dirname(os.path.abspath(__file__))
PARSE_ONLY = ["syntax error", "invalid character", "unclosed string", "indentation error", "not supported",
    "Tab characters", "invalid escape", "invalid hex", "invalid binary", "invalid octal", "invalid float",
    "invalid base", "Trailing comma", "Implicit string", "missing else", "not associative", "keyword argument must",
    "expected an indented", "octal escape", "floating-point", "Starlark does not"]
same = diff = skipped = 0
bad = []
for f in sorted(glob.glob(D + "/s*.txt")):
    n = os.path.basename(f)[:-4]
    c = W + "/bcache/" + n + ".cmp"
    if not os.path.exists(c):
        continue
    txt = open(c, encoding="latin-1").read()
    err = txt.split("--err\n", 1)[1]
    exp = []
    syntactic = False
    for line in err.splitlines():
        m = re.match(r"ERROR: <WS>/e/BUILD:(\d+):(\d+): (.*)$", line)
        if m:
            exp.append("BUILD:%s:%s: %s" % m.groups())
        if line.startswith("ERROR: package contains errors"):
            syntactic = True
    if exp and not all(any(p in e.split(": ", 1)[1] for p in PARSE_ONLY) for e in exp):
        skipped += 1  # later-phase (checker/resolver) messages: covered by the e2e runner
        continue
    # zel prints Latin-1 chars as UTF-8; Bazel writes them back as raw bytes (= source bytes).
    out = subprocess.run([EX, f], capture_output=True).stdout.decode("utf-8").encode("latin-1").decode("latin-1").splitlines()
    # Only compare against real parse errors: a later-phase error (resolver, name checks) means
    # the bazel list may contain non-parse messages; heuristically the parse errors come first.
    if out == exp:
        same += 1
    elif not out and not any(True for _ in exp):
        same += 1
    else:
        diff += 1
        bad.append(n)
        if "-v" in sys.argv:
            print("=== %s: %r" % (n, open(f, encoding="latin-1").read()[:120]))
            print("  bazel:", exp)
            print("  zel  :", out)
print("same=%d diff=%d skipped(non-parse)=%d" % (same, diff, skipped))
print("diffs:", " ".join(bad))
