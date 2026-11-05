from hconf import VBZ
import json,os,subprocess,sys,re,time,hashlib
from concurrent.futures import ThreadPoolExecutor
Z=os.environ.get('ZEL_BIN','target/release/zel'); B=os.environ.get('BAZEL_BIN','/tmp/bazel-9.3.2')
A=VBZ+'/arch'
files=sorted(os.listdir(A))
sel=sys.argv[1:]
if sel: files=[f for f in files if any(s in f for s in sel)]
def nm(o): return 'r_'+re.sub(r'[^a-z0-9]','_',o.lower())
def tree(root):
    out=[]
    for dp,dn,fn in os.walk(root):
        for x in sorted(dn+fn):
            p=os.path.join(dp,x); r=os.path.relpath(p,root)
            if r in('REPO.bazel',) or r.startswith('..'): continue
            st=os.lstat(p)
            if os.path.islink(p): out.append((r,'L',os.readlink(p)))
            elif os.path.isdir(p): out.append((r,'D',oct(st.st_mode&0o777)))
            elif os.path.isfile(p):
                try: h=hashlib.md5(open(p,'rb').read()).hexdigest()
                except Exception as e: h='unreadable'
                out.append((r,'F',oct(st.st_mode&0o777),h))
            else: out.append((r,'S',oct(st.st_mode)))
    return sorted(out)
def runside(k,bin,chunk,idx):
    W=f'{VBZ}/ws_{k}_{idx}'; ob=f'{VBZ}/ob_{k}_{idx}'
    os.makedirs(W,exist_ok=True)
    m='module(name="t")\nhttp_archive = use_repo_rule("@bazel_tools//tools/build_defs/repo:http.bzl","http_archive")\n'
    for o in chunk:
        ty=''
        if o.startswith('bare_named') or o.startswith('hdrname'): pass
        m+=f'http_archive(name="{nm(o)}",urls=["file://{A}/{o}"],'+ (f'type="{o.rsplit(".",1)[1]}",' if False else '')+')\n'
    open(W+'/MODULE.bazel','w').write(m)
    for f in('BUILD','WORKSPACE','.bazelrc'): open(f'{W}/{f}','w').write('')
    res={}
    for o in chunk:
        n=nm(o)
        try: p=subprocess.run([bin,'--output_base='+ob,'fetch','--repo=@'+n,'--enable_workspace=false','--lockfile_mode=off','--repo_contents_cache=','--registry=file://'+VBZ+'/reg'],cwd=W,capture_output=True,text=True,timeout=300)
        except subprocess.TimeoutExpired: res[o]=('TIMEOUT','',None); continue
        d=subprocess.run(f'ls -d {ob}/external/*+{n} {ob}/external/{n} 2>/dev/null|head -1',shell=True,capture_output=True,text=True).stdout.strip()
        err=[l for l in p.stderr.replace('\r','\n').splitlines() if l.startswith('ERROR')]
        err=[re.sub(re.escape(VBZ)+r'/\S*','P',re.sub(r'\b[0-9a-f]{64}\b','H',l)) for l in err]
        res[o]=(p.returncode,err[:3],tree(d) if d and p.returncode==0 else None)
    subprocess.run([bin,'--output_base='+ob,'shutdown'],cwd=W,capture_output=True,timeout=60)
    return res
os.makedirs(VBZ+'/emptyreg',exist_ok=True)
N=6
chunks=[files[i::N] for i in range(N)]
def side(k,bin):
    with ThreadPoolExecutor(N) as ex:
        rs=list(ex.map(lambda a:runside(k,bin,a[1],a[0]),enumerate(chunks)))
    d={}
    for r in rs: d.update(r)
    return d
rb=side('b',B); rz=side('z',Z)
ok=0;bad=[]
for o in files:
    b,z=rb[o],rz[o]
    if b[0]==z[0] and b[2]==z[2]: ok+=1; continue
    bad.append(o)
    print('DIFF',o,'rc b/z',b[0],z[0])
    print('   B:',b[1]); print('   Z:',z[1])
    if b[2] and z[2]:
        sb,sz=set(map(tuple,b[2])),set(map(tuple,z[2])); print('   onlyB',sorted(sb-sz)[:4],'\n   onlyZ',sorted(sz-sb)[:4])
    elif b[2] is not None or z[2] is not None: print('   treeB',len(b[2] or []),'treeZ',len(z[2] or []))
print('TOTAL',len(files),'ok',ok,'diff',len(bad))
rcs={}
for o in files: rcs.setdefault(rb[o][0],0); rcs[rb[o][0]]+=1
print('bazel rc dist',rcs)
