#!/usr/bin/env python3
"""New (non-verifier) cquery/aquery commands -> new_cmds.tsv (ids n<N>)."""
import itertools
ex=['//a:lib','//a:gg','//b:t1','//a:sel','deps(//a:cl,1)','rdeps(//...,//a:c.h)','//a:lib2 + //a:tr',
    "kind(filegroup,//a:all)","attr(tags,fast,//a:all)","filter('lib',deps(//a:lib2))","somepath(//b:t1,//c:cdata)",
    '@ext//lib:gen','allpaths(//a:tr,//a:x.txt)','//a:sm_cat','//:top_alias','//a:lm','labels(srcs,//a:lib2)','//b:all']
cq=['','--output=label_kind','--output=files','--output=build','--output=location','--output=transitions --transitions=full',
    '--noimplicit_deps','--notool_deps','-c opt --output=label','--cpu=k8 --output=label',
    '--define=A=1 --output=transitions --transitions=lite','--platforms=@bazel_tools//tools:host_platform --output=label',
    '--output=starlark --starlark:expr="target.label.name"','--show_config_fragments=transitive','--output=jsonproto','--output=summary',
    '--output=graph','--output=textproto']
aq=['','--output=jsonproto','--output=textproto','--output=commands','--output=summary','--noinclude_commandline','--noinclude_artifacts',
    '-c opt','--noimplicit_deps','--include_param_files','--include_file_write_contents','--features=foo']
rows=[]
n=0
for e in ex:
    for f in cq:
        n+=1; rows.append(f"n{n}\tcquery '{e}' {f} --keep_going".replace("'","'",1))
for e in ex[:14]:
    for f in aq:
        n+=1; rows.append(f"n{n}\taquery '{e}' {f} --keep_going")
# config() / universe / aspects / misc
misc=["cquery 'config(//a:lib,target)' --output=label --keep_going","cquery 'config(//a:lib,null)' --keep_going",
 "cquery '//a:lib' --output=starlark --starlark:file=/tmp/cqh/fmt.star --keep_going","cquery '//a:lib' --output=starlark --starlark:expr='str(providers(target).keys())' --keep_going",
 "cquery '//a:all' --universe_scope=//a:lib --keep_going","cquery '//a:all' --infer_universe_scope --keep_going",
 "cquery '//a:lib' --aspects=//:defs.bzl%asp --output=label --keep_going","cquery '//a:lib' --aspects=//:defs.bzl%asp --include_aspects --output=label --keep_going",
 "cquery '//a:lib' --noinclude_aspects --output=label --keep_going","cquery '//a:lib' --output=transitions --transitions=none --keep_going",
 "cquery '//a:lib' --output=label --order_output=full --keep_going","cquery 'deps(//a:lib2)' --output=label --order_output=full --keep_going",
 "aquery 'inputs(\".*x.txt\",//a:lib)' --keep_going","aquery 'outputs(\".*lib.txt\",//a:lib)' --keep_going","aquery 'mnemonic(\"CatIt\",//a:all)' --keep_going",
 "aquery '//a:lib' --aspects=//:defs.bzl%asp --keep_going","aquery '//a:lib' --universe_scope=//a:lib --keep_going"]
for m in misc:
    n+=1; rows.append(f"n{n}\t{m}")
open('new_cmds.tsv','w').write("\n".join(rows)+"\n")
print(len(rows))
