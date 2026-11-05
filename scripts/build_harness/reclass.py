#!/usr/bin/env python3
"""Re-classify saved case dirs (b.norm/z.norm/key written by h.py) with the shared comparator
(scripts/harness_common/compare.py).  No version-skew rewrites exist any more: the spec is the real 9.3.2.
usage: reclass.py <outdir> [--cases]  -> SAME / ORDER (proven-nondeterministic, order-only) / REAL clusters."""
import os, re, sys, collections
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "harness_common"))
import compare as C
d = sys.argv[1]
def parse(p):
    t = open(p).read()
    rc = int(t.split("\n")[0].split("=")[1])
    so = t.split("--stdout--\n")[1].split("\n--stderr--\n")[0]
    se = t.split("\n--stderr--\n")[1]
    return rc, so, se
res = collections.Counter(); diffs = []
for c in sorted(os.listdir(d), key=lambda x: int(x[1:]) if re.match(r"c\d+$", x) else 10**9):
    if not re.match(r"c\d+$", c) or not os.path.exists(f"{d}/{c}/z.norm"): continue
    b = parse(f"{d}/{c}/b.norm"); z = parse(f"{d}/{c}/z.norm")
    cmd = open(f"{d}/{c}/cmd").read().strip()
    kf = f"{d}/{c}/key"
    ev = C.load_evidence(HERE, open(kf).read().strip()) if os.path.exists(kf) else None
    st, why = C.judge(b, z, ev, cmd)
    if st in ("SAME", "ORDER"): res[st] += 1; continue
    res["REAL"] += 1; diffs.append((c, cmd, b, z, why))
print(dict(res), "total", sum(res.values()))
g = collections.defaultdict(list)
for c, cmd, b, z, why in diffs:
    bl = C.lines_of(b[1] + "\n" + b[2]); zl = C.lines_of(z[1] + "\n" + z[2])
    ob = [l for l in bl if l not in set(zl)]; oz = [l for l in zl if l not in set(bl)]
    key = (ob[0][:80] if ob else "-") + " || " + (oz[0][:80] if oz else "-") + ("" if b[0] == z[0] else f" [rc {b[0]} vs {z[0]}]")
    g[key].append((c, cmd))
with open(d + "/clusters.txt", "w") as f:
    for k, v in sorted(g.items(), key=lambda x: -len(x[1])):
        f.write("%d %s\n    e.g. %s: %s\n" % (len(v), k, v[0][0], v[0][1]))
if "--cases" in sys.argv:
    with open(d + "/nonsame.txt", "w") as f:
        for c, cmd, b, z, why in diffs: f.write(cmd + "\n")
