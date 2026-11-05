"""Differential cases for the real @bazel_tools//tools/build_defs/repo/*.bzl repo rules (run unmodified on zel's Starlark)
vs real bazel: http_archive/http_file/http_jar, git_repository/new_git_repository, local_repository/new_local_repository,
utils.bzl helpers (parse_netrc, use_netrc, get_auth, patch, update_attrs, workspace_and_buildfile, maybe, ...),
cache.bzl, jvm.bzl, java.bzl.  Local fixtures only (file:// archives, local git repos, local python http server).

  python3 setup_native.py   # (optional; cases_native.py calls it at import)
  ZEL_BIN=... python3 cases_native.py [name-substr...]
"""
from hconf import VBZ, PORT
from harness import *
import hashlib, base64, textwrap, os, subprocess
import setup_native

GN = setup_native.build()
URL = 'http://127.0.0.1:' + PORT
cases = []


def SHA(p): return hashlib.sha256(open(p, 'rb').read()).hexdigest()
def SRI(p): return 'sha256-' + base64.b64encode(hashlib.sha256(open(p, 'rb').read()).digest()).decode()
def SRI512(p): return 'sha512-' + base64.b64encode(hashlib.sha512(open(p, 'rb').read()).digest()).decode()
def SRI1(p): return 'sha1-' + base64.b64encode(hashlib.sha1(open(p, 'rb').read()).digest()).decode()


A = VBZ + '/arch/'
FU = lambda n: 'file://' + A + n
TGZ, ZIP = SHA(A + 'small.tar.gz'), SHA(A + 'small.zip')
HELLO = SHA(VBZ + '/www/hello.txt')
SECRET = hashlib.sha256(b'secret\n').hexdigest()
gitg = lambda *a: setup_native.git(GN, *a)
C1, C2, C3 = gitg('rev-parse', 'v1^{commit}'), gitg('rev-parse', 'v2^{commit}'), gitg('rev-parse', 'HEAD')
FILES = {'ok.patch': open(VBZ + '/www/ok.patch').read(), 'bad.patch': open(VBZ + '/www/bad.patch').read(),
         'bf.BUILD': 'filegroup(name="all",srcs=glob(["**"]))\n', 'ws.txt': 'workspace(name="zz")\n',
         'local/a.txt': 'la\n', 'local/sub/b.txt': 'lb\n', 'local/BUILD.bazel': 'filegroup(name="l")\n',
         'netrc_ws': 'machine 127.0.0.1 login user password pass\n', 'sub/pkg/BUILD.bazel': '', 'sub/pkg/bf2.BUILD': 'x\n'}


def N(name, mod, files=None, args=None, flags=None, env=None, pre=None, tree=None, **kw):
    f = {'MODULE.bazel': 'module(name="root")\n' + mod}
    f.update(FILES)
    f.update(files or {})
    cases.append(dict(name='n_' + name, args=args or ['fetch', '--repo=@t'], files=f, flags=flags or [], env=env, dbg=True,
                      tree=tree if tree is not None else ['external/*+t'], pre=pre or [], cmp=kw.pop('cmp', []), jcmp=kw.pop('jcmp', []), **kw))


HA = 'http_archive = use_repo_rule("@bazel_tools//tools/build_defs/repo:http.bzl","http_archive")\n'
HF = 'http_file = use_repo_rule("@bazel_tools//tools/build_defs/repo:http.bzl","http_file")\n'
HJ = 'http_jar = use_repo_rule("@bazel_tools//tools/build_defs/repo:http.bzl","http_jar")\n'
GR = 'git_repository = use_repo_rule("@bazel_tools//tools/build_defs/repo:git.bzl","git_repository")\nnew_git_repository = use_repo_rule("@bazel_tools//tools/build_defs/repo:git.bzl","new_git_repository")\n'
LR = 'local_repository = use_repo_rule("@bazel_tools//tools/build_defs/repo:local.bzl","local_repository")\nnew_local_repository = use_repo_rule("@bazel_tools//tools/build_defs/repo:local.bzl","new_local_repository")\n'
q = lambda s: '"' + s.replace('\\', '\\\\').replace('"', '\\"') + '"'
ha = lambda attrs, **kw: N('ha_' + kw.pop('n'), HA + f'http_archive(name="t",{attrs})\n', **kw)


# ------------------------------------------------------------------ http_archive: formats x checksum styles
fmts = [('tgz', 'small.tar.gz'), ('zip', 'small.zip'), ('bz2', 'small.tar.bz2'), ('7z', 'small.7z'), ('deb', 'pkg_gzip.deb')]
for k, f in fmts:
    if not os.path.exists(A + f):
        continue
    for style, ck in (('sha', f'sha256="{SHA(A + f)}"'), ('sri', f'integrity="{SRI(A + f)}"'), ('sri512', f'integrity="{SRI512(A + f)}"'),
                      ('sri1', f'integrity="{SRI1(A + f)}"'), ('none', ''), ('badsha', 'sha256="' + '0' * 64 + '"'), ('upper', f'sha256="{SHA(A + f).upper()}"')):
        ha(f'url="{FU(f)}"' + (',' + ck if ck else '') + (',strip_prefix="top"' if k in ('tgz', 'zip', 'bz2', '7z') else ''), n=f'fmt_{k}_{style}')
# strip / prefix
for n, a in [('strip', 'strip_prefix="top"'), ('strip_bad', 'strip_prefix="zzz"'), ('strip_dot', 'strip_prefix="./top"'), ('strip_slash', 'strip_prefix="top/"'),
             ('strip_deep', 'strip_prefix="top/b"'), ('strip_file', 'strip_prefix="top/a.txt"'), ('add', 'add_prefix="pre"'), ('add_deep', 'add_prefix="a/b/c"'),
             ('strip_add', 'strip_prefix="top",add_prefix="pre/fix"'), ('comp1', 'strip_components=1'), ('comp2', 'strip_components=2'), ('comp9', 'strip_components=9'),
             ('comp_neg', 'strip_components=-1'), ('comp_strip', 'strip_components=1,strip_prefix="top"'), ('add_abs', 'add_prefix="/abs"'), ('add_dotdot', 'add_prefix="../x"'),
             ('type_zip_on_gz', 'type="zip"'), ('type_tgz', 'type="tgz"'), ('type_tar_gz', 'type="tar.gz"'), ('type_bad', 'type="rar"'), ('type_empty', 'type=""'),
             ('type_dot', 'type=".tar.gz"'), ('type_upper', 'type="TAR.GZ"'), ('canon', 'canonical_id="abc"'), ('canon_empty', 'canonical_id=""')]:
    ha(f'url="{FU("small.tar.gz")}",sha256="{TGZ}",{a}', n=n)
# url / urls forms
for n, a in [('urls_only', f'urls=["{FU("small.tar.gz")}"],sha256="{TGZ}"'), ('url_urls', f'url="{FU("small.tar.gz")}",urls=["{FU("small.tar.gz")}"],sha256="{TGZ}"'),
             ('urls_fallback', f'urls=["file:///nonexistent/x.tar.gz","{FU("small.tar.gz")}"],sha256="{TGZ}"'), ('no_url', f'sha256="{TGZ}"'),
             ('urls_empty', f'urls=[],sha256="{TGZ}"'), ('url_empty', f'url="",sha256="{TGZ}"'), ('url_bad_scheme', f'url="ftp://x/y.tar.gz",sha256="{TGZ}"'),
             ('url_missing_file', f'url="file:///nonexistent/x.tar.gz",sha256="{TGZ}"'), ('url_missing_nosha', 'url="file:///nonexistent/x.tar.gz"'),
             ('sha_integrity', f'url="{FU("small.tar.gz")}",sha256="{TGZ}",integrity="{SRI(A + "small.tar.gz")}"'),
             ('sha_short', f'url="{FU("small.tar.gz")}",sha256="abc"'), ('sri_bad', f'url="{FU("small.tar.gz")}",integrity="sha256-xx"'),
             ('sri_nodash', f'url="{FU("small.tar.gz")}",integrity="sha256{TGZ}"'), ('sri_unknown_algo', f'url="{FU("small.tar.gz")}",integrity="md5-AAAA"'),
             ('http_404', f'url="{URL}/zz.tar.gz",sha256="{TGZ}"'), ('http_ok', f'url="{URL}/small.tar.gz",sha256="{TGZ}"'),
             ('http_redir', f'url="{URL}/redir",sha256="{HELLO}",type="tar.gz"'), ('http_500', f'url="{URL}/err500",sha256="{TGZ}"')]:
    ha(a, n=n)
# unknown / wrong-typed attrs
for n, a in [('foo', 'foo="x"'), ('rename_files', 'rename_files={"a":"b"}'), ('strip_int', 'strip_prefix=1'), ('urls_str', 'urls="x"'),
             ('patches_str', 'patches="x"'), ('patch_strip_str', 'patch_strip="1"'), ('build_file_int', 'build_file=1'),
             ('build_file_bad_label', 'build_file="//:bad::label"'), ('type_int', 'type=1'), ('strip_components_str', 'strip_components="1"'),
             ('remote_patches_list', 'remote_patches=["x"]'), ('auth_patterns_list', 'auth_patterns=["x"]'), ('netrc_int', 'netrc=1'), ('repo_mapping', 'repo_mapping={"@a":"@b"}'),
             ('files_attr', 'files={"x":"//:bf.BUILD"}'), ('integrity_none', 'integrity=None')]:
    ha(f'url="{FU("small.tar.gz")}",sha256="{TGZ}",{a}', n='bad_' + n)
# build_file / workspace_file handling
bf = f'url="{FU("small.tar.gz")}",sha256="{TGZ}",strip_prefix="top",'
for n, a in [('bf', 'build_file="//:bf.BUILD"'), ('bf_at', 'build_file="@//:bf.BUILD"'), ('bf_atat', 'build_file="@@//:bf.BUILD"'), ('bf_colon', 'build_file=":bf.BUILD"'),
             ('bf_rel', 'build_file="bf.BUILD"'), ('bf_pkg', 'build_file="//sub/pkg:bf2.BUILD"'), ('bf_missing', 'build_file="//:nope"'), ('bf_dir', 'build_file="//local:BUILD.bazel"'),
             ('bf_ext', 'build_file="@bazel_tools//tools/build_defs/repo:BUILD.repo"'), ('bf_bad_repo', 'build_file="@nonexistent//:x"'),
             ('bfc', 'build_file_content="filegroup(name=\'z\')\\n"'), ('bfc_empty', 'build_file_content=""'), ('bfc_multi', 'build_file_content="a\\nb\\n\\n"'),
             ('bfc_unicode', 'build_file_content="# \u00fc\u20ac\\n"'), ('bf_both', 'build_file="//:bf.BUILD",build_file_content="x"'),
             ('ws', 'workspace_file="//:ws.txt"'), ('wsc', 'workspace_file_content="workspace(name=\'q\')\\n"'), ('ws_both', 'workspace_file="//:ws.txt",workspace_file_content="x"'),
             ('ws_missing', 'workspace_file="//:nope"'), ('bf_ws', 'build_file="//:bf.BUILD",workspace_file="//:ws.txt"'),
             ('bfc_ws', 'build_file_content="x",workspace_file_content="y"')]:
    N('ha_' + n, HA + f'http_archive(name="t",{bf}{a})\n')
# patches
for n, a in [('p', 'patches=["//:ok.patch"],patch_args=["-p1"]'), ('p_default', 'patches=["//:ok.patch"]'), ('p_strip0', 'patches=["//:ok.patch"],patch_args=["-p0"]'),
             ('p_strip2', 'patches=["//:ok.patch"],patch_args=["-p2"]'), ('p_bad', 'patches=["//:bad.patch"],patch_args=["-p1"]'), ('p_bad_default', 'patches=["//:bad.patch"]'),
             ('p_missing', 'patches=["//:nope.patch"]'), ('p_twice', 'patches=["//:ok.patch","//:ok.patch"],patch_args=["-p1"]'),
             ('p_tool', 'patches=["//:ok.patch"],patch_tool="patch",patch_args=["-p1"]'), ('p_tool_bad', 'patches=["//:ok.patch"],patch_tool="zz_no_tool",patch_args=["-p1"]'),
             ('p_tool_nostrip', 'patches=["//:ok.patch"],patch_tool="patch"'), ('p_args_multi', 'patches=["//:ok.patch"],patch_args=["-p1","--verbose"]'),
             ('p_args_dry', 'patches=["//:ok.patch"],patch_args=["-p1","--dry-run"]'), ('p_args_bad', 'patches=["//:ok.patch"],patch_args=["--zzz"]'),
             ('p_args_fuzz', 'patches=["//:ok.patch"],patch_args=["-p1","-F3"]'), ('p_strip_attr_only', 'patches=["//:ok.patch"],patch_args=["-p","1"]'),
             ('cmds', 'patch_cmds=["echo cmd > cmdout.txt","mkdir -p gen"]'), ('cmds_fail', 'patch_cmds=["exit 3"]'), ('cmds_stderr', 'patch_cmds=["echo err >&2; exit 1"]'),
             ('cmds_win', 'patch_cmds_win=["echo no"]'), ('cmds_both', 'patch_cmds=["echo a > a1"],patch_cmds_win=["echo b > b1"]'),
             ('cmds_after_patch', 'patches=["//:ok.patch"],patch_args=["-p1"],patch_cmds=["cat a.txt > seen.txt"]'),
             ('cmds_env', 'patch_cmds=["echo $PWD > pwd.txt"]'), ('cmds_empty', 'patch_cmds=[]'), ('cmds_chmod', 'patch_cmds=["chmod 600 a.txt"]'),
             ('cmds_rm', 'patch_cmds=["rm -rf b"]'), ('p_cmd_order', 'patches=["//:ok.patch"],patch_args=["-p1"],patch_cmds=["echo $(cat a.txt) >> a.txt"]'),
             ('remote_patch', f'remote_patches={{"{URL}/ok.patch":"{SRI(VBZ + "/www/ok.patch")}"}},remote_patch_strip=1'),
             ('remote_patch_nostrip', f'remote_patches={{"{URL}/ok.patch":"{SRI(VBZ + "/www/ok.patch")}"}}'),
             ('remote_patch_badint', f'remote_patches={{"{URL}/ok.patch":"{SRI(VBZ + "/www/hello.txt")}"}},remote_patch_strip=1'),
             ('remote_patch_404', f'remote_patches={{"{URL}/zz.patch":"{SRI(VBZ + "/www/ok.patch")}"}},remote_patch_strip=1'),
             ('remote_patch_noint', f'remote_patches={{"{URL}/ok.patch":""}},remote_patch_strip=1'),
             ('remote_patch_file', f'remote_patches={{"file://{VBZ}/www/ok.patch":"{SRI(VBZ + "/www/ok.patch")}"}},remote_patch_strip=1'),
             ('remote_patch_file_noint', f'remote_patches={{"file://{VBZ}/www/ok.patch":""}},remote_patch_strip=1'),
             ('remote_patch_and_local', f'patches=["//:ok.patch"],remote_patches={{"file://{VBZ}/www/ok.patch":"{SRI(VBZ + "/www/ok.patch")}"}},remote_patch_strip=1,patch_args=["-p1"]'),
             ('remote_files', f'remote_file_urls={{"extra.txt":["file://{VBZ}/www/hello.txt"]}},remote_file_integrity={{"extra.txt":"{SRI(VBZ + "/www/hello.txt")}"}}'),
             ('remote_files_noint', f'remote_file_urls={{"extra.txt":["file://{VBZ}/www/hello.txt"]}}'),
             ('remote_files_badint', f'remote_file_urls={{"extra.txt":["file://{VBZ}/www/hello.txt"]}},remote_file_integrity={{"extra.txt":"{SRI(VBZ + "/www/ok.patch")}"}}'),
             ('remote_files_sub', f'remote_file_urls={{"x/y/z.txt":["file://{VBZ}/www/hello.txt"]}},remote_file_integrity={{"x/y/z.txt":"{SRI(VBZ + "/www/hello.txt")}"}}'),
             ('remote_files_escape', f'remote_file_urls={{"../z.txt":["file://{VBZ}/www/hello.txt"]}},remote_file_integrity={{"../z.txt":"{SRI(VBZ + "/www/hello.txt")}"}}'),
             ('remote_files_overwrite', f'remote_file_urls={{"a.txt":["file://{VBZ}/www/hello.txt"]}},remote_file_integrity={{"a.txt":"{SRI(VBZ + "/www/hello.txt")}"}}'),
             ('remote_files_multi', f'remote_file_urls={{"e1":["file://{VBZ}/www/hello.txt"],"e2":["file:///nonexistent","file://{VBZ}/www/hello.txt"]}},remote_file_integrity={{"e1":"{SRI(VBZ + "/www/hello.txt")}","e2":"{SRI(VBZ + "/www/hello.txt")}"}}'),
             ('remote_files_extra_int', f'remote_file_urls={{"e1":["file://{VBZ}/www/hello.txt"]}},remote_file_integrity={{"e1":"{SRI(VBZ + "/www/hello.txt")}","e9":"{SRI(VBZ + "/www/hello.txt")}"}}'),
             ('remote_module_file', f'remote_module_file_urls=["file://{VBZ}/www/hello.txt"],remote_module_file_integrity="{SRI(VBZ + "/www/hello.txt")}"'),
             ('remote_module_file_noint', f'remote_module_file_urls=["file://{VBZ}/www/hello.txt"]')]:
    N('ha_' + n, HA + f'http_archive(name="t",{bf}{a})\n')
# overlay / existing files in the archive
N('ha_overlay_over_buildfile', HA + f'http_archive(name="t",{bf}build_file_content="x",patch_cmds=["rm -f BUILD.bazel; echo late > BUILD.bazel"])\n')

# ------------------------------------------------------------------ auth / netrc via the local server (/auth wants Basic user:pass)
HFA = lambda attrs: HF + f'http_file(name="t",url="{URL}/auth",sha256="{SECRET}",{attrs})\n'
for n, a, kw in [
        ('none', '', {}),
        ('netrc_attr', 'netrc="' + VBZ + '/netrc_basic"', {}), ('netrc_default', 'netrc="' + VBZ + '/netrc_default"', {}), ('netrc_bad', 'netrc="' + VBZ + '/netrc_bad"', {}),
        ('netrc_missing', 'netrc="' + VBZ + '/nonexistent"', {}), ('netrc_empty_attr', 'netrc=""', {}),
        ('auth_patterns_basic', 'netrc="' + VBZ + '/netrc_basic",auth_patterns={"127.0.0.1":"Basic <login>:<password>"}', {}),
        ('auth_patterns_bearer', 'netrc="' + VBZ + '/netrc_basic",auth_patterns={"127.0.0.1":"Bearer <password>"}', {}),
        ('auth_patterns_nonetrc', 'auth_patterns={"127.0.0.1":"Basic <login>"}', {}),
        ('auth_patterns_literal', 'netrc="' + VBZ + '/netrc_basic",auth_patterns={"127.0.0.1":"Basic dXNlcjpwYXNz"}', {}),
        ('auth_patterns_nomatch', 'netrc="' + VBZ + '/netrc_basic",auth_patterns={"example.com":"Bearer <password>"}', {}),
        ('auth_patterns_badtype', 'auth_patterns={"127.0.0.1":1}', {}),
        ('env_netrc', '', {'flags': ['--repo_env=NETRC=' + VBZ + '/netrc_basic']}),
        ('env_netrc_missing', '', {'flags': ['--repo_env=NETRC=' + VBZ + '/nonexistent']}),
        ('env_netrc_bad', '', {'flags': ['--repo_env=NETRC=' + VBZ + '/netrc_bad']}),
        ('env_home_netrc', '', {'flags': ['--repo_env=HOME=' + VBZ + '/hnetrc']}),
        ('attr_beats_env', 'netrc="' + VBZ + '/netrc_basic"', {'flags': ['--repo_env=NETRC=' + VBZ + '/netrc_bad']})]:
    N('hf_auth_' + n, HFA(a), **kw)
os.makedirs(VBZ + '/hnetrc', exist_ok=True)
open(VBZ + '/hnetrc/.netrc', 'w').write('machine 127.0.0.1 login user password pass\n')
N('ha_netrc_ws', HA + f'http_archive(name="t",url="{URL}/small.tar.gz",sha256="{TGZ}",netrc="{VBZ}/netrc_basic")\n')

# ------------------------------------------------------------------ http_file
hf = lambda attrs, n, **kw: N('hf_' + n, HF + f'http_file(name="t",{attrs})\n', **kw)
HU = f'url="{FU("../www/hello.txt")}"'
for n, a in [('basic', f'url="{URL}/hello.txt",sha256="{HELLO}"'), ('exec', f'url="{URL}/hello.txt",sha256="{HELLO}",executable=True'), ('noexec', f'url="{URL}/hello.txt",sha256="{HELLO}",executable=False'),
             ('path', f'url="{URL}/hello.txt",sha256="{HELLO}",downloaded_file_path="sub/x.txt"'), ('path_deep', f'url="{URL}/hello.txt",sha256="{HELLO}",downloaded_file_path="a/b/c/x.txt"'),
             ('path_dotdot', f'url="{URL}/hello.txt",sha256="{HELLO}",downloaded_file_path="../x"'), ('path_abs', f'url="{URL}/hello.txt",sha256="{HELLO}",downloaded_file_path="/abs"'),
             ('path_empty', f'url="{URL}/hello.txt",sha256="{HELLO}",downloaded_file_path=""'), ('path_ws', f'url="{URL}/hello.txt",sha256="{HELLO}",downloaded_file_path="WORKSPACE"'),
             ('path_build', f'url="{URL}/hello.txt",sha256="{HELLO}",downloaded_file_path="BUILD.bazel"'), ('path_build2', f'url="{URL}/hello.txt",sha256="{HELLO}",downloaded_file_path="BUILD"'),
             ('path_repo', f'url="{URL}/hello.txt",sha256="{HELLO}",downloaded_file_path="REPO.bazel"'), ('path_space', f'url="{URL}/hello.txt",sha256="{HELLO}",downloaded_file_path="a b.txt"'),
             ('path_quote', f'url="{URL}/hello.txt",sha256="{HELLO}",downloaded_file_path="a\\"b.txt"'), ('path_unicode', f'url="{URL}/hello.txt",sha256="{HELLO}",downloaded_file_path="\u00fc.txt"'),
             ('path_name_file', f'url="{URL}/hello.txt",sha256="{HELLO}",downloaded_file_path="file"'), ('path_dot', f'url="{URL}/hello.txt",sha256="{HELLO}",downloaded_file_path="."'),
             ('file_url', f'url="{FU("../www/hello.txt")}"'), ('file_url_sri', f'url="{FU("../www/hello.txt")}",integrity="{SRI(VBZ + "/www/hello.txt")}"'),
             ('integrity', f'url="{URL}/hello.txt",integrity="{SRI(VBZ + "/www/hello.txt")}"'), ('badsha', f'url="{URL}/hello.txt",sha256="{"0" * 64}"'),
             ('urls', f'urls=["{URL}/zz","{URL}/hello.txt"],sha256="{HELLO}"'), ('url_urls', f'url="{URL}/hello.txt",urls=["{URL}/hello.txt"],sha256="{HELLO}"'),
             ('no_url', f'sha256="{HELLO}"'), ('url_404', f'url="{URL}/zz",sha256="{HELLO}"'), ('canon', f'url="{URL}/hello.txt",sha256="{HELLO}",canonical_id="c"'),
             ('nosha_http', f'url="{URL}/hello.txt"'), ('bad_attr', f'url="{URL}/hello.txt",sha256="{HELLO}",foo=1'), ('strip', f'url="{URL}/hello.txt",sha256="{HELLO}",strip_prefix="x"'),
             ('build_file', f'url="{URL}/hello.txt",sha256="{HELLO}",build_file="//:bf.BUILD"'), ('headers', f'url="{URL}/hello.txt",sha256="{HELLO}",headers={{"X":"y"}}'),
             ('exec_file_url', f'url="{FU("../www/hello.txt")}",executable=True,downloaded_file_path="run.sh"'), ('sri512', f'url="{FU("../www/hello.txt")}",integrity="{SRI512(VBZ + "/www/hello.txt")}"'),
             ('type_attr', f'url="{URL}/hello.txt",sha256="{HELLO}",type="zip"'), ('name_alias', f'url="{URL}/hello.txt",sha256="{HELLO}"')]:
    hf(a, n)

# ------------------------------------------------------------------ http_jar
hj = lambda attrs, n, **kw: N('hj_' + n, HJ + f'http_jar(name="t",{attrs})\n', **kw)
JU = lambda n: 'file://' + VBZ + '/jar/' + n
JS = lambda n: SHA(VBZ + '/jar/' + n)
for n, a in [('basic', f'url="{JU("x.jar")}",sha256="{JS("x.jar")}"'), ('integrity', f'url="{JU("x.jar")}",integrity="{SRI(VBZ + "/jar/x.jar")}"'), ('nosum', f'url="{JU("x.jar")}"'),
             ('urls', f'urls=["file:///zz","{JU("x.jar")}"],sha256="{JS("x.jar")}"'), ('badsha', f'url="{JU("x.jar")}",sha256="{"0" * 64}"'), ('no_url', f'sha256="{JS("x.jar")}"'),
             ('canon', f'url="{JU("x.jar")}",canonical_id="q"'), ('bad_attr', f'url="{JU("x.jar")}",foo=1'), ('exec', f'url="{JU("x.jar")}",executable=True'),
             ('file_path', f'url="{JU("x.jar")}",downloaded_file_path="y.jar"'), ('http', f'url="{URL}/hello.txt",sha256="{HELLO}"'), ('both_sums', f'url="{JU("x.jar")}",sha256="{JS("x.jar")}",integrity="{SRI(VBZ + "/jar/x.jar")}"'),
             ('sources', f'url="{JU("x.jar")}",sha256="{JS("x.jar")}",sources_url="{JU("x-sources.jar")}"'), ('404', 'url="file:///nonexistent.jar"'), ('netrc', f'url="{JU("x.jar")}",netrc="{VBZ}/netrc_basic"'),
             ('no_mod', f'url="{JU("x.jar")}",sha256="{JS("x.jar")}",build_file="//:x"')]:
    hj(a, n)

# ------------------------------------------------------------------ local_repository / new_local_repository
lr = lambda body, n, **kw: N('lr_' + n, LR + body + '\n', **kw)
for n, a in [('basic', 'path="local"'), ('dot_slash', 'path="./local"'), ('trailing', 'path="local/"'), ('abs', f'path="{VBZ}/gitnat"'), ('sub', 'path="local/sub"'), ('dotdot', 'path="local/../local"'),
             ('missing', 'path="nonexistent"'), ('file', 'path="MODULE.bazel"'), ('nopath', ''), ('bad_attr', 'path="local",foo=1'),
             ('int', 'path=1'), ('list', 'path=["a"]'), ('abs_missing', 'path="/nonexistent/x"'), ('tilde', 'path="~/x"'), ('space', 'path="local sub"'), ('build_file', 'path="local",build_file="//:bf.BUILD"'),
             ('both_workspace', 'path="local",workspace_file_content="x"'), ('links', f'path="{VBZ}/src/links"'), ('repo_mapping', 'path="local",repo_mapping={}'), ('name_only', ''), ('git_dir', f'path="{VBZ}/gitnat/subm"')]:
    lr(f'local_repository(name="t",{a})', n)
for n, a in [('basic', 'path="local",build_file_content="filegroup(name=\'a\')\\n"'), ('bf', 'path="local",build_file="//:bf.BUILD"'), ('both', 'path="local",build_file="//:bf.BUILD",build_file_content="x"'),
             ('none', 'path="local"'), ('ws', 'path="local",build_file_content="x",workspace_file_content="workspace(name=\'q\')"'), ('wsf', 'path="local",build_file_content="x",workspace_file="//:ws.txt"'),
             ('ws_both', 'path="local",build_file_content="x",workspace_file="//:ws.txt",workspace_file_content="y"'), ('missing', 'path="nonexistent",build_file_content="x"'),
             ('abs', f'path="{VBZ}/gitnat",build_file_content="x"'), ('abs_exec', f'path="{VBZ}/arch/../src/small",build_file_content="x"'), ('file', 'path="MODULE.bazel",build_file_content="x"'),
             ('nopath', 'build_file_content="x"'), ('bad_attr', 'path="local",build_file_content="x",foo=1'), ('bf_missing', 'path="local",build_file="//:nope"'), ('bf_dir', 'path="local",build_file="//local:BUILD.bazel"'),
             ('empty_content', 'path="local",build_file_content=""'), ('dot', 'path=".",build_file_content="x"'), ('over_existing_build', f'path="{VBZ}/gitnat",build_file_content="# over\\n"'),
             ('unicode', 'path="local",build_file_content="# \u00fc\\n"'), ('bf_pkg', 'path="local",build_file="//sub/pkg:bf2.BUILD"'), ('links', f'path="{VBZ}/src/links",build_file_content="x"')]:
    lr(f'new_local_repository(name="t",{a})', 'new_' + n)
N('lr_twice_fetch', LR + 'local_repository(name="t",path="local")\n', pre=[['fetch', '--repo=@t']])
N('lr_nonexistent_fetch_all', LR + 'local_repository(name="t",path="nonexistent")\n', args=['fetch', '--all'])

# ------------------------------------------------------------------ git
def G(attrs, n, rule='git_repository', **kw):
    N('git_' + n, GR + f'{rule}(name="t",remote="{GN}",{attrs})\n', **kw)


for n, a in [('c1', f'commit="{C1}"'), ('c2', f'commit="{C2}"'), ('c3', f'commit="{C3}"'), ('v1', 'tag="v1"'), ('v2', 'tag="v2"'), ('br1', 'branch="br1"'), ('main', 'branch="main"'),
             ('feat', 'branch="feat/x"'), ('refs_heads', 'branch="refs/heads/br1"'), ('noref', ''), ('bad_commit', f'commit="{"1" * 40}"'), ('short_commit', f'commit="{C1[:8]}"'),
             ('upper_commit', f'commit="{C1.upper()}"'), ('bad_tag', 'tag="nope"'), ('bad_branch', 'branch="nope"'), ('commit_tag', f'commit="{C1}",tag="v1"'), ('commit_branch', f'commit="{C1}",branch="br1"'),
             ('tag_branch', 'tag="v1",branch="br1"'), ('three_refs', f'commit="{C1}",tag="v1",branch="br1"'), ('shallow', f'commit="{C1}",shallow_since="2020-01-01"'),
             ('shallow_new', f'commit="{C1}",shallow_since="2030-01-01"'), ('shallow_bad', f'commit="{C1}",shallow_since="zzz"'), ('shallow_tag', 'tag="v1",shallow_since="2020-01-01"'),
             ('shallow_branch', 'branch="br1",shallow_since="2020-01-01"'), ('strip', f'commit="{C2}",strip_prefix="sub"'), ('strip_missing', f'commit="{C2}",strip_prefix="zzz"'),
             ('strip_deep', f'commit="{C2}",strip_prefix="sub/d"'), ('strip_file', f'commit="{C2}",strip_prefix="a.txt"'), ('strip_dot', f'commit="{C2}",strip_prefix="./sub"'),
             ('strip_slash', f'commit="{C2}",strip_prefix="sub/"'), ('verbose', f'commit="{C1}",verbose=True'), ('quiet', f'commit="{C1}",verbose=False'),
             ('init_sub', f'commit="{C3}",init_submodules=True'), ('init_sub_rec', f'commit="{C3}",init_submodules=True,recursive_init_submodules=True'), ('rec_only', f'commit="{C3}",recursive_init_submodules=True'),
             ('nosub', f'commit="{C3}"'), ('init_sub_c1', f'commit="{C1}",init_submodules=True'), ('patches', f'commit="{C1}",patches=["//:ok.patch"],patch_args=["-p1"]'),
             ('patch_bad', f'commit="{C1}",patches=["//:bad.patch"],patch_args=["-p1"]'), ('patch_cmds', f'commit="{C1}",patch_cmds=["echo hi > h.txt"]'), ('patch_cmds_fail', f'commit="{C1}",patch_cmds=["exit 2"]'),
             ('patch_cmds_win', f'commit="{C1}",patch_cmds_win=["echo no"]'), ('patch_tool', f'commit="{C1}",patches=["//:ok.patch"],patch_tool="patch",patch_args=["-p1"]'),
             ('remote_patch', f'commit="{C1}",remote_patches={{"file://{VBZ}/www/ok.patch":"{SRI(VBZ + "/www/ok.patch")}"}},remote_patch_strip=1'),
             ('remote_files', f'commit="{C1}",remote_file_urls={{"extra":["file://{VBZ}/www/hello.txt"]}},remote_file_integrity={{"extra":"{SRI(VBZ + "/www/hello.txt")}"}}'),
             ('bad_attr', f'commit="{C1}",foo=1'), ('sha_attr', f'commit="{C1}",sha256="x"'), ('canonical', f'commit="{C1}",canonical_id="x"'), ('name_str', f'commit=1'),
             ('build_file', f'commit="{C1}",build_file="//:bf.BUILD"'), ('build_file_content', f'commit="{C1}",build_file_content="x"'), ('ws_file', f'commit="{C1}",workspace_file_content="x"'),
             ('strip_comp', f'commit="{C1}",strip_components=1'), ('type_attr', f'commit="{C1}",type="zip"'), ('commit_empty', 'commit=""'), ('tag_empty', 'tag=""'), ('branch_empty', 'branch=""'),
             ('shallow_empty', f'commit="{C1}",shallow_since=""'), ('module_bazel', f'commit="{C2}"'), ('remote_module_file', f'commit="{C1}",remote_module_file_urls=["file://{VBZ}/www/hello.txt"],remote_module_file_integrity="{SRI(VBZ + "/www/hello.txt")}"')]:
    G(a, n)
for n, a in [('c1', f'commit="{C1}"'), ('c2', f'commit="{C2}"'), ('tag', 'tag="v1"'), ('branch', 'branch="br1"'), ('noref', ''), ('bf', f'commit="{C1}",build_file="//:bf.BUILD"'),
             ('bfc', f'commit="{C1}",build_file_content="filegroup(name=\'x\')\\n"'), ('both', f'commit="{C1}",build_file="//:bf.BUILD",build_file_content="x"'), ('bf_missing', f'commit="{C1}",build_file="//:nope"'),
             ('strip', f'commit="{C2}",build_file_content="x",strip_prefix="sub"'), ('ws', f'commit="{C1}",build_file_content="x",workspace_file_content="workspace(name=\'q\')"'),
             ('wsf', f'commit="{C1}",build_file_content="x",workspace_file="//:ws.txt"'), ('patches', f'commit="{C1}",build_file_content="x",patches=["//:ok.patch"],patch_args=["-p1"]'),
             ('patch_cmds', f'commit="{C1}",build_file_content="x",patch_cmds=["echo hi > h.txt"]'), ('init_sub', f'commit="{C3}",build_file_content="x",init_submodules=True'),
             ('over_existing_build', f'commit="{C1}",build_file_content="# replaced\\n"'), ('verbose', f'commit="{C1}",build_file_content="x",verbose=True'), ('shallow', f'commit="{C1}",build_file_content="x",shallow_since="2020-01-01"'),
             ('bad_attr', f'commit="{C1}",build_file_content="x",foo=1'), ('empty_content', f'commit="{C1}",build_file_content=""')]:
    G(a, 'new_' + n, rule='new_git_repository')
for n, rem in [('remote_file_url', f'file://{GN}'), ('remote_missing', f'{VBZ}/nonexistent'), ('remote_empty', ''), ('remote_relative', 'gitrel'), ('remote_not_git', f'{VBZ}/www'), ('remote_http', f'http://127.0.0.1:{PORT}/nogit'),
               ('remote_ssh', 'ssh://127.0.0.1:1/x.git'), ('remote_space', f'{GN} x'), ('remote_dotgit', f'{GN}/.git')]:
    N('git_' + n, GR + f'git_repository(name="t",remote="{rem}",commit="{C1}")\n')
N('git_no_remote', GR + f'git_repository(name="t",commit="{C1}")\n')
N('git_twice', GR + f'git_repository(name="t",remote="{GN}",commit="{C1}")\n', pre=[['fetch', '--repo=@t']])
N('git_force', GR + f'git_repository(name="t",remote="{GN}",branch="br1")\n', pre=[['fetch', '--repo=@t']], flags=['--force'] if False else [])
N('git_two_repos', GR + f'git_repository(name="t",remote="{GN}",commit="{C1}")\ngit_repository(name="u",remote="{GN}",commit="{C2}")\n', args=['fetch', '--all'], tree=['external/*+t', 'external/*+u'])
N('git_via_ext', GR.replace('use_repo_rule', 'use_repo_rule'), files={'ext.bzl': f'''load("@bazel_tools//tools/build_defs/repo:git.bzl","git_repository")
def _i(mctx):
    git_repository(name="t",remote="{GN}",commit="{C1}")
ex=module_extension(implementation=_i)
''', 'MODULE.bazel': 'module(name="root")\ne=use_extension("//:ext.bzl","ex")\nuse_repo(e,"t")\n'}, args=['fetch', '--repo=@t'], tree=['external/*+ex+t'])
N('git_via_ext_branch', '', files={'ext.bzl': f'''load("@bazel_tools//tools/build_defs/repo:git.bzl","git_repository")
def _i(mctx):
    git_repository(name="t",remote="{GN}",branch="br1")
ex=module_extension(implementation=_i)
''', 'MODULE.bazel': 'module(name="root")\ne=use_extension("//:ext.bzl","ex")\nuse_repo(e,"t")\n'}, args=['fetch', '--repo=@t'], tree=['external/*+ex+t'])

# ------------------------------------------------------------------ utils.bzl / cache.bzl via a custom repository rule
def U(name, body, attrs='', **kw):
    R = '''load("@bazel_tools//tools/build_defs/repo:utils.bzl","patch","read_netrc","use_netrc","update_attrs","workspace_and_buildfile","get_auth","read_user_netrc","parse_netrc","maybe","download_remote_files")
load("@bazel_tools//tools/build_defs/repo:cache.bzl","get_default_canonical_id","DEFAULT_CANONICAL_ID_ENV")
load("@bazel_tools//tools/build_defs/repo:http.bzl","http_archive")
def _impl(rctx):
''' + textwrap.indent(textwrap.dedent(body).strip('\n'), '    ') + '''
    rctx.file("BUILD.bazel","")
r=repository_rule(implementation=_impl,attrs={"patches":attr.label_list(),"patch_args":attr.string_list(default=["-p0"]),"patch_cmds":attr.string_list(),"patch_cmds_win":attr.string_list(),"patch_tool":attr.string(),"build_file":attr.label(),"build_file_content":attr.string(),"workspace_file":attr.label(),"workspace_file_content":attr.string(),"netrc":attr.string(),"auth_patterns":attr.string_dict(),"remote_patches":attr.string_dict(),"remote_patch_strip":attr.int(),"remote_file_urls":attr.string_list_dict(),"remote_file_integrity":attr.string_dict(),"patch_strip":attr.int(default=0),"canonical_id":attr.string()},environ=["NETRC","BAZEL_HTTP_RULES_URLS_AS_DEFAULT_CANONICAL_ID"])
'''
    N('ut_' + name, 'r=use_repo_rule("//:r.bzl","r")\nr(name="t"' + (',' + attrs if attrs else '') + ')\n', files={'r.bzl': R}, **kw)


netrcs = {
    'basic': 'machine a login b password c\\ndefault login d password e\\n', 'multi': 'machine a\\n login b\\n password c\\n#comment\\nmacdef x\\n  y\\n\\nmachine z login q',
    'empty': '', 'only_default': 'default login x password y', 'no_login': 'machine a password c', 'no_password': 'machine a login b', 'account': 'machine a login b password c account d',
    'quoted': 'machine a login "b c" password "d\\\\"e"', 'unterminated_quote': 'machine a login "b', 'machine_missing': 'machine', 'login_missing': 'machine a login', 'macdef_only': 'macdef x\\n  y\\n\\n',
    'macdef_eof': 'macdef x\\n  y', 'comment_only': '# c\\n', 'tabs': 'machine\\ta\\tlogin\\tb\\tpassword\\tc', 'crlf': 'machine a login b password c\\r\\nmachine d login e password f\\r\\n',
    'dup_machine': 'machine a login 1 password 2\\nmachine a login 3 password 4', 'unknown_tok': 'machine a foo bar', 'default_not_last': 'default login x password y\\nmachine a login b password c',
    'unicode': 'machine a login \u00fc password \u20ac', 'inline_comment': 'machine a login b # x\\n password c', 'key_after_default': 'default login a password b machine z login c', 'long': 'machine ' + 'x' * 300 + ' login a password b',
    'two_defaults': 'default login a password b\\ndefault login c password d', 'password_with_hash': 'machine a login b password p#w', 'only_machine_name': 'machine a login', 'login_only': 'login a password b',
    'macdef_default': 'macdef default\\n echo\\n\\nmachine a login b password c'}
for k, v in netrcs.items():
    v = v.replace('"', '\\"')
    U('parse_netrc_' + k, f'rctx.file("o",str(parse_netrc("{v}")))')
    U('parse_netrc_fn_' + k, f'rctx.file("o",str(parse_netrc("{v}","my.netrc")))')
for k, (n, u, p) in {'basic': ('{"a.b":{"login":"x","password":"y"}}', '["http://a.b/c"]', '{"a.b":"Basic <login>:<password>"}'),
                     'nopat': ('{"a.b":{"login":"x","password":"y"}}', '["http://a.b/c"]', '{}'), 'default': ('{"":{"login":"x","password":"y"}}', '["http://z/c"]', '{}'),
                     'bearer': ('{"a.b":{"login":"x","password":"y"}}', '["https://a.b/c"]', '{"a.b":"Bearer <password>"}'), 'port': ('{"a.b":{"login":"x","password":"y"}}', '["https://a.b:8080/c"]', '{}'),
                     'nomatch': ('{"q":{"login":"x","password":"y"}}', '["http://a.b/c"]', '{}'), 'multi_url': ('{"a":{"login":"x","password":"y"},"b":{"login":"p","password":"q"}}', '["http://a/x","http://b/y","http://c/z"]', '{}'),
                     'no_login': ('{"a":{"password":"y"}}', '["http://a/x"]', '{}'), 'no_password': ('{"a":{"login":"y"}}', '["http://a/x"]', '{}'), 'pat_only': ('{}', '["http://a/x"]', '{"a":"Foo"}'),
                     'pat_missing_var': ('{"a":{"login":"x"}}', '["http://a/x"]', '{"a":"B <password>"}'), 'userinfo': ('{"a":{"login":"x","password":"y"}}', '["http://u:p@a/x"]', '{}'),
                     'empty_urls': ('{"a":{"login":"x","password":"y"}}', '[]', '{}'), 'upper_host': ('{"a":{"login":"x","password":"y"}}', '["http://A/x"]', '{}'), 'ip': ('{"127.0.0.1":{"login":"x","password":"y"}}', '["http://127.0.0.1:1/x"]', '{}'),
                     'account': ('{"a":{"login":"x","password":"y","account":"z"}}', '["http://a/x"]', '{"a":"Acct <account>"}'), 'bad_scheme': ('{"a":{"login":"x","password":"y"}}', '["ftp://a/x"]', '{}'), 'no_scheme': ('{"a":{"login":"x","password":"y"}}', '["a/x"]', '{}'),
                     'file_url': ('{"a":{"login":"x","password":"y"}}', '["file:///a/x"]', '{}')}.items():
    U('use_netrc_' + k, f'rctx.file("o",str(use_netrc({n},{u},{p})))')
U('update_attrs_basic', 'rctx.file("o",str(update_attrs({"a":1,"b":2},["a","b","c"],{"a":9})))')
U('update_attrs_override_missing', 'rctx.file("o",str(update_attrs({"a":1},["a"],{"zz":9})))')
U('update_attrs_empty', 'rctx.file("o",str(update_attrs({},[],{})))')
U('update_attrs_none', 'rctx.file("o",str(update_attrs({"a":None,"b":""},["a","b"],{})))')
U('update_attrs_types', 'rctx.file("o",str(update_attrs({"a":[1],"b":{"x":1},"c":True},["a","b","c"],{"c":False})))')
U('update_attrs_bad', 'update_attrs(1,2,3)')
U('read_netrc_ok', 'rctx.file("n","machine h login u password p\\n")\nrctx.file("o",str(read_netrc(rctx,"n")))')
U('read_netrc_missing', 'read_netrc(rctx,"nonexistent")')
U('read_netrc_dir', 'rctx.file("d/x","1")\nread_netrc(rctx,"d")')
U('read_netrc_bad', 'rctx.file("n","machine")\nread_netrc(rctx,"n")')
U('read_netrc_abs', f'rctx.file("o",str(read_netrc(rctx,"{VBZ}/netrc_basic")))')
U('read_user_netrc_home', 'rctx.file("o",str(read_user_netrc(rctx)))', flags=['--repo_env=HOME=' + VBZ + '/hnetrc'])
U('read_user_netrc_nohome', 'rctx.file("o",str(read_user_netrc(rctx)))', flags=['--repo_env=HOME=/nonexistent'])
U('read_user_netrc_env', 'rctx.file("o",str(read_user_netrc(rctx)))', flags=['--repo_env=NETRC=' + VBZ + '/netrc_basic'])
U('get_auth_none', 'rctx.file("o",str(get_auth(rctx,["http://a.b/c"])))', flags=['--repo_env=HOME=/nonexistent'])
U('get_auth_attr', 'rctx.file("o",str(get_auth(rctx,["http://127.0.0.1/c","https://x/y"])))', f'netrc="{VBZ}/netrc_basic"')
U('get_auth_attr_pat', 'rctx.file("o",str(get_auth(rctx,["http://127.0.0.1/c"])))', f'netrc="{VBZ}/netrc_basic",auth_patterns={{"127.0.0.1":"Bearer <password>"}}')
U('get_auth_env', 'rctx.file("o",str(get_auth(rctx,["http://127.0.0.1/c"])))', flags=['--repo_env=NETRC=' + VBZ + '/netrc_basic'])
U('get_auth_home', 'rctx.file("o",str(get_auth(rctx,["http://127.0.0.1/c"])))', flags=['--repo_env=HOME=' + VBZ + '/hnetrc'])
U('get_auth_missing_attr', 'rctx.file("o",str(get_auth(rctx,["http://127.0.0.1/c"])))', f'netrc="{VBZ}/nonexistent"')
U('get_auth_default', 'rctx.file("o",str(get_auth(rctx,["http://q/c"])))', f'netrc="{VBZ}/netrc_default"')
U('get_auth_noargs', 'get_auth(rctx)')
U('wab_bf', 'workspace_and_buildfile(rctx)', 'build_file="//:bf.BUILD"')
U('wab_bfc', 'workspace_and_buildfile(rctx)', 'build_file_content="filegroup(name=\'c\')\\n"')
U('wab_none', 'workspace_and_buildfile(rctx)')
U('wab_both', 'workspace_and_buildfile(rctx)', 'build_file="//:bf.BUILD",build_file_content="x"')
U('wab_ws', 'workspace_and_buildfile(rctx)', 'workspace_file="//:ws.txt"')
U('wab_wsc', 'workspace_and_buildfile(rctx)', 'workspace_file_content="workspace(name=\'q\')\\n"')
U('wab_ws_both', 'workspace_and_buildfile(rctx)', 'workspace_file="//:ws.txt",workspace_file_content="x"')
U('wab_bf_ws', 'workspace_and_buildfile(rctx)', 'build_file="//:bf.BUILD",workspace_file="//:ws.txt"')
U('wab_existing_workspace', 'rctx.file("WORKSPACE","old")\nworkspace_and_buildfile(rctx)', 'workspace_file_content="new"')
U('wab_existing_build', 'rctx.file("BUILD","old")\nrctx.file("BUILD.bazel","old2")\nworkspace_and_buildfile(rctx)', 'build_file_content="new"')
U('wab_existing_ws_bazel', 'rctx.file("WORKSPACE.bazel","old")\nworkspace_and_buildfile(rctx)', 'workspace_file_content="new"')
U('wab_existing_ws_nocontent', 'rctx.file("WORKSPACE","old")\nworkspace_and_buildfile(rctx)')
U('patch_basic', 'rctx.file("a.txt","hello\\n")\npatch(rctx)', 'patches=["//:ok.patch"],patch_args=["-p1"]')
U('patch_nopatches', 'patch(rctx)')
U('patch_explicit', 'rctx.file("a.txt","hello\\n")\npatch(rctx,patches=[Label("//:ok.patch")],patch_args=["-p1"])')
U('patch_cmds', 'patch(rctx,patch_cmds=["echo x > x"])')
U('patch_cmds_win_on_unix', 'patch(rctx,patch_cmds_win=["echo x > x"])')
U('patch_cmds_both_unix', 'patch(rctx,patch_cmds=["echo u > u"],patch_cmds_win=["echo w > w"])')
U('patch_args_strip', 'rctx.file("a.txt","hello\\n")\npatch(rctx,patches=[Label("//:ok.patch")],patch_args=["-p0"])')
U('patch_tool', 'rctx.file("a.txt","hello\\n")\npatch(rctx,patches=[Label("//:ok.patch")],patch_tool="patch",patch_args=["-p1"])')
U('patch_dir', 'rctx.file("d/a.txt","hello\\n")\npatch(rctx,patches=[Label("//:ok.patch")],patch_args=["-p1"],patch_directory="d")')
U('patch_dir_missing', 'patch(rctx,patches=[Label("//:ok.patch")],patch_args=["-p1"],patch_directory="nodir")')
U('patch_strip_attr', 'rctx.file("a.txt","hello\\n")\npatch(rctx,patches=[Label("//:ok.patch")],patch_args=["-p1","--verbose"])')
U('patch_remote', f'rctx.file("a.txt","hello\\n")\nr=patch(rctx,remote_patches={{"file://{VBZ}/www/ok.patch":"{SRI(VBZ + "/www/ok.patch")}"}},remote_patch_strip=1)\nrctx.file("o",str(r))')
U('patch_remote_attr', f'rctx.file("a.txt","hello\\n")\nr=patch(rctx)\nrctx.file("o",str(r))', f'remote_patches={{"file://{VBZ}/www/ok.patch":"{SRI(VBZ + "/www/ok.patch")}"}},remote_patch_strip=1')
U('patch_returns', f'r=patch(rctx)\nrctx.file("o",str(r))')
U('dl_remote_files', f'r=download_remote_files(rctx)\nrctx.file("o",str(r))', f'remote_file_urls={{"x":["file://{VBZ}/www/hello.txt"]}},remote_file_integrity={{"x":"{SRI(VBZ + "/www/hello.txt")}"}}')
U('dl_remote_files_none', 'rctx.file("o",str(download_remote_files(rctx)))')
U('dl_remote_files_nointeg', f'rctx.file("o",str(download_remote_files(rctx)))', f'remote_file_urls={{"x":["file://{VBZ}/www/hello.txt"]}}')
U('canonical_default', 'rctx.file("o",str(get_default_canonical_id(rctx,["http://a/b"])))')
U('canonical_default_env', 'rctx.file("o",str(get_default_canonical_id(rctx,["http://a/b","https://c/d"])))', flags=['--repo_env=BAZEL_HTTP_RULES_URLS_AS_DEFAULT_CANONICAL_ID=1'])
U('canonical_default_env0', 'rctx.file("o",str(get_default_canonical_id(rctx,["http://a/b"])))', flags=['--repo_env=BAZEL_HTTP_RULES_URLS_AS_DEFAULT_CANONICAL_ID=0'])
U('canonical_default_env_empty', 'rctx.file("o",str(get_default_canonical_id(rctx,["http://a/b"])))', flags=['--repo_env=BAZEL_HTTP_RULES_URLS_AS_DEFAULT_CANONICAL_ID='])
U('canonical_default_env_urls_empty', 'rctx.file("o",str(get_default_canonical_id(rctx,[])))', flags=['--repo_env=BAZEL_HTTP_RULES_URLS_AS_DEFAULT_CANONICAL_ID=1'])
U('canonical_env_const', 'rctx.file("o",str(DEFAULT_CANONICAL_ID_ENV))')
U('exports_dir', 'rctx.file("o",str(sorted([x for x in dir(rctx)])))')
# maybe() in MODULE-level extension
N('ut_maybe_ext', '', files={'ext.bzl': f'''load("@bazel_tools//tools/build_defs/repo:utils.bzl","maybe")
load("@bazel_tools//tools/build_defs/repo:http.bzl","http_archive","http_file")
def _i(mctx):
    maybe(http_file,name="t",url="{FU("../www/hello.txt")}")
    maybe(http_file,name="t",url="{FU("../www/hello.txt")}")
ex=module_extension(implementation=_i)
''', 'MODULE.bazel': 'module(name="root")\ne=use_extension("//:ext.bzl","ex")\nuse_repo(e,"t")\n'}, tree=['external/*+ex+t'], args=['fetch', '--repo=@t'])
N('ut_maybe_ext_dup', '', files={'ext.bzl': f'''load("@bazel_tools//tools/build_defs/repo:http.bzl","http_file")
def _i(mctx):
    http_file(name="t",url="{FU("../www/hello.txt")}")
    http_file(name="t",url="{FU("../www/hello.txt")}")
ex=module_extension(implementation=_i)
''', 'MODULE.bazel': 'module(name="root")\ne=use_extension("//:ext.bzl","ex")\nuse_repo(e,"t")\n'}, tree=['external/*+ex+t'], args=['fetch', '--repo=@t'])
N('ut_ext_http_archive_attr_errors', '', files={'ext.bzl': f'''load("@bazel_tools//tools/build_defs/repo:http.bzl","http_archive")
def _i(mctx):
    http_archive(name="t",url="{FU("small.tar.gz")}",foo=1)
ex=module_extension(implementation=_i)
''', 'MODULE.bazel': 'module(name="root")\ne=use_extension("//:ext.bzl","ex")\nuse_repo(e,"t")\n'}, tree=['external/*+ex+t'], args=['fetch', '--repo=@t'])
N('ut_ext_http_archive_missing_name', '', files={'ext.bzl': f'''load("@bazel_tools//tools/build_defs/repo:http.bzl","http_archive")
def _i(mctx):
    http_archive(url="{FU("small.tar.gz")}")
ex=module_extension(implementation=_i)
''', 'MODULE.bazel': 'module(name="root")\ne=use_extension("//:ext.bzl","ex")\nuse_repo(e,"t")\n'}, args=['fetch', '--repo=@t'])
N('ut_ext_local_missing_path', '', files={'ext.bzl': '''load("@bazel_tools//tools/build_defs/repo:local.bzl","local_repository")
def _i(mctx):
    local_repository(name="t")
ex=module_extension(implementation=_i)
''', 'MODULE.bazel': 'module(name="root")\ne=use_extension("//:ext.bzl","ex")\nuse_repo(e,"t")\n'}, args=['fetch', '--repo=@t'])

# ------------------------------------------------------------------ jvm.bzl / java.bzl
JV = 'load("@bazel_tools//tools/build_defs/repo:jvm.bzl","jvm_maven_import_external","jvm_import_external")\n'
def J(name, body, **kw):
    N('jvm_' + name, '', files={'ext.bzl': JV + 'def _i(mctx):\n' + textwrap.indent(textwrap.dedent(body).strip('\n'), '    ') + '\nex=module_extension(implementation=_i)\n',
                                'MODULE.bazel': 'module(name="root")\ne=use_extension("//:ext.bzl","ex")\nuse_repo(e,"t")\n'}, tree=['external/*+ex+t'], args=['fetch', '--repo=@t'], **kw)


J('import_basic', f'jvm_import_external(name="t",artifact_sha256="{JS("x.jar")}",jar_urls=["{JU("x.jar")}"],rule_name="java_import",licenses=["notice"])')
J('import_srcs', f'jvm_import_external(name="t",artifact_sha256="{JS("x.jar")}",jar_urls=["{JU("x.jar")}"],srcjar_sha256="{JS("x-sources.jar")}",srcjar_urls=["{JU("x-sources.jar")}"],rule_name="java_import",licenses=["notice"])')
J('import_badsha', f'jvm_import_external(name="t",artifact_sha256="{"0" * 64}",jar_urls=["{JU("x.jar")}"],rule_name="java_import",licenses=["notice"])')
J('import_no_urls', f'jvm_import_external(name="t",artifact_sha256="{JS("x.jar")}",rule_name="java_import",licenses=["notice"])')
J('import_deps', f'jvm_import_external(name="t",artifact_sha256="{JS("x.jar")}",jar_urls=["{JU("x.jar")}"],rule_name="java_import",licenses=["notice"],deps=["@a//:b"],runtime_deps=["@c//:d"],exports=["@e//:f"],neverlink=True,testonly_=True)')
J('import_extra', f'jvm_import_external(name="t",artifact_sha256="{JS("x.jar")}",jar_urls=["{JU("x.jar")}"],rule_name="java_import",licenses=["notice"],extra_build_file_content="# extra\\n",generated_rule_name="g",generated_linkable_rule_name="gl",default_visibility=["//a:b"])')
J('import_nolic', f'jvm_import_external(name="t",artifact_sha256="{JS("x.jar")}",jar_urls=["{JU("x.jar")}"],rule_name="java_import")')
J('import_badrule', f'jvm_import_external(name="t",artifact_sha256="{JS("x.jar")}",jar_urls=["{JU("x.jar")}"],rule_name="zz_import",licenses=["notice"])')
J('import_unknown_attr', f'jvm_import_external(name="t",artifact_sha256="{JS("x.jar")}",jar_urls=["{JU("x.jar")}"],rule_name="java_import",licenses=["notice"],zz=1)')
J('maven_basic', f'jvm_maven_import_external(name="t",artifact="g:a:1",artifact_sha256="{JS("x.jar")}",server_urls=["{JU("")[:-1]}"],licenses=["notice"])')
J('maven_bad_coords', f'jvm_maven_import_external(name="t",artifact="g:a",artifact_sha256="{JS("x.jar")}",server_urls=["x"],licenses=["notice"])')
J('maven_packaging', f'jvm_maven_import_external(name="t",artifact="g:a:aar:1",artifact_sha256="{JS("x.jar")}",server_urls=["file:///nonexistent"],licenses=["notice"])')
J('maven_srcs', f'jvm_maven_import_external(name="t",artifact="g:a:1",artifact_sha256="{JS("x.jar")}",srcjar_sha256="{JS("x-sources.jar")}",server_urls=["file:///nonexistent"],licenses=["notice"])')
J('maven_classifier', f'jvm_maven_import_external(name="t",artifact="g:a:jar:cls:1",artifact_sha256="{JS("x.jar")}",server_urls=["file:///nonexistent"],licenses=["notice"])')
J('maven_5parts', f'jvm_maven_import_external(name="t",artifact="g:a:b:c:d:e",artifact_sha256="{JS("x.jar")}",server_urls=["file:///nonexistent"],licenses=["notice"])')
J('maven_no_server', f'jvm_maven_import_external(name="t",artifact="g:a:1",artifact_sha256="{JS("x.jar")}",licenses=["notice"])')
J('maven_fetch_srcs_env', f'jvm_maven_import_external(name="t",artifact="g:a:1",artifact_sha256="{JS("x.jar")}",srcjar_sha256="{JS("x-sources.jar")}",server_urls=["file:///nonexistent"],licenses=["notice"])', flags=['--repo_env=BAZEL_JVM_FETCH_SOURCES=1'])
N('java_import_external', '', files={'ext.bzl': f'''load("@bazel_tools//tools/build_defs/repo:java.bzl","java_import_external")
def _i(mctx):
    java_import_external(name="t",jar_sha256="{JS("x.jar")}",jar_urls=["{JU("x.jar")}"],licenses=["notice"])
ex=module_extension(implementation=_i)
''', 'MODULE.bazel': 'module(name="root")\ne=use_extension("//:ext.bzl","ex")\nuse_repo(e,"t")\n'}, tree=['external/*+ex+t'], args=['fetch', '--repo=@t'])
N('java_import_external_kw', '', files={'ext.bzl': f'''load("@bazel_tools//tools/build_defs/repo:java.bzl","java_import_external")
def _i(mctx):
    java_import_external(name="t",jar_sha256="{JS("x.jar")}",jar_urls=["{JU("x.jar")}"],licenses=["notice"],deps=["@a//:b"],neverlink=True,testonly_=True,generated_rule_name="g",extra_build_file_content="#x\\n",exports=["@q//:q"],runtime_deps=["@r//:r"],default_visibility=["//v:v"])
ex=module_extension(implementation=_i)
''', 'MODULE.bazel': 'module(name="root")\ne=use_extension("//:ext.bzl","ex")\nuse_repo(e,"t")\n'}, tree=['external/*+ex+t'], args=['fetch', '--repo=@t'])

# repo-rule properties exercised through an extension returning reproducible metadata (lockfile)
N('ha_fetch_all', HA + f'http_archive(name="t",url="{FU("small.tar.gz")}",sha256="{TGZ}")\nhttp_archive(name="u",url="{FU("small.zip")}",sha256="{ZIP}")\n', args=['fetch', '--all'], tree=['external/*+t', 'external/*+u'])


# ------------------------------------------------------------------ Starlark call-binding errors inside extension / repo rule code
def E_(name, defs, body, trail=True, **kw):
    N('eng_' + name, '', files={'ext.bzl': ('load("@bazel_tools//tools/build_defs/repo:local.bzl","local_repository")\n' if trail else '') + defs + '\ndef _i(mctx):\n' + textwrap.indent(textwrap.dedent(body).strip('\n'), '    ') + ('\n    local_repository(name="t",path="local")' if trail else '') + '\nex=module_extension(implementation=_i)\n',
                                'MODULE.bazel': 'module(name="root")\ne=use_extension("//:ext.bzl","ex")\nuse_repo(e,"t")\n'}, tree=['external/*+ex+t'], args=['fetch', '--repo=@t'], **kw)


for k, (d, b) in {
        'unexpected_kw1': ('def f(a): pass', 'f(1, b=2)'), 'unexpected_kw2': ('def f(a): pass', 'f(1, b=2, c=3)'), 'missing1': ('def f(a): pass', 'f()'),
        'missing2': ('def f(a, b): pass', 'f()'), 'missing_kwonly': ('def f(*, a): pass', 'f()'), 'missing_kwonly2': ('def f(*, a, b): pass', 'f()'),
        'multi_value': ('def f(a): pass', 'f(1, a=2)'), 'too_many_pos': ('def f(a): pass', 'f(1, 2)'), 'too_many_pos2': ('def f(): pass', 'f(1)'),
        'too_many_pos3': ('def f(a, b=1): pass', 'f(1, 2, 3)'), 'kwonly_as_pos': ('def f(*, a): pass', 'f(1)'), 'unexpected_with_kwargs_ok': ('def f(**kw): return kw', 'f(a=1)'),
        'missing_with_default': ('def f(a, b=1): pass', 'f(b=2)'), 'dup_kw': ('def f(**kw): pass', 'f(a=1, **{"a": 2})'), 'star_args_nonseq': ('def f(*a): pass', 'f(*1)'),
        'starstar_nondict': ('def f(**a): pass', 'f(**1)'), 'nested': ('def g(x): fail("boom")\ndef f(a): g(a)', 'f(1)'), 'fail_in_ext': ('', 'fail("top")'),
        'index_oob': ('', 'x=[1]\nx[3]'), 'dict_key': ('', 'x={}\nx["a"]'), 'none_attr': ('', 'x=None\nx.foo'), 'type_err': ('', '1+"a"'), 'int_call': ('', 'x=1\nx()'),
        'str_idx': ('', '"a".foo()'), 'div_zero': ('', '1//0'), 'rec': ('def f(): f()', 'f()'),
        'lambda_err': ('f=lambda: 1//0', 'f()'), 'comp_err': ('', '[1//0 for x in [1]]'), 'nested3': ('def a(): fail("deep")\ndef b(): a()\ndef c(): b()', 'c()'),
        'assert_like': ('', 'fail("m", "n", sep="-")'), 'fail_attr': ('', 'fail(msg="kw")'), 'fail_int': ('', 'fail(1)'), 'fail_list': ('', 'fail([1,2])')}.items():
    E_(k, d, b)
E_('repo_rule_call_unknown', 'load("@bazel_tools//tools/build_defs/repo:local.bzl","local_repository")', 'local_repository(name="t",path="local",zz=1)', trail=False)
E_('repo_rule_call_missing_mandatory', 'load("@bazel_tools//tools/build_defs/repo:local.bzl","local_repository")', 'local_repository(name="t")', trail=False)
E_('repo_rule_call_noname', 'load("@bazel_tools//tools/build_defs/repo:local.bzl","local_repository")', 'local_repository(path="local")', trail=False)
E_('repo_rule_call_pos', 'load("@bazel_tools//tools/build_defs/repo:local.bzl","local_repository")', 'local_repository("t",path="local")', trail=False)
E_('repo_rule_call_badname', 'load("@bazel_tools//tools/build_defs/repo:local.bzl","local_repository")', 'local_repository(name=1,path="local")', trail=False)
E_('repo_rule_call_emptyname', 'load("@bazel_tools//tools/build_defs/repo:local.bzl","local_repository")', 'local_repository(name="",path="local")', trail=False)
E_('repo_rule_call_twice', 'load("@bazel_tools//tools/build_defs/repo:local.bzl","local_repository")', 'local_repository(name="t",path="local")\nlocal_repository(name="t",path="local")', trail=False)
E_('repo_rule_call_ok', 'load("@bazel_tools//tools/build_defs/repo:local.bzl","local_repository")', 'local_repository(name="t",path="local")', trail=False)
E_('repo_rule_call_type', 'load("@bazel_tools//tools/build_defs/repo:local.bzl","local_repository")', 'local_repository(name="t",path=1)', trail=False)
E_('repo_rule_call_none_attr', 'load("@bazel_tools//tools/build_defs/repo:local.bzl","local_repository")', 'local_repository(name="t",path=None)', trail=False)
E_('repo_rule_call_tags', 'load("@bazel_tools//tools/build_defs/repo:local.bzl","local_repository")', 'local_repository(name="t",path="local",tags=["a"])', trail=False)
E_('repo_rule_call_visibility', 'load("@bazel_tools//tools/build_defs/repo:local.bzl","local_repository")', 'local_repository(name="t",path="local",visibility=["//visibility:public"])', trail=False)


# order in which unknown attributes are reported
N('unk_order_zz_aa', LR + 'local_repository(name="t",path="local",zz=1,aa=2)\n')
N('unk_order_aa_zz', LR + 'local_repository(name="t",path="local",aa=1,zz=2)\n')
N('unk_order_ext', '', files={'ext.bzl': 'load("@bazel_tools//tools/build_defs/repo:local.bzl","local_repository")\ndef _i(mctx):\n    local_repository(name="t",path="local",zz=1,aa=2)\nex=module_extension(implementation=_i)\n', 'MODULE.bazel': 'module(name="root")\ne=use_extension("//:ext.bzl","ex")\nuse_repo(e,"t")\n'}, tree=['external/*+ex+t'])
N('unk_order_http', HA + f'http_archive(name="t",url="{FU("small.tar.gz")}",sha256="{TGZ}",remote_file_urls={{}},remote_file_integrity={{}},zzz=1,remote_patch_strip=1,bbb=2)\n')
N('unk_order_http2', HA + f'http_archive(name="t",url="{FU("small.tar.gz")}",sha256="{TGZ}",remote_file_integrity={{}},remote_file_urls={{}},zzz=1)\n')

if __name__ == '__main__':
    cases = [c for c in cases if c]
    print(len(cases))
    main(cases, jobs=int(os.environ.get('JOBS', '5')))
