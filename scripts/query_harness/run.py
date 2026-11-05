#!/usr/bin/env python3
"""Differential query harness: zel vs real bazel.
Env: ZEL_BIN, BAZEL_BIN (default /tmp/bazel-9.3.2, the real binary, never bazelisk), QH_ROOT (default /tmp/qh), BAZEL_SRC (~/Development/bazel for protos).
Usage: run.py [--bazel] [--shards N] [--only RE] CMDS.tsv...
  TSV lines: ID<TAB>CMD  or ID<TAB>WS:CMD (WS=ws|wsbroken, default ws).
  --bazel refreshes baseline/ID.b.* (real bazel); otherwise runs zel and compares against baseline.
Writes summary to $QH_ROOT/summary.txt.
"""
import os,re,sys
sys.path.insert(0,os.path.join(os.path.dirname(os.path.abspath(__file__)),'..','harness_common'))
import compare as CMP
import shutil,subprocess,threading,queue,glob
HERE=os.path.dirname(os.path.abspath(__file__))
ROOT=os.environ.get('QH_ROOT','/tmp/qh')
ZEL=os.environ.get('ZEL_BIN','/tmp/zel-target/release/zel')
BAZEL=os.environ.get('BAZEL_BIN','/tmp/bazel-9.3.2')
SRC=os.path.expanduser(os.environ.get('BAZEL_SRC','~/Development/bazel'))
BASE=HERE+'/baseline'
os.makedirs(BASE,exist_ok=True)
def ns(s,ws,ob):
    s=s.replace('/private/tmp/','/tmp/').replace(ws,'@WS@').replace(ob,'@OB@')
    s=re.sub(r'\x1b\[[0-9;]*[A-Za-z]','',s).replace('\r','')
    return s
def nh(s):
    s=re.sub(r'\b[0-9a-f]{64}\b','H64',s)
    return re.sub(r'(?<![0-9a-zA-Z_])[0-9a-f]{7}(?![0-9a-zA-Z_])','H7',s)
noise=re.compile(r'^(\s*- Only in|WARNING: Running Bazel server needs to be killed|INFO: Current working directory|Computing main repo|INFO: [0-9]+ process|Starting local Bazel|\s+no actions running|INFO: (Elapsed|Analyzed|Build completed|Invocation|Streaming|Current date|Repository|Reading|Computing|Analyzed)|Loading:|Analyzing:|\(|DEBUG:| no actions|Starting local|\[|Computing|INFO: Build|INFO: From|INFO: Writing|Fetching|.*\d+ packages loaded|INFO: Elapsed|\s*currently loading)')
def decode(cmd,data):
    m=re.search(r'--output=(\w+)',cmd)
    if not m or m.group(1) not in('proto','streamed_proto'): return None
    pf=SRC+'/src/main/protobuf/build.proto'
    def dec(msg,d): return subprocess.run(['protoc','-I',SRC,'--decode='+msg,pf],input=d,capture_output=True).stdout.decode('utf8','replace')
    if m.group(1)=='proto': return dec('blaze_query.QueryResult',data)
    out=[];i=0
    while i<len(data):
        n=0;sh=0
        while True:
            b=data[i];i+=1;n|=(b&0x7f)<<sh;sh+=7
            if not b&0x80:break
        out.append(dec('blaze_query.Target',data[i:i+n]));i+=n
    return '\n--\n'.join(out)
def run_one(tool,cmd,wsdir,ob):
    env={k:v for k,v in os.environ.items() if k!='USE_BAZEL_VERSION'}
    r=subprocess.run('timeout 300 %s --max_idle_secs=60 --output_base=%s %s'%(tool,ob,cmd),shell=True,cwd=wsdir,capture_output=True,stdin=subprocess.DEVNULL,env=env)
    return r.stdout,r.stderr,r.returncode
def classify(cmd,b,z,key=None):
    """[] when identical (or identical modulo PROVEN real-bazel nondeterminism: see harness_common/compare.py)."""
    (bo,be,brc),(zo,ze,zrc)=b,z
    bd=decode(cmd,bo); zd=decode(cmd,zo)
    b2=bd if bd is not None else bo.decode('utf8','replace'); z2=zd if zd is not None else zo.decode('utf8','replace')
    bl=[l for l in be.decode('utf8','replace').split('\n') if l and not noise.match(l)]
    zl=[l for l in ze.decode('utf8','replace').split('\n') if l and not noise.match(l)]
    ev=CMP.load_evidence(HERE,key) if key else None
    st,why=CMP.judge((brc,b2,'\n'.join(bl)),(zrc,z2,'\n'.join(zl)),ev,cmd)
    return ['~ORDER'] if st=='ORDER' else ([] if st=='SAME' else ['REAL: '+why[:160]])
def main():
    a=sys.argv[1:];bz=False;shards=6;only=None;files=[]
    while a:
        x=a.pop(0)
        if x=='--bazel':bz=True
        elif x=='--shards':shards=int(a.pop(0))
        elif x=='--only':only=re.compile(a.pop(0))
        else:files.append(x)
    cmds=[]
    for f in files:
        for l in open(f):
            l=l.rstrip('\n')
            if not l:continue
            i,c=l.split('\t',1)
            w='ws'
            m=re.match(r'^(ws|wsbroken):(.*)$',c)
            if m:w,c=m.groups()
            if only and not only.search(c):continue
            cmds.append((i,w,c))
    os.makedirs(ROOT,exist_ok=True)
    tool=BAZEL if bz else ZEL
    q=queue.Queue()
    for c in cmds:q.put(c)
    res={}
    def worker(k):
        base='%s/w%d'%(ROOT,k); ob='%s/ob%s%d'%(ROOT,'b' if bz else 'z',k)
        if os.path.exists(base):shutil.rmtree(base)
        os.makedirs(base)
        shutil.copytree(HERE+'/ext',base+'/ext')
        for w in('ws','wsbroken'): shutil.copytree(HERE+'/'+w,base+'/'+w)
        while True:
            try:i,w,c=q.get_nowait()
            except queue.Empty:return
            wsdir=base+'/'+w
            o,e,rc=run_one(tool,c,wsdir,ob)
            wsr=os.path.realpath(wsdir)
            def n(x):return ns(x.decode('utf8','replace'),wsr,os.path.realpath(ob)) .replace(wsdir,'@WS@').replace(ob,'@OB@').encode()
            o,e=n(o),n(e)
            if bz:
                for ext,d in(('out',o),('err',e),('rc',str(rc).encode())):open('%s/%s.b.%s'%(BASE,i,ext),'wb').write(d)
                res[i]=(c,[])
            else:
                try:
                    b=(open('%s/%s.b.out'%(BASE,i),'rb').read(),open('%s/%s.b.err'%(BASE,i),'rb').read(),int(open('%s/%s.b.rc'%(BASE,i)).read()))
                except FileNotFoundError:
                    res[i]=(c,['NOBASELINE']);continue
                od=ROOT+'/out';os.makedirs(od,exist_ok=True)
                for ext,d in(('out',o),('err',e),('rc',str(rc).encode())):open('%s/%s.z.%s'%(od,i,ext),'wb').write(d)
                res[i]=(c,classify(c,b,(o,e,rc),CMP.case_key('query',w,c)))
    ts=[threading.Thread(target=worker,args=(k,)) for k in range(shards)]
    [t.start() for t in ts];[t.join() for t in ts]
    ok=sum(1 for c,s in res.values() if not s or s==['~ORDER']); od=sum(1 for c,s in res.values() if s==['~ORDER'])
    with open(ROOT+'/summary.txt','w') as f:
        for i in sorted(res,key=lambda x:int(re.sub(r'\D','',x) or 0)):
            f.write('%s\t%s\t%s\n'%(i,','.join(res[i][1]) or 'OK',res[i][0]))
    print('total',len(res),'identical',ok,'(strict',ok-od,'order-only-proven',od,') real',len(res)-ok)
if __name__=='__main__': main()
