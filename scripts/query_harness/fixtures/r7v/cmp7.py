import sys,re,os,collections
sys.path.insert(0,'/tmp/r7v/cq'); sys.argv=['x']
import lib
def rd(p): return open(p,'rb').read()
nz=lambda s: s
rows=[]
for l in open('/tmp/r7v/cmds.tsv'):
    i,c=l.rstrip('\n').split('\t',1)
    try:
        b=(rd(f'/tmp/r7v/out_bazel/{i}.out'),rd(f'/tmp/r7v/out_bazel/{i}.err'),int(rd(f'/tmp/r7v/out_bazel/{i}.rc')))
        z=(rd(f'/tmp/r7v/out_zel/{i}.out'),rd(f'/tmp/r7v/out_zel/{i}.err'),int(rd(f'/tmp/r7v/out_zel/{i}.rc')))
    except FileNotFoundError: continue
    # path normalization: strip /private
    f=lambda x:tuple(re.sub(rb'/wk[01]/',b'/wkN/',y.replace(b'/private/tmp',b'/tmp')) for y in x[:2])+(x[2],)
    st=lib.classify(c,f(b),f(z)) if True else []
    rows.append((i,c,st))
ok=sum(1 for r in rows if not r[2])
print('total',len(rows),'identical',ok)
with open('/tmp/r7v/summary7.txt','w') as o:
    for i,c,st in rows: o.write(f'{i}\t{",".join(st) or "OK"}\t{c}\n')
