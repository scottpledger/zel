#!/usr/bin/env python3
"""
Sets up test workspaces for differential testing between Zel and Bazel 9.3.2.
"""
import os
import shutil
import subprocess

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
import tempfile
_SCRATCH = os.environ.get("DIFF_BZLMOD_DIR") or os.path.join(tempfile.gettempdir(), "diff_bzlmod")
TEST_ROOT = os.path.join(_SCRATCH, "cases")
os.makedirs(TEST_ROOT, exist_ok=True)

# 1. Stub platforms
stub_plat = os.path.join(_SCRATCH, "platforms")
os.makedirs(os.path.join(stub_plat, "host"), exist_ok=True)
with open(os.path.join(stub_plat, "MODULE.bazel"), "w") as f:
    f.write('module(name="platforms", version="0.0.10")\n')
with open(os.path.join(stub_plat, "BUILD.bazel"), "w") as f:
    f.write('package(default_visibility=["//visibility:public"])\nplatform(name="host")\n')
with open(os.path.join(stub_plat, "host", "BUILD.bazel"), "w") as f:
    f.write('alias(name="host", actual="//:host", visibility=["//visibility:public"])\n')
with open(os.path.join(stub_plat, "host", "constraints.bzl"), "w") as f:
    f.write('HOST_CONSTRAINTS = []\n')
with open(os.path.join(stub_plat, "host", "extension.bzl"), "w") as f:
    f.write('def host_platform_repo(**kwargs): pass\n')

def mkws(name, files):
    d = os.path.join(TEST_ROOT, name)
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d, exist_ok=True)
    files.setdefault("BUILD.bazel", "")
    for p, c in files.items():
        fp = os.path.join(d, p)
        os.makedirs(os.path.dirname(fp), exist_ok=True)
        with open(fp, "w") as f:
            f.write(c)
    return d

# MODULE.bazel restrictions
mkws("ws_if", {"MODULE.bazel": 'module(name="test")\nif True:\n  pass\n'})
mkws("ws_for", {"MODULE.bazel": 'module(name="test")\nfor i in [1]:\n  pass\n'})
mkws("ws_def", {"MODULE.bazel": 'module(name="test")\ndef f(): pass\n'})
mkws("ws_load", {"MODULE.bazel": 'load("//:x.bzl", "x")\nmodule(name="test")\n'})
mkws("ws_unknown_fn", {"MODULE.bazel": 'module(name="test")\nfrobnicate()\n'})
mkws("ws_syntax", {"MODULE.bazel": 'module(name="test"\n'})
mkws("ws_dup_mod", {"MODULE.bazel": 'module(name="test")\nmodule(name="test2")\n'})
mkws("ws_mod_after", {"MODULE.bazel": 'register_toolchains("//:t")\nmodule(name="test")\n'})
mkws("ws_dup_dep", {"MODULE.bazel": 'module(name="test")\nbazel_dep(name="a", version="1.0")\nbazel_dep(name="a", version="1.1")\n'})
mkws("ws_self_dep", {"MODULE.bazel": 'module(name="test")\nbazel_dep(name="test", version="1.0")\n'})
mkws("ws_bad_mod_name", {"MODULE.bazel": 'module(name="Bad_Name")\n'})
mkws("ws_bad_repo_name", {"MODULE.bazel": 'module(name="test")\nbazel_dep(name="a", version="1.0", repo_name="Bad Repo")\n'})
mkws("ws_no_ver", {"MODULE.bazel": 'module(name="test")\nbazel_dep(name="a")\n'})
mkws("ws_multi_ver_one", {"MODULE.bazel": 'module(name="test")\nmultiple_version_override(module_name="a", versions=["1.0"])\n'})
mkws("ws_fail", {"MODULE.bazel": 'module(name="test")\nfail("custom boom")\n'})

# include() cases
mkws("ws_inc_ok", {
    "MODULE.bazel": 'module(name="test")\ninclude("//:inc.MODULE.bazel")\n',
    "inc.MODULE.bazel": 'bazel_dep(name="a", version="1.0")\n'
})
mkws("ws_inc_sub", {
    "MODULE.bazel": 'module(name="test")\ninclude("//sub:inc.MODULE.bazel")\n',
    "sub/BUILD.bazel": '',
    "sub/inc.MODULE.bazel": 'bazel_dep(name="a", version="1.0")\n'
})
mkws("ws_inc_missing", {"MODULE.bazel": 'module(name="test")\ninclude("//:nope.MODULE.bazel")\n'})
mkws("ws_inc_bad_ext", {"MODULE.bazel": 'module(name="test")\ninclude("//:inc.bazel")\n', "inc.bazel": ""})
mkws("ws_inc_dot", {"MODULE.bazel": 'module(name="test")\ninclude("//:.dot.MODULE.bazel")\n', ".dot.MODULE.bazel": ""})
mkws("ws_inc_rel", {"MODULE.bazel": 'module(name="test")\ninclude(":inc.MODULE.bazel")\n', "inc.MODULE.bazel": ""})
mkws("ws_inc_repo", {"MODULE.bazel": 'module(name="test")\ninclude("@foo//:inc.MODULE.bazel")\n'})
mkws("ws_inc_cycle", {
    "MODULE.bazel": 'module(name="test")\ninclude("//:cycle.MODULE.bazel")\n',
    "cycle.MODULE.bazel": 'include("//:cycle.MODULE.bazel")\n'
})

# bazel_compatibility cases
mkws("ws_compat_ok", {"MODULE.bazel": 'module(name="test", bazel_compatibility=[">=1.0.0", "<=99.0.0"])\n'})
mkws("ws_compat_fail", {"MODULE.bazel": 'module(name="test", bazel_compatibility=[">=99.0.0"])\n'})
mkws("ws_compat_neg", {"MODULE.bazel": 'module(name="test", bazel_compatibility=["-9.3.2"])\n'})
mkws("ws_compat_neg_ok", {"MODULE.bazel": 'module(name="test", bazel_compatibility=["-1.0.0"])\n'})
mkws("ws_compat_bad", {"MODULE.bazel": 'module(name="test", bazel_compatibility=["~1"])\n'})

# Overrides
mkws("ws_lpo_ok", {
    "MODULE.bazel": 'module(name="test")\nbazel_dep(name="dep")\nlocal_path_override(module_name="dep", path="dep_dir")\n',
    "dep_dir/MODULE.bazel": 'module(name="dep")\n',
    "dep_dir/BUILD.bazel": 'filegroup(name="g", visibility=["//visibility:public"])\n'
})
mkws("ws_lpo_missing", {
    "MODULE.bazel": 'module(name="test")\nbazel_dep(name="dep")\nlocal_path_override(module_name="dep", path="nope_dir")\n'
})
mkws("ws_override_ws", {
    "MODULE.bazel": 'module(name="test")\nbazel_dep(name="dep")\n',
    "sub_dep/MODULE.bazel": 'module(name="dep")\n',
    "sub_dep/BUILD.bazel": 'filegroup(name="g", visibility=["//visibility:public"])\n'
})
mkws("ws_git_noargs", {
    "MODULE.bazel": 'module(name="test")\nbazel_dep(name="dep")\ngit_override(module_name="dep", remote="file:///nonexistent/nope")\n'
})
mkws("ws_use_repo_rule_dev", {
    "MODULE.bazel": 'module(name="test")\nr = use_repo_rule("//:rules.bzl", "my_rule", dev_dependency=True)\n',
    "rules.bzl": 'def _impl(rctx): pass\nmy_rule = repository_rule(implementation=_impl)\n'
})
mkws("ws_isolate_err", {
    "MODULE.bazel": 'module(name="test")\next = use_extension("//:rules.bzl", "my_ext", isolate=True)\n',
    "rules.bzl": 'def _impl(ctx): pass\nmy_ext = module_extension(implementation=_impl)\n'
})

# Build-based cases with genrules
mkws("ws_build", {
    "MODULE.bazel": 'module(name="test", version="0.1")\nbazel_dep(name="platforms", version="1.0.0")\n',
    "BUILD.bazel": '''package(default_visibility=["//visibility:public"])

genrule(
    name = "g1",
    outs = ["g1.txt"],
    cmd = "echo hello_g1 > $@",
)

genrule(
    name = "g2",
    srcs = [":g1"],
    outs = ["g2.txt"],
    cmd = "cat $< > $@ && echo hello_g2 >> $@",
)

genrule(
    name = "g3",
    srcs = [":g2"],
    outs = ["g3.txt"],
    cmd = "wc -l < $< > $@",
)
''',
})

# repository_ctx and module_ctx cases
mkws("ws_ctx_unknown_kw", {
    "MODULE.bazel": 'module(name="test")\nr = use_repo_rule("//:rules.bzl", "r")\nr(name="rr")\n',
    "rules.bzl": 'def _impl(rctx): rctx.template("out.txt", "in.txt", substitutions={}, watch="no")\nr = repository_rule(implementation=_impl)\n',
    "in.txt": "hello\n",
})
mkws("ws_ctx_mctx_delete", {
    "MODULE.bazel": 'module(name="test")\next = use_extension("//:ext.bzl", "del_ext")\nuse_repo(ext, "foo")\n',
    "ext.bzl": 'def _impl(ctx): ctx.delete("foo")\ndel_ext = module_extension(implementation=_impl)\n',
})
mkws("ws_ctx_watch_workdir", {
    "MODULE.bazel": 'module(name="test")\nr = use_repo_rule("//:rules.bzl", "r")\nr(name="rr")\n',
    "rules.bzl": 'def _impl(rctx): rctx.watch("BUILD.bazel")\nr = repository_rule(implementation=_impl)\n',
})
mkws("ws_ctx_read_watch_bad", {
    "MODULE.bazel": 'module(name="test")\nr = use_repo_rule("//:rules.bzl", "r")\nr(name="rr")\n',
    "rules.bzl": 'def _impl(rctx): rctx.file("f.txt", "hi"); rctx.read("f.txt", watch="bad")\nr = repository_rule(implementation=_impl)\n',
})
mkws("ws_ctx_delete_outside", {
    "MODULE.bazel": 'module(name="test")\nr = use_repo_rule("//:rules.bzl", "r")\nr(name="rr")\n',
    "rules.bzl": '''def _impl(rctx):
    res = rctx.delete("../outside")
    if res != False: fail("expected False")
    rctx.file("BUILD.bazel", "")
r = repository_rule(implementation=_impl)
''',
})
mkws("ws_ctx_which_slash", {
    "MODULE.bazel": 'module(name="test")\nr = use_repo_rule("//:rules.bzl", "r")\nr(name="rr")\n',
    "rules.bzl": 'def _impl(rctx): rctx.which("/bin/sh")\nr = repository_rule(implementation=_impl)\n',
})
mkws("ws_ctx_int_hex", {
    "MODULE.bazel": 'module(name="test")\nr = use_repo_rule("//:rules.bzl", "r")\nr(name="rr")\n',
    "rules.bzl": '''def _impl(rctx):
    if int("0x1f", 16) != 31: fail("bad hex")
    rctx.file("BUILD.bazel", "")
r = repository_rule(implementation=_impl)
''',
})
mkws("ws_ctx_dir_rctx", {
    "MODULE.bazel": 'module(name="test")\nr = use_repo_rule("//:rules.bzl", "r")\nr(name="rr")\n',
    "rules.bzl": '''def _impl(rctx):
    d = dir(rctx)
    for m in ["attr", "file", "read", "which", "execute", "path"]:
        if m not in d: fail("missing " + m)
    rctx.file("BUILD.bazel", "")
r = repository_rule(implementation=_impl)
''',
})
mkws("ws_ctx_dir_mctx", {
    "MODULE.bazel": 'module(name="test")\next = use_extension("//:ext.bzl", "ext")\nuse_repo(ext, "foo")\n',
    "ext.bzl": '''def _gen(rctx):
    rctx.file("BUILD.bazel", "")
gen = repository_rule(implementation=_gen)

def _impl(ctx):
    d = dir(ctx)
    for m in ["modules", "is_dev_dependency", "which", "execute", "download", "extension_metadata"]:
        if m not in d: fail("missing " + m)
    gen(name="foo")
ext = module_extension(implementation=_impl)
''',
})
mkws("ws_ctx_label_str", {
    "MODULE.bazel": 'module(name="test")\next = use_extension("//:ext.bzl", "ext")\nuse_repo(ext, "foo")\n',
    "ext.bzl": '''def _gen(rctx):
    rctx.file("BUILD.bazel", "")
gen = repository_rule(implementation=_gen)

def _impl(ctx):
    s = str(Label("//:x"))
    if s != "@@//:x": fail("unexpected label str: " + s)
    gen(name="foo")
ext = module_extension(implementation=_impl)
''',
})
mkws("ws_ctx_attr_bad", {
    "MODULE.bazel": 'module(name="test")\nr = use_repo_rule("//:rules.bzl", "r")\nr(name="rr", bad_attr="foo")\n',
    "rules.bzl": 'def _impl(rctx): rctx.file("BUILD.bazel", "")\nr = repository_rule(implementation=_impl, attrs={"good_attr": attr.string()})\n',
})
mkws("ws_ctx_ext_return_int", {
    "MODULE.bazel": 'module(name="test")\next = use_extension("//:ext.bzl", "ext")\nuse_repo(ext, "foo")\n',
    "ext.bzl": 'def _impl(ctx): return 5\next = module_extension(implementation=_impl)\n',
})
mkws("ws_ctx_ext_meta_bad", {
    "MODULE.bazel": 'module(name="test")\next = use_extension("//:ext.bzl", "ext")\nuse_repo(ext, "foo")\n',
    "ext.bzl": '''def _impl(ctx):
    return ctx.extension_metadata(root_module_direct_deps=["nonexistent"], root_module_direct_dev_deps=[])
ext = module_extension(implementation=_impl)
''',
})
mkws("ws_ctx_exec_timeout", {
    "MODULE.bazel": 'module(name="test")\nr = use_repo_rule("//:rules.bzl", "r")\nr(name="rr")\n',
    "rules.bzl": '''def _impl(rctx):
    res = rctx.execute(["sleep", "3"], timeout=1)
    if "Timed out" not in res.stderr: fail("expected Timed out in stderr")
    rctx.file("BUILD.bazel", "")
r = repository_rule(implementation=_impl)
''',
})
mkws("ws_ctx_exec_nonexist", {
    "MODULE.bazel": 'module(name="test")\nr = use_repo_rule("//:rules.bzl", "r")\nr(name="rr")\n',
    "rules.bzl": '''def _impl(rctx):
    res = rctx.execute(["nonexistent_bin_123"])
    if res.return_code == 0: fail("expected non-zero return code")
    rctx.file("BUILD.bazel", "")
r = repository_rule(implementation=_impl)
''',
})

# Native repo rules
mkws("ws_native_new_local", {
    "MODULE.bazel": 'module(name="test")\nnew_local_repository = use_repo_rule("@bazel_tools//tools/build_defs/repo:local.bzl", "new_local_repository")\nnew_local_repository(name="loc", path="loc_src", build_file_content="filegroup(name=\'all\')")\n',
    "loc_src/file.txt": "hello\n",
})
mkws("ws_native_http_file", {
    "MODULE.bazel": 'module(name="test")\nhttp_file = use_repo_rule("@bazel_tools//tools/build_defs/repo:http.bzl", "http_file")\nhttp_file(name="hf", urls=["file://' + os.path.join(TEST_ROOT, "ws_native_http_file", "sample.txt") + '"])\n',
    "sample.txt": "sample content\n",
})

# Archive cases (tar, zip, gz)
import tarfile, io, gzip, zipfile

arch_dir = os.path.join(TEST_ROOT, "archives")
os.makedirs(arch_dir, exist_ok=True)

# 1. Tar symlink with .. leaving repo dir
tar_symlink_path = os.path.join(arch_dir, "symlink_dotdot.tar")
with tarfile.open(tar_symlink_path, "w") as tar:
    ti_dir = tarfile.TarInfo(name="pkg")
    ti_dir.type = tarfile.DIRTYPE
    ti_dir.mode = 0o755
    tar.addfile(ti_dir)
    ti_sym = tarfile.TarInfo(name="pkg/dotdot")
    ti_sym.type = tarfile.SYMTYPE
    ti_sym.linkname = "../outside"
    tar.addfile(ti_sym)
    f_content = b"filegroup(name='all')\n"
    ti_f = tarfile.TarInfo(name="pkg/BUILD.bazel")
    ti_f.size = len(f_content)
    ti_f.mode = 0o644
    tar.addfile(ti_f, io.BytesIO(f_content))

mkws("ws_tar_symlink_dotdot", {
    "MODULE.bazel": 'module(name="test")\nhttp_archive = use_repo_rule("@bazel_tools//tools/build_defs/repo:http.bzl", "http_archive")\nhttp_archive(name="ar", urls=["file://' + tar_symlink_path + '"], strip_prefix="pkg")\n',
})

# 2. Bare gzip with FNAME
gz_fname_path = os.path.join(arch_dir, "test_file.txt.gz")
with open(gz_fname_path, "wb") as f:
    with gzip.GzipFile(filename="extracted_name.txt", mode="wb", fileobj=f) as gz:
        gz.write(b"content inside gzip\n")

mkws("ws_bare_gz", {
    "MODULE.bazel": 'module(name="test")\nhttp_file = use_repo_rule("@bazel_tools//tools/build_defs/repo:http.bzl", "http_file")\nhttp_file(name="gzfile", urls=["file://' + gz_fname_path + '"])\n',
})

# 3. Zip archive extraction
zip_path = os.path.join(arch_dir, "test.zip")
with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED) as zf:
    zf.writestr("pkg/BUILD.bazel", "filegroup(name='all')\n")
    zf.writestr("pkg/data.txt", "zip test data\n")

mkws("ws_zip_extract", {
    "MODULE.bazel": 'module(name="test")\nhttp_archive = use_repo_rule("@bazel_tools//tools/build_defs/repo:http.bzl", "http_archive")\nhttp_archive(name="zar", urls=["file://' + zip_path + '"], strip_prefix="pkg")\n',
})

print("Test workspaces initialized at:", TEST_ROOT)
