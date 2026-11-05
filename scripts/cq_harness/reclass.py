#!/usr/bin/env python3
"""reclass.py ROOT CMDS.tsv...: re-classify stored zel outputs of a run (ROOT/out) against baseline/ with the current run.py rules."""
import sys,os,re
HERE=os.path.dirname(os.path.abspath(__file__))
src=open(HERE+'/run.py').read().split("def main():")[0]
ns={'__file__':HERE+'/run.py'}
exec(src,ns)
R=sys.argv[1]; B=HERE+'/baseline'
cmds={}
for f in sys.argv[2:]:
    for l in open(f):
        l=l.rstrip('\n')
        if l: i,c=l.split('\t',1); cmds[i]=re.sub(r'^(ws|wsbroken):','',c)
res={}
for i,c in cmds.items():
    try:
        b=(open(f'{B}/{i}.b.out','rb').read(),open(f'{B}/{i}.b.err','rb').read(),int(open(f'{B}/{i}.b.rc').read()))
        z=(open(f'{R}/out/{i}.z.out','rb').read(),open(f'{R}/out/{i}.z.err','rb').read(),int(open(f'{R}/out/{i}.z.rc').read()))
    except FileNotFoundError: continue
    res[i]=ns['classify'](c,b,z)
import collections
tot=collections.Counter(); ok=collections.Counter()
with open(R+'/summary2.txt','w') as f:
    for i,s in res.items():
        k=i[0]; tot[k]+=1; ok[k]+= (not s)
        f.write('%s\t%s\t%s\n'%(i,','.join(s) or 'OK',cmds[i]))
print({k:f'{ok[k]}/{tot[k]}' for k in tot})
