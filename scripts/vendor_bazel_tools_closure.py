#!/usr/bin/env python3
"""Vendors MODULE.bazel files (only) for the bazel_dep closure of the embedded bazel_tools
MODULE.bazel into third_party/bcr_modules (needs network once; tests are then hermetic via
make_vendored_registry.py). Usage: vendor_bazel_tools_closure.py"""
import os, re, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
src = open(os.path.join(ROOT, "crates/zel-bazel-tools/resources/MODULE.bazel")).read()
# Reuse the helpers of vendor_registry_closure without running its main loop.
code = open(os.path.join(ROOT, "scripts/vendor_registry_closure.py")).read().split("seen, todo =")[0]
ns = {"__file__": os.path.join(ROOT, "scripts/vendor_registry_closure.py")}
exec(code, ns)
seen, todo = set(), list(ns["deps"](src))
while todo:
    nv = todo.pop()
    if nv in seen:
        continue
    seen.add(nv)
    todo.extend(ns["deps"](ns["module_text"](*nv)))
print("closure:", len(seen))
