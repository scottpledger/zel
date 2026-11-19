#!/usr/bin/env python3
"""
scripts/sweep_flag_acceptance.py

Per-command flag-acceptance sweep: every flag x every command, in real Bazel and in Zel.

For each flag of the real binary's `help flags-as-proto` and each command C (except `startup`), `canonicalize-flags
--for_command=C -- --<flag>[=x]` is run in both binaries (it parses the flag with exactly the option set of C and
nothing else):

  * pairs where C is in the flag's `commands` list ("valid"): both binaries must accept the flag (i.e. not fail with
    "Unrecognized option"), and their complete output (stdout, stderr, exit code) must be identical;
  * all other pairs ("invalid"): both must reject it with the same message and exit code.

Flags that only exist on one side, or whose `commands` list differs between real and Zel (already
accounted for by scripts/verify_flags_vs_bazel.sh) are skipped and listed. Real bazel results are cached under
$ZEL_SWEEP_CACHE (default /tmp/zel_sweep_cache) keyed by flag/command/real version.

Usage: sweep_flag_acceptance.py [--jobs N] [--commands a,b] [--limit N] [--sample K] [--no-cache]
                                [--values 1,-1,a=b,//:t] [--first-command-only]
(`--values ... --first-command-only` is the converter-validation sweep: every flag probed with several garbage values.)
Full sweep (23k pairs): scripts/heavy.sh python3 scripts/sweep_flag_acceptance.py --jobs 2
Environment: REAL_BAZEL, ZEL, BAZEL_SRC_DIR (for protoc/bazel_flags.proto), ZEL_FLAGS_DUMP.
Exit status 0 iff no unexplained difference.
"""

import argparse
import concurrent.futures
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import flags_proto_diff as fpd  # noqa: E402

REPO = os.path.dirname(HERE)
REAL = os.environ.get("REAL_BAZEL") or "/tmp/bazel-9.3.2"  # the real 9.3.2 binary (not bazelisk)
TARGET = os.environ.get("CARGO_TARGET_DIR", os.path.join(REPO, "target"))
ZEL = os.environ.get("ZEL", os.path.join(TARGET, "debug", "zel"))
BAZEL_SRC = os.environ.get("BAZEL_SRC_DIR", os.path.expanduser("~/Development/bazel"))
DUMP = os.environ.get("ZEL_FLAGS_DUMP", os.path.join(TARGET, "debug", "zel-flags-dump"))


def run(cmd, cwd, timeout=120):
    # Zel acts as bazelisk when USE_BAZEL_VERSION is set (it would re-exec real Bazel and make the comparison vacuous).
    env = {k: v for k, v in os.environ.items() if k != "USE_BAZEL_VERSION"}
    p = subprocess.run(cmd, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout, env=env)
    return p.returncode, p.stdout.decode("utf-8", "replace"), p.stderr.decode("utf-8", "replace")


def decode_flags(b64_text, tmp):
    path = os.path.join(tmp, "flags.b64")
    with open(path, "w") as fh:
        fh.write(b64_text)
    txt = os.path.join(tmp, "flags.txt")
    proto = os.path.join(BAZEL_SRC, "src/main/protobuf/bazel_flags.proto")
    if shutil.which("protoc") and os.path.exists(proto):
        raw = subprocess.run(["base64", "-D"], stdin=open(path), stdout=subprocess.PIPE).stdout
        out = subprocess.run(["protoc", "--proto_path=" + os.path.dirname(proto), "--decode=bazel_flags.FlagCollection",
                              "bazel_flags.proto"], input=raw, stdout=subprocess.PIPE).stdout
        with open(txt, "wb") as fh:
            fh.write(out)
    else:
        with open(txt, "wb") as fh:
            fh.write(subprocess.run([DUMP], stdin=open(path), stdout=subprocess.PIPE).stdout)
    return fpd.parse(txt)


NOISE = ("Starting local Bazel server", "Bazelisk version", "WARNING: Running Bazel server needs to be killed",
         "Extracting Bazel installation", "Computing main repo mapping", "Another command (pid=")


def normalize(s):
    return "\n".join(l for l in s.split("\n") if not l.startswith(NOISE))


def flag_of(arg):
    return arg.lstrip("-").split("=", 1)[0]


def probe_args(flag, command, variant):
    """`variant` is "pos" (the default probe value `x`), "val:<value>" (a garbage-value probe) or "neg"."""
    name = flag["name"]
    if variant == "pos":
        return "--%s" % name if flag.get("requires_value") == "false" else "--%s=x" % name
    if variant.startswith("val:"):
        return "--%s=%s" % (name, variant[4:])
    return "--no%s" % name


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--jobs", type=int, default=6)
    ap.add_argument("--commands", default="")
    ap.add_argument("--limit", type=int, default=0, help="only the first N flags (debugging)")
    ap.add_argument("--sample", type=int, default=0, help="only every K-th flag")
    ap.add_argument("--no-cache", action="store_true")
    ap.add_argument("--values", default="",
                    help="comma-separated extra garbage values probed as --flag=<value> (besides `x`), e.g. 1,-1,a=b,//:t")
    ap.add_argument("--max-report", type=int, default=60, help="how many mismatches to print (all go to --json-out)")
    ap.add_argument("--json-out", default="", help="write every mismatch as JSON to this file")
    ap.add_argument("--first-command-only", action="store_true",
                    help="probe each flag only with the first command of its `commands` list (value sweeps)")
    args = ap.parse_args()

    work = tempfile.mkdtemp(prefix="zel_sweep.")
    ws = os.path.join(work, "ws")
    os.makedirs(ws)
    open(os.path.join(ws, "WORKSPACE"), "w").close()
    # A MODULE.bazel avoids the one-time "Bazel will create an empty MODULE.bazel file" warning of real Bazel.
    open(os.path.join(ws, "MODULE.bazel"), "w").close()
    cache_dir = os.environ.get("ZEL_SWEEP_CACHE", "/tmp/zel_sweep_cache")
    os.makedirs(cache_dir, exist_ok=True)

    rc, real_b64, err = run([REAL, "--output_base=%s/real_flags" % work, "help", "flags-as-proto"], ws)
    if rc != 0:
        print("real bazel failed: " + err, file=sys.stderr)
        return 2
    _, real_ver, _ = run([REAL, "--output_base=%s/real_flags" % work, "--max_idle_secs=30", "version"], ws)
    real_key = hashlib.sha1(real_ver.encode()).hexdigest()[:10]
    rc, zel_b64, err = run([ZEL, "--output_base=%s/zel_flags" % work, "help", "flags-as-proto"], ws)
    if rc != 0:
        print("zel failed: " + err, file=sys.stderr)
        return 2
    real_flags = {f["name"]: f for f in decode_flags(real_b64, work)}
    zel_flags = {f["name"]: f for f in decode_flags(zel_b64, work + "/..") } if False else None
    zdir = os.path.join(work, "z")
    os.makedirs(zdir)
    zel_flags = {f["name"]: f for f in decode_flags(zel_b64, zdir)}

    commands = sorted({c for f in real_flags.values() for c in f.get("commands", []) if c != "startup"}
                      | {c for f in zel_flags.values() for c in f.get("commands", []) if c != "startup"})
    if args.commands:
        commands = [c for c in commands if c in args.commands.split(",")]
    names = sorted(real_flags)
    skipped = []
    todo = []
    for name in names:
        rf, zf = real_flags[name], zel_flags.get(name)
        if zf is None:
            skipped.append((name, "only in real"))
            continue
        if rf.get("commands") != zf.get("commands"):
            skipped.append((name, "commands differ (real=%s zel=%s)" % (rf.get("commands"), zf.get("commands"))))
            continue
        if rf.get("commands") == ["startup"]:
            continue
        todo.append(rf)
    for name in sorted(set(zel_flags) - set(real_flags)):
        skipped.append((name, "only in zel"))
    if args.sample:
        todo = todo[::args.sample]
    if args.limit:
        todo = todo[:args.limit]

    jobs = []
    extra_values = [v for v in args.values.split(",") if v != ""] if args.values else []
    for f in todo:
        cmds = commands
        if args.first_command_only:
            cmds = [c for c in f.get("commands", []) if c in commands][:1]
        for c in cmds:
            valid = c in f.get("commands", [])
            variants = ["pos"] + ["val:" + v for v in extra_values if f.get("requires_value") != "false"] \
                + (["neg"] if f.get("has_negative_flag") == "true" and not args.first_command_only else [])
            for v in variants:
                jobs.append((f, c, v, valid))

    tls = threading.local()
    counter = [0]
    lock = threading.Lock()

    def slot():
        if not hasattr(tls, "n"):
            with lock:
                tls.n = counter[0]
                counter[0] += 1
        return tls.n

    results = []

    def one(job):
        f, c, v, valid = job
        arg = probe_args(f, c, v)
        key = hashlib.sha1(("%s|%s|%s|%s" % (real_key, c, arg, "m")).encode()).hexdigest()
        cpath = os.path.join(cache_dir, key + ".json")
        n = slot()
        cmd_tail = ["canonicalize-flags", "--for_command=" + c, "--", arg]
        real_out = None
        if not args.no_cache and os.path.exists(cpath):
            with open(cpath) as fh:
                real_out = json.load(fh)
        if real_out is None:
            r = run([REAL, "--output_base=%s/real_%d" % (work, n), "--max_idle_secs=60"] + cmd_tail, ws)
            real_out = [r[0], normalize(r[1]), normalize(r[2])]
            with open(cpath, "w") as fh:
                json.dump(real_out, fh)
        z = run([ZEL, "--output_base=%s/zel_%d" % (work, n), "--max_idle_secs=60"] + cmd_tail, ws)
        zel_out = [z[0], normalize(z[1]), normalize(z[2])]
        return (job, real_out, zel_out)

    mismatches = []
    stats = {"valid": 0, "invalid": 0}
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as ex:
        for i, (job, real_out, zel_out) in enumerate(ex.map(one, jobs)):
            f, c, v, valid = job
            arg = probe_args(f, c, v)
            stats["valid" if valid else "invalid"] += 1
            real_unrec = "Unrecognized option" in real_out[2]
            zel_unrec = "Unrecognized option" in zel_out[2]
            problems = []
            if valid and real_unrec:
                # real's flags-as-proto over-approximates (documented in verify-commands.md): it lists commands that
                # reject the flag; then zel must reject it identically.
                valid = False
            if valid and zel_unrec:
                problems.append("valid pair rejected by zel")
            if real_out != zel_out:
                problems.append("output differs")
            if problems:
                mismatches.append((c, arg, problems, real_out, zel_out))
            if (i + 1) % 2000 == 0:
                print("... %d/%d pairs, %d mismatches" % (i + 1, len(jobs), len(mismatches)), file=sys.stderr, flush=True)

    # shut down servers (real and zel), then the caller may run scripts/cleanup_servers.sh
    for n in range(counter[0]):
        subprocess.run([ZEL, "--output_base=%s/zel_%d" % (work, n), "shutdown"], cwd=ws, stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL)
        subprocess.run([REAL, "--output_base=%s/real_%d" % (work, n), "shutdown"], cwd=ws, stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL)
    print("flags swept: %d, commands: %d, pairs: %d (valid %d, invalid %d); skipped flags: %d" % (
        len(todo), len(commands), len(jobs), stats["valid"], stats["invalid"], len(skipped)))
    for name, why in skipped[:40]:
        print("  skipped %s: %s" % (name, why[:120]))
    if len(skipped) > 40:
        print("  ... %d more skipped" % (len(skipped) - 40))
    print("pairs identical: %d of %d (%.2f%%)" % (len(jobs) - len(mismatches), len(jobs),
                                                  100.0 * (len(jobs) - len(mismatches)) / max(1, len(jobs))))
    print("mismatches: %d" % len(mismatches))
    if args.json_out:
        with open(args.json_out, "w") as fh:
            json.dump([{"command": c, "arg": a, "problems": p, "real": r, "zel": z} for c, a, p, r, z in mismatches], fh,
                      indent=1)
    for c, arg, problems, ro, zo in mismatches[:args.max_report]:
        print("  [%s] %s: %s\n     real: %r\n     zel : %r" % (c, arg, "; ".join(problems), ro, zo))
    shutil.rmtree(work, ignore_errors=True)
    return 1 if mismatches else 0


if __name__ == "__main__":
    sys.exit(main())
