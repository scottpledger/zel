from hconf import VBZ
import base64
import os,json,shutil,subprocess,hashlib,base64
R=VBZ+'/reg'; M=VBZ+'/mods'
shutil.rmtree(R,ignore_errors=True); shutil.rmtree(M,ignore_errors=True)
os.makedirs(R+'/modules'); os.makedirs(M)
json.dump({'module_base_path':M},open(R+'/bazel_registry.json','w'))
def sri(p): return 'sha256-'+base64.b64encode(hashlib.sha256(open(p,'rb').read()).digest()).decode()
def w(p,c):
    os.makedirs(os.path.dirname(p),exist_ok=True); open(p,'w').write(c)
def meta(name,versions,yanked=None):
    w(f'{R}/modules/{name}/metadata.json',json.dumps({'versions':versions,'yanked_versions':yanked or {}}))
BF='filegroup(name="all",srcs=glob(["**/*.txt"]),visibility=["//visibility:public"])\n'
def local(name,ver,deps='',extra='',files=None,compat=None,compat_args=''):
    mf=f'module(name="{name}",version="{ver}"'+(f',compatibility_level={compat}' if compat is not None else '')+compat_args+')\n'+deps+extra
    w(f'{R}/modules/{name}/{ver}/MODULE.bazel',mf)
    d=f'{M}/{name}+{ver}'; w(d+'/MODULE.bazel',mf); w(d+'/BUILD.bazel',BF); w(d+'/f.txt',f'{name} {ver}\n')
    for p,c in (files or {}).items(): w(f'{d}/{p}',c)
    w(f'{R}/modules/{name}/{ver}/source.json',json.dumps({'type':'local_path','path':f'{name}+{ver}'}))
def dep(n,v,**kw):
    a=''.join(f',{k}={json.dumps(x) if not isinstance(x,bool) else str(x)}' for k,x in kw.items())
    return f'bazel_dep(name="{n}",version="{v}"{a})\n'
# diamond
local('ma','1.0',dep('mb','1.0')+dep('mc','1.0'))
local('mb','1.0',dep('md','1.0'))
local('mc','1.0',dep('md','1.1'))
for v in ['1.0','1.1','1.2']: local('md',v)
meta('md',['1.0','1.1','1.2'],{'1.2':'yanked for reasons'})
for n in ['ma','mb','mc']: meta(n,['1.0'])
# compat
local('me','1.0',compat=1); local('me','1.5',compat=1); local('me','2.0',compat=2)
meta('me',['1.0','1.5','2.0'])
local('mf','1.0',dep('me','1.0')); meta('mf',['1.0'])
local('mg','1.0',dep('me','2.0')); meta('mg',['1.0'])
# multiple version override targets
local('mh','1.0',dep('me','1.0')); local('mh','2.0',dep('me','2.0')); meta('mh',['1.0','2.0'])
# cycle
local('cya','1.0',dep('cyb','1.0')); local('cyb','1.0',dep('cya','1.0')); meta('cya',['1.0']); meta('cyb',['1.0'])
# bazel_compat
local('bc1','1.0',compat_args=',bazel_compatibility=[">=0.1.0"]'); meta('bc1',['1.0'])
local('bc2','1.0',compat_args=',bazel_compatibility=[">=99.0.0"]'); meta('bc2',['1.0'])
local('bc3','1.0',compat_args=',bazel_compatibility=[">=7.0.0","-9.3.2","<100.0.0"]'); meta('bc3',['1.0'])
# dev dep / repo_name / nodep
local('mi','1.0',dep('md','1.0',repo_name='dd')+dep('mj','1.0',dev_dependency=True)); local('mj','1.0'); meta('mi',['1.0']); meta('mj',['1.0'])
local('mk','1.0',dep('md','1.0',repo_name=None)); meta('mk',['1.0'])
# extensions
extbzl='''def _r(rctx):
    rctx.file("BUILD.bazel","filegroup(name='g',visibility=['//visibility:public'])\\n")
    rctx.file("v.txt",rctx.attr.v+"\\n")
rr=repository_rule(implementation=_r,attrs={"v":attr.string()})
def _impl(mctx):
    for m in mctx.modules:
        for t in m.tags.mk:
            rr(name=t.name,v=m.name+"|"+m.version+"|"+",".join(t.opt)+("|dev" if mctx.is_dev_dependency(t) else ""))
    return mctx.extension_metadata(reproducible=True) if hasattr(mctx,"extension_metadata") else None
mk=tag_class(attrs={"name":attr.string(),"opt":attr.string_list(default=[])})
ex=module_extension(implementation=_impl,tag_classes={"mk":mk})
def _impl2(mctx):
    rr(name="fixed",v="x")
ex2=module_extension(implementation=_impl2)
'''
local('mx','1.0',files={'ext.bzl':extbzl},extra='')
local('mx','2.0',files={'ext.bzl':extbzl},extra=''); meta('mx',['1.0','2.0'])
local('mu','1.0',dep('mx','1.0')+'e=use_extension("@mx//:ext.bzl","ex")\ne.mk(name="from_mu",opt=["u"])\nuse_repo(e,"from_mu")\n'); meta('mu',['1.0'])
# archive modules
def archmod(name,ver,fmt='tar.gz',strip=None,patches=None,overlay=None,patch_strip=1,integrity=True,deps='',extra_src=None,files=None):
    mf=f'module(name="{name}",version="{ver}")\n{deps}'
    d=f'{R}/modules/{name}/{ver}'; w(d+'/MODULE.bazel',mf)
    s=f'{VBZ}/modsrc/{name}_{ver}'; shutil.rmtree(s,ignore_errors=True)
    top=f'{s}/{strip}' if strip else s
    w(top+'/MODULE.bazel',mf); w(top+'/BUILD.bazel',BF); w(top+'/x.txt','x\n'); w(top+'/sub/y.txt','y\n')
    for p,c in (files or {}).items(): w(f'{top}/{p}',c)
    arc=f'{M}/{name}-{ver}.{fmt}'
    if fmt=='tar.gz': subprocess.run(['tar','czf',arc,'-C',s,'.'],check=True)
    elif fmt=='zip': subprocess.run(f'cd {s} && zip -qr {arc} .',shell=True,check=True)
    elif fmt=='tar.xz': subprocess.run(f'tar cf - -C {s} . | xz -c > {arc}',shell=True,check=True)
    elif fmt=='tar.zst': subprocess.run(f'tar cf - -C {s} . | zstd -q -c > {arc}',shell=True,check=True)
    elif fmt=='tar.bz2': subprocess.run(['tar','cjf',arc,'-C',s,'.'],check=True)
    elif fmt=='7z': subprocess.run(f'cd {s} && 7zz a -bso0 -bsp0 {arc} .',shell=True,check=True)
    sj={'url':'file://'+arc}
    if integrity: sj['integrity']=sri(arc)
    if strip: sj['strip_prefix']=strip
    if patches:
        sj['patches']={}; sj['patch_strip']=patch_strip
        for pn,pc in patches.items(): w(f'{d}/patches/{pn}',pc); sj['patches'][pn]=sri(f'{d}/patches/{pn}')
    if overlay:
        sj['overlay']={}
        for pn,pc in overlay.items(): w(f'{d}/overlay/{pn}',pc); sj['overlay'][pn]=sri(f'{d}/overlay/{pn}')
    if extra_src: sj.update(extra_src)
    w(d+'/source.json',json.dumps(sj)); meta(name,[ver])
for fmt in ['tar.gz','zip','tar.xz','tar.zst','tar.bz2','7z']:
    archmod('ar_'+fmt.replace('.','_'),'1.0',fmt=fmt,strip='pfx-1.0')
archmod('ar_nostrip','1.0')
archmod('ar_noint','1.0',integrity=False)
p1='--- a/x.txt\n+++ b/x.txt\n@@ -1 +1 @@\n-x\n+patched\n'
archmod('ar_patch','1.0',strip='pfx-1.0',patches={'p1.patch':p1})
archmod('ar_patchbad','1.0',strip='pfx-1.0',patches={'p1.patch':'--- a/x.txt\n+++ b/x.txt\n@@ -1 +1 @@\n-nomatch\n+patched\n'})
archmod('ar_overlay','1.0',strip='pfx-1.0',overlay={'ov.txt':'overlay\n','sub/ov2.txt':'o2\n'})
archmod('ar_badstrip','1.0',strip='nonexistent-1.0',extra_src={'strip_prefix':'wrong'})
archmod('ar_badint','1.0',extra_src={'integrity':'sha256-'+base64.b64encode(b'0'*32).decode()})
archmod('ar_missingurl','1.0',extra_src={'url':'file://'+VBZ+'/does/not/exist.tar.gz'})
archmod('ar_dep','1.0',deps=dep('md','1.1'))
archmod('ar_ext','1.0',files={'ext.bzl':extbzl})
# override-able plain module for repo overrides
local('ov','1.0'); meta('ov',['1.0'])
# module with explicit mismatched name/version
w(f'{R}/modules/badname/1.0/MODULE.bazel','module(name="other",version="1.0")\n'); w(f'{R}/modules/badname/1.0/source.json',json.dumps({'type':'local_path','path':'ov+1.0'})); meta('badname',['1.0'])
w(f'{R}/modules/badver/1.0/MODULE.bazel','module(name="badver",version="1.1")\n'); w(f'{R}/modules/badver/1.0/source.json',json.dumps({'type':'local_path','path':'ov+1.0'})); meta('badver',['1.0'])
w(f'{R}/modules/malf/1.0/MODULE.bazel','module(name="malf",version="1.0"\n'); w(f'{R}/modules/malf/1.0/source.json',json.dumps({'type':'local_path','path':'ov+1.0'})); meta('malf',['1.0'])
w(f'{R}/modules/nosrc/1.0/MODULE.bazel','module(name="nosrc",version="1.0")\n'); meta('nosrc',['1.0'])
w(f'{R}/modules/badsrc/1.0/MODULE.bazel','module(name="badsrc",version="1.0")\n'); w(f'{R}/modules/badsrc/1.0/source.json','{"type":"weird"}'); meta('badsrc',['1.0'])
w(f'{R}/modules/badmeta/metadata.json','{not json'); w(f'{R}/modules/badmeta/1.0/MODULE.bazel','module(name="badmeta",version="1.0")\n')
w(f'{R}/modules/emptyver/metadata.json','{"versions":[]}')
print('ok')
# Builtin (bazel_tools) closure stubs shipped with the harness.
_CS=os.path.join(os.path.dirname(os.path.abspath(__file__)),'closure_stubs')
for n in os.listdir(_CS+'/reg'):
    shutil.copytree(f'{_CS}/reg/{n}',f'{R}/modules/{n}',dirs_exist_ok=True)
for d in os.listdir(_CS+'/mods'):
    shutil.copytree(f'{_CS}/mods/{d}',f'{M}/{d}',dirs_exist_ok=True)
