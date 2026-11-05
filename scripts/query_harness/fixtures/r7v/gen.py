import itertools
C=[]
def a(c): C.append(c)
Q='query'
# --- query: functions
exprs=[
 "//lib:all","//lib:*","//lib/...","//app/...","//...","//data/...:all","//tools:all","//cfg:all","//pkgs:all",
 "deps(//app:bin)","deps(//app:bin,1)","deps(//app:bin,0)","deps(//app:app_test,2)","deps(//lib:l3)","deps(//app:sel)","deps(//app:al)","deps(//app:tc)",
 "rdeps(//...,//lib:l1)","rdeps(//...,//lib:l1,1)","rdeps(//app/...,//data:d)","rdeps(//lib:l3,//lib:l1)",
 "allrdeps(//lib:l1)","allrdeps(//lib:l1,1)",
 "somepath(//app:bin,//data:d)","somepath(//app:app_test,//lib:l1)","allpaths(//app:app_test,//lib:l1)","allpaths(//app:bin,//data:d)",
 "kind(cat,//...)","kind('^sh_',//...)","kind('rule',//lib:all)","kind('source file',deps(//app:bin))","kind('generated file',//lib:all)","kind(filegroup,//app:all)",
 "filter('l[0-9]$',//lib:all)","filter('^//app',deps(//app:app_test))","filter('\\.txt$',deps(//app:bin))",
 "attr(tags,t2,//...)","attr(tags,'\\[.*t1.*\\]',//lib:all)","attr(k,one,//...)","attr(n,7,//lib:all)","attr(flag,1,//lib:all)","attr(flag,0,//lib:all)","attr(opts,z,//lib:all)","attr(testonly,1,//lib:all)","attr(size,small,//tools:all)","attr(flaky,1,//tools:all)","attr(timeout,long,//tools:all)","attr(m,'x',//lib:all)",
 "visible(//app:bin,//lib:all)","visible(//app:uses_priv,//lib:all)","visible(//lib:l1,//app:all)","visible(//data:d,//...)",
 "labels(srcs,//lib:l2)","labels(srcs,//lib:all)","labels(data,//app:bin)","labels(tool,//lib:l3)","labels(deps,//tools:all)","labels(outs,//lib:gr)","labels(tools,//lib:gr)",
 "tests(//tools:ts)","tests(//tools:ts_fast)","tests(//tools:ts_nested)","tests(//tools:ts_all)","tests(//tools:all)","tests(//...)",
 "buildfiles(//app:bin)","buildfiles(deps(//app:app_test))","buildfiles(//lib:l1)","rbuildfiles(//rules.bzl)","rbuildfiles(//rules.bzl,//BUILD)","rbuildfiles(//lib:BUILD)",
 "loadfiles(//app:bin)","loadfiles(//lib:l1)","loadfiles(deps(//app:td))",
 "siblings(//lib:l1)","siblings(//lib:a.txt)","same_pkg_direct_rdeps(//lib:a.txt)","same_pkg_direct_rdeps(//lib:l1)",
 "some(//lib:all)","some(//nonexist:all)","some(//lib:l1 + //lib:l2)",
 "//lib:l1 + //lib:l2","//lib:all - //lib:l1","//lib:all except //lib:l2","//lib:all intersect //lib:l1 + //lib:l2","//lib:l1 ^ //lib:all","set(//lib:l1 //lib:l2)","set()","//lib:l1 union //lib:l2",
 "let x = //lib:l1 in deps($x)","let x = //lib:l1 in $x + $x","let x = //lib:all in let y = //lib:l2 in $x - $y",
 "//lib:l1+//lib:l2","//lib:l1 -//lib:l2",
 "//lib:a.txt","//lib:gr.out","//lib:gr2.out","//lib:l1.out","//lib:w1_dir","//lib:w1.txt","//lib:grl.out",
 "//lib:mac_l","//lib:mac_l_0","//lib:smac_fg","//lib:smac_cat","//lib:mac_l_2",
 "//lib:smac","//lib:nonexist","//nonexist:foo","//nonexist/...","//lib:","//lib:a.txt:b","@platforms//:incompatible","@platforms//cpu:x86_64","@@platforms//os:linux","@nonexist//:x","@@nonexist//:x","//:everything","//:al","//:al2","deps(//:al2)","//data/sub:inner","//data/sub:d2.txt","//data/sub:x.txt","//data:d2.txt",
 "//deep/...","//deep/x/...","deps(//deep/x:x)","rdeps(//deep/...,//deep/x/y:f)","//deep/x/y:f",
 "//pkgs:gq","//pkgs:gq2","deps(//pkgs:gq)","deps(//pkgs:gq2)","//pkgs:ea","//pkgs:al","deps(//pkgs:al)","//pkgs:envs","//pkgs:env_a","//pkgs:restr","//pkgs:incl","deps(//pkgs:incl)","deps(//pkgs:restr)","deps(//pkgs:envs)",
 "//data:pg","//data:pg2","deps(//data:pg)","labels(packages,//data:pg)","visible(//lib:l1,//data:pg)",
 "//cfg:all","deps(//cfg:lf)","deps(//cfg:strv)","//cfg:lf","//cfg:str","//cfg:num","deps(//app:sel)","deps(//app:sel_deps)","deps(//app:tc_ok)","deps(//app:al)",
 "//broken:all","deps(//broken:dep_bad)","//broken/...","//broken:good","//broken:dup","//broken/syn:s","//broken/loadfail:all","//broken:bad_src","rdeps(//broken/...,//broken:g.txt)",
 "//app:bin //lib:l1","//app:bin + ","(//app:bin","deps(","deps()","deps(//app:bin,x)","deps(//app:bin,-1)","foo(//app:bin)","kind(//app:bin)","kind('(',//app:bin)","filter('[',//lib:all)","attr(a,b)","attr(tags,x,//lib:all,y)","some()","let x = //app:bin","let x = //app:bin in $y","//app:bin in","'//app:bin'","\"//app:bin\"","//app:bin//lib","//app:bin;","deps(//app:bin) +","deps(//app:bin)) ","", " ","//","//...:all","//:all","//lib:all-targets","//lib:all-rules","//lib/...:all-targets","//lib/...:*","//lib/... - //lib:l1","//lib:l1 - //lib/...","@//lib:l1","@@//lib:l1","//lib/","//lib//","//lib:l1/","//app/../lib:l1","/lib:l1","lib:l1","//lib:l1:x","//.:x","//./lib:l1","//lib/.:l1",
 "deps(//app:app_test) intersect //lib:all","deps(//app:app_test) - deps(//app:bin)","deps(//app:bin) ^ deps(//app:app_test)","kind(cat,deps(//app:td))","filter('t$',kind(rule,//lib:all))",
]
fl=[ "--output=label","--output=label_kind","--output=location","--output=package","--output=build","--output=minrank","--output=maxrank","--output=graph","--output=xml","--output=proto","--output=streamed_proto","--output=streamed_jsonproto","--output=jsonproto","--output=textproto"]
# base exprs x output formats
core=["//lib:all","deps(//app:app_test)","//tools:all","//app:sel","//cfg:all","//pkgs:all","//data:all + //data/sub:all","//lib:a.txt + //lib:gr.out","//app:td + //app:sd","//lib:smac + //lib:mac_l_1"]
for e in exprs:
    a("query '%s' --keep_going"%e)
for e in core:
    for f in fl:
        a("query '%s' %s"%(e,f))
for e in ["deps(//app:app_test)","//lib:all","//app:sel_deps"]:
    for f in ["--noimplicit_deps","--nohost_deps","--notool_deps","--noimplicit_deps --nohost_deps --notool_deps","--nonodep_deps","--noimplicit_deps --output=graph","--noimplicit_deps --output=build","--order_output=full","--order_output=no","--order_output=deps","--order_output=auto","--output=label --order_output=full","--relative_locations","--output=location --relative_locations","--output=location --noimplicit_deps","--output=label_kind --order_output=no","--output=graph --nograph:factored","--output=graph --graph:node_limit=3","--output=graph --graph:node_limit=-1","--output=graph --graph:factored","--output=graph --nograph:factored","--output=build --noproto:locations","--output=proto --noproto:locations","--output=proto --proto:output_rule_attrs=srcs,k","--output=proto --proto:output_rule_attrs=","--output=proto --noproto:rule_inputs_and_outputs","--output=proto --noproto:default_values","--output=proto --proto:include_synthetic_attribute_hash","--output=proto --proto:flatten_selects","--output=proto --noproto:flatten_selects","--output=xml --noxml:line_numbers","--output=xml --noxml:default_values","--output=xml --xml:default_values","--output=xml --xml:line_numbers","--output=jsonproto --proto:output_rule_attrs=srcs","--output=proto --consistent_labels","--output=label --consistent_labels","--output=label_kind --consistent_labels","--output=build --consistent_labels","--output=xml --consistent_labels","--output=graph --consistent_labels","--output=location --consistent_labels","--output=proto --proto:instantiation_stack","--output=build --proto:instantiation_stack","--output=label --line_terminator_null","--output=label --null","--infer_universe_scope","--universe_scope=//lib/...","--universe_scope=//lib/... --output=label","--universe_scope=//app/...,//lib/... --output=label","--universe_scope=//nonexist/...","--strict_test_suite","--nostrict_test_suite","--tool_deps","--host_deps","--implicit_deps","--include_aspects","--noinclude_aspects","--include_aspects --output=proto","--include_commandline=false --output=proto","--include_artifacts","--noinclude_artifacts","--output=proto --noinclude_artifacts","--output=proto --include_artifacts","--query_file=/dev/null","--keep_going","--nokeep_going","--show_progress","--noshow_progress","--output=label --noshow_loading_progress","--bad_flag","--output=bogus","--order_output=bogus","--output=label --output=build"]:
        a("query '%s' %s"%(e,f))
# --query_file, stdin-like
a("query --query_file=/tmp/r7v/q1.txt")
a("query '//lib:l1' --query_file=/tmp/r7v/q1.txt")
a("query")
a("query --output=label")
a("query //lib:l1 //lib:l2")
a("query 'deps(//app:bin)' --output=label --keep_going --noimplicit_deps")
a("query //lib:l1 --universe_scope=//lib:l2")
a("query 'allrdeps(//lib:a.txt)' --universe_scope=//lib:all")
a("query 'rdeps(//lib:l1,//lib:a.txt)' --universe_scope=//lib:all --order_output=full")
a("query 'rdeps(//lib:l3,//lib:l1)' --universe_scope=//lib:l3 --output=label")
a("query '//lib:l1' -k --output=label")
a("query '//nonexist:x' -k")
a("query '//nonexist:x'")
a("query '//nonexist/...'")
a("query '//nonexist/...' -k")
a("query '//broken:all'")
a("query '//broken:all' -k")
a("query '//broken/...' -k --output=label")
a("query '//broken/...'")
a("query 'deps(//broken:dep_bad)' --output=label")
a("query 'deps(//broken:dep_bad)' -k --output=label")
a("query '//broken:good'")
a("query '//broken:bad_src' --output=build")
a("query '//broken:bad_src' --output=proto")
a("query 'deps(//broken:dep_bad)' -k --output=proto")
a("query 'deps(//broken:dep_bad)' -k --output=xml")
a("query 'deps(//broken:dep_bad)' -k --output=graph")
a("query 'deps(//broken:dep_bad)' -k --output=minrank")
a("query 'rdeps(//broken/..., //broken:g.txt)' -k")
a("query '//broken/loadfail:all' -k --output=label")
a("query '//broken/syn:all' -k --output=label")
a("query '//broken/syn:s' --output=build")
# aliases / macros / build formats
for t in ["//:al","//:al2","//app:al","//lib:smac_cat","//lib:mac_l","//lib:mac_l_0","//lib:gr","//lib:l1","//lib:l2","//app:uses_data","//tools:t","//tools:t_test","//tools:t_test2","//tools:ts","//tools:ts_fast","//tools:ts_nested","//pkgs:gq","//pkgs:ea","//pkgs:al","//pkgs:envs","//pkgs:env_a","//pkgs:restr","//data:pg","//cfg:lf","//cfg:str","//cfg:opt","//cfg:strv","//cfg:plat_always","//cfg:csr","//cfg:always","//app:sel","//app:tc","//app:td","//app:sd","//lib:w1","//lib:a.txt","//lib:gr.out","//lib:fg"]:
    for f in ["--output=build","--output=proto","--output=xml","--output=label_kind --output=location","--output=jsonproto"]:
        a("query '%s' %s"%(t,f))
for t in ["//app:sel","//app:al","//app:tc","//lib:l1","//pkgs:gq"]:
    for f in ["--output=build --noproto:locations","--output=xml --noxml:default_values","--output=build --proto:instantiation_stack"]:
        a("query '%s' %s"%(t,f))
# ---- cquery
CQ=[]
cexprs=["//lib:all","//app:all","//tools:all","//app:bin","deps(//app:bin)","deps(//app:app_test)","deps(//app:td)","deps(//app:sd)","//app:sel","deps(//app:sel_deps)","//app:al","//app:tc","//app:tc_ok","deps(//app:tc)","//cfg:all","//pkgs:all","//lib:gr","deps(//lib:gr)","//lib:l1 + //lib:l2","//lib:a.txt","//lib:gr.out","//lib:mac_l","//lib:smac","//lib:smac_cat","//tools:ts","tests(//tools:ts)","//tools:ts_all","//tools:ts_fast","//app:vis_bad","//app:fg_empty","//app:cfg_gen","//app:uses_priv","//app:uses_data","//lib:w1","//pkgs:gq","//pkgs:gq2","//pkgs:gq_bad","//pkgs:ea","//pkgs:envs","//pkgs:incl","//pkgs:restr","//data:all","//data:pg","//cfg:lf","deps(//cfg:lf)","rdeps(//...,//lib:l1)","rdeps(//app:app_test,//lib:l1)","somepath(//app:app_test,//lib:l1)","allpaths(//app:app_test,//lib:l1)","kind(cat,//...)","filter('l',//lib:all)","attr(n,7,//lib:all)","//...","//lib/...","//nonexist:foo","//broken:all","//broken:good","//broken/...","//deep/...","deps(//deep/x:x)","//:everything","//:al2","//app:bin + //lib:l1","somepath(//app:bin,//data:d)","labels(srcs,//lib:l2)","tests(//tools:all)","buildfiles(//app:bin)","let x = //lib:l1 in deps($x)","same_pkg_direct_rdeps(//lib:a.txt)","siblings(//lib:l1)","visible(//app:bin,//lib:all)","loadfiles(//lib:l1)","rbuildfiles(//rules.bzl)","//lib:l1 - //lib:l1","deps(//app:bin,1)","some(//lib:all)","set()","foo(//x)","deps(","//lib:nonexist","//app:bin //lib:l1"]
for e in cexprs: a("cquery '%s' --keep_going"%e)
cfgs=["","-c opt","-c dbg","-c fastbuild","--cpu=k8","--cpu=darwin_arm64","--define=FOO=bar","--define=FOO=baz","-c opt --define=FOO=bar","--platforms=//cfg:plat_always","--platforms=//cfg:plat_never","--//cfg:str=on","--//cfg:num=5","--copt=-O2","--host_cpu=k8","--experimental_exec_configuration_distinguisher=legacy","--noincompatible_auto_exec_groups","--features=foo","--strip=always","--compilation_mode=opt --cpu=k8","--//cfg:lf=//data:d","--platform_suffix=sfx","--action_env=A=B","--test_env=A=B","--notrim_test_configuration","--nocollect_code_coverage","--collect_code_coverage","--stamp","--nostamp","--instrument_test_targets","--check_visibility=false","--noimplicit_deps","--notool_deps","--nohost_deps","--universe_scope=//lib:l3 --nofetch","--output=label --noimplicit_deps"]
outs=["label","label_kind","build","proto","textproto","jsonproto","transitions=lite","transitions=full","files","graph","minrank","maxrank","location","summary","starlark --starlark:expr=target.label","starlark --starlark:expr='str(target.label)+\" \"+str(providers(target).keys())'","starlark --starlark:expr='[f.path for f in target.files.to_list()]'","starlark --starlark:expr='str(build_options(target))'","starlark --starlark:expr=\"str(providers(target))\"","xml","bogus"]
for o in outs:
    for e in ["//app:bin","//lib:l3","//app:td","//app:sel","deps(//app:app_test)"]:
        a("cquery '%s' --output=%s"%(e,o))
for cf in cfgs:
    for e in ["//app:sel","//lib:all","deps(//app:td)"]:
        a("cquery '%s' %s --output=label_kind"%(e,cf))
        a("cquery '%s' %s --output=files"%(e,cf)) if e=="//lib:all" else None
        a("cquery '%s' %s --output=transitions=lite"%(e,cf)) if e=="deps(//app:td)" else None
for cf in ["","-c opt","--//cfg:str=on","--cpu=k8 --define=FOO=bar"]:
    for e in ["config(//lib:l1,target)","config(//lib:l1,host)","config(//lib:l1,exec)","config(//lib:l1,null)","config(//lib:a.txt,target)","config(//lib:a.txt,null)","config(//lib:l1,0123456789)","config(//lib:l1,)","config(//lib:l1,bogus)","config(//app:td,target)","config(deps(//app:td),target)","config(//lib:l1 + //lib:l2,target)","config(//app:bin,exec)","config(//tools:t,exec)","config(//tools:t,target)","config(//app:uses_data,exec)","config(//lib:l1,target) + //lib:l2"]:
        a("cquery '%s' %s"%(e,cf))
for fl2 in ["--output=proto --noproto:include_configurations","--output=proto --proto:include_configurations","--output=proto --proto:output_rule_attrs=srcs","--output=proto --noproto:rule_inputs_and_outputs","--output=proto --noproto:locations","--output=proto --proto:include_synthetic_attribute_hash","--output=proto --noproto:include_configurations","--output=jsonproto --noproto:include_configurations","--output=textproto --proto:flatten_selects","--output=proto --noproto:flatten_selects","--output=label --noshow_config_fragments_in_labels","--output=label --show_config_fragments=direct","--output=label --show_config_fragments=transitive","--output=label --show_config_fragments=off","--output=label --transitions=lite","--output=label --transitions=full","--output=label --transitions=none","--output=label --tool_deps","--output=label --notool_deps","--output=label --noimplicit_deps","--output=label --nohost_deps","--output=label --consistent_labels","--output=build --consistent_labels","--output=proto --consistent_labels","--output=label --order_output=full","--output=label --order_output=no","--output=label --order_output=auto","--output=label --keep_going","--output=label --nokeep_going","--output=label --infer_universe_scope","--output=label --universe_scope=//lib:l3","--output=label --universe_scope=//app:bin","--output=label --incompatible_enable_cc_toolchain_resolution","--output=label --line_terminator_null","--output=label --noskip_incompatible_explicit_targets","--output=label --skip_incompatible_explicit_targets","--output=label --include_aspects","--output=label --aspects=//:rules.bzl%asp","--output=label --aspects=//:rules.bzl%asp --include_aspects","--output=label --aspects=//:rules.bzl%asp --noinclude_aspects","--output=files --aspects=//:rules.bzl%asp --output_groups=aspout","--output=files --output_groups=+aspout","--output=files --output_groups=default","--output=files --output_groups=bogus","--output=build --noproto:locations","--output=location --relative_locations","--output=build --proto:instantiation_stack","--output=label --max_config_changes_to_show=0","--output=label --max_config_changes_to_show=2","--output=label --max_config_changes_to_show=-1","--output=label --nofetch","--output=label --check_visibility","--output=label --nocheck_visibility","--output=label --analysis_testing_deps_limit=1","--output=label --nolegacy_external_runfiles","--output=label --experimental_bzl_visibility","--output=label --experimental_cc_shared_library"]:
    for e in ["deps(//app:app_test)","//app:td"]:
        a("cquery '%s' %s"%(e,fl2))
for t in ["//app:bin","//lib:l1","//lib:l3","//app:sel","//app:td","//app:sd","//app:al","//app:tc","//lib:a.txt","//lib:gr.out","//tools:t","//tools:t_test","//tools:ts","//pkgs:gq","//pkgs:ea","//cfg:opt","//cfg:str","//cfg:lf","//cfg:plat_always","//lib:smac_cat","//lib:mac_l_1","//app:cfg_gen","//lib:w1"]:
    for f in ["--output=proto","--output=transitions=full","--output=build","--output=files -c opt","--output=label_kind --cpu=k8"]:
        a("cquery '%s' %s"%(t,f))
# ---- aquery
aq=["//lib:l1","//lib:l3","//lib:gr","//lib:gr2.out","//lib:w1","//lib:fg","//app:bin","//app:app_test","//app:td","//app:sd","//app:sel","//app:al","//app:tc","//app:tc_ok","//app:cfg_gen","//app:uses_data","//tools:t","//tools:t_test","//tools:ts","//lib:smac_cat","//lib:mac_l","//pkgs:gq","//pkgs:ea","deps(//app:bin)","deps(//app:app_test)","deps(//app:td)","deps(//lib:l3)","//lib:all","//app:all","//tools:all","//...","//lib/...","//nonexist:x","//broken:good","//broken:all","//broken/...","rdeps(//...,//lib:l1)","//lib:l1 + //lib:l2","deps(//app:sd)","//lib:a.txt","//lib:l1.out","//lib:w1_dir","//data:d","//lib:gr_loc","deps(//lib:gr_loc)","//:everything","//:al","somepath(//app:app_test,//lib:l1)","attr(n,7,//lib:all)","kind(cat,//...)","//lib:l1 - //lib:l1","some(//lib:all)","foo()","deps("]
for e in aq: a("aquery '%s' --keep_going"%e)
aout=["text","textproto","proto","jsonproto","summary","bogus"]
for o in aout:
    for e in ["//lib:l1","//lib:gr","//lib:w1","//app:app_test","deps(//app:td)","//app:sel"]:
        a("aquery '%s' --output=%s"%(e,o))
for fl3 in ["--include_commandline","--noinclude_commandline","--include_artifacts","--noinclude_artifacts","--include_param_files","--noinclude_param_files","--include_file_write_contents","--noinclude_file_write_contents","--include_commandline=false --include_artifacts=false","--include_param_files --output=proto","--include_param_files --output=text","--include_param_files --include_commandline=false","--include_file_write_contents --output=text","--include_file_write_contents --output=proto","--include_file_write_contents --output=jsonproto","--include_file_write_contents --output=textproto","--noinclude_commandline --output=proto","--noinclude_commandline --output=textproto","--noinclude_artifacts --output=proto","--noinclude_artifacts --output=jsonproto","--noinclude_artifacts --output=text","--noinclude_commandline --noinclude_artifacts --output=text","--include_aspects","--noinclude_aspects","--aspects=//:rules.bzl%asp","--aspects=//:rules.bzl%asp --output=proto","--aspects=//:rules.bzl%asp --output=text","--aspects=//:rules.bzl%asp --include_aspects=false","--output_groups=aspout --aspects=//:rules.bzl%asp","--incompatible_enable_cc_toolchain_resolution","--tool_deps","--notool_deps","--noimplicit_deps","--nohost_deps","--output=proto --consistent_labels","--output=text --consistent_labels","--output=jsonproto --noinclude_commandline","--output=textproto --include_param_files","--skyframe_state","--output=text --skyframe_state","--output=bogus --skyframe_state","--order_output=full","--order_output=no","--order_output=auto","--universe_scope=//lib:l3","--infer_universe_scope","--line_terminator_null","--keep_going","--nokeep_going","--relative_locations","--noproto:locations","--proto:output_rule_attrs=srcs","--include_commandline=maybe","--noimplicit_deps --output=proto","--output=text -c opt","--output=text --cpu=k8","--output=text --define=FOO=bar","--output=text --platforms=//cfg:plat_always","--output=text --//cfg:str=on","--output=text --action_env=A=B","--output=text --features=foo","--output=text --copt=-O2","--output=text --experimental_exec_configuration_distinguisher=legacy","--output=proto -c opt","--output=jsonproto --cpu=k8"]:
    for e in ["//lib:l1","//lib:w1","//app:td"]:
        a("aquery '%s' %s"%(e,fl3))
for f in ["inputs('.*txt', //lib:l1)","inputs('a.txt', //lib:l1)","outputs('.*out', //lib:l1)","outputs('.*', //lib:gr)","mnemonic('Cat7', //lib:l1)","mnemonic('Cat.*', deps(//app:td))","mnemonic('Genrule', //lib:gr)","mnemonic('FileWrite', //lib:w1)","mnemonic('MkDir7', //lib:w1)","mnemonic('bogus', //lib:l1)","mnemonic('.*', //lib:w1)","mnemonic('(', //lib:l1)","inputs('(', //lib:l1)","outputs('(', //lib:l1)","inputs(//lib:l1)","mnemonic(//lib:l1)","mnemonic('Cat7')","inputs('a','b','c')","outputs('.*',inputs('.*',//lib:l1))","inputs('.*', //lib:l1) + mnemonic('Cat7', //lib:l1)","mnemonic('Cat7', //lib:l1) union mnemonic('Genrule', //lib:gr)","mnemonic('Cat7', //lib:l1) - mnemonic('Cat7', //lib:l1)","mnemonic('Cat7', deps(//app:td)) intersect //lib:l1","let x = //lib:l1 in mnemonic('Cat7', $x)","mnemonic('Cat7', //lib:l1 + //lib:l2)","mnemonic('^C', //lib:l1)","mnemonic('Cat7$', //lib:l1)","mnemonic('', //lib:l1)","inputs('', //lib:l1)","outputs('', //lib:l1)","inputs('.*', //app:app_test)","outputs('.*', //app:app_test)","mnemonic('.*', //app:app_test)","mnemonic('.*', //tools:t_test)","mnemonic('.*', //app:bin)","mnemonic('.*', //app:sel)","mnemonic('.*', //app:td)","mnemonic('.*', //pkgs:gq)","mnemonic('.*', //pkgs:ea)","mnemonic('.*',//lib:smac_cat)","mnemonic('.*',//lib:mac_l)","mnemonic('Symlink.*',deps(//app:bin))","mnemonic('Runfiles.*',//app:bin)","mnemonic('Source.*',//app:bin)","mnemonic('.*',//app:cfg_gen)"]:
    for o in ["","--output=text","--output=proto","--output=jsonproto","--output=summary"]:
        if o=="" or f.startswith("mnemonic('.*'") or f.startswith("inputs('.*txt'") :
            a("aquery \"%s\" %s"%(f,o))
for e in ["//lib:l1","//lib:w1","//app:bin","//app:app_test","//tools:t_test","//lib:gr"]:
    for f in ["--output=text -c opt","--output=text --cpu=k8 --output=text","--output=text --platforms=//cfg:plat_never","--output=text --noincompatible_auto_exec_groups","--output=text --stamp"]:
        a("aquery '%s' %s"%(e,f))
# dedupe
seen=set();out=[]
for c in C:
    if c not in seen: seen.add(c);out.append(c)
with open('/tmp/r7v/cmds.tsv','w') as f:
    for i,c in enumerate(out): f.write("%d\t%s\n"%(i+1,c))
print(len(out))
