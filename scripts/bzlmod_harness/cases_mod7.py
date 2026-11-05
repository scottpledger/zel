from hconf import VBZ
from harness import *
# Round-6 verifier's 6-graph x 130-invocation `mod` matrix (copied; paths made relocatable).
def mm(b,n='root',v='1.0'): return f'module(name="{n}",version="{v}")\n'+b
N1=mm('bazel_dep(name="ma",version="1.0")\nbazel_dep(name="mh",version="2.0")\nbazel_dep(name="mu",version="1.0")\nbazel_dep(name="mi",version="1.0")\nbazel_dep(name="mj",version="1.0",dev_dependency=True)\n')
N2=mm('bazel_dep(name="mf",version="1.0")\nbazel_dep(name="me",version="1.5",repo_name="eee")\nbazel_dep(name="md",version="1.0",repo_name=None)\n')
N3=mm('bazel_dep(name="mf",version="1.0")\nbazel_dep(name="mh",version="1.0")\nbazel_dep(name="mg",version="1.0")\nmultiple_version_override(module_name="me",versions=["1.0","2.0"])\n')
N4=mm('bazel_dep(name="mx",version="2.0")\nbazel_dep(name="mu",version="1.0")\ne=use_extension("@mx//:ext.bzl","ex")\ne.mk(name="n4a",opt=["1"])\ne.mk(name="n4b",opt=[])\nuse_repo(e,"n4a","n4b")\ni=use_extension("@mx//:ext.bzl","ex",isolate=True)\ni.mk(name="iso",opt=["z"])\nuse_repo(i,"iso")\nd=use_extension("@mx//:ext.bzl","ex2",dev_dependency=True)\nuse_repo(d,x="fixed")\n')
N5=mm('bazel_dep(name="md",version="1.0")\nbazel_dep(name="ma",version="1.0")\nsingle_version_override(module_name="md",version="1.1")\nlocal_path_override(module_name="mc",path="'+VBZ+'/mods/mc+1.0")\n')
N6=mm('bazel_dep(name="cya",version="1.0")\nbazel_dep(name="ma",version="1.0")\n')
G={'n1':N1,'n2':N2,'n3':N3,'n4':N4,'n5':N5,'n6':N6}
subs=[]
for s in ['graph','deps']:
    for a in [[],['--depth=1'],['--depth=2'],['--output=json'],['--output=graph'],['--charset=ascii'],['--verbose'],['--include_unused'],['--cycles'],['--include_builtin'],['--base_module=ma'],['--base_module=<root>'],['--base_module=md@1.0'],['--from=ma'],['--extension_info=all'],['--extension_info=usages'],['--extension_info=repos'],['--extension_info=all','--extension_filter=@mx//:ext.bzl%ex'],['--extension_filter=nonexist'],['--output=json','--verbose','--include_unused'],['--output=json','--extension_info=all'],['--output=graph','--verbose'],['--curses=no','--color=no'],['md'],['ma','mh'],['ma@1.0'],['<root>'],['nope']]:
        subs.append((s,a))
for a in [['md'],['me'],['md','me'],['ma','md'],['--verbose','md'],['--include_unused','md'],['--output=json','md'],['mj'],['nope'],['--from=ma','md'],[]]:
    subs.append(('explain',a))
    subs.append(('path',a)); subs.append(('all_paths',a))
for a in [['md'],['@md'],['@@md+'],['mx'],['--all_repos'],['--all_visible_repos'],['--output=streamed_proto','md'],['--output=text','md'],['--output=json','md'],['--output=streamed_jsonproto','md'],['ma','mb'],['@r1'],['@@mx++ex+n4a'],['mu'],['@bazel_tools'],['<root>'],['']]:
    subs.append(('show_repo',a))
for a in [['@mx//:ext.bzl%ex'],['@mx//:ext.bzl%ex','@mx//:ext.bzl%ex2'],['--base_module=mu','@mx//:ext.bzl%ex'],['nope'],['@mx//:ext.bzl%ex','--verbose'],[],['@@mx+//:ext.bzl%ex'],['--extension_filter=x']]:
    subs.append(('show_extension',a))
for a in [[],['ma'],['mx'],['--base_module=mu','mx'],['nope'],['@mx+']]:
    subs.append(('dump_repo_mapping',a))
for a in [['--help'],[],['nosuch'],['graph','--depth=x'],['graph','--output=bad'],['graph','--charset=bad'],['deps','--from=nope'],['explain','--depth=1','md'],['show_repo','--output=bad','md'],['--bogus']]:
    subs.append((a[0] if a and not a[0].startswith('--') else 'x',a))
cases=[]
for gn,mf in G.items():
    for i,(s,a) in enumerate(subs):
        if s=='x': args=['mod']+a
        else: args=['mod',s]+a if not (s in ('graph','deps','explain','path','all_paths','show_repo','show_extension','dump_repo_mapping')) else ['mod',s]+a
        if s=='x' and a and not a[0].startswith('-') and a[0] in ('graph',): args=['mod']+a
        cases.append(dict(name=f'{gn}_{i}',args=args,files={'MODULE.bazel':mf},flags=['--allow_yanked_versions=md@1.2'],ignore_stdout=False))
if __name__=='__main__':
    print(len(cases)); main(cases,jobs=6)
