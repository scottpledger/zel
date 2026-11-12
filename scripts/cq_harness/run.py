#!/usr/bin/env python3
"""Differential query harness: zel vs real bazel.
Env: ZEL_BIN, BAZEL_BIN (default /opt/homebrew/bin/bazel), QH_ROOT (default /tmp/cqh), BAZEL_SRC (~/Development/bazel for protos).
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
ROOT=os.environ.get('QH_ROOT','/tmp/cqh')
ZEL=os.environ.get('ZEL_BIN','/tmp/zel-target/release/zel')
BAZEL=os.environ.get('BAZEL_BIN','/tmp/bazel-9.3.2')  # real 9.3.2 binary, never bazelisk
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
noise=re.compile(r'^(Computing main repo|INFO: [0-9]+ process|Starting local Bazel|\s+no actions running|INFO: (Elapsed|Analyzed|Build completed|Invocation|Streaming|Current date|Repository|Reading|Computing|Analyzed)|Loading:|Analyzing:|\(| no actions|Starting local|\[|Computing|INFO: Build|INFO: From|INFO: Writing|Fetching|.*\d+ packages loaded|INFO: Elapsed|\s*currently loading)')
def decode(cmd,data):
    ms=re.findall(r'--output=(\w+)',cmd)
    out_fmt=ms[-1] if ms else None
    kind=cmd.split()[0]
    if kind in ('cquery','aquery') and out_fmt in ('proto','streamed_proto'):
        msg='analysis.CqueryResult' if kind=='cquery' else 'analysis.ActionGraphContainer'
        pf=SRC+'/src/main/protobuf/analysis_v2.proto'
        def dec(d): return subprocess.run(['protoc','-I',SRC,'--decode='+msg,pf],input=d,capture_output=True).stdout.decode('utf8','replace')
        if out_fmt=='proto': return dec(data)
        out=[];i=0
        while i<len(data):
            n=0;sh=0
            while i<len(data):
                b=data[i];i+=1;n|=(b&0x7f)<<sh;sh+=7
                if not b&0x80:break
            out.append(dec(data[i:i+n]));i+=n
        return ''.join(out)
    if kind=='query' and out_fmt not in('proto','streamed_proto'): return None
    if kind!='query': return None
    pf=SRC+'/src/main/protobuf/build.proto'
    def dec(msg,d): return subprocess.run(['protoc','-I',SRC,'--decode='+msg,pf],input=d,capture_output=True).stdout.decode('utf8','replace')
    if out_fmt=='proto': return dec('blaze_query.QueryResult',data)
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
BLOCK=re.compile(r'(?=^action \'|^actions \{|^ *\{\n *"targetId|^ *"targetId")',re.M)
ABS_KEY=re.compile(r'Mnemonic: (SourceSymlinkManifest|SymlinkTree)\b|mnemonic: "(SourceSymlinkManifest|SymlinkTree)"|"mnemonic": "(SourceSymlinkManifest|SymlinkTree)"')
def pathkeys(t):
    """The ONLY normalization beyond paths/hashes: ActionKeys of SymlinkTree-like actions fingerprint the
    absolute output-base path, so they differ between any two output bases (platform fact, not skew; the real
    binary run in two output bases yields different keys).  Every other ActionKey must match exactly."""
    if 'ActionKey' not in t and 'action_key' not in t and 'actionKey' not in t: return t
    out=[]
    for blk in BLOCK.split(t):
        if ABS_KEY.search(blk):
            blk=re.sub(r'(ActionKey: |action_key: "|"actionKey": ")[0-9a-f]{64}',r'\1PATHKEY',blk)
            blk=re.sub(r'FileWriteContents: \[[^\]]*\]','FileWriteContents: [PATHB64]',blk)
            blk=re.sub(r'file_write_contents: "[^"]*"','file_write_contents: "PATHB64"',blk)
            blk=re.sub(r'"fileWriteContents": "[^"]*"','"fileWriteContents": "PATHB64"',blk)
        out.append(blk)
    return ''.join(out)
def canon_aquery_tp(text):
    if 'actions {' not in text: return text
    blocks=[]; cur=[]
    for l in text.splitlines():
        if not l.strip(): continue
        cur.append(l.strip())
        if l.strip()=='}': blocks.append(cur); cur=[]
    pfs={}; arts={}; rcs={}; tgts={}; cfgs={}; asps={}; dss={}; acts=[]
    for b in blocks:
        kind=b[0].split()[0]
        kv={}; rkv={}; sub=None; sub_list=[]
        for l in b[1:-1]:
            if l.endswith('{'): sub={}
            elif l=='}': sub_list.append(tuple(sorted(sub.items()))); sub=None
            elif sub is not None:
                k,v=l.split(': ',1); sub[k]=v
            else:
                if ': ' not in l: return text
                k,v=l.split(': ',1); kv[k]=v; rkv.setdefault(k,[]).append(v)
        if kind=='path_fragments': pfs[kv['id']]=(kv.get('label',''),kv.get('parent_id'))
        elif kind=='artifacts': arts[kv['id']]=(kv.get('path_fragment_id'),kv.get('is_tree_artifact','false'))
        elif kind=='rule_classes': rcs[kv['id']]=kv.get('name','')
        elif kind=='targets': tgts[kv['id']]=(kv.get('label',''),kv.get('rule_class_id'))
        elif kind=='configuration': cfgs[kv['id']]=(kv.get('mnemonic',''),kv.get('platform_name',''),kv.get('checksum',''),kv.get('is_tool','false'))
        elif kind=='aspect_descriptors': asps[kv['id']]=(kv.get('name',''),tuple(sub_list))
        elif kind=='dep_set_of_files': dss[kv['id']]=(tuple(rkv.get('transitive_dep_set_ids',[])),tuple(rkv.get('direct_artifact_ids',[])))
        elif kind=='actions': acts.append((kv,rkv,tuple(sub_list)))
    def resolve_pf(pid):
        if not pid or pid not in pfs: return ''
        lbl,par=pfs[pid]; lbl=lbl.strip('"')
        return (resolve_pf(par)+'/'+lbl) if par else lbl
    def resolve_art(aid):
        pf_id,is_tree=arts[aid]
        return (resolve_pf(pf_id),is_tree)
    def resolve_ds(did):
        tids,aids=dss[did]
        return (tuple(resolve_ds(t) for t in tids),tuple(resolve_art(a) for a in aids))
    try:
        canon_acts=[]
        for kv,rkv,subs in acts:
            tlbl,rcid=tgts[kv['target_id']]
            rcname=rcs[rcid]
            cfg=cfgs.get(kv.get('configuration_id'))
            in_ds=tuple(resolve_ds(d) for d in rkv.get('input_dep_set_ids',[]))
            outs=tuple(resolve_art(a) for a in rkv.get('output_ids',[]))
            prim=resolve_art(kv['primary_output_id']) if 'primary_output_id' in kv else None
            asp_tup=tuple(asps[a] for a in rkv.get('aspect_descriptor_ids',[]))
            c=(tlbl,rcname,cfg,kv.get('mnemonic'),kv.get('action_key'),tuple(rkv.get('arguments',[])),in_ds,outs,prim,kv.get('execution_platform'),kv.get('is_exec'),kv.get('file_contents'),asp_tup,subs)
            canon_acts.append(c)
        return repr({
            'pfs': sorted(resolve_pf(k) for k in pfs),
            'arts': sorted(resolve_art(k) for k in arts),
            'rcs': sorted(rcs.values()),
            'tgts': sorted((l,rcs[r]) for l,r in tgts.values()),
            'cfgs': sorted(cfgs.values()),
            'dss': sorted(resolve_ds(k) for k in dss),
            'acts': sorted(canon_acts),
        })
    except Exception:
        return text

def canon_kg_err(lines):
    if not any('errors encountered while analyzing target' in l for l in lines): return lines
    blocks=[]; cur=[]
    for l in lines:
        if l.startswith(('ERROR: ','WARNING: ','INFO: ')):
            if cur: blocks.append('\n'.join(cur))
            cur=[l]
        else:
            if cur: cur.append(l)
            else: blocks.append(l)
    if cur: blocks.append('\n'.join(cur))
    def norm_blk(b):
        if b.startswith('WARNING: Targets were missing from graph: [') and b.endswith(']'):
            inner=b[len('WARNING: Targets were missing from graph: ['):-1]
            parts=inner.split(', ConfiguredTargetKey{')
            items=[x if x.startswith('ConfiguredTargetKey{') else 'ConfiguredTargetKey{'+x for x in parts]
            items.sort()
            return 'WARNING: Targets were missing from graph: ['+', '.join(items)+']'
        return b
    blocks=[norm_blk(b) for b in blocks]
    try:
        idx=next(i for i,b in enumerate(blocks) if b.startswith('INFO: Analysis succeeded for only '))
        pre=sorted(blocks[:idx])
        post=blocks[idx:]
        return '\n'.join(pre+post).split('\n')
    except StopIteration:
        return '\n'.join(blocks).split('\n')

def classify(cmd,b,z,key=None):
    """-> [] when identical (or identical modulo PROVEN real-bazel nondeterminism), else the list of problems.
    cquery/aquery are never relaxed (judge enforces it)."""
    (bo,be,brc),(zo,ze,zrc)=b,z
    bd=decode(cmd,bo); zd=decode(cmd,zo)
    b2=bd if bd is not None else bo.decode('utf8','replace'); z2=zd if zd is not None else zo.decode('utf8','replace')
    b2=pathkeys(b2); z2=pathkeys(z2)
    ms=re.findall(r'--output=(\w+)',cmd)
    if cmd.startswith('aquery') and ms and ms[-1] in ('proto','textproto','streamed_proto'):
        b2=canon_aquery_tp(b2); z2=canon_aquery_tp(z2)
    bl=[l for l in be.decode('utf8','replace').split('\n') if l and not noise.match(l)]
    zl=[l for l in ze.decode('utf8','replace').split('\n') if l and not noise.match(l)]
    if 'discard_analysis_cache' not in cmd and 'allow_analysis_cache_discard' not in cmd and 'max_config_changes_to_show' not in cmd:
        bl=[l for l in bl if not l.startswith('WARNING: Build option')]
        zl=[l for l in zl if not l.startswith('WARNING: Build option')]
    if any(l.startswith('ERROR: Skipping ') for l in bl) and any(l.startswith('ERROR: Skipping ') for l in zl):
        bl=[l for l in bl if l!='WARNING: Target pattern parsing failed.']
        zl=[l for l in zl if l!='WARNING: Target pattern parsing failed.']
    bl=canon_kg_err(bl); zl=canon_kg_err(zl)
    ev=CMP.load_evidence(HERE,key) if key else None
    st,why=CMP.judge((brc,b2,'\n'.join(bl)),(zrc,z2,'\n'.join(zl)),ev,cmd)
    if st=='ORDER': return ['~ORDER']
    if st=='SAME': return []
    return ['REAL: '+why[:160]]
def main():
    a=sys.argv[1:];bz=False;shards=2;only=None;files=[]
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
    for sf in ('fmt.star','bad.star','q1.txt'):
        if os.path.exists(HERE+'/'+sf): shutil.copy(HERE+'/'+sf, '/tmp/cqh/'+sf)
    tool=BAZEL if bz else ZEL
    q=queue.Queue()
    for c in cmds:q.put(c)
    res={}
    def worker(k):
        base='%s/w%d'%(ROOT,k); ob='%s/ob%s%d'%(ROOT,'b' if bz else 'z',k)
        if os.path.exists(base):shutil.rmtree(base)
        os.makedirs(base)
        shutil.copytree(HERE+'/ext',base+'/ext')
        for w in('ws','wsbroken'):
            shutil.copytree(HERE+'/'+w,base+'/'+w)
            shutil.copy(HERE+'/../../MODULE.bazel.lock',base+'/'+w+'/MODULE.bazel.lock')
            open(base+'/'+w+'/.bazelrc','w').write('common --registry=file:///tmp/vendored-registry\n')
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
                res[i]=(c,classify(c,b,(o,e,rc),CMP.case_key('cq',w,c)))
    ts=[threading.Thread(target=worker,args=(k,)) for k in range(shards)]
    [t.start() for t in ts];[t.join() for t in ts]
    ok=sum(1 for c,s in res.values() if not s or s==['~ORDER']); od=sum(1 for c,s in res.values() if s==['~ORDER'])
    with open(ROOT+'/summary.txt','w') as f:
        for i in sorted(res,key=lambda x:int(re.sub(r'\D','',x) or 0)):
            f.write('%s\t%s\t%s\n'%(i,','.join(res[i][1]) or 'OK',res[i][0]))
    print('total',len(res),'identical',ok,'(strict',ok-od,'order-only-proven',od,') real',len(res)-ok)
if __name__=='__main__': main()
