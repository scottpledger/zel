from hconf import VBZ,PORT
import sys,os
sys.path.insert(0,os.path.join(os.path.dirname(os.path.abspath(__file__)),'..','harness_common'))
import compare as CMP
import difflib,os,re,subprocess,shutil,sys,json,hashlib
from concurrent.futures import ThreadPoolExecutor
Z=os.environ.get('ZEL_BIN','target/debug/zel'); B=os.environ.get('BAZEL_BIN','/tmp/bazel-9.3.2')  # real 9.3.2 binary, never bazelisk
ROOT=VBZ+'/w'
REG='--registry=file://'+VBZ+'/reg'
COMMON=['--enable_workspace=false']
def norm(s,ws,ob,dbg=False):
    s=re.sub(r'\x1b\[[0-9;]*m','',s)
    s=s.replace('\r','\n')
    s=s.replace('/private'+ws,'WS').replace(ws,'WS').replace('/private'+ob,'OB').replace(ob,'OB')
    s=s.replace('/private/tmp','/tmp')
    # Platform fact (not skew): the install base of `@bazel_tools` is a hash of the running binary, so it differs between any two binaries.
    s=re.sub(r'/install/[0-9a-f]{32}/embedded_tools','/install/IB/embedded_tools',s)
    s=re.sub(r'temp\d{10,}','tempN',s)  # Bazel's random scratch dir inside the repo directory
    s=re.sub(r'\b[0-9a-f]{64}\b','H64',s)
    s=re.sub(r'sha256-[A-Za-z0-9+/]{43}=','SRI',s)
    # Resolution of the built-in bazel_tools closure is parallel. With an empty registry,
    # either implementation may report any one of the missing closure modules first.
    if '/emptyreg/modules/' in s:
        s=re.sub(r'(bazel_tools@_ -> )[^:]+:', r'\1<MISSING>:', s)
        s=re.sub(r'module [^ ]+ not found in registries:', 'module <MISSING> not found in registries:', s)
        s=re.sub(r'/emptyreg/modules/[^/]+/[^/]+/MODULE\.bazel', '/emptyreg/modules/<MISSING>/MODULE.bazel', s)
    pat=r'(?m)^(Starting local|\.\.\. still trying to connect|Computing main|Loading:|Analyzing:|INFO: (Elapsed|Analyzed|Build completed|Found|Invocation|Streaming|Current date|Repository|Reading|Fetching|Updated|All requested)|'+('' if dbg else 'DEBUG:|')+r'\(\d+:\d+:\d+\)|Fetching ).*\n?'
    s=re.sub(pat,'',s)
    return s
def run_one(bin,args,ws,ob,env=None):
    e=dict(os.environ); e.update(env or {})
    e.pop('USE_BAZEL_VERSION',None)  # zel acts as bazelisk when it is set
    try: p=subprocess.run([bin,'--output_base='+ob]+args,cwd=ws,capture_output=True,text=True,encoding='utf-8',errors='replace',timeout=180,env=e)
    except subprocess.TimeoutExpired: return 'TIMEOUT','',''
    return p.returncode,p.stdout,p.stderr
def do_case(c):
    name=c['name']; res={}
    for k,bin in (('b',B),('z',Z)):
        # Use the same workspace basename for both binaries. Repository rules can observe
        # `repository_ctx.workspace_root`; differing leaf names would make equivalent runs diverge.
        ws=f'{ROOT}/{name}/{k}/ws'; ob=f'{ROOT}/{name}/ob_{k}'
        shutil.rmtree(f'{ROOT}/{name}/{k}',ignore_errors=True); shutil.rmtree(ob,ignore_errors=True)
        os.makedirs(ws)
        files=dict(c.get('files',{}))
        files.setdefault('MODULE.bazel','module(name="root",version="1.0")\n')
        files.setdefault('BUILD.bazel','')
        for p,t in files.items():
            if t is None: continue
            os.makedirs(os.path.dirname(os.path.join(ws,p)),exist_ok=True); open(os.path.join(ws,p),'w').write(t.replace('18765',PORT))
        outs=[]
        CUR.p=[('/private'+ws,'WS'),(ws,'WS'),('/private'+ob,'OB'),(ob,'OB')]
        regflags=[] if c.get('noreg') else [REG]
        cmds=c.get('pre',[])+[c['args']]
        for a in cmds:
            a=[x.replace('18765',PORT) for x in a]
            # a = [cmd, ...]; insert common flags after cmd
            full=[a[0]]+COMMON+regflags+([] if c.get('shared_cache') else ['--repository_cache='+ob+'/rcache'])+[x.replace('18765',PORT) for x in c.get('flags',[])]+a[1:]
            rc,so,se=run_one(bin,full,ws,ob,c.get('env'))
            outs.append((rc,norm(so,ws,ob,c.get('dbg')),norm(se,ws,ob,c.get('dbg'))))
        post={}
        for f in c.get('cmp',[]):
            fp=os.path.join(ws,f)
            if os.path.isdir(fp): post[f]=tree(fp)
            elif os.path.exists(fp): post[f]=norm(open(fp,'rb').read().decode('latin-1'),ws,ob)
            else: post[f]=None
        for f in c.get('jcmp',[]):
            fp=os.path.join(ws,f)
            if os.path.exists(fp):
                try:
                    j=json.load(open(fp)); 
                    post['json:'+f]=norm(json.dumps(j,indent=1,sort_keys=True),ws,ob)
                except Exception as e: post['json:'+f]='BADJSON '+str(e)
            else: post['json:'+f]=None
        for f in c.get('cmp_exact',[]):
            # compared byte for byte (only the per-run workspace/output-base paths are replaced)
            fp=os.path.join(ws,f)
            if os.path.isfile(fp): post['exact:'+f]=_n(open(fp,'rb').read()).decode('latin-1')
            else: post['exact:'+f]=None
        for f in c.get('cmp_ob',[]):
            import glob
            ms=glob.glob(ob+'/'+f,recursive=True)
            post['ob:'+f]=sorted(os.path.relpath(m,ob) for m in ms)[:50]
            if len(ms)==1 and os.path.isfile(ms[0]): post['ob:'+f+':content']=norm(open(ms[0],'rb').read().decode('latin-1'),ws,ob)
        for f in c.get('tree',[]):
            import glob
            for m in glob.glob(ob+'/'+f)[:1]: post['tree:'+f]=tree(m)
        subprocess.run([bin,'--output_base='+ob,'shutdown'],cwd=ws,capture_output=True,timeout=60)
        res[k]=(outs,post)
    return name,res
def errlines(s,dbg=False):
  import os
  if os.environ.get('NORM2'): s='\n'.join(l for l in s.splitlines() if not l.startswith('\t\t') and not l.startswith('\tFile "<builtin>"') and l!='error executing MODULE.bazel file for the root module')
  if os.environ.get('NORM3'): s='\n'.join(l for l in s.splitlines() if not l.startswith(('   Traceback','\tFile ','Error in ')))
  return [l for l in s.splitlines() if l.strip() and not re.match(r'(INFO: |no actions running|Loading|Analyzing|\[\d|Computing|Starting|\(\d+:\d+)',l.strip()) ]
def compare(name,res,c):
    diffs=[]
    (bo,bp),(zo,zp)=res['b'],res['z']
    if any('FATAL: bazel crashed' in o[2] for o in bo) and not c.get('compare_crash'): return ['SKIPPED: reference bazel 9.3.2 crashed with an internal error (bazel bug, see verify notes); zel exit=%s'%[o[0] for o in zo]]
    for i,(b,z) in enumerate(zip(bo,zo)):
        if b[0]!=z[0]: diffs.append(f'step{i} exit b={b[0]} z={z[0]}')
        ev=CMP.load_evidence(os.path.dirname(os.path.abspath(__file__)),CMP.case_key('bzlmod','',name,'step%d'%i))
        if ev and b[0]==z[0] and not (b[1]==z[1] and errlines(b[2],c.get('dbg'))==errlines(z[2],c.get('dbg'))):
            st,why=CMP.judge((b[0],b[1],'\n'.join(errlines(b[2],c.get('dbg')))),(z[0],z[1],'\n'.join(errlines(z[2],c.get('dbg')))),ev,'')
            if st=='ORDER': continue
        if not c.get('ignore_stdout') and b[1]!=z[1]: diffs.append(f'step{i} stdout differs:\n'+''.join(list(difflib.unified_diff(b[1].splitlines(1),z[1].splitlines(1),'bazel','zel',n=1))[:40]))
        eb,ez=errlines(b[2],c.get('dbg')),errlines(z[2],c.get('dbg'))
        if not c.get('ignore_stderr') and eb!=ez: diffs.append(f'step{i} stderr differs:\n--- bazel\n'+'\n'.join(eb[:8])+'\n--- zel\n'+'\n'.join(ez[:8]))
    for f in set(bp)|set(zp):
        if bp.get(f)!=zp.get(f):
            a,b2=bp.get(f),zp.get(f)
            if isinstance(a,str) and isinstance(b2,str): diffs.append(f'file {f} differs:\n'+''.join(list(difflib.unified_diff(a.splitlines(1),b2.splitlines(1),'bazel','zel',n=0))[:60]))
            elif isinstance(a,list) and isinstance(b2,list): diffs.append(f'file {f} differs: only-bazel={sorted(set(a)-set(b2))[:10]} only-zel={sorted(set(b2)-set(a))[:10]}')
            else: diffs.append(f'file {f} differs:\n--- bazel\n{str(a)[:1500]}\n--- zel\n{str(b2)[:1500]}')
    return diffs
def main(cases,jobs=4):
    only=sys.argv[1:] 
    cases=[c for c in cases if not only or any(o in c['name'] for o in only)]
    ok=0;bad=[];skipped=[]
    with ThreadPoolExecutor(jobs) as ex:
        for name,res in ex.map(do_case,cases):
            c=[x for x in cases if x['name']==name][0]
            d=compare(name,res,c)
            if d and d[0].startswith('SKIPPED'): skipped.append(name); print('SKIP',name,d[0],flush=True); continue
            if d: bad.append(name); print('DIFF',name,flush=True); print('\n'.join(d),flush=True)
            else: ok+=1; print('OK  ',name,flush=True)
    print(f'TOTAL {len(cases)} ok={ok} diff={len(bad)} bazel_crash_skipped={len(skipped)}: {bad}')

import threading
CUR=threading.local()
def _n(b):
    for k,v in getattr(CUR,'p',[]): b=b.replace(k.encode(),v.encode())
    return b
def tree(root):
    out=[]
    for dp,dn,fn in os.walk(root):
        for x in sorted(dn+fn):
            p=os.path.join(dp,x); r=os.path.relpath(p,root)
            # Java's createTempDirectory suffix is random; compare the resulting entry and mode,
            # not two unrelated random suffixes.
            r=re.sub(r'(?<=/)temp\d+|^temp\d+', 'tempN', r)
            st=os.lstat(p)
            if os.path.islink(p): out.append(f'{r} L {_n(os.readlink(p).encode()).decode().replace(root,"ROOT")}')
            elif os.path.isdir(p): out.append(f'{r} D {oct(st.st_mode&0o777)}')
            else: out.append(f'{r} F {oct(st.st_mode&0o777)} {hashlib.md5(_n(open(p,"rb").read())).hexdigest()[:8]}')
    return sorted(out)
