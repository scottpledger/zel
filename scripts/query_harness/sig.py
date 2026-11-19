#!/usr/bin/env python3
# Group failing commands by signature of first differing lines (stdout and stderr).
import sys,os,re,collections,difflib
H=os.path.dirname(os.path.abspath(__file__));R=os.environ.get('QH_ROOT','/tmp/qh')
noise=re.compile(r'^(\s*- Only in|WARNING: Running Bazel server needs to be killed|INFO: Current working directory|Computing main repo|INFO: [0-9]+ process|Starting local Bazel|\s+no actions running|INFO: (Elapsed|Analyzed|Build completed|Found|Invocation|Streaming|Current date|Repository|Reading|Computing|Analyzed)|Loading:|Analyzing:|\(|DEBUG:| no actions|Starting local|WARNING: Build option|\[|Computing|INFO: Build|INFO: From|INFO: Writing|Fetching|.*\d+ packages loaded|INFO: Elapsed|\s*currently loading)')
def rd(p): return open(p,'rb').read().decode('utf8','replace')
def norm(l): return re.sub(r'\b[0-9a-f]{7}\b','H7',l)
g=collections.defaultdict(list)
for l in open(R+'/summary.txt'):
    i,st,cmd=l.rstrip('\n').split('\t',2)
    if st=='OK': continue
    key=[]
    for kind in('out','err'):
        b=[x for x in rd('%s/baseline/%s.b.%s'%(H,i,kind)).split('\n') if kind=='out' or (x and not noise.match(x))]
        z=[x for x in rd('%s/out/%s.z.%s'%(R,i,kind)).split('\n') if kind=='out' or (x and not noise.match(x))]
        d=[x for x in difflib.unified_diff(b,z,lineterm='',n=0) if x[:1] in'+-' and x[:3] not in('+++','---')]
        if d: key.append(kind+': '+' | '.join(x[:90] for x in d[:2]))
    g[(st,' || '.join(key))].append(i)
for k,v in sorted(g.items(),key=lambda x:-len(x[1])):
    print(len(v),k[0],k[1][:260],v[:5])
