#!/usr/bin/env python3
"""Vendors the transitive bazel_dep closure of the pinned rules (needs network).

Fetches MODULE.bazel for every (module, version) reachable via non-dev bazel_dep, into
third_party/bcr_modules/<name>/<version>/MODULE.bazel (what Bazel's resolver needs), and full
sources (via vendor_rules_sources.py's logic) for the highest selected version of each module
that is not yet in third_party/bcr.
Usage: vendor_registry_closure.py
"""
import os, re, sys, json
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vendor_rules_sources as v

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
BCR = os.path.join(ROOT, "third_party", "bcr")
MODS = os.path.join(ROOT, "third_party", "bcr_modules")
DEP = re.compile(r"bazel_dep\(([^)]*)\)", re.S)

def vkey(s):
    return [(0, int(p)) if p.isdigit() else (1, p) for p in re.split(r"[.\-]", s)]

def module_text(name, ver):
    p = os.path.join(MODS, name, ver, "MODULE.bazel")
    q = os.path.join(BCR, name, ver, "MODULE.bazel")
    for c in (p, q):
        if os.path.exists(c):
            return open(c).read()
    try:
        t = v.get(f"{v.REG}/{name}/{ver}/MODULE.bazel").decode()
    except Exception as e:
        print("missing", name, ver, e, file=sys.stderr); return ""
    os.makedirs(os.path.dirname(p), exist_ok=True)
    open(p, "w").write(t)
    return t

def deps(text):
    out = []
    for m in DEP.finditer(text):
        body = m.group(1)
        if re.search(r"dev_dependency\s*=\s*True", body):
            continue
        n = re.search(r'name\s*=\s*"([^"]+)"', body)
        ver = re.search(r'version\s*=\s*"([^"]*)"', body)
        if n and ver and ver.group(1):
            out.append((n.group(1), ver.group(1)))
    return out

import hashlib
BAZEL_SRC = os.environ.get("BAZEL_SRC_DIR", os.path.expanduser("~/Development/bazel"))
# Fail loudly if the pins drifted from the Bazel checkout's src/MODULE.tools (the spec: Bazel 9.3.2).
_tools = open(os.path.join(BAZEL_SRC, "src", "MODULE.tools")).read()
_want = dict(deps(_tools))
for _n, _ver in _want.items():
    assert v.MODULES.get(_n) == _ver, f"pin drift for {_n}: vendor_rules_sources.py={v.MODULES.get(_n)} MODULE.tools={_ver}"
assert set(v.MODULES) == set(_want), (set(v.MODULES) ^ set(_want))
_lock = json.load(open(os.path.join(BAZEL_SRC, "MODULE.bazel.lock")))["registryFileHashes"]

seen, todo = set(), list(v.MODULES.items())
while todo:
    nv = todo.pop()
    if nv in seen: continue
    seen.add(nv)
    todo.extend(deps(module_text(*nv)))
# Module selection (MVS) with pruning: only dependencies declared by *selected* versions count.
best = dict(v.MODULES)
while True:
    nxt, stack, vis = dict(v.MODULES), list(v.MODULES.items()), set()
    # BFS over the currently selected versions only
    while stack:
        n, ver = stack.pop()
        if best.get(n) != ver or (n, ver) in vis:
            continue
        vis.add((n, ver))
        for dn, dv in deps(module_text(n, ver)):
            if dn not in nxt or vkey(dv) > vkey(nxt[dn]):
                nxt[dn] = dv
            stack.append((dn, dv))
    # versions raised during this pass are re-examined on the next pass
    if nxt == best:
        break
    best = {n: max(best.get(n, ver), ver, key=vkey) if n in best else ver for n, ver in nxt.items()}
best = {n: ver for n, ver in best.items() if any(n == a for a, _ in seen)}
# Integrity: every discovered MODULE.bazel must hash like the one recorded in 9.3.2's lockfile.
_checked = _missing = 0
for _n, _ver in sorted(seen):
    _t = module_text(_n, _ver)
    _k = f"{v.REG}/{_n}/{_ver}/MODULE.bazel"
    if _k in _lock:
        assert hashlib.sha256(_t.encode()).hexdigest() == _lock[_k], f"MODULE.bazel hash mismatch vs 9.3.2 lockfile: {_n} {_ver}"
        _checked += 1
    else:
        _missing += 1
print(f"lockfile-verified MODULE.bazel files: {_checked}; not in the 9.3.2 lockfile: {_missing}")
print("closure:", len(seen), "modules:", len(best))
json.dump(best, open(os.path.join(ROOT, "third_party", "bcr_selected.json"), "w"), indent=1, sort_keys=True)
v.MODULES = {n: ver for n, ver in best.items() if not os.path.isdir(os.path.join(BCR, n, ver))}
print("to vendor:", v.MODULES)
v.main(BCR)
