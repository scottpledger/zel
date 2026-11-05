"""Corrupted-archive differential matrix (zel vs bazel). Usage: ZEL_BIN=.. python3 corrupt.py [gen] [substr...]
Bases come from $VBZ/arch; mutants go to $VBZ/corrupt. Compares rc, ALL ERROR lines (normalized), and the
resulting repo tree (even on failure). Hang = TIMEOUT."""
from hconf import VBZ
import os,sys,re,subprocess,hashlib,random
from concurrent.futures import ThreadPoolExecutor
Z=os.environ.get('ZEL_BIN','zel'); B=os.environ.get('BAZEL_BIN','/tmp/bazel-9.3.2')
A=VBZ+'/arch'; C=VBZ+'/corrupt'
BASES=['small.tar','small.tar.gz','small.tar.bz2','small.x-6.tar.xz','small.z-3.tar.zst','small.tar.lzma','small.cnone.tar.xz',
 'small.zip','small.store.zip','small.7z','small.deflate.7z','small.copy.7z','pkg_gzip.deb','pkg_xz.deb','pkg_zstd.deb','lib.ar','bare.gz','bare.xz','bare.zst','bare.bz2']
def mutants(n):
    out=[]
    ps=sorted({0,1,2,3,4,6,8,10,16,n//8,n//4,n//2,3*n//4,n-30,n-20,n-12,n-8,n-4,n-2,n-1})
    ps=[p for p in ps if 0<=p<n]
    for p in ps: out.append(('trunc%d'%p,lambda b,p=p:b[:p]))
    for p in ps:
        out.append(('flip%d'%p,lambda b,p=p:b[:p]+bytes([b[p]^0xff])+b[p+1:]))
    out.append(('zerohead',lambda b:b'\0'*16+b[16:]))
    out.append(('tailgarb1',lambda b:b+b'\0'))
    out.append(('tailgarb4',lambda b:b+b'\1\2\3\4'))
    out.append(('tailgarb512',lambda b:b+b'\0'*512))
    out.append(('tailtext',lambda b:b+b'hello world garbage\n'))
    out.append(('prepend',lambda b:b'junk'+b))
    out.append(('dup',lambda b:b+b))
    return out
def gen():
    os.makedirs(C,exist_ok=True); k=0
    for f in BASES:
        b=open(f'{A}/{f}','rb').read(); ms=mutants(len(b))
        # large bases: thin out flips
        if len(b)>5000: ms=[m for m in ms if m[0].startswith(('trunc','tail','prepend','dup','zero')) or __import__("zlib").crc32(m[0].encode())%2==0][:30]
        for tag,fn in ms:
            open(f'{C}/{tag}.{f}','wb').write(fn(b)); k+=1
    print('generated',k)
def nm(o): return 'r_'+re.sub(r'[^a-z0-9]','_',o.lower())
def tree(root):
    out=[]
    for dp,dn,fn in os.walk(root):
        for x in sorted(dn+fn):
            p=os.path.join(dp,x); r=os.path.relpath(p,root)
            if r=='REPO.bazel': continue
            if os.path.islink(p): out.append((r,'L',os.readlink(p)))
            elif os.path.isdir(p): out.append((r,'D',oct(os.lstat(p).st_mode&0o777)))
            else:
                try: h=hashlib.md5(open(p,'rb').read()).hexdigest()
                except Exception: h='unreadable'
                out.append((r,'F',oct(os.lstat(p).st_mode&0o777),h))
    return sorted(out)
def norm(l): return re.sub(re.escape(VBZ)+r'/\S*','P',re.sub(r'\b[0-9a-f]{64}\b','H',re.sub(r'/(private/)?tmp/\S*','P',l)))
def runside(k,bin,chunk,idx):
    W=f'{VBZ}/cws_{k}_{idx}'; ob=f'{VBZ}/cob_{k}_{idx}'; os.makedirs(W,exist_ok=True)
    m='module(name="t")\nhttp_archive = use_repo_rule("@bazel_tools//tools/build_defs/repo:http.bzl","http_archive")\n'
    for o in chunk: m+=f'http_archive(name="{nm(o)}",urls=["file://{C}/{o}"])\n'
    open(W+'/MODULE.bazel','w').write(m)
    for f in('BUILD','WORKSPACE','.bazelrc'): open(f'{W}/{f}','w').write('')
    res={}
    for o in chunk:
        n=nm(o)
        try: p=subprocess.run([bin,'--output_base='+ob,'fetch','--repo=@'+n,'--enable_workspace=false','--lockfile_mode=off','--repo_contents_cache=','--registry=file://'+VBZ+'/reg'],cwd=W,capture_output=True,text=True,errors='replace',timeout=120)
        except subprocess.TimeoutExpired: res[o]=('TIMEOUT',[],None); continue
        d=subprocess.run(f'ls -d {ob}/external/*+{n} {ob}/external/{n} 2>/dev/null|head -1',shell=True,capture_output=True,text=True).stdout.strip()
        err=[norm(l) for l in p.stderr.replace('\r','\n').splitlines() if not l.startswith(('INFO','Computing','Starting','Another command','pid=','owner=','cwd=','tty=','Waiting for it','WARNING','Fetching repository','Loading','Analyzing','DEBUG'))and l.strip()]
        res[o]=(p.returncode,err,tree(d) if d else None)
    subprocess.run([bin,'--output_base='+ob,'shutdown'],cwd=W,capture_output=True,timeout=60)
    return res
def side(k,bin,files,N=6):
    ch=[files[i::N] for i in range(N)]
    with ThreadPoolExecutor(N) as ex: rs=list(ex.map(lambda a:runside(k,bin,a[1],a[0]),enumerate(ch)))
    d={}
    for r in rs: d.update(r)
    return d
if __name__=='__main__':
    a=sys.argv[1:]
    if a and a[0]=='gen': gen(); a=a[1:]
    files=sorted(os.listdir(C))
    if a: files=[f for f in files if any(s in f for s in a)]
    rb=side('b',B,files); rz=side('z',Z,files); ok=0; bad=0
    for o in files:
        b,z=rb[o],rz[o]
        if b==z: ok+=1; continue
        bad+=1; print('DIFF',o,'rc b/z',b[0],z[0]); print('  B:',b[1][:4]); print('  Z:',z[1][:4])
        if b[2] is not None and z[2] is not None:
            sb,sz=set(map(tuple,b[2])),set(map(tuple,z[2])); print('  onlyB',sorted(sb-sz)[:3],'onlyZ',sorted(sz-sb)[:3])
        elif b[2] is not None or z[2] is not None: print('  tree b/z',None if b[2] is None else len(b[2]),None if z[2] is None else len(z[2]))
    print('TOTAL',len(files),'ok',ok,'diff',bad)
