#!/usr/bin/env python3
"""Round 6C: >=300 new aquery/cquery commands (ids m<N>) -> new6c_cmds.tsv; exercises executables/tests/genrules/keys."""
ex=['//b:t1','//b:t2','//b:tool','//a:gg','//a:lib','//a:lib2','//b:all','deps(//b:t1)','deps(//a:gg)','rdeps(//...,//a:x.txt)',
    'kind(genrule,//a:all)','//a:cl','//a:sel','//a:tr','@ext//lib:gen','//:top_alias','deps(//b:tool,1)','//a:all']
aq=['','--output=text','--output=jsonproto','--output=textproto','--output=proto','--output=summary','--output=commands',
    '--noinclude_commandline','--noinclude_artifacts','--include_param_files','--include_file_write_contents','-c opt',
    '--nostamp','--stamp','--features=foo','--action_env=FOO=bar','--host_action_env=X=1','--noinclude_aspects','--cpu=k8',
    '--platforms=@bazel_tools//tools:host_platform','--define=A=1','--copt=-O2','--noimplicit_deps','--notool_deps']
cq=['--output=label','--output=label_kind','--output=files','--output=proto','--output=jsonproto','--output=textproto','--output=summary',
    '--output=starlark --starlark:expr="target.label"','--output=starlark --starlark:expr="providers(target).keys()"',
    '--output=starlark --starlark:expr="[f.path for f in target[DefaultInfo].files.to_list()]"',
    '--output=transitions --transitions=lite','--output=transitions --transitions=full','--noimplicit_deps --output=label','-c dbg --output=label']
rows=[];n=0
for e in ex:
    for f in aq:
        n+=1; rows.append(f"m{n}\taquery '{e}' {f} --keep_going")
for e in ex[:10]:
    for f in cq:
        n+=1; rows.append(f"m{n}\tcquery '{e}' {f} --keep_going")
misc=["aquery 'mnemonic(\"TestRunner\",//b:all)' --keep_going","aquery 'mnemonic(\"SymlinkTree\",//b:all)' --keep_going",
 "aquery 'mnemonic(\"RunfilesTree\",//b:all)' --keep_going","aquery 'mnemonic(\"RepoMappingManifest\",//b:all)' --keep_going",
 "aquery 'outputs(\".*runfiles.*\",//b:t1)' --keep_going","aquery 'inputs(\".*test-setup.*\",//b:t1)' --keep_going",
 "aquery '//:all_tests' --keep_going","aquery '//:tagged' --keep_going","aquery 'tests(//:all_tests)' --keep_going",
 "aquery '//...' --output=jsonproto --keep_going","aquery '//...' --output=textproto --keep_going","aquery '//...' --output=proto --keep_going",
 "aquery '//b:t1' --output=streamed_proto --keep_going","aquery '//a:gg' --output=streamed_proto --keep_going"]
for m in misc:
    n+=1; rows.append(f"m{n}\t{m}")
open('new6c_cmds.tsv','w').write("\n".join(rows)+"\n"); print(len(rows))
