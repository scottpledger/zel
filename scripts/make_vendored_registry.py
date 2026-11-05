#!/usr/bin/env python3
"""Builds a local Bazel registry (file:// usable via --registry) from third_party/bcr.

Usage: make_vendored_registry.py <out_dir>
Each module version becomes modules/<name>/<version>/{MODULE.bazel,source.json(local_path)}.
"""
import json, os, shutil, sys

root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "third_party", "bcr")
root = os.path.abspath(root)
out = os.path.abspath(sys.argv[1])
os.makedirs(out, exist_ok=True)
json.dump({"mirrors": []}, open(os.path.join(out, "bazel_registry.json"), "w"))
for name in sorted(os.listdir(root)):
    vers = sorted(os.listdir(os.path.join(root, name)))
    md = os.path.join(out, "modules", name)
    os.makedirs(md, exist_ok=True)
    json.dump({"versions": vers, "yanked_versions": {}}, open(os.path.join(md, "metadata.json"), "w"))
    for v in vers:
        d = os.path.join(md, v)
        os.makedirs(d, exist_ok=True)
        shutil.copy(os.path.join(root, name, v, "MODULE.bazel"), os.path.join(d, "MODULE.bazel"))
        json.dump({"type": "local_path", "path": os.path.join(root, name, v)}, open(os.path.join(d, "source.json"), "w"))
mods = os.path.abspath(os.path.join(root, "..", "bcr_modules"))
if os.path.isdir(mods):
    for name in sorted(os.listdir(mods)):
        md = os.path.join(out, "modules", name)
        os.makedirs(md, exist_ok=True)
        meta = os.path.join(md, "metadata.json")
        have = set(json.load(open(meta))["versions"]) if os.path.exists(meta) else set()
        for v in sorted(os.listdir(os.path.join(mods, name))):
            d = os.path.join(md, v)
            os.makedirs(d, exist_ok=True)
            if not os.path.exists(os.path.join(d, "MODULE.bazel")):
                shutil.copy(os.path.join(mods, name, v, "MODULE.bazel"), os.path.join(d, "MODULE.bazel"))
                # Only MODULE.bazel is vendored for this version: serve the highest vendored sources
                # (MVS may select a lower version than the one vendored).
                src = os.path.join(mods, name, v)
                if os.path.isdir(os.path.join(root, name)):
                    vs = sorted(os.listdir(os.path.join(root, name)))
                    src = os.path.join(root, name, vs[-1])
                json.dump({"type": "local_path", "path": src}, open(os.path.join(d, "source.json"), "w"))
            have.add(v)
        json.dump({"versions": sorted(have), "yanked_versions": {}}, open(meta, "w"))
print("registry at", out)
