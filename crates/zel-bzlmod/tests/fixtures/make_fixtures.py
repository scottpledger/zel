#!/usr/bin/env python3
import os
import json
import shutil

FX_DIR = os.path.dirname(os.path.abspath(__file__))
R = os.path.join(FX_DIR, "reg")
S = os.path.join(FX_DIR, "mods")
W = os.path.join(FX_DIR, "main")

shutil.rmtree(R, ignore_errors=True)
os.makedirs(R, exist_ok=True)
with open(os.path.join(R, "bazel_registry.json"), "w") as f:
    json.dump({"module_base_path": S}, f)
shutil.rmtree(S, ignore_errors=True)
os.makedirs(S, exist_ok=True)

# Stub modules of the builtin (bazel_tools) dependency closure, shared with the bzlmod harness.
_STUBS = os.path.join(FX_DIR, "..", "..", "..", "..", "scripts", "bzlmod_harness", "closure_stubs")
for _n in os.listdir(os.path.join(_STUBS, "reg")):
    shutil.copytree(os.path.join(_STUBS, "reg", _n), os.path.join(R, "modules", _n), dirs_exist_ok=True)
for _d in os.listdir(os.path.join(_STUBS, "mods")):
    shutil.copytree(os.path.join(_STUBS, "mods", _d), os.path.join(S, _d), dirs_exist_ok=True)

def mod(name, ver, deps=(), compat=1, files=None, yanked=None, extra=""):
    d = os.path.join(R, "modules", name, ver)
    os.makedirs(d, exist_ok=True)
    m = f'module(name="{name}", version="{ver}", compatibility_level={compat})\n'
    for dn, dv in deps:
        m += f'bazel_dep(name="{dn}", version="{dv}")\n'
    m += extra
    with open(os.path.join(d, "MODULE.bazel"), "w") as f:
        f.write(m)
    sd = os.path.join(S, f"{name}+{ver}")
    os.makedirs(sd, exist_ok=True)
    with open(os.path.join(sd, "MODULE.bazel"), "w") as f:
        f.write(m)
    with open(os.path.join(sd, "BUILD.bazel"), "w") as f:
        f.write('filegroup(name="all", srcs=glob(["*.txt"]), visibility=["//visibility:public"])\n')
    with open(os.path.join(sd, "f.txt"), "w") as f:
        f.write(f"{name}{ver}\n")
    for fn, c in (files or {}).items():
        fp = os.path.join(sd, fn)
        os.makedirs(os.path.dirname(fp), exist_ok=True)
        with open(fp, "w") as f:
            f.write(c)
    with open(os.path.join(d, "source.json"), "w") as f:
        json.dump({"type": "local_path", "path": f"{name}+{ver}"}, f)
    md = os.path.join(R, "modules", name, "metadata.json")
    cur = json.load(open(md)) if os.path.exists(md) else {"versions": [], "yanked_versions": {}}
    if ver not in cur["versions"]:
        cur["versions"].append(ver)
    if yanked:
        cur["yanked_versions"][ver] = yanked
    with open(md, "w") as f:
        json.dump(cur, f)

mod('a', '1.0')
mod('a', '1.1', yanked="bad")
mod('a', '1.2')
mod('a', '2.0', compat=2)
mod('b', '1.0', deps=[('a', '1.0')])
mod('c', '1.0', deps=[('a', '1.1'), ('b', '1.0')])
mod('nodep', '1.0')
mod('devm', '1.0')
plat_files = {
    'host/constraints.bzl': 'HOST_CONSTRAINTS = []\n',
    'host/extension.bzl': 'def host_platform_repo(**kwargs): pass\n',
    'host/BUILD.bazel': 'alias(name="host", actual="//:host", visibility=["//visibility:public"])\n',
    'BUILD.bazel': 'package(default_visibility=["//visibility:public"])\nplatform(name="host")\n',
}
mod('platforms', '0.0.10', files=plat_files)
mod('platforms', '1.0.0', files=plat_files)

extbzl = '''def _gen_r(rctx):
    rctx.file("BUILD.bazel", "filegroup(name='g', visibility=['//visibility:public'])\\n")
    rctx.file("v.txt", rctx.attr.v)
gen = repository_rule(implementation=_gen_r, attrs={"v":attr.string()})

def _impl(ctx):
    for m in ctx.modules:
        for t in m.tags.mk:
            gen(name=t.name, v=m.name)
    return ctx.extension_metadata(reproducible=True)

mk = tag_class(attrs={"name":attr.string()})
ext = module_extension(implementation=_impl, tag_classes={"mk":mk})
'''
mod('ext', '1.0', files={'ext.bzl': extbzl}, extra='ext = use_extension("//:ext.bzl","ext")\next.mk(name="from_ext")\nuse_repo(ext,"from_ext")\n')

# Main workspace
shutil.rmtree(W, ignore_errors=True)
os.makedirs(W, exist_ok=True)
with open(os.path.join(W, "MODULE.bazel"), "w") as f:
    f.write('''module(name="main", version="0.1")
bazel_dep(name="c", version="1.0")
bazel_dep(name="a", version="1.1", repo_name="aa")
bazel_dep(name="ext", version="1.0")
bazel_dep(name="nodep", version="1.0", repo_name=None)
bazel_dep(name="devm", version="1.0", dev_dependency=True)
ext = use_extension("@ext//:ext.bzl", "ext")
ext.mk(name="from_main")
use_repo(ext, "from_main")
''')

with open(os.path.join(W, "BUILD.bazel"), "w") as f:
    f.write('filegroup(name="t", srcs=["@aa//:all"])\n')

# Add stubs for cc_configure in rules_cc to satisfy bazel_tools
for v in ["0.2.25", "0.2.17"]:
    cdir = os.path.join(S, f"rules_cc+{v}", "cc", "private", "toolchain")
    os.makedirs(cdir, exist_ok=True)
    with open(os.path.join(cdir, "BUILD.bazel"), "w") as f:
        f.write("")
    with open(os.path.join(cdir, "cc_configure.bzl"), "w") as f:
        f.write("def cc_configure(): pass\n")
    tdir = os.path.join(S, f"rules_cc+{v}", "cc", "toolchains")
    os.makedirs(tdir, exist_ok=True)
    with open(os.path.join(tdir, "BUILD.bazel"), "w") as f:
        f.write("")
    with open(os.path.join(tdir, "toolchain_config_utils.bzl"), "w") as f:
        f.write("""MSVC_ENVVARS = []
def find_vc_path(ctx): return None
def setup_vc_env_vars(ctx, vc_path): return {}
""")

print("Fixtures created successfully in", FX_DIR)
