import os, sys, re, collections
d = sys.argv[1]
def parse(p):
    t = open(p).read()
    rc = t.split("\n")[0]
    so = t.split("--stdout--\n")[1].split("\n--stderr--\n")[0]
    se = t.split("\n--stderr--\n")[1]
    return rc, so, se
def lines(s): return [l for l in s.split("\n") if l.strip() and not re.match(r"^INFO: \d+ process", l) and "coverageoutputgenerator.Main" not in l and not re.match(r"^INFO: Lcov|^INFO: Lcov", l)]
same = merged = diff = 0
rows = []
for c in sorted(os.listdir(d), key=lambda x: int(x[1:]) if x[0] == 'c' and x[1:].isdigit() else 10**9):
    if not c.startswith("c") or not c[1:].isdigit(): continue
    p = os.path.join(d, c)
    if not os.path.exists(p + "/z.norm"): continue
    b = parse(p + "/b.norm"); z = parse(p + "/z.norm")
    cmd = open(p + "/cmd").read().strip()
    if b[0]==z[0] and lines(b[1])==lines(z[1]) and lines(b[2])==lines(z[2]): same += 1; st = "SAME"
    elif b[0] == z[0] and sorted(lines(b[1]) + lines(b[2])) == sorted(lines(z[1]) + lines(z[2])):
        merged += 1; st = "STREAM"
    elif b[0] == z[0] and lines(b[1]) + lines(b[2]) == lines(z[1]) + lines(z[2]):
        st = "STREAM"; merged += 1
    else: diff += 1; st = "DIFF"
    rows.append((c, st, cmd))
print("same", same, "stream-only", merged, "diff", diff, "total", same + merged + diff)
open(d + "/summary.tsv", "w").write("\n".join("%s\t%s\t%s" % r for r in rows))
