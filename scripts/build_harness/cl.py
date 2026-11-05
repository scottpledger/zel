import os,collections,re,sys
d=sys.argv[1]
rows=[l.split("\t") for l in open(d+"/summary.tsv").read().split("\n")]
groups=collections.defaultdict(list)
def L(p):
    t=open(p).read(); return [l for l in t.split("\n") if l.strip() and not l.startswith("--std") and not re.match(r"^INFO: \d+ process",l) and "coverageoutputgenerator" not in l]
for r in rows:
    if len(r)<3: continue
    c,st,cmd=r
    if st!="REAL": continue
    b=L(f"{d}/{c}/b.norm"); z=L(f"{d}/{c}/z.norm")
    sb=set(b); sz=set(z)
    onlyb=[l for l in b if l not in sz]; onlyz=[l for l in z if l not in sb]
    key=(onlyb[0][:100] if onlyb else "-")+"  ||  "+(onlyz[0][:100] if onlyz else "-")
    groups[key].append((c,cmd))
out=[]
for k,v in sorted(groups.items(), key=lambda x:-len(x[1])):
    out.append("%d %s\n    e.g. %s: %s | %s: %s"%(len(v),k,v[0][0],v[0][1],v[-1][0],v[-1][1]))
open(d+"/clusters.txt","w").write("\n".join(out))
print(len(groups))
