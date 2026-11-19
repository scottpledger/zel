#!/usr/bin/env python3
"""
scripts/sync_help_resources.py

Regenerates crates/zel-server/resources/help/* from the Bazel 9.3.2 source tree (the spec):

  * `src/main/java/.../runtime/commands/*.txt`          (build, test, help, startup_options, ...)
  * `src/main/java/.../bazel/commands/{fetch,mod,vendor}.txt`
  * `src/main/java/.../runtime/mobileinstall/mobile-install.txt`
  * `scripts/bazel-complete-{header,template}.bash`      (bash completion scaffolding that
                                                          `help completion` wraps around the
                                                          generated variables)
  * `canonicalize.txt` is the `help = "..." + "..."` annotation string of CanonicalizeCommand.java
    (there is no resource file for it), assembled by concatenating its Java string literals.

With `--check` nothing is written; the exit status says whether the committed files are stale.
The command usage *options* blocks, `flags-as-proto`, completion variables, `everything-as-html`
and the info/license text are not stored here: they are generated at run time from the
option tables (`scripts/extract_bazel_options.py` -> flag_tables.rs), the command list and
`scripts/generate_license.py`.
"""

import argparse
import os
import re
import sys

JAVA = "src/main/java/com/google/devtools/build/lib"
SOURCES = [
    (f"{JAVA}/runtime/commands", None),
    (f"{JAVA}/bazel/commands", {"fetch.txt", "mod.txt", "vendor.txt"}),
    (f"{JAVA}/runtime/mobileinstall", {"mobile-install.txt"}),
]


def collect(bazel):
    files = {}
    for rel, only in SOURCES:
        d = os.path.join(bazel, rel)
        for n in sorted(os.listdir(d)):
            if n.endswith(".txt") and (only is None or n in only):
                files[f"help/{n}"] = open(os.path.join(d, n), "rb").read()
    for n in ("bazel-complete-header.bash", "bazel-complete-template.bash"):
        files[f"help/{n}"] = open(os.path.join(bazel, "scripts", n), "rb").read()
    java = open(os.path.join(bazel, JAVA, "runtime/commands/CanonicalizeCommand.java")).read()
    m = re.search(r'name = "canonicalize-flags",.*?help =\s*', java, re.S)
    rest = java[m.end():]
    rest = rest[: rest.index(")\npublic")]
    lits = re.findall(r'"((?:[^"\\]|\\.)*)"', rest)
    text = "".join(bytes(l, "utf-8").decode("unicode_escape") for l in lits)
    # The annotation text ends with "...\n%{options}"; the resource file keeps it verbatim plus
    # the trailing newline (expandHelpTopic splits on %{options}).
    files["help/canonicalize.txt"] = (text + "\n").encode()
    return files


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bazel-dir", default=os.environ.get("BAZEL_SRC_DIR", os.path.expanduser("~/Development/bazel")))
    ap.add_argument("--check", action="store_true")
    a = ap.parse_args()
    root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "crates/zel-server/resources")
    files = collect(os.path.abspath(a.bazel_dir))
    # Only files Zel actually embeds are kept.
    used = set(re.findall(r'resources/(help/[A-Za-z0-9_.\-]+)', open(os.path.join(root, "..", "src/commands/help.rs")).read()))
    used |= {"help/bazel-complete-header.bash", "help/bazel-complete-template.bash"}
    stale = []
    for name in sorted(used):
        if name not in files:
            print(f"warning: no Bazel source for {name}", file=sys.stderr)
            continue
        p = os.path.join(root, name)
        cur = open(p, "rb").read() if os.path.exists(p) else None
        if cur != files[name]:
            stale.append(name)
            if not a.check:
                os.makedirs(os.path.dirname(p), exist_ok=True)
                open(p, "wb").write(files[name])
    if a.check:
        if stale:
            print("stale: " + ", ".join(stale), file=sys.stderr)
            sys.exit(1)
        print(f"help resources up to date ({len(used)} files)")
    else:
        print(f"synced {len(used)} files; rewritten: {', '.join(stale) or 'none'}")


if __name__ == "__main__":
    main()
