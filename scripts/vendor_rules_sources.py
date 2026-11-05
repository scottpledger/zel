#!/usr/bin/env python3
"""Download pinned BCR modules (the pins of ~/Development/bazel/src/MODULE.tools, Bazel 9.3.2) into a vendor directory.

Usage: vendor_rules_sources.py <out_dir>
Layout: <out_dir>/<module>/<version>/ (extracted sources, patches applied per source.json),
plus a copy of the registry's MODULE.bazel as <out_dir>/<module>/<version>/MODULE.bazel.
"""
import hashlib, io, json, os, subprocess, sys, tarfile, urllib.request, zipfile

REG = "https://bcr.bazel.build/modules"
# The pins of Bazel 9.3.2's src/MODULE.tools (the @bazel_tools module). Kept in sync by
# `vendor_registry_closure.py`, which re-reads them from the Bazel checkout and fails on any drift.
MODULES = {
    "rules_cc": "0.2.17", "rules_java": "9.1.0", "rules_python": "1.7.0",
    "protobuf": "33.4", "platforms": "1.0.0", "rules_license": "1.0.0",
    "rules_shell": "0.6.1", "abseil-cpp": "20250814.1", "bazel_features": "1.42.1",
    "apple_support": "2.5.2", "zlib": "1.3.1.bcr.5", "rules_apple": "4.1.0",
    "rules_swift": "3.1.2", "buildozer": "8.5.1",
}

def get(url, tries=6):
    import time
    for i in range(tries):
        try:
            with urllib.request.urlopen(url, timeout=120) as r:
                return r.read()
        except Exception as e:
            print("retry", url, e, file=sys.stderr)
            if i == tries - 1: raise
            time.sleep(2 ** i)

def main(out):
    for name, ver in MODULES.items():
        dest = os.path.join(out, name, ver)
        if os.path.isdir(dest) and os.listdir(dest):
            print("skip", name, ver); continue
        os.makedirs(dest, exist_ok=True)
        src = json.loads(get(f"{REG}/{name}/{ver}/source.json"))
        data = get(src["url"])
        want = src.get("integrity", "")
        if want.startswith("sha256-"):
            import base64
            got = base64.b64encode(hashlib.sha256(data).digest()).decode()
            assert got == want[7:], f"integrity mismatch {name}"
        strip = src.get("strip_prefix", "")
        if src["url"].endswith(".zip"):
            z = zipfile.ZipFile(io.BytesIO(data))
            for m in z.namelist():
                if m.endswith("/"): continue
                rel = m[len(strip) + 1:] if strip and m.startswith(strip + "/") else m
                if not rel: continue
                p = os.path.join(dest, rel); os.makedirs(os.path.dirname(p), exist_ok=True)
                open(p, "wb").write(z.read(m)); os.chmod(p, 0o755 if (z.getinfo(m).external_attr >> 16) & 0o111 else 0o644)
        else:
            t = tarfile.open(fileobj=io.BytesIO(data))
            for m in t.getmembers():
                if not m.isfile(): continue
                rel = m.name[len(strip) + 1:] if strip and m.name.startswith(strip + "/") else m.name
                if not rel: continue
                p = os.path.join(dest, rel); os.makedirs(os.path.dirname(p), exist_ok=True)
                open(p, "wb").write(t.extractfile(m).read()); os.chmod(p, 0o755 if m.mode & 0o111 else 0o644)
        for pf, _ in (src.get("patches") or {}).items():
            patch = get(f"{REG}/{name}/{ver}/patches/{pf}")
            subprocess.run(["patch", f"-p{src.get('patch_strip', 0)}", "-d", dest],
                           input=patch, check=True)
        open(os.path.join(dest, "MODULE.bazel"), "wb").write(get(f"{REG}/{name}/{ver}/MODULE.bazel"))
        print("ok", name, ver)

if __name__ == "__main__":
    main(sys.argv[1])
