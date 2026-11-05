#!/usr/bin/env python3
"""
scripts/generate_license.py

Regenerates crates/zel-server/resources/LICENSE the way Bazel's own build does:
`src/main/java/com/google/devtools/build/lib/runtime/commands/license:license`
(`merge_licenses.bzl`) merges `//:LICENSE` and `//third_party:srcs`, keeping every input whose
path contains one of ASSEMBLY_EXCEPTION / DISCLAIMER / LICENSE / license / THIRD_PARTY_README,
and runs

    for f in $SRCS; do echo ===== $f ===== && cat $f && echo && echo ; done > $OUT

The inputs are *discovered* in the Bazel source tree (no hard-coded list of third_party files):
the main LICENSE first, then every matching file below third_party/ in path order (this is
the order `glob(["**"])` + the sub-package `:srcs` filegroups yield for the files of the
`//third_party:srcs` closure). The `license` command prints a header, this text, and the license
files of a bundled JDK/JRE (handled at run time by `commands/license.rs`).

"""

import argparse
import os
import sys

SUBSTRINGS = ["ASSEMBLY_EXCEPTION", "DISCLAIMER", "LICENSE", "license", "THIRD_PARTY_README"]


def merge_license_inputs(bazel_src):
    """Returns the exec-path-relative list of files merge_licenses would receive, in order."""
    files = ["LICENSE"]
    tp = os.path.join(bazel_src, "third_party")
    found = []
    for root, dirs, names in os.walk(tp):
        dirs.sort()
        for n in names:
            rel = os.path.relpath(os.path.join(root, n), bazel_src)
            if any(s in rel for s in SUBSTRINGS):
                found.append(rel)
    files.extend(sorted(found))
    return files


def render(bazel_src, files):
    out = bytearray()
    for rel in files:
        with open(os.path.join(bazel_src, rel), "rb") as f:
            data = f.read()
        out += f"===== {rel} =====\n".encode()
        out += data
        out += b"\n\n"  # `echo && echo`
    return bytes(out)


def main():
    parser = argparse.ArgumentParser(description="Generate LICENSE resource from Bazel sources")
    parser.add_argument("--bazel-dir", default=os.environ.get("BAZEL_SRC_DIR", os.path.expanduser("~/Development/bazel")))
    parser.add_argument("--out", default=os.environ.get("OUT_FILE"))
    parser.add_argument("--check", action="store_true", help="fail if the output file differs")
    args = parser.parse_args()

    bazel_src = os.path.abspath(args.bazel_dir)
    repo_root = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
    out_file = os.path.abspath(args.out) if args.out else os.path.join(repo_root, "crates/zel-server/resources/LICENSE")

    if not os.path.isfile(os.path.join(bazel_src, "LICENSE")):
        print(f"Error: {bazel_src}/LICENSE not found", file=sys.stderr)
        sys.exit(1)
    files = merge_license_inputs(bazel_src)
    data = render(bazel_src, files)
    if args.check:
        cur = open(out_file, "rb").read() if os.path.exists(out_file) else b""
        if cur != data:
            print(f"{out_file} is stale; run scripts/generate_license.py", file=sys.stderr)
            sys.exit(1)
        print(f"{out_file} up to date ({len(files)} inputs)")
        return
    os.makedirs(os.path.dirname(out_file), exist_ok=True)
    with open(out_file, "wb") as out:
        out.write(data)
    print(f"Generated {out_file} from {len(files)} inputs: " + ", ".join(files))


if __name__ == "__main__":
    main()
