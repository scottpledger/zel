from hconf import VBZ
from harness import *
import json
def mm(body,name='root',ver='1.0'): return f'module(name="{name}",version="{ver}")\n'+body
G1=mm('bazel_dep(name="ma",version="1.0")\nbazel_dep(name="mi",version="1.0")\nbazel_dep(name="mj",version="1.0",dev_dependency=True)\nbazel_dep(name="mu",version="1.0",repo_name="muu")\nbazel_dep(name="mx",version="1.0")\n'
 'e=use_extension("@mx//:ext.bzl","ex")\ne.mk(name="r1",opt=["a","b"])\nuse_repo(e,"r1","from_mu" if False else "r1")\n'
 'd=use_extension("@mx//:ext.bzl","ex",dev_dependency=True)\nd.mk(name="devr")\nuse_repo(d,"devr")\n'
 'e2=use_extension("@mx//:ext.bzl","ex2")\nuse_repo(e2,my_fixed="fixed")\n')
G1=G1.replace('use_repo(e,"r1","from_mu" if False else "r1")','use_repo(e,"r1")')
G_COMPAT=mm('bazel_dep(name="mf",version="1.0")\nbazel_dep(name="me",version="1.5")\n')
G_COMPAT_BAD=mm('bazel_dep(name="mf",version="1.0")\nbazel_dep(name="mg",version="1.0")\n')
G_MVO=mm('bazel_dep(name="mf",version="1.0")\nbazel_dep(name="mg",version="1.0")\nmultiple_version_override(module_name="me",versions=["1.0","2.0"])\n')
G_MVO2=mm('bazel_dep(name="mh",version="1.0")\nbazel_dep(name="mh",version="2.0",repo_name="mh2")\nmultiple_version_override(module_name="mh",versions=["1.0","2.0"])\n')
G_YANK=mm('bazel_dep(name="md",version="1.2")\n')
G_CYC=mm('bazel_dep(name="cya",version="1.0")\n')
G_SVO=mm('bazel_dep(name="ma",version="1.0")\nsingle_version_override(module_name="md",version="1.2")\n')
G_SVO2=mm('bazel_dep(name="ma",version="1.0")\nsingle_version_override(module_name="md",version="1.0")\n')
G_SVO3=mm('bazel_dep(name="ma",version="1.0")\nsingle_version_override(module_name="mc",version="1.0",registry="file://'+VBZ+'/reg")\n')
G_LPO=mm('bazel_dep(name="ma",version="1.0")\nlocal_path_override(module_name="md",path="'+VBZ+'/mods/md+1.0")\n')
G_NV=mm('bazel_dep(name="md",version="1.0")\nbazel_dep(name="ov",version="1.0")\nlocal_path_override(module_name="ov",path="./lpo")\n')
G_NODEV=mm('bazel_dep(name="mj",version="1.0",dev_dependency=True)\nbazel_dep(name="md",version="1.0")\n')
G_ARCH=mm('bazel_dep(name="ar_tar_gz",version="1.0")\nbazel_dep(name="ar_zip",version="1.0")\nbazel_dep(name="ar_patch",version="1.0")\nbazel_dep(name="ar_overlay",version="1.0")\nbazel_dep(name="ar_7z",version="1.0")\nbazel_dep(name="ar_tar_xz",version="1.0")\nbazel_dep(name="ar_tar_zst",version="1.0")\nbazel_dep(name="ar_tar_bz2",version="1.0")\nbazel_dep(name="ar_nostrip",version="1.0")\n')
G_BC=mm('bazel_dep(name="bc1",version="1.0")\nbazel_dep(name="bc3",version="1.0")\n')
G_BC2=mm('bazel_dep(name="bc2",version="1.0")\n')
G_FLAG=mm('flag_alias(name="myflag",starlark_flag="//:flag")\nbazel_dep(name="md",version="1.0")\n')
G_INC=mm('include("//:more.MODULE.bazel")\n')
INC_FILES={'more.MODULE.bazel':'bazel_dep(name="md",version="1.1")\nbazel_dep(name="mb",version="1.0")\n'}
G_REPONAMES=mm('bazel_dep(name="md",version="1.0",repo_name="mydd")\nbazel_dep(name="mj",version="1.0",repo_name="jj",dev_dependency=True)\nbazel_dep(name="mi",version="1.0")\n')
G_NOREPO=mm('bazel_dep(name="md",version="1.0",repo_name=None)\nbazel_dep(name="mb",version="1.0")\n')
cases=[]
def C(name,args,mf,files=None,flags=None,**kw):
    f={'MODULE.bazel':mf}; f.update(files or {})
    cases.append(dict(name=name,args=args,files=f,flags=(['--allow_yanked_versions=md@1.2'] if flags is None else flags),**kw))
roots={'g1':G1,'compat':G_COMPAT,'mvo':G_MVO,'mvo2':G_MVO2,'svo':G_SVO,'lpo':G_LPO,'nodev':G_NODEV,'repon':G_REPONAMES,'norepo':G_NOREPO}
subs=[('graph',[]),('deps',[]),('graph',['--depth=1']),('graph',['--depth=0']),('graph',['--output=json']),('graph',['--output=graph']),('graph',['--charset=ascii']),('graph',['--verbose']),('graph',['--include_unused']),('graph',['--cycles']),('graph',['--extension_info=all']),('graph',['--extension_info=usages','--extension_filter=@mx//:ext.bzl%ex']),('graph',['--base_module=ma']),('graph',['--base_module=<root>']),('graph',['--include_builtin']),('graph',['--nodep_dependencies']),('graph',['--noinclude_unused']),('deps',['md']),('deps',['ma','mi']),('deps',['md@1.1']),('deps',['--depth=2','ma']),('explain',['md']),('explain',['md','--verbose']),('explain',['md','--include_unused']),('path',['md']),('path',['--from=ma','md']),('path',['--from=mb','md']),('all_paths',['md']),('all_paths',['--from=ma','md','me']),('show_repo',['md']),('show_repo',['@md']),('show_repo',['@@md+']),('show_repo',['md@1.1']),('show_repo',['<root>']),('show_repo',['@@']),('show_repo',['--all_repos']),('show_repo',['--all_visible_repos']),('show_repo',['--base_module=ma','mb']),('show_repo',['--output=json','md']),('show_extension',['@mx//:ext.bzl%ex']),('show_extension',['@@mx+//:ext.bzl%ex2']),('show_extension',['--base_module=mu','@mx//:ext.bzl%ex']),('show_extension',['@mx//:ext.bzl%nonexist']),('dump_repo_mapping',['']),('dump_repo_mapping',['ma']),('dump_repo_mapping',['','ma']),('dump_repo_mapping',['nonexist'])]
for rn in ['g1']:
    for i,(s,a) in enumerate(subs):
        C(f'{rn}_{s}_{i}',['mod',s]+a,roots[rn])
# restricted subset over other roots
sub2=[('graph',[]),('graph',['--output=json']),('graph',['--verbose']),('graph',['--include_unused']),('deps',[]),('explain',['me']),('explain',['md']),('path',['me']),('all_paths',['me']),('show_repo',['me']),('show_repo',['--all_repos']),('dump_repo_mapping',[''])]
for rn in ['compat','mvo','svo','lpo','nodev','repon','norepo','mvo2']:
    for i,(s,a) in enumerate(sub2):
        C(f'{rn}_{s}_{i}',['mod',s]+a,roots[rn],files={'lpo/MODULE.bazel':'module(name="ov",version="1.0")\n','lpo/BUILD.bazel':''} if False else None,flags=['--allow_yanked_versions=md@1.2'])
C('compat_bad',['mod','graph'],G_COMPAT_BAD)
C('cycle',['mod','graph'],G_CYC)
C('cycle_deps',['mod','deps'],G_CYC)
C('yank_err',['mod','graph'],G_YANK,flags=[])
C('yank_allow_all',['mod','graph'],G_YANK,flags=['--allow_yanked_versions=all'])
C('yank_env',['mod','graph'],G_YANK,flags=[],env={'BZLMOD_ALLOW_YANKED_VERSIONS':'md@1.2'})
C('yank_bad_fmt',['mod','graph'],G_YANK,flags=['--allow_yanked_versions=md'])
C('yank_graph_verbose',['mod','graph','--verbose'],G_YANK)
C('yank_deps_noyank',['mod','graph'],mm('bazel_dep(name="md",version="1.1")\n'),flags=[])
C('svo_yanked',['mod','graph'],G_SVO,flags=[])
C('svo2',['mod','graph'],G_SVO2)
C('svo3',['mod','graph'],G_SVO3)
C('svo_show',['mod','show_repo','md'],G_SVO)
C('lpo_show',['mod','show_repo','md'],G_LPO)
C('nv_lpo_missing',['mod','graph'],G_NV)
C('nv_lpo_ok',['mod','show_repo','ov'],G_NV,files={'lpo/MODULE.bazel':'module(name="ov",version="1.0")\n','lpo/BUILD.bazel':''})
C('nv_lpo_name_mismatch',['mod','graph'],G_NV,files={'lpo/MODULE.bazel':'module(name="zzz",version="1.0")\n','lpo/BUILD.bazel':''})
C('bc_ok',['mod','graph'],G_BC)
C('bc_bad',['mod','graph'],G_BC2)
C('bc_ignore',['mod','graph'],G_BC2,flags=['--check_bazel_compatibility=off'])
C('bc_error_explicit',['mod','graph'],G_BC2,flags=['--check_bazel_compatibility=error'])
C('bc_warn',['mod','graph'],G_BC2,flags=['--check_bazel_compatibility=warning'])
C('bc_root_bad',['mod','graph'],'module(name="root",version="1.0",bazel_compatibility=[">=99.0.0"])\n')
C('bc_root_bad_fmt',['mod','graph'],'module(name="root",version="1.0",bazel_compatibility=["99"])\n')
C('bc_root_excl',['mod','graph'],'module(name="root",version="1.0",bazel_compatibility=["-9.3.2"])\n')
C('bc_root_excl2',['mod','graph'],mm('bazel_compatibility([">=1.0.0","-9.3.2"])\n'))
C('bc_root_lt',['mod','graph'],'module(name="root",version="1.0",bazel_compatibility=["<1.0.0"])\n')
C('bc_root_ok',['mod','graph'],'module(name="root",version="1.0",bazel_compatibility=[">=1.0.0","<99.0.0"])\n')
C('flag_alias',['mod','graph'],G_FLAG)
C('flag_alias_bad',['mod','graph'],mm('flag_alias(name="x y",starlark_flag="//:f")\n'))
C('flag_alias_bad2',['mod','graph'],mm('flag_alias(name="a",starlark_flag="bad")\n'))
C('flag_alias_dup',['mod','graph'],mm('flag_alias(name="a",starlark_flag="//:f")\nflag_alias(name="a",starlark_flag="//:g")\n'))
C('include_ok',['mod','graph'],G_INC,files=INC_FILES)
C('include_missing',['mod','graph'],G_INC)
C('include_nested',['mod','graph'],mm('include("//:a.MODULE.bazel")\n'),files={'a.MODULE.bazel':'include("//sub:b.MODULE.bazel")\n','sub/b.MODULE.bazel':'bazel_dep(name="md",version="1.0")\n','sub/BUILD':''})
C('include_not_module_suffix',['mod','graph'],mm('include("//:a.bzl")\n'),files={'a.bzl':''})
C('include_label_ext',['mod','graph'],mm('include("@foo//:a.MODULE.bazel")\n'))
C('include_cycle',['mod','graph'],mm('include("//:a.MODULE.bazel")\n'),files={'a.MODULE.bazel':'include("//:a.MODULE.bazel")\n'})
C('include_in_included_module',['mod','graph'],mm('include("//:a.MODULE.bazel")\n'),files={'a.MODULE.bazel':'module(name="x")\n'})
C('include_rel',['mod','graph'],mm('include(":a.MODULE.bazel")\n'),files={'a.MODULE.bazel':'bazel_dep(name="md",version="1.0")\n'})
C('include_pkg_boundary',['mod','graph'],mm('include("//sub:a.MODULE.bazel")\n'),files={'sub/a.MODULE.bazel':'bazel_dep(name="md",version="1.0")\n','sub/BUILD':''})
C('include_nobuild_pkg',['mod','graph'],mm('include("//sub:a.MODULE.bazel")\n'),files={'sub/a.MODULE.bazel':'bazel_dep(name="md",version="1.0")\n'})
C('include_dup',['mod','graph'],mm('include("//:a.MODULE.bazel")\ninclude("//:a.MODULE.bazel")\n'),files={'a.MODULE.bazel':'bazel_dep(name="md",version="1.0")\n'})
# language restrictions
for nm,body in [('if','if True:\n  bazel_dep(name="md",version="1.0")\n'),('for','for i in [1]:\n  bazel_dep(name="md",version="1.0")\n'),('def','def f():\n  pass\n'),('load','load("//:x.bzl","y")\n'),('unknown','foo()\n'),('lambda','x=lambda: 1\n'),('glob','glob(["x"])\n'),('native','native.foo()\n'),('print','print("hello")\n'),('var','x="a"\nbazel_dep(name="md",version="1.0")\n'),('list_comp','x=[i for i in range(3)]\n'),('fail','fail("boom")\n'),('module_twice','module(name="a")\n'),('module_late','bazel_dep(name="md",version="1.0")\nmodule(name="r")\n'),('dep_no_ver','bazel_dep(name="md")\n'),('dep_bad_name','bazel_dep(name="MD!",version="1.0")\n'),('dep_bad_ver','bazel_dep(name="md",version="x.y")\n'),('dep_dup','bazel_dep(name="md",version="1.0")\nbazel_dep(name="md",version="1.1")\n'),('dep_dup_repo','bazel_dep(name="md",version="1.0",repo_name="a")\nbazel_dep(name="mb",version="1.0",repo_name="a")\n'),('dep_self','bazel_dep(name="root",version="1.0")\n'),('dep_pos','bazel_dep("md","1.0")\n'),('dep_maxcompat','bazel_dep(name="md",version="1.0",max_compatibility_level=2)\n'),('dep_neg_compat','bazel_dep(name="md",version="1.0",max_compatibility_level=-5)\n'),('dep_empty_ver','bazel_dep(name="md",version="")\n'),('nodep_dep','bazel_dep(name="md",version="")\n'),('syntax','bazel_dep(name="md",version="1.0"\n'),('syntax2','x = = 1\n'),('undefined','y\n'),('toplevel_ret','return\n'),('dup_override','single_version_override(module_name="md",version="1.0")\nsingle_version_override(module_name="md",version="1.1")\n'),('override_root','single_version_override(module_name="root",version="1.0")\n'),('override_unknown','local_path_override(module_name="zzz",path="x")\n'),('override_nonroot_ok','bazel_dep(name="md",version="1.0")\nlocal_path_override(module_name="md",path="/nonexistent")\n'),('mvo_one','multiple_version_override(module_name="md",versions=["1.0"])\n'),('mvo_dup','multiple_version_override(module_name="md",versions=["1.0","1.0"])\n'),('mvo_mixed','multiple_version_override(module_name="md",versions=["1.0","1.1"])\nsingle_version_override(module_name="md",version="1.0")\n'),('svo_patch_noversion','single_version_override(module_name="md",patches=["//:p"])\n'),('svo_patch_strip','single_version_override(module_name="md",patch_strip=3)\n'),('svo_all_empty','single_version_override(module_name="md")\n'),('use_ext_nonlabel','e=use_extension("foo","bar")\n'),('use_ext_noname','e=use_extension("//:x.bzl","")\n'),('use_repo_unknown_arg','e=use_extension("//:x.bzl","e")\nuse_repo(e,1)\n'),('use_repo_dup','e=use_extension("//:x.bzl","e")\nuse_repo(e,"a","a")\n'),('use_repo_kwargs_dup','e=use_extension("//:x.bzl","e")\nuse_repo(e,"a",a="b")\n'),('use_repo_empty','e=use_extension("//:x.bzl","e")\nuse_repo(e)\n'),('use_repo_rule_ok','r=use_repo_rule("//:x.bzl","r")\nr(name="n")\n'),('use_repo_rule_dup','r=use_repo_rule("//:x.bzl","r")\nr(name="n")\nr(name="n")\n'),('use_repo_rule_noname','r=use_repo_rule("//:x.bzl","r")\nr()\n'),('inject_repo_missing','e=use_extension("//:x.bzl","e")\ninject_repo(e,"x")\n'),('override_repo_x','e=use_extension("//:x.bzl","e")\noverride_repo(e,"x")\n'),('register_tc','register_toolchains("//:a","@x//:b")\nregister_execution_platforms("//:p")\n'),('register_tc_bad','register_toolchains("not a label")\n'),('register_tc_dev','register_toolchains("//:a",dev_dependency=True)\n'),('module_bad_compat','module(name="a",compatibility_level="x")\n'),('module_bad_name','module(name="A b")\n'),('module_empty_name','module(name="")\n'),('module_name_dash','module(name="a-b_c.d")\n'),('module_repo_name','module(name="a",repo_name="b")\n'),('module_bazel_compat','module(name="a",bazel_compatibility=[">=1.0.0"])\n'),('module_pos','module("a","1.0")\n'),('module_version_bad','module(name="a",version="1.0!")\n'),('module_ver_prerelease','module(name="a",version="1.0-rc.1+build")\n'),('module_ver_leadingzero','module(name="a",version="01.0")\n'),('module_none_name','module(version="1.0")\n')]:
    C('lang_'+nm,['mod','graph'],body if nm in('module_twice','module_late','module_none_name') and False else (mm(body) if not nm.startswith('module_') or nm in ('module_twice','module_late') else body+''),flags=['--allow_yanked_versions=md@1.2'] if False else None)
# module-body variants where module() is the thing under test were passed raw
for c in cases:
    if c['name'] in ('lang_module_twice',): c['files']['MODULE.bazel']='module(name="root")\nmodule(name="a")\n'
    if c['name']=='lang_module_late': c['files']['MODULE.bazel']='bazel_dep(name="md",version="1.0")\nmodule(name="r")\n'
if __name__=='__main__':
    print(len(cases)); main(cases,jobs=5)
