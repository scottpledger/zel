"""Round 7A cases: isolated extension usages, module_ctx API, lockfile inputs, registry validation, downloader/cache.
Run: ZEL_BIN=<zel> python3 cases_7a.py [name-substr...]"""
from hconf import VBZ, PORT
from harness import *


def mm(body, name='root', ver='1.0'):
    return f'module(name="{name}",version="{ver}")\n' + body


cases = []


def C(name, args, mf, files=None, flags=None, **kw):
    f = {'MODULE.bazel': mf}
    f.update(files or {})
    cases.append(dict(name=name, args=args, files=f, flags=flags or [], **kw))


# ---------------- isolate -------------------------------------------------
ISO = '--experimental_isolated_extension_usages'
EXT_LOCAL = '''
def _r(rctx):
    rctx.file("BUILD.bazel", "filegroup(name='g',visibility=['//visibility:public'])\\n")
    rctx.file("v.txt", rctx.attr.v + "\\n")
rr = repository_rule(implementation=_r, attrs={"v": attr.string()})
def _impl(mctx):
    for m in mctx.modules:
        for t in m.tags.mk:
            rr(name=t.name, v=m.name + "|" + ",".join(t.opt) + "|iso=" + str(mctx.is_isolated))
mk = tag_class(attrs={"name": attr.string(), "opt": attr.string_list(default=[])})
ex = module_extension(implementation=_impl, tag_classes={"mk": mk})
'''
ISO_ROOTS = {
    'one': mm('e = use_extension("//:ext.bzl", "ex", isolate = True)\ne.mk(name="a", opt=["1"])\nuse_repo(e, "a")\n'),
    'two': mm('e = use_extension("//:ext.bzl", "ex", isolate = True)\ne.mk(name="a")\nuse_repo(e, "a")\n'
              'f = use_extension("//:ext.bzl", "ex", isolate = True)\nf.mk(name="b")\nuse_repo(f, "b")\n'),
    'mixed': mm('e = use_extension("//:ext.bzl", "ex")\ne.mk(name="a")\nuse_repo(e, "a")\n'
                'f = use_extension("//:ext.bzl", "ex", isolate = True)\nf.mk(name="b")\nuse_repo(f, "b")\n'),
    'false': mm('e = use_extension("//:ext.bzl", "ex", isolate = False)\ne.mk(name="a")\nuse_repo(e, "a")\n'),
    'dev': mm('e = use_extension("//:ext.bzl", "ex", isolate = True, dev_dependency = True)\ne.mk(name="a")\nuse_repo(e, "a")\n'),
    'noimport': mm('e = use_extension("//:ext.bzl", "ex", isolate = True)\ne.mk(name="a")\n'),
    'dep': mm('bazel_dep(name="mx", version="2.0")\ni = use_extension("@mx//:ext.bzl", "ex", isolate = True)\ni.mk(name="iso", opt=["z"])\nuse_repo(i, "iso")\n'
              'e = use_extension("@mx//:ext.bzl", "ex")\ne.mk(name="n4a")\nuse_repo(e, "n4a")\n'),
    'dep2': mm('bazel_dep(name="mx", version="2.0")\ni = use_extension("@mx//:ext.bzl", "ex", isolate = True)\ni.mk(name="i1")\nuse_repo(i, "i1")\n'
               'j = use_extension("@mx//:ext.bzl", "ex", isolate = True)\nj.mk(name="i2")\nuse_repo(j, "i2")\n'),
    'mx2': mm('bazel_dep(name="mx", version="2.0")\ni = use_extension("@mx//:ext.bzl", "ex", isolate = True)\ni.mk(name="i1")\nuse_repo(i, "i1")\n'
              'k = use_extension("@mx//:ext.bzl", "ex2", isolate = True)\nuse_repo(k, k1="fixed")\n'),
}
iso_subs = [('mod', ['graph']), ('mod', ['deps']), ('mod', ['graph', '--output=json']), ('mod', ['graph', '--extension_info=all']),
            ('mod', ['graph', '--extension_info=usages']), ('mod', ['graph', '--extension_info=repos']),
            ('mod', ['graph', '--extension_info=all', '--verbose']), ('mod', ['show_repo', '--all_visible_repos']),
            ('mod', ['dump_repo_mapping', '']), ('mod', ['show_extension', '@mx//:ext.bzl%ex']),
            ('mod', ['show_extension', '//:ext.bzl%ex']), ('mod', ['graph', '--include_unused', '--extension_info=all']),
            ('fetch', ['--all']), ('fetch', ['--repo=@a'])]
for rn, mf in ISO_ROOTS.items():
    for i, (cmd, a) in enumerate(iso_subs):
        files = {'ext.bzl': EXT_LOCAL}
        for flag, tag in (([ISO], 'f'), ([], 'n')):
            if tag == 'n' and i > 3:
                continue
            # i==7 is show_repo (prints the install path, normalized); show_repo of an isolated repo name is unaffected
            C(f'iso_{rn}_{tag}{i}', [cmd] + a, mf, files=files, flags=flag, cmp=[],
              jcmp=['MODULE.bazel.lock'])


# ---------------- module_ctx API / lockfile recordedInputs ------------------
def ext_with(body, extra_load='', tags='', meta=''):
    return extra_load + """
def _r(rctx):
    rctx.file("BUILD.bazel", "filegroup(name='g',visibility=['//visibility:public'])\\n")
    rctx.file("v.txt", rctx.attr.v + "\\n")
rr = repository_rule(implementation=_r, attrs={"v": attr.string(), "l": attr.label()})
def _impl(mctx):
""" + "\n".join("    " + l for l in body.strip().splitlines()) + """
    rr(name="gen", v="x")
""" + meta + """
mk = tag_class(attrs={"name": attr.string(), "lbl": attr.label(), "ls": attr.label_list(), "s": attr.string()})
ex = module_extension(implementation=_impl, tag_classes={"mk": mk}, environ=ENVIRON)
""".replace("ENVIRON", "[]")

ROOT_EXT = mm('e = use_extension("//:ext.bzl", "ex")\nuse_repo(e, "gen")\n')
ROOT_EXT_TAG = lambda tag: mm('e = use_extension("//:ext.bzl", "ex")\n' + tag + '\nuse_repo(e, "gen")\n')
WS_FILES = {'data.txt': 'hello\n', 'dir/a.txt': 'a\n', 'dir/b.txt': 'b\n', 'BUILD.bazel': '', 'dir/BUILD.bazel': ''}
MC = {
    'getenv_set': 'x = mctx.getenv("ZEL_7A_SET")',
    'getenv_unset': 'x = mctx.getenv("ZEL_7A_UNSET_X")',
    'getenv_default': 'x = mctx.getenv("ZEL_7A_UNSET_X", "d")',
    'getenv_two': 'mctx.getenv("ZEL_7A_SET")\nmctx.getenv("ZEL_7A_UNSET_X")',
    'getenv_space': 'mctx.getenv("ZEL_7A_SPACE")',
    'getenv_nl': 'mctx.getenv("ZEL_7A_NL")',
    'getenv_bs': 'mctx.getenv("ZEL_7A_BS")',
    'getenv_empty': 'mctx.getenv("ZEL_7A_EMPTY")',
    'os_environ': 'x = mctx.os.environ.get("ZEL_7A_SET")',
    'read_ws': 'x = mctx.read(mctx.path(Label("//:data.txt")))',
    'read_rel': 'x = mctx.read("data.txt")',
    'read_auto': 'x = mctx.read(mctx.path(Label("//:data.txt")), watch="auto")',
    'read_yes': 'x = mctx.read(mctx.path(Label("//:data.txt")), watch="yes")',
    'read_no': 'x = mctx.read(mctx.path(Label("//:data.txt")), watch="no")',
    'read_nofile': 'mctx.path(Label("//:data.txt"))',
    'watch_file': 'mctx.watch(mctx.path(Label("//:data.txt")))',
    'watch_missing': 'mctx.watch(mctx.path(Label("//:nonexistent.txt")))',
    'watch_dir': 'mctx.watch(mctx.path(Label("//:dir")))',
    'watch_twice': 'mctx.watch(mctx.path(Label("//:data.txt")))\nmctx.watch(mctx.path(Label("//:data.txt")))',
    'readdir': 'x = mctx.path(Label("//:dir")).readdir()',
    'readdir_nowatch': 'x = mctx.path(Label("//:dir")).readdir(watch="no")',
    'readdir_yes': 'x = mctx.path(Label("//:dir")).readdir(watch="yes")',
    'readdir_auto': 'x = mctx.path(Label("//:dir")).readdir(watch="auto")',
    'path_exists': 'x = mctx.path(Label("//:data.txt")).exists',
    'file_write': 'mctx.file("out.txt", "content")',
    'file_write_read': 'mctx.file("out.txt", "content")\nx = mctx.read("out.txt")',
    'file_exec': 'mctx.file("out.sh", "echo hi", executable=True)',
    'file_outside': 'mctx.file("/tmp/zel_7a_outside.txt", "content")',
    'file_legacy': 'mctx.file("out.txt", "c", legacy_utf8=False)',
    'file_empty': 'mctx.file("out.txt")',
    'file_dir_path': 'mctx.file("a/b/c.txt", "x")',
    'read_wd_missing': 'mctx.read("missing.txt")',
    'path_wd': 'print(str(mctx.path(".")))',
    'path_wd_rel': 'print(str(mctx.path("sub/x")))',
    'execute_wd': 'r = mctx.execute(["pwd"])\nprint(r.stdout.strip())',
    'execute_ok': 'r = mctx.execute(["echo", "hi"])',
    'which': 'x = mctx.which("sh")',
    'is_dev': 'x = mctx.root_module_has_non_dev_dependency',
    'dir_mctx': 'print(dir(mctx))',
    'facts_empty': 'print(mctx.facts)',
    'facts_type': 'print(type(mctx.facts))',
    'tag_sort': 'x = [mctx.tag_sort_key(t) for m in mctx.modules for t in m.tags.mk]',
    'report': 'mctx.report_progress("hello")',
    'report_empty': 'mctx.report_progress()',
    'wd_exists': 'print(mctx.path(".").exists)',
    'wd_basename': 'print(mctx.path(".").basename)',
}
mc_env = {'ZEL_7A_SET': 'val', 'ZEL_7A_SPACE': 'a b  c', 'ZEL_7A_NL': 'a\nb', 'ZEL_7A_BS': 'a\\b', 'ZEL_7A_EMPTY': ''}
for k, body in MC.items():
    for sub in (('lock', ['mod', 'deps']), ('fetch', ['fetch', '--repo=@gen'])):
        C(f'mc_{k}_{sub[0]}', sub[1], ROOT_EXT_TAG('e.mk(name="a")'), files=dict(WS_FILES, **{'ext.bzl': ext_with(body)}),
          flags=['--lockfile_mode=update'], jcmp=['MODULE.bazel.lock'], env=mc_env, dbg=True)

META = {
    'meta_repro': 'return mctx.extension_metadata(reproducible=True)',
    'meta_deps': 'return mctx.extension_metadata(root_module_direct_deps=["gen"])',
    'meta_deps_dev': 'return mctx.extension_metadata(root_module_direct_deps=["gen"], root_module_direct_dev_deps=[])',
    'meta_dev_only': 'return mctx.extension_metadata(root_module_direct_deps=[], root_module_direct_dev_deps=["gen"])',
    'meta_all': 'return mctx.extension_metadata(root_module_direct_deps="all")',
    'meta_all_dev': 'return mctx.extension_metadata(root_module_direct_dev_deps="all")',
    'meta_all_both': 'return mctx.extension_metadata(root_module_direct_deps="all", root_module_direct_dev_deps="all")',
    'meta_facts': 'return mctx.extension_metadata(facts={"a": 1})',
    'meta_facts_repro': 'return mctx.extension_metadata(facts={"a": [1, 2]}, reproducible=True)',
    'meta_facts_deps': 'return mctx.extension_metadata(facts={"k": "v"}, root_module_direct_deps=["gen"], root_module_direct_dev_deps=[])',
    'meta_overlap': 'return mctx.extension_metadata(root_module_direct_deps=["gen"], root_module_direct_dev_deps=["gen"])',
    'meta_missing': 'return mctx.extension_metadata(root_module_direct_deps=["nonexist"])',
    'meta_bad_all': 'return mctx.extension_metadata(root_module_direct_deps="some")',
    'meta_none': 'return mctx.extension_metadata()',
    'meta_empty_both': 'return mctx.extension_metadata(root_module_direct_deps=[], root_module_direct_dev_deps=[])',
}
for k, body in META.items():
    for sub in (('lock', ['mod', 'deps']), ('fetch', ['fetch', '--repo=@gen'])):
        C(f'mx_{k}_{sub[0]}', sub[1], ROOT_EXT, files=dict(WS_FILES, **{'ext.bzl': ext_with('', meta='    ' + body)}),
          flags=['--lockfile_mode=update'], jcmp=['MODULE.bazel.lock'], dbg=True)

LBL = {
    'attr_label': ('e.mk(name="a", lbl="//:data.txt")', 'x = [t.lbl for m in mctx.modules for t in m.tags.mk]'),
    'attr_label_path': ('e.mk(name="a", lbl="//:data.txt")', 'x = [mctx.read(mctx.path(t.lbl)) for m in mctx.modules for t in m.tags.mk]'),
    'attr_labels': ('e.mk(name="a", ls=["//:data.txt", "//dir:a.txt"])', 'x = [t.ls for m in mctx.modules for t in m.tags.mk]'),
    'attr_str': ('e.mk(name="a", s="v")', 'x = [t.s for m in mctx.modules for t in m.tags.mk]'),
    'label_call': ('e.mk(name="a")', 'x = Label("//:data.txt")'),
    'label_call_dep': ('e.mk(name="a")', 'x = Label("@mx//:x")'),
    'label_call_bt': ('e.mk(name="a")', 'x = Label("@bazel_tools//:x")'),
}
for k, (tag, body) in LBL.items():
    mf = mm('bazel_dep(name="mx", version="2.0")\ne = use_extension("//:ext.bzl", "ex")\n' + tag + '\nuse_repo(e, "gen")\n')
    for sub in (('lock', ['mod', 'deps']), ('fetch', ['fetch', '--repo=@gen'])):
        C(f'lbl_{k}_{sub[0]}', sub[1], mf, files=dict(WS_FILES, **{'ext.bzl': ext_with(body)}),
          flags=['--lockfile_mode=update'], jcmp=['MODULE.bazel.lock'], dbg=True)

LOADS = {
    'load_dep': 'load("@mx//:ext.bzl", _x = "ex")\n',
    'load_bt': 'load("@bazel_tools//tools/build_defs/repo:utils.bzl", "maybe")\n',
    'load_local': 'load("//:helper.bzl", "H")\n',
    'load_local_chain': 'load("//:helper2.bzl", "H2")\n',
    'load_rel': 'load(":helper.bzl", "H")\n',
}
for k, ld in LOADS.items():
    mf = mm('bazel_dep(name="mx", version="2.0")\ne = use_extension("//:ext.bzl", "ex")\nuse_repo(e, "gen")\n')
    for sub in (('lock', ['mod', 'deps']), ('fetch', ['fetch', '--repo=@gen'])):
        C(f'ld_{k}_{sub[0]}', sub[1], mf,
          files=dict(WS_FILES, **{'ext.bzl': ext_with('x = 1', extra_load=ld), 'helper.bzl': 'H = 1\n', 'helper2.bzl': 'load(":helper.bzl", "H")\nH2 = H\n'}),
          flags=['--lockfile_mode=update'], jcmp=['MODULE.bazel.lock'], dbg=True)


# ---------------- registry URL validation / builtin closure / spec errors ----------------
REG_ROOT = mm('bazel_dep(name="ma", version="1.0")\n')
GOOD = 'file://' + VBZ + '/reg'
reg_urls = {
    'ftp': 'ftp://x', 'bare': VBZ + '/reg', 'empty': '', 'relfile': 'file:reg', 'filehost': 'file://reg', 'httpempty': 'http://',
    'httpscolon': 'https:', 'upperfile': 'FILE://' + VBZ + '/reg', 'foocolon': 'foo:bar', 'ipv6': 'http://[bad', 'nonexist': 'file:///nonexistent',
    'space': 'file:///tmp/a b', 'pct': 'file:///tmp/a%zz', 'colon0': ':x', 'one': 'x', 'dots': '../reg', 'slashes': '//x/y',
    'localhost': 'file://localhost' + VBZ + '/reg', 'trailing': GOOD + '/', 'ok_then_bad': None,
}
for k, u in reg_urls.items():
    for cmd in (['mod', 'graph'], ['fetch', '--all'], ['mod', 'show_repo', 'ma']):
        if u is None:
            args = cmd + ['--registry=' + GOOD, '--registry=ftp://x']
        else:
            args = cmd + ['--registry=' + u]
        C(f'reg_{k}_{cmd[0]}{len(cmd)}', args, REG_ROOT, noreg=True, flags=[])
# default-registry-less builds with an empty registry: builtin closure modules missing (skew: names the module of HEAD's closure)
for cmd in (['mod', 'graph'], ['fetch', '--all'], ['mod', 'deps'], ['mod', 'graph', '--include_builtin']):
    C(f'reg_empty_closure_{cmd[0]}{len(cmd)}', cmd + ['--registry=file://' + VBZ + '/emptyreg'], mm(''), noreg=True, flags=[], skew='allowlist #2: HEAD closure names bazel_skylib first')
# spec errors
for mod in ('nosrc', 'badsrc', 'noint', 'badmeta', 'badname', 'badver', 'emptyver', 'malf', 'ar_badint'):
    for cmd in (['mod', 'show_repo', mod], ['fetch', '--repo=@' + mod], ['mod', 'graph']):
        C(f'regspec_{mod}_{cmd[0]}{len(cmd)}', cmd, mm(f'bazel_dep(name="{mod}", version="1.0")\n'), flags=[])

# ---------------- downloader config ----------------
import hashlib as _h
try:
    HH = _h.sha256(open(VBZ + '/www/hello.txt', 'rb').read()).hexdigest()
except Exception:
    HH = '0' * 64
URL = f'http://127.0.0.1:{PORT}'
RB = '''def _i(rctx):
    rctx.file("BUILD.bazel", "")
    r = rctx.download(url=rctx.attr.u, output="o.txt", sha256="%s", allow_fail=True)
    print("R", r.success)
t = repository_rule(_i, attrs={"u": attr.string()})
''' % HH
DMOD = 'module(name="root")\nt=use_repo_rule("//:r.bzl","t")\nt(name="t",u="%s/hello.txt")\n' % URL
dl_cfgs = {
    'empty': '', 'comment': '# nothing\n\n', 'bad_word': 'frobnicate x\n', 'rewrite1': 'rewrite a\n', 'rewrite3': 'rewrite a b c\n', 'allow2': 'allow a b\n',
    'block0': 'block\n', 'allow0': 'allow\n', 'blocked_msg0': 'all_blocked_message\n', 'blocked_msg2': 'all_blocked_message a\nall_blocked_message b\n',
    'badregex': 'rewrite ( x\n', 'badurlenc': 'rewrite a b${urlencode(x)}\n', 'goodurlenc': 'rewrite 127.0.0.1:PORT/(.*) 127.0.0.1:PORT/${urlencode($1)}\n',
    'block_all': 'block *\n', 'block_all_msg': 'all_blocked_message custom msg here\nblock *\n', 'block_host': 'block 127.0.0.1\n',
    'allow_block_all': 'allow 127.0.0.1\nblock *\n', 'block_allow_all': 'block *\nallow 127.0.0.1\n', 'allow_upper': 'allow 127.0.0.1\nblock *\n'.upper().replace('ALLOW', 'allow').replace('BLOCK', 'block'),
    'block_other': 'block example.com\n', 'rewrite_same': 'rewrite 127.0.0.1:PORT/(.*) 127.0.0.1:PORT/$1\n',
    'rewrite_catchall': 'rewrite 127.0.0.1:PORT/(.*) 127.0.0.1:PORT/hello.txt\n', 'rewrite_miss': 'rewrite 127.0.0.1:PORT/nothing(.*) 127.0.0.1:1/$1\n',
    'rewrite_bad_host': 'rewrite 127.0.0.1:PORT/(.*) 127.0.0.1:1/$1\n', 'rewrite_two': 'rewrite 127.0.0.1:PORT/(.*) 127.0.0.1:1/$1\nrewrite 127.0.0.1:PORT/(.*) 127.0.0.1:PORT/$1\n',
    'rewrite_block': 'rewrite 127.0.0.1:PORT/(.*) example.invalid/$1\nblock example.invalid\n', 'rewrite_full': 'rewrite 127.0.0.1:PORT/hello.txt 127.0.0.1:PORT/hello.txt\n',
    'crlf': 'block example.com\r\n', 'tabs': 'block\texample.com\n', 'indented': '   block example.com\n', 'trailing_comment': 'block example.com # c\n',
}
for k, cfg in dl_cfgs.items():
    cfg = cfg.replace('PORT', PORT)
    for fl, tag in ((['--downloader_config=dl.cfg'], 'n'), (['--experimental_downloader_config=dl.cfg'], 'o')):
        if tag == 'o' and k not in ('empty', 'bad_word', 'block_all', 'allow_block_all', 'rewrite_catchall'):
            continue
        skew = {'badurlenc': 'allowlist #16 (HEAD UrlRewriterConfig validates urlencode syntax)', 'rewrite_bad_host': 'allowlist #17 (HEAD does not retry ConnectException)',
                'rewrite_two': 'allowlist #17'}.get(k)
        cases.append(dict(name=f'dlc_{k}_{tag}', args=['fetch', '--repo=@t'], **({'skew': skew} if skew else {}), files={'MODULE.bazel': DMOD, 'r.bzl': RB, 'BUILD.bazel': '', 'dl.cfg': cfg},
                          flags=fl, dbg=True, tree=['external/+t+t']))
for k, fl in {'missing': ['--downloader_config=nofile.cfg'], 'empty_path': ['--downloader_config='], 'two': ['--downloader_config=dl.cfg', '--downloader_config=dl2.cfg'],
              'two_empty': ['--downloader_config=dl.cfg', '--downloader_config='], 'abs': ['--downloader_config=' + VBZ + '/nonexistent.cfg'],
              'twomissing': ['--downloader_config=a.cfg', '--downloader_config=b.cfg']}.items():
    for cmd in (['fetch', '--repo=@t'], ['mod', 'graph'], ['info', 'release']):
        cases.append(dict(name=f'dlc2_{k}_{cmd[0]}{len(cmd)}', args=cmd, **({'skew': 'allowlist #21 (9.3.2 honours only the last --downloader_config)'} if k in ('two', 'two_empty', 'twomissing') else {}), files={'MODULE.bazel': DMOD, 'r.bzl': RB, 'BUILD.bazel': '', 'dl.cfg': 'block example.com\n', 'dl2.cfg': 'block *\n'},
                          flags=fl, dbg=True))

# ---------------- default repository cache ----------------
CRB = '''def _i(rctx):
    rctx.file("BUILD.bazel", "")
    r = rctx.download(url=rctx.attr.u, output="o.txt", sha256="%s", allow_fail=True)
    print("R", r.success)
t = repository_rule(_i, attrs={"u": attr.string()})
''' % HH
CMOD = 'module(name="root")\nt=use_repo_rule("//:r.bzl","t")\nt(name="t",u="%s/hello.txt")\nt(name="u",u="http://127.0.0.1:1/hello.txt")\nt(name="w",u="https://example.invalid/hello.txt")\n' % URL
for k, flags in {'default': [], 'explicit_empty': ['--repository_cache='], 'explicit_dir': ['--repository_cache=cachedir'], 'disable_dl': ['--repository_disable_download'],
                 'distdir': ['--distdir=dd']}.items():
    for repo in ('@u', '@w'):
        cases.append(dict(name=f'rc_{k}_{repo[1]}', args=['fetch', '--repo=' + repo], **({'skew': 'allowlist #17 (HEAD does not retry ConnectException)'} if repo == '@u' else {}), pre=[['fetch', '--repo=@t']] if k != 'explicit_empty' else [], shared_cache=(k == 'default'),
                          files={'MODULE.bazel': CMOD, 'r.bzl': CRB, 'BUILD.bazel': '', 'dd/hello.txt': 'hello download\n'}, flags=flags, dbg=True, tree=['external/+t+u']))

# ---------------- vendor marker (compared byte for byte) ----------------
VMOD = 'module(name="root")\nt=use_repo_rule("//:r.bzl","t")\nt(name="t",u="%s/hello.txt")\nu=use_repo_rule("//:r2.bzl","u")\nu(name="v", a="x", b=["1","2"], c={"k":"v"}, d=True, e=3)\n' % URL
R2 = '''def _i(rctx):
    rctx.file("BUILD.bazel", "")
    rctx.file("v.txt", "%s" % rctx.getenv("ZEL_7A_SET", "none"))
u = repository_rule(_i, attrs={"a": attr.string(), "b": attr.string_list(), "c": attr.string_dict(), "d": attr.bool(), "e": attr.int()}, environ=["ZEL_7A_SET"])
'''
for k, args in {'one': ['vendor', '--repo=@v', '--vendor_dir=vd'], 'both': ['vendor', '--repo=@t', '--repo=@v', '--vendor_dir=vd']}.items():
    for envs in ({}, {'ZEL_7A_SET': 'x y'}):
        cases.append(dict(name=f'vm_{k}_{"env" if envs else "noenv"}', args=args, env=envs,
                          files={'MODULE.bazel': VMOD, 'r.bzl': RB, 'r2.bzl': R2, 'BUILD.bazel': ''},
                          skew='allowlist #13 (marker version/semantics/attr serialization differ in 9.3.2)',
                          cmp_exact=['vd/@+u+v.marker', 'vd/@+t+t.marker'], tree=['external/+u+v'], cmp=['vd']))
if __name__ == '__main__':
    print(len(cases))
    main(cases, jobs=6)
