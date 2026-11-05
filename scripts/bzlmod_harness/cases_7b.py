"""Round 7B differential cases: Starlark reprs / static errors, execute edge cases, mod texts, override flags,
fetch/query nofetch + ambiguity, MODULE.bazel kwarg errors. Run: ZEL_BIN=... python3 cases_7b.py [substr...]"""
import cases_ctx
from cases_ctx import R
from hconf import VBZ
from harness import *
cases = cases_ctx.cases
cases.clear()

# ---------------------------------------------------------------- reprs in a repository rule (print goes to DEBUG:)
reprs = {
 'struct_str': 'print(struct(a="x"))', 'struct_nested': 'print(struct(a=struct(b="y", c=[1, "z"]), d={"k": "v"}))',
 'struct_repr': 'print(repr(struct(a="x", b=None, c=True)))', 'struct_empty': 'print(struct())',
 'struct_in_list': 'print([struct(a="q")])', 'struct_in_dict': 'print({"s": struct(a="q")})',
 'struct_unicode': 'print(struct(a="héllo\\n\\t\\"q\\""))', 'struct_sorted': 'print(struct(b=1, a=2))',
 'struct_label': 'print(struct(l=Label("//:r.bzl")))', 'struct_tuple': 'print(struct(t=(1,), u=(1, 2)))',
 'label_print': 'print(Label("//:r.bzl"))', 'label_str': 'print(str(Label("//:r.bzl")))', 'label_repr': 'print(repr(Label("//:r.bzl")))',
 'label_pkg': 'print(Label("//a/b:c"), Label("//a/b"), Label("//a/b:b"), Label("//:a"))',
 'label_repo_apparent': 'print(Label("@root//:x"), Label("@@//:x"), Label("@bazel_tools//tools:x"))',
 'label_list': 'print([Label("//:a"), Label("//a:a")])', 'label_dict': 'print({"k": Label("//:a")})',
 'label_unknown_repo': 'print(Label("@nonexistent//:x"))', 'label_canon': 'print(Label("@@foo+//:x"), Label("@@bazel_tools//:x"))',
 'label_relative': 'print(Label("//:r.bzl").relative(":x"), Label("//a:b").relative("//c:d"))',
 'label_attrs': 'l = Label("//a:b")\nprint(l.name, l.package, l.repo_name, l.workspace_name, l.workspace_root)',
 'os_print': 'print(rctx.os)', 'os_str': 'print(str(rctx.os))', 'os_repr': 'print(repr(rctx.os))', 'os_type': 'print(type(rctx.os))',
 'os_fields': 'print(rctx.os.name, rctx.os.arch, type(rctx.os.environ))', 'os_dir': 'print(dir(rctx.os))',
 'ctx_print': 'print(rctx)', 'ctx_type': 'print(type(rctx))', 'attr_str': 'print(str(rctx.attr))', 'attr_type': 'print(type(rctx.attr))',
 'path_print': 'print(rctx.path("a"))', 'path_type': 'print(type(rctx.path("a")))', 'path_repr': 'print(repr(rctx.path("a")))',
 'func_str': 'def f(): pass\nprint(str(f))', 'func_lambda': 'print(str(lambda x: x))', 'func_builtin': 'print(str(len), str(print), str(rctx.file))',
 'func_nested': 'def g():\n    def h(): pass\n    return h\nprint(g())', 'func_type': 'def f(): pass\nprint(type(f), type(len))',
 'rule_str': 'print(str(repository_rule), type(repository_rule))', 'attr_fn': 'print(str(attr.string), type(attr.string))',
 'type_names': 'print(type(1), type(1.0), type("a"), type([]), type(()), type({}), type(None), type(True), type(depset()), type(struct()), type(range(1)))',
 'depset_repr': 'print(depset([1, 2]), depset(), repr(depset(["a"])))', 'range_repr': 'print(range(3), range(1, 5, 2), repr(range(2)))',
 'float_repr': 'print(1.0, 1.5, 1e20, 1e-5, 0.1+0.2, 3.0/2)', 'int_big': 'print(2**70, -2**63)', 'string_repr': 'print(repr("a\\"b\\n\\x01\\u00e9"), "é")',
 'set_repr': 'print(set([1, 2]), repr(set()))', 'none_bool': 'print(None, True, False, [None])', 'tuple_repr': 'print((), (1,), (1, "a"))',
 'dict_repr': 'print({}, {"a": 1, 2: [3]}, {(1, 2): "x"})', 'list_nested': 'print([[1], [[2]], {"a": []}])',
 'print_sep': 'print("a", "b", sep="-")', 'print_multi': 'print(1, "a", None, [1], struct(a=1))',
 'bool_ops': 'print(not 1, 1 and 2, 0 or 3)', 'json_enc': 'print(json.encode(struct(a=[1, "b"])), json.encode({"a": 1}))',
 'json_dec_repr': 'print(json.decode("{\\"a\\": [1, 2.5, null, true]}"))', 'proto_enc': 'print(proto.encode_text(struct(a=1, b="x")))',
}
for n, b in reprs.items():
    R('rp_' + n, b)

# ---------------------------------------------------------------- static errors in .bzl
def bzl_case(name, bzl, extra=None, main_mod=None, args=None, flags=None):
    mod = main_mod or 'module(name="root")\nr=use_repo_rule("//:r.bzl","r")\nr(name="t")\n'
    f = {'MODULE.bazel': mod, 'r.bzl': bzl}
    f.update(extra or {})
    cases.append(dict(name='b7_' + name, args=args or ['fetch', '--repo=@t'], files=f, flags=flags or [], dbg=True, tree=['external/*+t'], pre=[], cmp=[]))
OKRULE = '\ndef _impl(rctx):\n    rctx.file("BUILD.bazel", "")\nr = repository_rule(implementation=_impl)\n'
bzl_case('ok', 'x = 1' + OKRULE)
bzl_case('undef_toplevel', 'y = zzz\n' + OKRULE)
bzl_case('undef_in_def_unused', 'def f():\n    return zzz\n' + OKRULE)
bzl_case('undef_in_impl', 'def _impl(rctx):\n    print(zzz)\n    rctx.file("BUILD.bazel", "")\nr = repository_rule(implementation=_impl)\n')
bzl_case('undef_typo_didyoumean', 'def _impl(rctx):\n    rctx.file("BUILD.bazel", "")\nr = repository_rule(implementaton=_impl)\nprint(repository_rul)\n')
bzl_case('undef_typo_len', 'x = lenn([1])\n' + OKRULE)
bzl_case('undef_in_lambda', 'f = lambda: zzz\n' + OKRULE)
bzl_case('undef_in_comprehension', 'x = [zzz for i in range(3)]\n' + OKRULE)
bzl_case('undef_in_default', 'def f(a=zzz): pass\n' + OKRULE)
bzl_case('undef_two', 'a = zz1\nb = zz2\n' + OKRULE)
bzl_case('undef_in_if', 'if True:\n    pass\nelse:\n    x = zzz\n' + OKRULE)
bzl_case('undef_after_def', 'def f():\n    return later\nlater = 1\n' + OKRULE)
bzl_case('undef_comp_var_leak', 'x = [i for i in range(3)]\ny = i\n' + OKRULE)
bzl_case('undef_attr_of_undef', 'x = zzz.foo\n' + OKRULE)
bzl_case('undef_call_undef', 'zzz()\n' + OKRULE)
bzl_case('undef_in_loaded_file', 'load("//:o.bzl", "o")\n' + OKRULE, extra={'o.bzl': 'o = zzz\n', 'BUILD.bazel': ''})
bzl_case('undef_load_missing_symbol', 'load("//:o.bzl", "nothere")\n' + OKRULE, extra={'o.bzl': 'o = 1\n', 'BUILD.bazel': ''})
bzl_case('undef_load_missing_file', 'load("//:nofile.bzl", "x")\n' + OKRULE, extra={'BUILD.bazel': ''})
bzl_case('syntax_eq', 'x = = 1\n' + OKRULE)
bzl_case('syntax_unclosed', 'x = (1,\n' + OKRULE)
bzl_case('syntax_indent', 'def f():\nreturn 1\n' + OKRULE)
bzl_case('syntax_load_late', 'x = 1\nload("//:o.bzl", "o")\n' + OKRULE, extra={'o.bzl': 'o = 1\n', 'BUILD.bazel': ''})
bzl_case('syntax_bad_char', 'x = 1 $ 2\n' + OKRULE)
bzl_case('syntax_dup_param', 'def f(a, a): pass\n' + OKRULE)
bzl_case('syntax_kwarg_order', 'def f(a=1, b): pass\n' + OKRULE)
bzl_case('syntax_return_toplevel', 'return 1\n' + OKRULE)
bzl_case('syntax_break_toplevel', 'break\n' + OKRULE)
bzl_case('syntax_for_toplevel', 'for i in [1]:\n    pass\n' + OKRULE)
bzl_case('syntax_if_toplevel', 'if True:\n    pass\n' + OKRULE)
bzl_case('syntax_reassign_load', 'load("//:o.bzl", "o")\no = 2\n' + OKRULE, extra={'o.bzl': 'o = 1\n', 'BUILD.bazel': ''})
bzl_case('syntax_global_reassign', 'x = 1\nx = 2\n' + OKRULE)
bzl_case('syntax_string_unterminated', 'x = "abc\n' + OKRULE)
bzl_case('syntax_lone_star', 'def f(*): pass\n' + OKRULE)
bzl_case('syntax_two_stars', 'def f(**a, b): pass\n' + OKRULE)
bzl_case('syntax_bad_assign_target', '1 = x\n' + OKRULE)
bzl_case('syntax_semicolon', 'x = 1; y = 2\n' + OKRULE)
bzl_case('syntax_tab_indent', 'def f():\n\treturn 1\n' + OKRULE)
bzl_case('syntax_lambda_noarg', 'f = lambda: 1\n' + OKRULE)
bzl_case('syntax_empty_file', '')
bzl_case('syntax_while', 'while True:\n    pass\n' + OKRULE)
bzl_case('syntax_class', 'class A: pass\n' + OKRULE)
bzl_case('syntax_import', 'import os\n' + OKRULE)
bzl_case('syntax_try', 'try:\n    pass\nexcept:\n    pass\n' + OKRULE)
bzl_case('syntax_in_loaded', 'load("//:o.bzl", "o")\n' + OKRULE, extra={'o.bzl': 'o = = 1\n', 'BUILD.bazel': ''})
# extension .bzl static errors
EXTMOD = 'module(name="root")\ne=use_extension("//:e.bzl","ex")\nuse_repo(e,"x")\n'
EXTOK = '\ndef _impl(mctx):\n    pass\nex = module_extension(implementation=_impl)\n'
def ext_case(name, bzl, extra=None):
    bzl_case('ext_' + name, bzl, extra=dict({'e.bzl': bzl, 'BUILD.bazel': ''}, **(extra or {})), main_mod=EXTMOD, args=['mod', 'graph', '--extension_info=all']) if False else None
    f = {'MODULE.bazel': EXTMOD, 'e.bzl': bzl, 'BUILD.bazel': ''}; f.update(extra or {})
    cases.append(dict(name='b7_ext_' + name, args=['fetch', '--all'], files=f, flags=[], dbg=True, tree=[], pre=[], cmp=[]))
ext_case('ok', EXTOK)
ext_case('undef', 'x = zzz\n' + EXTOK)
ext_case('undef_in_impl', 'def _impl(mctx):\n    zzz()\nex = module_extension(implementation=_impl)\n')
ext_case('syntax', 'x = = 1\n' + EXTOK)
ext_case('load_late', 'x = 1\nload("//:o.bzl", "o")\n' + EXTOK, extra={'o.bzl': 'o = 1\n'})
ext_case('load_missing', 'load("//:nofile.bzl", "o")\n' + EXTOK)
ext_case('undef_typo', 'def _impl(mctx):\n    mctxx.os\nex = module_extension(implementation=_impl)\n')
ext_case('undef_in_loaded', 'load("//:o.bzl", "o")\n' + EXTOK, extra={'o.bzl': 'o = zzz\n'})
ext_case('not_ext', 'ex = 1\n')
ext_case('missing_ext', 'zz = 1\n')
ext_case('dict_unhashable', 'x = {[1]: 2}\n' + EXTOK)
ext_case('dict_unhashable_in_impl', 'def _impl(mctx):\n    x = {[1]: 2}\nex = module_extension(implementation=_impl)\n')
ext_case('dict_dup_key', 'x = {"a": 1, "a": 2}\n' + EXTOK)
ext_case('dict_unhashable_struct', 'x = {struct(a=[1]): 2}\n' + EXTOK)

# ---------------------------------------------------------------- native.*
R('native_unknown', 'print(native.nonexist)')
R('native_unknown_call', 'native.nonexist()')
R('native_bazel_version', 'print(native.bazel_version)')
R('native_existing_rule', 'print(native.existing_rule("x"), native.existing_rules())')
R('native_dir', 'print("bazel_version" in dir(native), "existing_rule" in dir(native), "cc_library" in dir(native), "genrule" in dir(native))')
R('native_type', 'print(type(native))')
R('native_getattr', 'print(getattr(native, "zzz", "dflt"), hasattr(native, "genrule"), hasattr(native, "zzz"))')
R('native_package_name', 'print(native.package_name())')
R('native_glob', 'print(native.glob(["*"]))')

# ---------------------------------------------------------------- execute edge cases
ex = {
 'timeout_zero': 'r = rctx.execute(["echo", "hi"], timeout=0)\nprint(r.return_code, r.stdout)',
 'timeout_neg': 'r = rctx.execute(["echo", "hi"], timeout=-1)\nprint(r.return_code, r.stdout)',
 'timeout_float': 'r = rctx.execute(["echo", "hi"], timeout=1.5)\nprint(r.return_code)',
 'timeout_str': 'r = rctx.execute(["echo", "hi"], timeout="5")',
 'timeout_expire': 'r = rctx.execute(["sleep", "5"], timeout=1)\nprint(r.return_code, repr(r.stdout), repr(r.stderr))',
 'timeout_expire_sh': 'r = rctx.execute(["sh", "-c", "echo before; sleep 5; echo after"], timeout=1)\nprint(r.return_code, repr(r.stdout))',
 'env_basic': 'r = rctx.execute(["sh", "-c", "echo $FOO"], environment={"FOO": "bar"})\nprint(r.stdout)',
 'env_none_value': 'r = rctx.execute(["sh", "-c", "echo [$FOO]"], environment={"FOO": None})\nprint(r.stdout)',
 'env_int_value': 'r = rctx.execute(["echo"], environment={"FOO": 1})',
 'env_int_key': 'r = rctx.execute(["echo"], environment={1: "x"})',
 'env_list': 'r = rctx.execute(["echo"], environment=["a"])',
 'env_empty_key': 'r = rctx.execute(["sh", "-c", "echo ok"], environment={"": "x"})\nprint(r.return_code, r.stdout)',
 'env_overrides_path': 'r = rctx.execute(["sh", "-c", "echo $PATH"], environment={"PATH": "/nonexistent"})\nprint(r.stdout)',
 'env_clear_path': 'r = rctx.execute(["/bin/sh", "-c", "echo [$PATH]"], environment={"PATH": ""})\nprint(r.stdout)',
 'env_inherits_repo_env': 'r = rctx.execute(["sh", "-c", "echo [$ZEL_NOT_SET_XYZ]"])\nprint(r.stdout)',
 'env_pwd': 'r = rctx.execute(["pwd"])\nprint(r.stdout.strip().endswith("+t"))',
 'env_equals_in_key': 'r = rctx.execute(["sh", "-c", "env | grep -c ^A=B"], environment={"A=B": "c"})\nprint(r.return_code, r.stdout)',
 'env_unicode': 'r = rctx.execute(["sh", "-c", "echo $FOO"], environment={"FOO": "héllo"})\nprint(r.stdout)',
 'quiet_true': 'r = rctx.execute(["echo", "hi"], quiet=True)\nprint(r.stdout)',
 'quiet_false': 'r = rctx.execute(["echo", "hi"], quiet=False)\nprint(r.stdout)',
 'quiet_str': 'r = rctx.execute(["echo", "hi"], quiet="no")',
 'workdir_missing': 'r = rctx.execute(["pwd"], working_directory="nodir")\nprint(r.return_code, r.stdout)',
 'workdir_sub': 'rctx.file("sub/x", "")\nr = rctx.execute(["pwd"], working_directory="sub")\nprint(r.stdout.strip().endswith("sub"))',
 'workdir_abs': 'r = rctx.execute(["pwd"], working_directory="/tmp")\nprint(r.stdout.strip())',
 'cmd_not_found': 'r = rctx.execute(["definitely_not_a_command_xyz"])\nprint(r.return_code)',
 'cmd_not_found_path': 'r = rctx.execute(["/definitely/not/here"])\nprint(r.return_code)',
 'cmd_empty_list': 'r = rctx.execute([])',
 'cmd_empty_string': 'r = rctx.execute([""])',
 'cmd_nonstring': 'r = rctx.execute(["echo", 1])',
 'cmd_not_list': 'r = rctx.execute("echo hi")',
 'cmd_tuple': 'r = rctx.execute(("echo", "hi"))\nprint(r.stdout)',
 'cmd_path_obj': 'r = rctx.execute([rctx.path("a.sh")])',
 'cmd_label_arg': 'r = rctx.execute(["echo", Label("//:r.bzl")])\nprint(r.stdout)',
 'cmd_path_arg': 'r = rctx.execute(["echo", rctx.path("x")])\nprint(r.stdout.strip().endswith("x"))',
 'exit_code': 'r = rctx.execute(["sh", "-c", "exit 7"])\nprint(r.return_code)',
 'exit_signal': 'r = rctx.execute(["sh", "-c", "kill -9 $$"])\nprint(r.return_code)',
 'stderr_capture': 'r = rctx.execute(["sh", "-c", "echo err >&2"])\nprint(repr(r.stderr))',
 'stdout_bytes': 'r = rctx.execute(["printf", "\\\\303\\\\251\\\\377"])\nprint(repr(r.stdout))',
 'stdout_big': 'r = rctx.execute(["sh", "-c", "head -c 100000 /dev/zero | tr \\\\\\\\0 a"])\nprint(len(r.stdout))',
 'result_type': 'r = rctx.execute(["true"])\nprint(type(r), dir(r))',
 'result_str': 'r = rctx.execute(["true"])\nprint(r)',
 'unknown_kw': 'r = rctx.execute(["true"], foo=1)',
 'too_many_pos': 'r = rctx.execute(["true"], 1, {}, True, "x", "y")',
 'args_kw': 'r = rctx.execute(arguments=["echo", "hi"])\nprint(r.stdout)',
 'which_basic': 'print(rctx.which("sh") != None)',
 'which_missing': 'print(rctx.which("definitely_not_a_command_xyz"))',
 'which_slash': 'print(rctx.which("/bin/sh"))',
 'which_empty': 'print(rctx.which(""))',
 'which_rel_slash': 'print(rctx.which("bin/sh"))',
 'getenv_basic': 'print(rctx.getenv("HOME") != None, rctx.getenv("ZEL_NOT_SET_XYZ"), rctx.getenv("ZEL_NOT_SET_XYZ", "d"))',
 'getenv_badtype': 'print(rctx.getenv(1))',
 'execute_shell_quote': 'r = rctx.execute(["sh", "-c", "echo \\"$0 $1\\"", "a b", "c"])\nprint(r.stdout)',
 'execute_env_order': 'r = rctx.execute(["sh", "-c", "echo $A$B"], environment={"B": "2", "A": "1"})\nprint(r.stdout)',
}
for n, b in ex.items():
    R('ex_' + n, b, files={'a.sh': '#!/bin/sh\necho hi\n'})

# ---------------------------------------------------------------- `mod` texts
MODS = 'module(name="root",version="1.0")\nbazel_dep(name="ma",version="1.0")\nbazel_dep(name="mx",version="2.0")\nbazel_dep(name="mu",version="1.0",repo_name="muu")\ne=use_extension("@mx//:ext.bzl","ex")\ne.mk(name="r1",opt=[])\nuse_repo(e,"r1")\n'
def M(name, args, mod=MODS, files=None, flags=None):
    f = {'MODULE.bazel': mod}; f.update(files or {})
    cases.append(dict(name='m7_' + name, args=args, files=f, flags=flags if flags is not None else [], dbg=True, tree=[], pre=[], cmp=[]))
for i, a in enumerate([
    ['graph', '--base_module=zz'], ['graph', '--base_module=zz@1.0'], ['graph', '--base_module=ma@9.9'], ['graph', '--base_module=@zz'],
    ['graph', '--base_module=@@zz+'], ['graph', '--base_module=@ma'], ['graph', '--base_module=ma,mx'], ['graph', '--base_module='],
    ['graph', '--base_module=<root>'], ['graph', '--base_module=ma@_'], ['graph', '--base_module=@@ma+'], ['graph', '--base_module=@@'],
    ['graph', '--from=zz'], ['graph', '--from=ma,zz'], ['graph', '--from='], ['graph', '--from=ma@9'], ['graph', '--from=@zz'],
    ['deps', 'zz'], ['deps', 'ma@9'], ['deps', '@zz'], ['deps', '@@zz+'], ['deps', 'ma,mx'], ['deps', ''], ['deps', '@'], ['deps', '@@'],
    ['explain', 'zz'], ['path', 'zz'], ['all_paths', 'zz'], ['path', '--from=zz', 'ma'], ['all_paths', '--from=ma', 'zz'],
    ['graph', '--extension_filter=nonexist'], ['graph', '--extension_filter=@mx//:ext.bzl'], ['graph', '--extension_filter=@mx//:ext.bzl%'],
    ['graph', '--extension_filter=@zz//:ext.bzl%ex'], ['graph', '--extension_filter=//:x.bzl%y'], ['graph', '--extension_filter=@mx//:ext.bzl%nonexist'],
    ['graph', '--extension_filter=@mx//:ext.bzl%ex,@mx//:ext.bzl%zz'], ['graph', '--extension_filter='], ['graph', '--extension_filter=,'],
    ['graph', '--extension_usages=zz'], ['graph', '--extension_usages=ma@9'], ['graph', '--extension_usages=@zz'],
    ['graph', '--output=bad'], ['graph', '--output=TEXT'], ['graph', '--output='], ['graph', '--output=streamed_proto'], ['graph', '--output=streamed_jsonproto'],
    ['deps', '--output=streamed_proto'], ['explain', '--output=graph', 'ma'], ['show_repo', '--output=json', 'ma'], ['show_repo', '--output=graph', 'ma'],
    ['show_repo', '--output=bad', 'ma'], ['show_extension', '--output=json', '@mx//:ext.bzl%ex'], ['show_extension', '--output=graph', '@mx//:ext.bzl%ex'],
    ['show_repo', '@mx+'], ['show_repo', '@@mx+'], ['show_repo', 'mx+'], ['show_repo', '@zz'], ['show_repo', '@@zz+'], ['show_repo', 'zz'], ['show_repo', 'zz@1.0'],
    ['show_repo', '@'], ['show_repo', '@@'], ['show_repo', ''], ['show_repo', '@ma', '@zz'], ['show_repo', '@@ma+'], ['show_repo', '@muu'], ['show_repo', 'mu'],
    ['show_repo', '--all_repos', 'ma'], ['show_repo', '--all_visible_repos', '--all_repos'], ['deps', '--all_repos'], ['graph', '--all_visible_repos'],
    ['show_repo'], ['show_extension'], ['show_extension', 'nonexist'], ['show_extension', '@mx//:ext.bzl'], ['show_extension', '@mx//:ext.bzl%zz'],
    ['show_extension', '@zz//:ext.bzl%ex'], ['show_extension', '//:x.bzl%y'], ['show_extension', 'ma'],
    ['dump_repo_mapping'], ['dump_repo_mapping', ''], ['dump_repo_mapping', '@mx+'], ['dump_repo_mapping', 'mx+'], ['dump_repo_mapping', 'zz'], ['dump_repo_mapping', 'zz+'],
    ['dump_repo_mapping', '@@mx+'], ['dump_repo_mapping', 'ma+', 'mx+'], ['dump_repo_mapping', '@'], ['dump_repo_mapping', 'a b'], ['dump_repo_mapping', 'ma', 'zz'],
    ['graph', 'extra'], ['graph', '--depth=x'], ['graph', '--depth=-5'], ['graph', '--charset=zz'], ['graph', '--extension_info=zz'],
    ['tidy', 'x'], ['tidy', '--all_repos'], ['nosuch'], ['GRAPH'], ['Deps'], [],
]):
    M(f'txt_{i:03d}', ['mod'] + a)

# ---------------------------------------------------------------- --override_* / --inject_* flags and fetch / query
flagcases = [
    '--override_repository=mx', '--override_repository=@mx=/tmp', '--override_repository=mx+=/nonexistent', '--override_repository=mx+=relpath',
    '--override_repository=a/b=/tmp', '--override_repository==/tmp', '--override_repository=mx=', '--override_repository=@@mx+=/tmp',
    '--override_repository=mx=/tmp', '--override_repository=zz=/tmp', '--override_repository=mx+=%workspace%/lpo', '--override_repository=mx+=%workspace%/nolpo',
    '--override_module=md', '--override_module=MD=/tmp', '--override_module=md=', '--override_module=ma=/nonexistent', '--override_module=-a=/tmp',
    '--override_module=a-=/tmp', '--override_module=ma=%workspace%/lpo', '--override_module=zz=/tmp',
    '--inject_repository=zz', '--inject_repository=@zz=/tmp', '--inject_repository=zz=/tmp', '--inject_repository=zz=/nonexistent',
]
for i, f in enumerate(flagcases):
    M(f'ovr_{i:02d}', ['build', '@mx//:all'], flags=[f], files={'lpo/BUILD.bazel': '', 'lpo/MODULE.bazel': 'module(name="mx")\n', 'lpo/REPO.bazel': ''})
    M(f'ovrm_{i:02d}', ['mod', 'graph'], flags=[f], files={'lpo/BUILD.bazel': '', 'lpo/MODULE.bazel': 'module(name="mx")\n'})
for i, f in enumerate(['--override_repository=mx+=%workspace%/lpo', '--override_repository=ma+=%workspace%/lpo', '--override_repository=mx=%workspace%/lpo']):
    M(f'ovrshow_{i}', ['mod', 'show_repo', '@mx', '@ma', '--all_visible_repos'] if False else ['mod', 'show_repo', 'mx'], flags=[f], files={'lpo/BUILD.bazel': '', 'lpo/MODULE.bazel': 'module(name="mx")\n'})
    M(f'ovrshow2_{i}', ['mod', 'show_repo', '--all_repos'], flags=[f], files={'lpo/BUILD.bazel': '', 'lpo/MODULE.bazel': 'module(name="mx")\n'})
for i, a in enumerate([
    ['fetch', '--repo='], ['fetch', '--repo=mx'], ['fetch', '--repo=@'], ['fetch', '--repo=@@'], ['fetch', '--repo=@mx+'], ['fetch', '--repo=@@mx+'],
    ['fetch', '--repo=@zz'], ['fetch', '--repo=@@zz'], ['fetch', '--repo=@ma', '--repo=@zz'], ['fetch', '--repo=@ma,@mx'], ['fetch', '--repo= @ma'],
    ['fetch', '--repo=@a b'], ['fetch', '--repo=@ma', '--all'], ['fetch', '--repo=@ma', '//...'], ['fetch'], ['fetch', '--all', '//...'],
    ['fetch', '--nofetch', '--repo=@ma'], ['fetch', '--nofetch'], ['fetch', '--fetch=false', '--repo=@ma'], ['fetch', '--fetch=true', '--repo=@ma'],
    ['query', '--nofetch', '@ma//...'], ['query', '--nofetch', '@mx//:all'], ['build', '--nofetch', '@mx//:all'], ['build', '--nofetch', '//:all'],
    ['query', '@zz//...'], ['query', '@zz//:x'], ['build', '@zz//:x'], ['query', '@@zz+//...'], ['query', '@ma//:all'], ['query', '//:all'],
    ['query', 'deps(//:all)'], ['query', '//:all', '--output=label_kind'], ['aquery', '//:all'], ['build', '//:all'], ['cquery', '//:all'],
    ['query', '//:all + //:t'], ['query', '-//:all'], ['query', '//:*'], ['query', '//:all-targets'], ['query', '//...'], ['build', '--nobuild', '//:all'],
    ['fetch', '//:all'], ['fetch', '//...'], ['fetch', '@ma//:all'], ['fetch', '@zz//...'], ['fetch', '//:t'],
    ['query', '--nofetch', '//...'], ['build', '--nofetch', '--nobuild', '//...'], ['query', '--nofetch', '@mx//:x'], ['query', '--keep_going', '@zz//...'],
]):
    M(f'fq_{i:02d}', a, mod=MODS, files={'BUILD.bazel': 'filegroup(name="all", srcs=[])\nfilegroup(name="t", srcs=[])\n'})

# ---------------------------------------------------------------- MODULE.bazel kwarg / syntax errors, lockfile after fetch --repo
for i, mf in enumerate([
    'module(name="root", name="x")', 'module(name="root")\nbazel_dep(name="ma", version="1.0", name="mb")', 'module(name="root")\nbazel_dep(name="ma", version="1.0", version="2.0")',
    'module(name="root")\nbazel_dep(name="ma", **{"name": "mb"})', 'module(name="root")\nbazel_dep(**{"name": "ma", "name": "mb"})',
    'module(name="root")\nbazel_dep("ma", name="ma")', 'module(name="root")\nuse_extension("//:e.bzl", "ex", dev_dependency=True, dev_dependency=False)',
    'module(name="root")\nx = {[1]: 2}', 'module(name="root")\nx = {"a": 1, "a": 2}', 'module(name="root")\nx = zzz', 'module(name="root"\nx = 1',
    'module(name="root")\nbazel_dep(name="ma", version="1.0", bogus=1)', 'module(name="root")\nbazel_dep(name="ma", 1.0)',
    'module(name="root")\nbazel_dep(version="1.0")', 'module(name=1)', 'module(name="root", version=1)',
]):
    for cmd in (['mod', 'graph'], ['build', '//:all'], ['fetch', '--all']):
        M(f'kw_{i:02d}_{cmd[0]}', cmd, mod=mf)
for i, a in enumerate([['fetch', '--repo=@ma'], ['fetch', '--repo=@mx'], ['fetch', '--repo=@r1'], ['fetch', '--all'], ['fetch', '--repo=@ma', '--lockfile_mode=update'],
                       ['fetch', '--repo=@ma', '--lockfile_mode=off'], ['fetch', '--repo=@bazel_tools'], ['fetch', '--repo=@muu'], ['mod', 'graph'], ['mod', 'show_repo', 'ma']]):
    c = dict(name=f'l7_{i:02d}', args=a, files={'MODULE.bazel': MODS, 'BUILD.bazel': ''}, flags=[], dbg=True, tree=[], pre=[], cmp=['MODULE.bazel.lock'])
    cases.append(c)

if __name__ == '__main__':
    print(len(cases))
    main(cases, jobs=6)
