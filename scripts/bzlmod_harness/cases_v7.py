import sys,os
sys.path.insert(0,'/Users/pledger/Development/zel/scripts/bzlmod_harness')
from hconf import VBZ
from harness import *
cases=[]
def C(name,args,files,flags=None,**kw):
    f=dict(files); cases.append(dict(name='v7_'+name,args=args,files=f,flags=flags if flags is not None else [],**kw))
def lp(name,ver,deps='',extra=''):
    return {f'{name}/MODULE.bazel':f'module(name="{name}",version="{ver}")\n{deps}{extra}',f'{name}/BUILD.bazel':'exports_files(["x.txt"])\n',f'{name}/x.txt':name}
def merge(*ds):
    o={}
    for d in ds: o.update(d)
    return o
def lpo(n): return f'local_path_override(module_name="{n}",path="{n}")\n'
# ---------- graphs
EXT_BZL='''def _impl(mctx):
    for m in mctx.modules:
        for t in m.tags.mk:
            repo_rule(name=t.name, v=t.v)
def _rr(rctx):
    rctx.file("BUILD.bazel","filegroup(name='f',srcs=['v.txt'],visibility=['//visibility:public'])")
    rctx.file("v.txt",rctx.attr.v)
repo_rule=repository_rule(_rr,attrs={"v":attr.string()})
ex=module_extension(_impl,tag_classes={"mk":tag_class(attrs={"name":attr.string(),"v":attr.string(default="d")})})
'''
G={}
G['diam']=merge(lp('la','1.0','bazel_dep(name="lb",version="1.0")\nbazel_dep(name="lc",version="1.0",repo_name="ccc")\n'),
    lp('lb','1.0','bazel_dep(name="ld",version="1.0")\n'),lp('lc','1.0','bazel_dep(name="ld",version="1.0")\nbazel_dep(name="lb",version="1.0")\n'),lp('ld','1.0'),
    {'MODULE.bazel':'module(name="root",version="1.0")\nbazel_dep(name="la",version="1.0")\nbazel_dep(name="lb",version="1.0")\nbazel_dep(name="ldev",version="0.1",dev_dependency=True)\n'+''.join(lpo(n) for n in ['la','lb','lc','ld','ldev'])},lp('ldev','0.1','bazel_dep(name="ld",version="1.0")\n'))
G['ext']=merge(lp('ea','1.0','e=use_extension("//:ext.bzl","ex")\ne.mk(name="ea_r",v="1")\nuse_repo(e,"ea_r")\n'),
    lp('eb','2.0','bazel_dep(name="ea",version="1.0")\ne=use_extension("@ea//:ext.bzl","ex")\ne.mk(name="eb_r",v="2")\nuse_repo(e,"eb_r")\n'),
    {'ea/ext.bzl':EXT_BZL,'ext.bzl':EXT_BZL,'MODULE.bazel':'module(name="root",version="1.0")\nbazel_dep(name="ea",version="1.0")\nbazel_dep(name="eb",version="2.0")\n'+lpo('ea')+lpo('eb')+
     'e=use_extension("@ea//:ext.bzl","ex")\ne.mk(name="root_r",v="r")\ne.mk(name="root_r2",v="r2")\nuse_repo(e,"root_r","root_r2",aa="ea_r")\n'
     'i=use_extension("//:ext.bzl","ex",isolate=True)\ni.mk(name="iso",v="i")\nuse_repo(i,"iso")\n'
     'd=use_extension("//:ext.bzl","ex",dev_dependency=True)\nd.mk(name="devr")\nuse_repo(d,"devr")\n'})
G['ver']=merge({'MODULE.bazel':'module(name="root",version="1.0",compatibility_level=3)\nbazel_dep(name="ma",version="1.0")\nbazel_dep(name="me",version="1.0")\nbazel_dep(name="mh",version="1.0")\nsingle_version_override(module_name="me",version="1.5")\n'})
G['mvo']=merge({'MODULE.bazel':'module(name="root",version="1.0")\nbazel_dep(name="mf",version="1.0")\nbazel_dep(name="mg",version="1.0")\nmultiple_version_override(module_name="me",versions=["1.0","2.0"])\n'})
G['unused']=merge({'MODULE.bazel':'module(name="root",version="1.0")\nbazel_dep(name="md",version="1.0")\nbazel_dep(name="ma",version="1.0")\nbazel_dep(name="md",version="1.1",repo_name="md11")\n'})
G['nodeps']=merge({'MODULE.bazel':'module(name="root",version="1.0",repo_name="rr")\nbazel_dep(name="mi",version="1.0",repo_name=None)\nbazel_dep(name="mj",version="1.0",dev_dependency=True)\ninclude("//:inc.MODULE.bazel")\n','inc.MODULE.bazel':'bazel_dep(name="mb",version="1.0")\nbazel_dep(name="mc",version="1.0",repo_name="cc")\n'})
subs=[(['graph'],''),(['graph','--depth=1'],'d1'),(['graph','--depth=0'],'d0'),(['graph','--depth=2'],'d2'),(['graph','--output=json'],'json'),(['graph','--output=graph'],'dot'),
 (['graph','--charset=ascii'],'asc'),(['graph','--charset=utf8'],'utf'),(['graph','--verbose'],'verb'),(['graph','--include_unused'],'unused'),(['graph','--include_unused','--verbose'],'unusedv'),
 (['graph','--cycles'],'cyc'),(['graph','--extension_info=usages'],'eiu'),(['graph','--extension_info=repos'],'eir'),(['graph','--extension_info=all'],'eia'),(['graph','--extension_info=all','--extension_filter=//:ext.bzl%ex'],'eif'),
 (['graph','--extension_info=all','--output=json'],'eiaj'),(['graph','--from=ma'],'fromma'),(['graph','--from=la'],'fromla'),(['graph','--from=nonexist'],'fromno'),
 (['deps'],'deps'),(['deps','--output=json'],'depsj'),(['deps','--depth=1'],'deps1'),(['deps','--base_module=la'],'depsla'),(['deps','--include_builtin'],'depsb'),
 (['all_paths','ld'],'ap_ld'),(['all_paths','ld','--from=la'],'ap_ld_la'),(['path','ld'],'p_ld'),(['path','ld','lb'],'p_ld_lb'),(['path','ma','--output=json'],'p_ma_j'),(['all_paths','mb','--verbose'],'ap_mb_v'),
 (['explain','ld'],'ex_ld'),(['explain','lb','ld'],'ex_lb_ld'),(['explain','me'],'ex_me'),(['explain','md'],'ex_md'),(['explain','md','--verbose'],'ex_md_v'),(['explain','me','--output=json'],'ex_me_j'),
 (['show_repo','ld'],'sr_ld'),(['show_repo','@ld'],'sr_at'),(['show_repo','@@ld+'],'sr_can'),(['show_repo','ld','--output=streamed_proto'],'sr_proto'),(['show_repo','ld','--output=streamed_jsonproto'],'sr_jproto'),(['show_repo','ld','--output=json'],'sr_json'),
 (['show_repo','--all_repos'],'sr_all'),(['show_repo','<root>'],'sr_root'),(['show_repo','root'],'sr_root2'),(['show_repo','ea','--base_module=eb'],'sr_base'),(['show_repo','ea_r'],'sr_extrepo'),(['show_repo','@@+ex+ea_r'],'sr_canext'),
 (['show_repo','--extension_info=all','e1'],'sr_ei'),(['show_repo','nonexist'],'sr_none'),(['show_repo','@@nonexist'],'sr_none2'),
 (['dump_repo_mapping',''],'drm_root'),(['dump_repo_mapping','la+'],'drm_la'),(['dump_repo_mapping','', 'la+'],'drm_two'),(['dump_repo_mapping','nonexist'],'drm_none'),(['dump_repo_mapping'],'drm_noarg'),
 (['show_extension','//:ext.bzl%ex'],'se1'),(['show_extension','@ea//:ext.bzl%ex'],'se2'),(['show_extension','@@ea+//:ext.bzl%ex'],'se3'),(['show_extension','//:ext.bzl%nope'],'se_no'),(['show_extension'],'se_noarg'),(['show_extension','@ea//:ext.bzl%ex','--base_module=eb'],'se_base'),
 (['graph','--bogus'],'bogus'),(['foo'],'unk'),(['graph','--depth=x'],'depthx'),(['graph','--output=xml'],'outxml'),(['graph','--charset=latin'],'cslatin'),
 (['tidy'],'tidy'),(['deps','--lockfile_mode=error'],'lm_err'),(['graph','--noenable_bzlmod'],'nobzlmod'),(['graph','--extension_info=bogus'],'eibogus'),(['graph','--extension_filter=//:ext.bzl%ex'],'efonly'),
]
for g,files in G.items():
    for a,t in subs:
        # all subs against all graphs would be 6*75=450: pick a stable subset per graph
        pass
import zlib
for g,files in G.items():
    for i,(a,t) in enumerate(subs):
        if g!='ext' and g!='diam' and (zlib.crc32((g+t).encode())%3): continue
        if g in('ver','mvo','unused','nodeps') and a[0] in('show_extension',) and zlib.crc32(t.encode())%2: continue
        C(f'mod_{g}_{t}',['mod']+a,files,flags=['--allow_yanked_versions=md@1.2'],ignore_stderr=False)
# ---------- lockfile content for new extension fixtures
def lockcase(name,ext,mod_extra='',extra_files=None,cmd=None,flags=None,pre=None):
    files=merge({'MODULE.bazel':'module(name="root",version="1.0")\n'+mod_extra,'ext.bzl':ext,'BUILD.bazel':''},extra_files or {})
    cases.append(dict(name='v7_lock_'+name,args=cmd or ['fetch','--all'],files=files,flags=flags or [],jcmp=['MODULE.bazel.lock'],pre=pre or []))
USE='e=use_extension("//:ext.bzl","ex")\ne.t(name="a",n=1,l=["x"],d={"k":"v"},b=True,lab="//:f",f="1.5")\nuse_repo(e,"r")\n'
def ex(body,tags='{"t":tag_class(attrs={"name":attr.string(),"n":attr.int(),"l":attr.string_list(),"d":attr.string_dict(),"b":attr.bool(),"lab":attr.label(),"f":attr.string()})}',kw=''):
    return 'def _rr(rctx):\n    rctx.file("BUILD.bazel","")\nrr=repository_rule(_rr,attrs={"x":attr.string(),"l":attr.string_list()})\ndef _impl(mctx):\n'+body+'\nex=module_extension(_impl,tag_classes='+tags+kw+')\n'
lockcase('basic',ex('    rr(name="r")'),USE)
lockcase('reproducible',ex('    rr(name="r")\n    return mctx.extension_metadata(reproducible=True)'),USE)
lockcase('meta_roots',ex('    rr(name="r")\n    return mctx.extension_metadata(root_module_direct_deps=["r"],root_module_direct_dev_deps=[])'),USE)
lockcase('meta_all',ex('    rr(name="r")\n    return mctx.extension_metadata(root_module_direct_deps="all",root_module_direct_dev_deps=[])'),USE)
lockcase('env',ex('    x=mctx.getenv("LK_V","none")\n    rr(name="r",x=x)',kw=',environ=["LK_V"]'),USE,flags=['--repo_env=LK_V=hello'])
lockcase('env_unset',ex('    x=mctx.getenv("LK_V","none")\n    rr(name="r",x=x)',kw=',environ=["LK_V"]'),USE)
lockcase('os_dep',ex('    rr(name="r",x=mctx.os.name)',kw=',os_dependent=True'),USE)
lockcase('arch_dep',ex('    rr(name="r",x=mctx.os.arch)',kw=',arch_dependent=True'),USE)
lockcase('both_dep',ex('    rr(name="r")',kw=',os_dependent=True,arch_dependent=True'),USE)
lockcase('read_file',ex('    x=mctx.read(Label("//:data.txt"))\n    rr(name="r",x=x)'),USE,{'data.txt':'abc'})
lockcase('execute',ex('    r=mctx.execute(["echo","hi"])\n    rr(name="r",x=r.stdout)'),USE)
lockcase('download',ex('    mctx.download(url="http://127.0.0.1:18877/hello.txt",output="h.txt",sha256="5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03")\n    rr(name="r")'),USE)
lockcase('download_nohash',ex('    mctx.download(url="http://127.0.0.1:18877/hello.txt",output="h.txt")\n    rr(name="r")'),USE)
lockcase('dev_only',ex('    rr(name="r")'),'e=use_extension("//:ext.bzl","ex",dev_dependency=True)\ne.t(name="a")\nuse_repo(e,"r")\n')
lockcase('isolated',ex('    rr(name="r")'),'e=use_extension("//:ext.bzl","ex",isolate=True)\ne.t(name="a")\nuse_repo(e,"r")\n')
lockcase('two_ext',ex('    rr(name="r")'),USE+'e2=use_extension("//:ext.bzl","ex")\ne2.t(name="b")\nuse_repo(e2,"r")\n')
lockcase('fail',ex('    fail("boom")'),USE)
lockcase('nonjson_attr',ex('    rr(name="r",l=["a","b"])'),'e=use_extension("//:ext.bzl","ex")\ne.t(name="a",l=["z","y"],d={"b":"1","a":"2"})\nuse_repo(e,"r")\n')
lockcase('unicode',ex('    rr(name="r",x="héllo \\u2028 <>&\'")'),USE)
lockcase('facts',ex('    rr(name="r")\n    return mctx.extension_metadata(facts={"k":"v","n":[1,2]})'),USE)
lockcase('facts_reproducible',ex('    rr(name="r")\n    return mctx.extension_metadata(reproducible=True,facts={"a":1})'),USE)
lockcase('mode_off',ex('    rr(name="r")'),USE,flags=['--lockfile_mode=off'])
lockcase('mode_refresh',ex('    rr(name="r")'),USE,flags=['--lockfile_mode=refresh'])
lockcase('mode_error',ex('    rr(name="r")'),USE,flags=['--lockfile_mode=error'])
lockcase('relock',ex('    rr(name="r")'),USE,pre=[['fetch','--all']])
lockcase('relock_chg_env',ex('    x=mctx.getenv("LK_V","none")\n    rr(name="r",x=x)',kw=',environ=["LK_V"]'),USE,pre=[['fetch','--all','--repo_env=LK_V=1']],flags=[])
lockcase('build',ex('    rr(name="r")'),USE,cmd=['build','@r//:all'],extra_files={})
lockcase('mod_deps',ex('    rr(name="r")'),USE,cmd=['mod','deps'])
lockcase('mod_tidy_lock',ex('    rr(name="r")'),USE,cmd=['mod','graph'])
lockcase('nested_label',ex('    rr(name="r",x=str(Label("@@//:x")))'),USE)
lockcase('tags_label_canon',ex('    rr(name="r")'),'bazel_dep(name="ma",version="1.0")\ne=use_extension("//:ext.bzl","ex")\ne.t(name="a",lab="@ma//:x")\nuse_repo(e,"r")\n')
lockcase('tags_many',ex('    rr(name="r")'),'e=use_extension("//:ext.bzl","ex")\nfor i in range(5):\n    e.t(name="a%d"%i,n=i)\nuse_repo(e,"r")\n')
lockcase('repo_names',ex('    rr(name="r")'),'e=use_extension("//:ext.bzl","ex")\ne.t(name="a")\nuse_repo(e,x="r")\n')
# ---------- repository_ctx behaviors (starlark repo rules)
RC={}
RC['read']='x=rctx.read("in.txt")\n    rctx.file("out.txt",x+"|"+str(len(x)))'
RC['read_nonexist']='rctx.read("nope.txt")'
RC['file_exec']='rctx.file("a.sh","#!/bin/sh\\necho hi\\n",executable=True)\n    r=rctx.execute(["./a.sh"])\n    rctx.file("o",r.stdout+str(r.return_code))'
RC['file_noexec']='rctx.file("a.sh","x",executable=False)\n    rctx.file("sub/dir/b","y")'
RC['file_legacy']='rctx.file("a",content="x",legacy_utf8=False)'
RC['template']='rctx.template("o.txt",Label("//:tpl.txt"),substitutions={"%A%":"1","%B%":"two"},executable=False)'
RC['template_strict']='rctx.template("o.txt",Label("//:tpl.txt"),substitutions={"%Z%":"1"})'
RC['template_nostrict']='rctx.template("o.txt",Label("//:tpl.txt"),substitutions={"%Z%":"1"},watch_template="no")'
RC['symlink']='rctx.symlink(Label("//:in.txt"),"lnk")\n    rctx.symlink("sub/target","lnk2")'
RC['symlink_dir']='rctx.symlink(rctx.workspace_root,"lnkdir")'
RC['path']='p=rctx.path("a/b/../c")\n    rctx.file("o",str(p)+"|"+str(p.basename)+"|"+str(p.dirname)+"|"+str(p.exists)+"|"+str(p.is_dir))'
RC['path_label']='p=rctx.path(Label("//:in.txt"))\n    rctx.file("o",str(p.exists)+str(p.basename))'
RC['path_readdir']='rctx.file("d/a","1")\n    rctx.file("d/b","2")\n    rctx.file("o",str(sorted([str(x.basename) for x in rctx.path("d").readdir()])))'
RC['path_readdir_nodir']='rctx.path("nope").readdir()'
RC['path_get_child']='p=rctx.path("d").get_child("x","y")\n    rctx.file("o",str(p))'
RC['which']='rctx.file("o",str(rctx.which("sh"))+"|"+str(rctx.which("zzznotfound")))'
RC['execute_env']='r=rctx.execute(["sh","-c","echo $FOO"],environment={"FOO":"bar"})\n    rctx.file("o",r.stdout)'
RC['execute_fail']='r=rctx.execute(["sh","-c","echo e >&2; exit 3"])\n    rctx.file("o",str(r.return_code)+"|"+r.stderr)'
RC['execute_timeout']='r=rctx.execute(["sleep","5"],timeout=1)\n    rctx.file("o",str(r.return_code))'
RC['execute_cwd']='rctx.file("sub/x","1")\n    r=rctx.execute(["ls"],working_directory="sub")\n    rctx.file("o",r.stdout)'
RC['execute_quiet']='r=rctx.execute(["echo","q"],quiet=False)\n    rctx.file("o",r.stdout)'
RC['execute_nobin']='rctx.execute(["zzz_no_such_bin"])'
RC['execute_empty']='rctx.execute([])'
RC['execute_badarg']='rctx.execute(["echo",1])'
RC['os']='rctx.file("o",rctx.os.name+"|"+rctx.os.arch+"|"+str(sorted(rctx.os.environ.keys())==sorted(rctx.os.environ.keys())))'
RC['getenv']='rctx.file("o",str(rctx.getenv("RCV"))+"|"+str(rctx.getenv("RCV2","dflt")))'
RC['attr']='rctx.file("o",str(rctx.attr.s)+str(rctx.attr.l)+str(rctx.attr.d)+str(rctx.attr.i)+str(rctx.attr.b)+rctx.attr.name)'
RC['name']='rctx.file("o",rctx.name)'
RC['workspace_root']='rctx.file("o",str(rctx.workspace_root))'
RC['delete']='rctx.file("d/a","1")\n    rctx.file("o",str(rctx.delete("d"))+str(rctx.delete("nonexist")))'
RC['rename']='rctx.file("a","1")\n    rctx.rename("a","b")\n    rctx.file("o",rctx.read("b"))'
RC['rename_missing']='rctx.rename("a","b")'
RC['report_progress']='rctx.report_progress("hi")\n    rctx.file("o","1")'
RC['download_ok']='r=rctx.download(url="http://127.0.0.1:18877/hello.txt",output="h",sha256="5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03")\n    rctx.file("o",str(r.success)+r.sha256[:6])'
RC['download_badhash']='rctx.download(url="http://127.0.0.1:18877/hello.txt",output="h",sha256="'+'0'*64+'")'
RC['download_404']='rctx.download(url="http://127.0.0.1:18877/nonexist",output="h")'
RC['download_nonblock']='t=rctx.download(url="http://127.0.0.1:18877/hello.txt",output="h",block=False)\n    r=t.wait()\n    rctx.file("o",str(r.success))'
RC['download_integrity']='rctx.download(url="http://127.0.0.1:18877/hello.txt",output="h",integrity="sha256-WJG1tSLV3whtD/CxEPvZ0hu0/HFjrzTQgoai6Eb2vgM=")'
RC['download_allow_fail']='r=rctx.download(url="http://127.0.0.1:18877/nonexist",output="h",allow_fail=True)\n    rctx.file("o",str(r.success))'
RC['download_hdr']='rctx.download(url="http://127.0.0.1:18877/echo",output="h",headers={"X-Custom":["a","b"]})\n    rctx.file("o",rctx.read("h"))'
RC['download_exec']='rctx.download(url="http://127.0.0.1:18877/hello.txt",output="h",executable=True)'
RC['download_auth']='rctx.download(url="http://127.0.0.1:18877/auth",output="h",auth={"http://127.0.0.1:18877/auth":{"type":"basic","login":"user","password":"pass"}})\n    rctx.file("o",rctx.read("h"))'
RC['download_multi']='rctx.download(url=["http://127.0.0.1:18877/nonexist","http://127.0.0.1:18877/hello.txt"],output="h")\n    rctx.file("o",rctx.read("h"))'
RC['download_noout']='rctx.download(url="http://127.0.0.1:18877/hello.txt")'
RC['da_tgz']='rctx.download_and_extract(url="http://127.0.0.1:18877/small.tar.gz",strip_prefix="top",output="x")'
RC['da_zip']='r=rctx.download_and_extract(url="http://127.0.0.1:18877/small.zip",sha256="'+'0'*64+'")'
RC['da_type']='rctx.download_and_extract(url="http://127.0.0.1:18877/noname",type="tar.gz",strip_prefix="top")'
RC['da_badtype']='rctx.download_and_extract(url="http://127.0.0.1:18877/small.tar.gz",type="rar")'
RC['da_rename']='rctx.download_and_extract(url="http://127.0.0.1:18877/small.tar.gz",strip_prefix="top",rename_files={"a.txt":"A.txt"})'
RC['da_badprefix']='rctx.download_and_extract(url="http://127.0.0.1:18877/small.tar.gz",strip_prefix="nopfx")'
RC['da_allow_fail']='r=rctx.download_and_extract(url="http://127.0.0.1:18877/nonexist",allow_fail=True)\n    rctx.file("o",str(r.success))'
RC['extract']='rctx.download(url="http://127.0.0.1:18877/small.tar.gz",output="a.tgz")\n    rctx.extract("a.tgz",output="e",strip_prefix="top")'
RC['extract_missing']='rctx.extract("nope.tgz")'
RC['extract_7z']='rctx.download(url="http://127.0.0.1:18877/small.7z",output="a.7z")\n    rctx.extract("a.7z")'
RC['patch']='rctx.file("a.txt","hello\\n")\n    rctx.patch(Label("//:p.patch"),strip=0)\n    rctx.file("o",rctx.read("a.txt"))'
RC['patch_fuzz']='rctx.file("a.txt","hello\\nworld\\n")\n    rctx.patch(Label("//:p.patch"),strip=1)'
RC['watch']='rctx.watch(Label("//:in.txt"))\n    rctx.file("o","w")'
RC['watch_tree']='rctx.watch_tree(".")\n    rctx.file("o","w")'
RC['repo_metadata']='rctx.file("o","m")\n    return rctx.repo_metadata(reproducible=True)'
RC['repo_metadata_attrs']='rctx.file("o","m")\n    return rctx.repo_metadata(attrs_for_reproducibility={"s":"x"})'
RC['return_dict']='rctx.file("o","d")\n    return {"s":"x"}'
RC['return_other']='return 3'
RC['fail']='fail("explicit failure")'
RC['fail_attr']='fail("bad", rctx.attr.s)'
RC['print']='print("hello from rule")\n    rctx.file("o","p")'
RC['dir_rctx']='rctx.file("o",str(dir(rctx)))'
RC['str_rctx']='rctx.file("o",str(rctx))'
RC['str_path']='rctx.file("o",str(rctx.path("x"))+repr(rctx.path("x")))'
RC['label_in_rule']='rctx.file("o",str(Label("//:in.txt"))+str(Label("@ma//:x")))'
RC['read_dir']='rctx.read(".")'
RC['read_watch']='rctx.read("in.txt",watch="no")\n    rctx.file("o","1")'
RC['file_on_dir']='rctx.file("d/a","1")\n    rctx.file("d","x")'
RC['file_escape']='rctx.file("../out","1")'
RC['file_abs']='rctx.file("/tmp/zbh7_abs_x","1")'
RC['symlink_escape']='rctx.symlink("a","../b")'
RC['mkdir_via_exec']='rctx.execute(["mkdir","-p","a/b"])\n    rctx.file("a/b/f","1")\n    rctx.file("o",str(rctx.path("a/b").readdir()))'
RC['path_hash']='rctx.file("o",str(hash(rctx.path("x")) == hash(rctx.path("x"))))'
RC['path_eq']='rctx.file("o",str(rctx.path("x")==rctx.path("x"))+str(rctx.path("x")<rctx.path("y")))'
RC['path_concat']='rctx.file("o",str(rctx.path("x").get_child("a")))'
RC['path_realpath']='rctx.file("t","1")\n    rctx.symlink("t","l")\n    rctx.file("o",str(rctx.path("l").realpath))'
RC['workspace_name']='rctx.file("o",rctx.workspace_root.basename)'
RC['unknown_attr_access']='rctx.file("o",str(rctx.attr.nope))'
RC['execute_stdin']='r=rctx.execute(["cat"])\n    rctx.file("o",r.stdout)'
for k,body in RC.items():
    rule='def _impl(rctx):\n    rctx.file("BUILD.bazel","exports_files([\'o\',\'o.txt\'])")\n    '+body+'\n    return None\nr=repository_rule(_impl,attrs={"s":attr.string(default="sv"),"l":attr.string_list(default=["a"]),"d":attr.string_dict(default={"k":"v"}),"i":attr.int(default=3),"b":attr.bool(default=True)},environ=["RCV"],local=False)\n'
    if 'return' in body.split('\n')[-1]: rule=rule.replace('\n    return None','')
    files={'MODULE.bazel':'module(name="root",version="1.0")\nbazel_dep(name="ma",version="1.0")\nr=use_repo_rule("//:r.bzl","r")\nr(name="t")\n','r.bzl':rule,'BUILD.bazel':'exports_files(["in.txt","tpl.txt","p.patch"])','in.txt':'IN\n','tpl.txt':'a=%A% b=%B%\n','p.patch':'--- a.txt\n+++ a.txt\n@@ -1 +1 @@\n-hello\n+HELLO\n','sub/target':'T'}
    cases.append(dict(name='v7_rctx_'+k,args=['fetch','--repo=@t'],files=files,flags=[],cmp=['bazel-root/external/'] if False else [],env={'RCV':'envv'},tree=['external/+r+t']))
# repeat several with fetch + dbg
for k in ['fail','print','download_404','execute_nobin','da_badprefix']:
    pass
# ---------- native vs starlark repo rules w/ all attrs
HTTP='http://127.0.0.1:18877/'
NAT={}
NAT['hf_all']='http_file(name="t",url="%shello.txt",sha256="5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03",executable=True,downloaded_file_path="dir/h.txt",canonical_id="cid",integrity="",netrc="",auth_patterns={})'%HTTP
NAT['hf_urls']='http_file(name="t",urls=["%snonexist","%shello.txt"])'%(HTTP,HTTP)
NAT['hf_both']='http_file(name="t",url="%shello.txt",urls=["%shello.txt"])'%(HTTP,HTTP)
NAT['hf_none']='http_file(name="t")'
NAT['hf_badsha']='http_file(name="t",url="%shello.txt",sha256="abc")'%HTTP
NAT['hf_integrity']='http_file(name="t",url="%shello.txt",integrity="sha256-WJG1tSLV3whtD/CxEPvZ0hu0/HFjrzTQgoai6Eb2vgM=")'%HTTP
NAT['hf_hdrs']='http_file(name="t",url="%shello.txt",downloaded_file_path="a/b/c")'%HTTP
NAT['hf_badpath']='http_file(name="t",url="%shello.txt",downloaded_file_path="../x")'%HTTP
NAT['hj']='http_jar(name="t",url="%shello.txt")'%HTTP
NAT['hj_sha']='http_jar(name="t",url="%shello.txt",sha256="5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03",integrity="")'%HTTP
NAT['ha_all']='http_archive(name="t",url="%ssmall.tar.gz",strip_prefix="top",type="tar.gz",build_file_content="exports_files(glob([\'**\']))",add_prefix="pre",canonical_id="x",remote_file_urls={},remote_file_integrity={},remote_patches={},remote_patch_strip=0,patch_args=["-p1"],patch_cmds=["echo hi > cmd.txt"],patch_cmds_win=[],patch_tool="",patches=[],workspace_file_content="")'%HTTP
NAT['ha_bf']='http_archive(name="t",url="%ssmall.zip",strip_prefix="top",build_file="//:bf.BUILD")'%HTTP
NAT['ha_bf_both']='http_archive(name="t",url="%ssmall.zip",build_file="//:bf.BUILD",build_file_content="x")'%HTTP
NAT['ha_patch']='http_archive(name="t",url="%ssmall.tar.gz",strip_prefix="top",patches=["//:p2.patch"],patch_args=["-p0"],build_file_content="")'%HTTP
NAT['ha_patchtool']='http_archive(name="t",url="%ssmall.tar.gz",strip_prefix="top",patches=["//:p2.patch"],patch_tool="patch",patch_args=["-p0"],build_file_content="")'%HTTP
NAT['ha_files']='http_archive(name="t",url="%ssmall.tar.gz",strip_prefix="top",files={"BUILD.bazel":"//:bf.BUILD"})'%HTTP
NAT['ha_nourl']='http_archive(name="t")'
NAT['ha_badsha']='http_archive(name="t",url="%ssmall.tar.gz",sha256="%s")'%(HTTP,'1'*64)
NAT['ha_rename']='http_archive(name="t",url="%ssmall.tar.gz",strip_prefix="top",rename_files={"a.txt":"AAA.txt"},build_file_content="")'%HTTP
NAT['ha_remote_patches']='http_archive(name="t",url="%ssmall.tar.gz",strip_prefix="top",remote_patches={"%sok.patch":""},remote_patch_strip=0,build_file_content="")'%(HTTP,HTTP)
NAT['ha_workspace']='http_archive(name="t",url="%ssmall.tar.gz",strip_prefix="top",workspace_file="//:ws.txt",build_file_content="")'%HTTP
NAT['ha_netrc']='http_archive(name="t",url="%ssmall.tar.gz",strip_prefix="top",netrc="/nonexistent/netrc",build_file_content="")'%HTTP
NAT['ha_authpat']='http_archive(name="t",url="%sauth",auth_patterns={"127.0.0.1:18877":"Bearer <password>"})'%HTTP
NAT['ha_mirror']='http_archive(name="t",urls=["%snonexist","%ssmall.tar.gz"],strip_prefix="top",build_file_content="")'%(HTTP,HTTP)
NAT['ha_stripdot']='http_archive(name="t",url="%ssmall.tar.gz",strip_prefix="./top",build_file_content="")'%HTTP
NAT['ha_type_zip_bad']='http_archive(name="t",url="%ssmall.tar.gz",type="zip",build_file_content="")'%HTTP
NAT['ha_7z']='http_archive(name="t",url="%ssmall.7z",strip_prefix="top",build_file_content="")'%HTTP
NAT['lr']='local_repository(name="t",path="lrdir")'
NAT['lr_missing']='local_repository(name="t",path="nope")'
NAT['lr_abs']='local_repository(name="t",path="/tmp")'
NAT['nlr']='new_local_repository(name="t",path="lrdir",build_file_content="exports_files(glob([\'*\']))")'
NAT['nlr_bf']='new_local_repository(name="t",path="lrdir",build_file="//:bf.BUILD")'
NAT['nlr_none']='new_local_repository(name="t",path="lrdir")'
NAT['nlr_both']='new_local_repository(name="t",path="lrdir",build_file="//:bf.BUILD",build_file_content="x")'
NAT['nlr_file']='new_local_repository(name="t",path="lrdir/a.txt",build_file_content="")'
NAT['nlr_ws']='new_local_repository(name="t",path="lrdir",build_file_content="",workspace_file_content="workspace(name=\'zz\')")'
NAT['unk_attr']='http_file(name="t",url="%shello.txt",bogus=1)'%HTTP
NAT['badtype']='http_file(name="t",url=3)'
NAT['hf_name_kw']='http_file(name="t",url="%shello.txt",sha256=None)'%HTTP
NAT['dupe_name']='http_file(name="t",url="%shello.txt")\nhttp_file(name="t",url="%shello.txt")'%(HTTP,HTTP)
NAT['badname']='http_file(name="a b",url="%shello.txt")'%HTTP
NAT['emptyname']='http_file(name="",url="%shello.txt")'%HTTP
for k,call in NAT.items():
    mod='module(name="root",version="1.0")\nhttp_archive=use_repo_rule("@bazel_tools//tools/build_defs/repo:http.bzl","http_archive")\nhttp_file=use_repo_rule("@bazel_tools//tools/build_defs/repo:http.bzl","http_file")\nhttp_jar=use_repo_rule("@bazel_tools//tools/build_defs/repo:http.bzl","http_jar")\nlocal_repository=use_repo_rule("@bazel_tools//tools/build_defs/repo:local.bzl","local_repository")\nnew_local_repository=use_repo_rule("@bazel_tools//tools/build_defs/repo:local.bzl","new_local_repository")\n'+call+'\n'
    files={'MODULE.bazel':mod,'BUILD.bazel':'exports_files(["bf.BUILD","p2.patch","ws.txt"])','bf.BUILD':'exports_files(glob(["**"]))\n','p2.patch':'--- a.txt\n+++ a.txt\n@@ -1 +1 @@\n-hello\n+HELLO\n','ws.txt':'workspace(name="w")\n','lrdir/a.txt':'a\n','lrdir/sub/b.txt':'b\n'}
    cases.append(dict(name='v7_nat_'+k,args=['fetch','--repo=@t'],files=files,flags=[],tree=['external/+*+t']))
# ---------- fetch/vendor/lockfile_mode variants
RB='def _i(rctx):\n    rctx.file("BUILD.bazel","exports_files([\'v.txt\'])")\n    rctx.file("v.txt",rctx.attr.a)\nu=repository_rule(_i,attrs={"a":attr.string()})\n'
base={'MODULE.bazel':'module(name="root",version="1.0")\nbazel_dep(name="ma",version="1.0")\nu=use_repo_rule("//:r.bzl","u")\nu(name="v1",a="x")\nu(name="v2",a="y")\n','r.bzl':RB,'BUILD.bazel':'filegroup(name="all",srcs=["@v1//:v.txt","@v2//:v.txt"])'}
FV=[(['fetch','--all'],'all'),(['fetch','--repo=@v1'],'repo1'),(['fetch','--repo=@v1','--repo=@v2'],'repo12'),(['fetch','--repo=@@v1'],'repo_can'),(['fetch','--repo=@nonexist'],'repo_none'),(['fetch','--repo=@ma'],'repo_ma'),
 (['fetch','//:all'],'target'),(['fetch','//:nonexist'],'target_none'),(['fetch','--configure'],'configure'),(['fetch','--all','--force'],'force'),(['fetch','--all','--nofetch'],'nofetch'),(['fetch'],'noarg'),(['fetch','--all','//:all'],'allplus'),
 (['fetch','--all','--lockfile_mode=error'],'lm_error'),(['fetch','--all','--lockfile_mode=off'],'lm_off'),(['fetch','--all','--lockfile_mode=refresh'],'lm_refresh'),(['fetch','--all','--lockfile_mode=bogus'],'lm_bogus'),
 (['vendor','--vendor_dir=vd','--repo=@v1'],'vend1'),(['vendor','--vendor_dir=vd','//:all'],'vend_t'),(['vendor','--vendor_dir=vd','--repo=@v1','--repo=@ma'],'vend_ma'),(['vendor','--repo=@v1'],'vend_nodir'),(['vendor','--vendor_dir=/tmp/zbh7vd_abs','--repo=@v1'],'vend_abs'),(['vendor','--vendor_dir=vd'],'vend_noarg'),(['vendor','--vendor_dir=vd','--repo=@nonexist'],'vend_none'),
 (['build','//:all','--vendor_dir=vd'],'build_vd'),(['sync'],'sync'),(['sync','--only=v1'],'sync_only'),(['build','//:all','--nofetch'],'build_nofetch'),(['build','//:all','--override_repository=v1=/tmp'],'ovr_repo'),(['build','//:all','--override_repository=v1=relative'],'ovr_rel'),(['build','//:all','--override_repository=nope'],'ovr_bad'),
 (['build','//:all','--registry=file:///nonexistent'],'badreg'),(['build','//:all','--ignore_dev_dependency'],'ign_dev'),(['build','//:all','--check_direct_dependencies=error'],'cdd_err'),(['build','//:all','--check_direct_dependencies=warning'],'cdd_warn'),(['build','//:all','--check_bazel_compatibility=off'],'cbc_off'),(['build','//:all','--allow_yanked_versions=ma@1.0'],'ay'),(['build','//:all','--module_mirrors=http://x'],'mm'),(['build','//:all','--repo_env=FOO=bar'],'renv'),
 (['build','//:all','--experimental_repository_cache_hardlinks'],'hl'),(['build','//:all','--repository_disable_download'],'nodl'),(['build','//:all','--experimental_repository_downloader_retries=2'],'dlr'),(['build','//:all','--experimental_downloader_config=none'],'dlc'),(['build','//:all','--distdir=/nonexist'],'distdir'),
 (['build','//:all','--experimental_repository_resolved_file=resolved.bzl'],'resolved'),(['build','//:all','--experimental_repository_cache_urls_as_default_canonical_id'],'cid'),
 (['query','@v1//:all'],'q_v1'),(['query','@v1//:v.txt','--output=build'],'q_build'),(['cquery','@v1//:v.txt'],'cq'),(['query','@nonexist//:all'],'q_none'),(['query','deps(//:all)','--output=label'],'q_deps'),
 (['info','output_base'],'info_ob'),(['mod','deps','--lockfile_mode=update'],'moddeps'),(['clean','--expunge'],'cleanexp'),
]
for a,t in FV:
    pre=[]
    if t in('build_vd','sync','sync_only'): pre=[['vendor','--vendor_dir=vd','--repo=@v1']]
    cases.append(dict(name='v7_fv_'+t,args=a,files=base,flags=[],pre=pre,jcmp=['MODULE.bazel.lock'],tree=['external/+u+v1'],cmp=['vd'] if 'vd' in ' '.join(a) else []))
# ---------- module file errors
ME={}
ME['syntax']='module(name="root"'
ME['dupmodule']='module(name="root")\nmodule(name="root")'
ME['late_module']='bazel_dep(name="ma",version="1.0")\nmodule(name="root")'
ME['badver']='module(name="root",version="1.a.b!")'
ME['badname']='module(name="Root!")'
ME['dupdep']='bazel_dep(name="ma",version="1.0")\nbazel_dep(name="ma",version="1.0")'
ME['dupreponame']='bazel_dep(name="ma",version="1.0",repo_name="x")\nbazel_dep(name="mb",version="1.0",repo_name="x")'
ME['nonexist_mod']='bazel_dep(name="zzzz",version="1.0")'
ME['nonexist_ver']='bazel_dep(name="ma",version="9.9")'
ME['override_nodep']='single_version_override(module_name="ma",version="1.0")'
ME['dup_override']='bazel_dep(name="ma",version="1.0")\nsingle_version_override(module_name="ma",version="1.0")\nlocal_path_override(module_name="ma",path=".")'
ME['mvo_one']='bazel_dep(name="ma",version="1.0")\nmultiple_version_override(module_name="ma",versions=["1.0"])'
ME['git_override_bad']='bazel_dep(name="ma",version="1.0")\ngit_override(module_name="ma",remote="file:///nonexist",commit="abc")'
ME['archive_override_bad']='bazel_dep(name="ma",version="1.0")\narchive_override(module_name="ma",urls=["http://127.0.0.1:18877/nonexist"])'
ME['use_ext_nolabel']='e=use_extension("nolabel","x")'
ME['use_repo_nouse']='use_repo(1,"x")'
ME['use_repo_dupe']='e=use_extension("//:e.bzl","x")\nuse_repo(e,"a","a")'
ME['register_toolchains_bad']='register_toolchains("not a label")'
ME['register_exec_bad']='register_execution_platforms("//:p","bad:::label")'
ME['register_ok']='register_toolchains("//:t","@ma//:t")\nregister_execution_platforms("//:p")'
ME['unknown_global']='foo()'
ME['load_stmt']='load("//:x.bzl","y")'
ME['native']='native.foo()'
ME['bad_compat']='module(name="root",compatibility_level="a")'
ME['bazel_compat']='module(name="root",bazel_compatibility=[">=100.0.0"])'
ME['bazel_compat_bad']='module(name="root",bazel_compatibility=["~1"])'
ME['bazel_compat_ok']='module(name="root",bazel_compatibility=[">=0.0.1","<100"])'
ME['repo_name_bad']='module(name="root",repo_name="a b")'
ME['module_kw']='module(foo=1)'
ME['dev_dep_only']='bazel_dep(name="ma",version="1.0",dev_dependency=True)'
ME['inc_missing']='include("//:nonexist.MODULE.bazel")'
ME['inc_nested']='include("//:i1.MODULE.bazel")'
ME['inc_cycle']='include("//:c.MODULE.bazel")'
ME['inc_bad_label']='include("@ma//:x")'
ME['inc_modcall']='include("//:i2.MODULE.bazel")'
ME['flag_alias_bad']='flag_alias(name="x",starlark_flag="notalabel")'
ME['flag_alias_ok']='flag_alias(name="x",starlark_flag="//:f")'
ME['flag_alias_dup']='flag_alias(name="x",starlark_flag="//:f")\nflag_alias(name="x",starlark_flag="//:g")'
ME['repo_rule_nonlabel']='r=use_repo_rule("nolabel","r")'
ME['inject']='inject_repo(1,"a")'
ME['override_repo_bad']='override_repo(1,"a")'
ME['print_in_module']='print("hi from MODULE")'
ME['fail_in_module']='fail("module fail")'
ME['str_ops']='x=[1,2,3]\nx.append(4)\nprint(x)\nmodule(name="root")'
ME['for_loop']='for i in range(2):\n    bazel_dep(name="m"+"abc"[i],version="1.0")'
ME['unicode_name']='module(name="rööt")'
ME['ver_leading0']='module(name="root",version="01.0")'
ME['ver_prerelease']='module(name="root",version="1.0-rc1+build.5")'
ME['ver_empty_ident']='module(name="root",version="1..0")'
ME['dep_ver_empty']='bazel_dep(name="ma",version="")'
ME['dep_nover_noov']='bazel_dep(name="ma")'
ME['dep_nover_ov']='bazel_dep(name="ma")\nlocal_path_override(module_name="ma",path="/tmp")'
ME['max_compat']='bazel_dep(name="ma",version="1.0",max_compatibility_level=2)'
ME['self_dep']='bazel_dep(name="root",version="1.0")'
ME['nodep_deps']='bazel_dep(name="ma",version="1.0")\nuse_extension("@ma//:e.bzl","x")'
ME['ext_bad_kw']='e=use_extension("//:e.bzl","x",bogus=1)'
ME['tag_pos']='e=use_extension("//:e.bzl","x")\ne.tag(1)'
ME['ext_dev_isolate']='e=use_extension("//:e.bzl","x",dev_dependency=True,isolate=True)'
ME['tag_call_bad_ext']='e=use_extension("//:nonexist.bzl","x")\ne.tag(name="a")\nuse_repo(e,"a")'
for k,body in ME.items():
    mf=body if body.startswith(('module','e=')) or 'module(' in body else 'module(name="root",version="1.0")\n'+body
    files={'MODULE.bazel':mf,'BUILD.bazel':'','i1.MODULE.bazel':'include("//:i3.MODULE.bazel")','i3.MODULE.bazel':'bazel_dep(name="mb",version="1.0")','c.MODULE.bazel':'include("//:c.MODULE.bazel")','i2.MODULE.bazel':'module(name="x")','e.bzl':'x=module_extension(lambda c: None)'}
    cases.append(dict(name='v7_me_'+k,args=['mod','graph'] if int(zlib.crc32(k.encode()))%2 else ['build','//:all'],files=files,flags=[]))
print(len(cases))
if __name__=='__main__': main(cases,jobs=2)
