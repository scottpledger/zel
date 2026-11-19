#!/usr/bin/env python3
"""Verifier harness (round 5). usage: h.py <casefile> <outdir> [seq]
Each line: optional 'PRE: shell ;; ' prefix then bazel args (shlex).
Minimal normalization: abs paths, timings, hashes, uuid, ansi, progress lines. (`INFO: N processes` and `N total actions` are compared.)"""
import sys, os, re, shlex, subprocess, shutil, concurrent.futures as cf
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "harness_common"))
import compare as C
HERE = os.path.dirname(os.path.abspath(__file__))
ZEL = os.environ.get("ZEL_BIN", "/tmp/zel-target/release/zel")
BAZEL = os.environ.get("BAZEL_BIN", "/tmp/bazel-9.3.2")  # the real 9.3.2 binary, never bazelisk
FX = os.environ.get("FX", os.path.join(HERE, "fixtures", "ws1"))
def run(binp, ws, ob, args, timeout=150):
    try:
        env = dict(os.environ)
        env.pop("USE_BAZEL_VERSION", None)  # zel acts as bazelisk when it is set
        p = subprocess.run([binp, "--output_base=" + ob, "--max_idle_secs=60"] + args, cwd=ws, capture_output=True, timeout=timeout, stdin=subprocess.DEVNULL, env=env)
        return p.stdout.decode(errors="replace"), p.stderr.decode(errors="replace"), p.returncode
    except subprocess.TimeoutExpired:
        return "", "TIMEOUT", 999
def norm(s, ws, ob):
    s = s.replace("/private/tmp", "/tmp")
    ws = ws.replace("/private/tmp", "/tmp"); ob = ob.replace("/private/tmp", "/tmp")
    s = s.replace(ws, "WS").replace(ob, "OB")
    s = re.sub(r"/tmp/vb5/[^ /]+/(w[bz]|ob-[bz])\S*", "OB", s)
    s = re.sub(r"/tmp/vb5-[bz]-[^-\s/]+-", "/tmp/vb5-", s)
    s = re.sub(r"\x1b\[[0-9;]*[mKA-Z]", "", s)
    out = []
    for l in s.replace("\r", "\n").split("\n"):
        if re.match(r"^(\(\d\d:\d\d:\d\d\) )?(Computing main|Loading:|Analyzing:|Fetching|PROGRESS:|\[[\d,]+ / |Starting local| no actions running|INFO:\s*$|\s*$)", l):
            continue
        # java.util.logging SimpleFormatter header of the coverage output generator (wall clock).
        l = re.sub(r"([A-Z][a-z]{2} \d\d, \d{4} \d{1,2}:\d\d:\d\d [AP]M )(com\.google\.devtools\.coverageoutputgenerator\.\S+ \S+)", r"JAVALOG \2", l)
        # linux-sandbox --sandbox_debug timestamp, child PID, and argv pointer address
        l = re.sub(r"^\d+\.\d+: (src/main/tools/linux-sandbox)", r"TS: \1", l)
        l = re.sub(r'(execvp\([^,]+, )0x[0-9a-f]+(\))', r'\1ADDR\2', l)
        if l.startswith("TS: src/main/tools/linux-sandbox.cc:215: done manipulating pipes"):
            continue
        l = re.sub(r"(linux-sandbox-pid1 has PID )\d+", r"\1PID", l)
        l = re.sub(r"^\(\d\d:\d\d:\d\d\) ", "(TS) ", l)  # --show_timestamps (wall clock)
        l = re.sub(r"^-- Test timed out at \d{4}-\d\d-\d\d \d\d:\d\d:\d\d UTC --$", "-- Test timed out at TS UTC --", l)
        l = re.sub(r"localhost/(\[0:0:0:0:0:0:0:1\]|127\.0\.0\.1):", "localhost/LOOPBACK:", l)
        l = re.sub(r"bazel-out/_tmp/actions/stdout-\d+", "bazel-out/_tmp/actions/stdout-N", l)
        l = re.sub(r"\d+\.\d+ ?s\b", "Ns", l)
        l = re.sub(r"\(\d+ packages loaded[^)]*\)", "(N packages loaded)", l)
        l = re.sub(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", "UUID", l)
        l = re.sub(r"[0-9a-f]{32,64}", "HASH", l)
        l = re.sub(r"sandbox/(linux-sandbox|darwin-sandbox|processwrapper-sandbox)/\d+/", r"sandbox/\1/N/", l)
        l = re.sub(r"Critical Path: \S+", "Critical Path: T", l)
        l = re.sub(r"\d+ targets configured", "N targets configured", l)
        out.append(l)
    return "\n".join(out)
def one(label, base, pre, args, fresh):
    # base: dir with wsb wsz obb obz
    wsb, wsz, obb, obz = [os.path.join(base, x) for x in ("wsb", "wsz", "obb", "obz")]
    if fresh:
        for w, o, b in ((wsb, obb, BAZEL), (wsz, obz, ZEL)):
            subprocess.run([b, "--output_base=" + o, "--max_idle_secs=60", "clean"], cwd=w, capture_output=True, timeout=100)
    if pre:
        # per-worker flaky markers (t_flaky.sh keys them by wN/ob[bz]); never wipe other workers'
        pre = pre.replace("/tmp/vb5-flaky-*", "/tmp/vb5-flaky-%s*" % os.path.basename(base))
        for w in (wsb, wsz):
            subprocess.run(pre, shell=True, cwd=w, capture_output=True)
    res = []
    for b, w, o, t in ((BAZEL, wsb, obb, "b"), (ZEL, wsz, obz, "z")):
        # Caches/files named /tmp/vb5-* are private per tool (a shared disk cache would let the
        # second tool hit entries the first one wrote, which real runs never see).
        targs = [a.replace("/tmp/vb5-", "/tmp/vb5-%s-%s-" % (t, os.path.basename(base))) for a in args]
        if fresh:
            for a in targs:
                for m in re.findall(r"/tmp/vb5-%s-%s-[^\s=,]+" % (t, os.path.basename(base)), a):
                    subprocess.run(["rm", "-rf", m], capture_output=True)
        so, se, rc = run(b, w, o, targs)
        res.append((norm(so, w, o), norm(se, w, o), rc, so, se))
    return res
def setup(base, fx):
    for n in ("wsb", "wsz"):
        shutil.rmtree(os.path.join(base, n), ignore_errors=True)
        shutil.copytree(fx, os.path.join(base, n), symlinks=True)
        open(os.path.join(base, n, ".bazelrc"), "w").write("common --registry=file:///tmp/vendored-registry\n")
    shutil.copy(os.path.join(fx, "MODULE.bazel.lock"), os.path.join(base, "wsz", "MODULE.bazel.lock")) if False else None
    for n in ("obb", "obz"):
        os.makedirs(os.path.join(base, n), exist_ok=True)
SEQ_NAME = None
results_kind = {}
def runcase(base, i, line, seq, outdir):
    pre=None
    if " ;; " in line:
        pre, line = line.split(" ;; ", 1)
    args = shlex.split(line)
    res = one(i, base, pre, args, not seq)
    (bo, be, brc, bro, bre), (zo, ze, zrc, zro, zre) = res
    d = os.path.join(outdir, "c%d" % i); os.makedirs(d, exist_ok=True)
    open(d + "/cmd", "w").write(line + "\n")
    for nm, v in (("b.out", bro), ("b.err", bre), ("z.out", zro), ("z.err", zre)):
        open(d + "/" + nm, "w").write(v)
    open(d + "/b.norm", "w").write("RC=%d\n--stdout--\n%s\n--stderr--\n%s\n" % (brc, bo, be))
    open(d + "/z.norm", "w").write("RC=%d\n--stdout--\n%s\n--stderr--\n%s\n" % (zrc, zo, ze))
    # evidence key: plain cases are keyed by (fixture, pre, cmd); sequence steps by (file, step index)
    key = C.case_key("build", os.path.basename(FX), line, pre or "")
    if SEQ_NAME: key = C.case_key("build", os.path.basename(FX), line, "seq:%s:%d" % (SEQ_NAME, i - 1))
    open(d + "/key", "w").write(key + "\n")
    ev = C.load_evidence(HERE, key)
    st, why = C.judge((brc, bo, be), (zrc, zo, ze), ev, line)
    if st == "REAL" and brc == zrc and sorted((bo+"\n"+be).split("\n")) == sorted((zo+"\n"+ze).split("\n")):
        st = "REAL(stream/order)"
    print(st, i, line, ("" if st in ("SAME", "ORDER") else "-- " + why[:200]), flush=True)
    results_kind[st] = results_kind.get(st, 0) + 1
    return st in ("SAME", "ORDER")
def main():
    cfile, outdir = sys.argv[1], os.path.abspath(sys.argv[2])
    seq = len(sys.argv) > 3 and sys.argv[3] == "seq"
    global SEQ_NAME
    SEQ_NAME = os.path.basename(cfile) if seq else None
    nw = int(os.environ.get("NW", "2"))
    os.makedirs(outdir, exist_ok=True)
    lines = [l.rstrip("\n") for l in open(cfile) if l.strip() and not l.startswith("#")]
    if seq: nw = 1
    if os.environ.get('CASE_RANGE'):  # a:b = 0-based slice of the case lines (batches <= 50, server shutdown after each)
        a, b = map(int, os.environ['CASE_RANGE'].split(':'))
        lines = lines[a:b]
        assert not seq, 'sequences must not be sliced'
    bases = []
    for k in range(nw):
        base = os.path.join(outdir, "w%d" % k); os.makedirs(base, exist_ok=True)
        setup(base, FX); bases.append(base)
        if seq:
            for w, o, b in (("wsb", "obb", BAZEL), ("wsz", "obz", ZEL)):
                subprocess.run([b, "--output_base=" + os.path.join(base, o), "--max_idle_secs=60", "clean"], cwd=os.path.join(base, w), capture_output=True, timeout=100)
    import threading, queue
    q = queue.Queue()
    off = int(os.environ['CASE_RANGE'].split(':')[0]) if os.environ.get('CASE_RANGE') else 0
    for i, l in enumerate(lines, 1 + off): q.put((i, l))
    results = {}
    def worker(base):
        while True:
            try: i, l = q.get_nowait()
            except queue.Empty: return
            try: results[i] = runcase(base, i, l, seq, outdir)
            except Exception as e:
                print("ERR", i, l, e, flush=True); results[i] = False
    ts = [threading.Thread(target=worker, args=(b,)) for b in bases]
    [t.start() for t in ts]; [t.join() for t in ts]
    for base in bases:
        for w, o, b in (("wsb", "obb", BAZEL), ("wsz", "obz", ZEL)):
            subprocess.run([b, "--output_base=" + os.path.join(base, o), "shutdown"], cwd=os.path.join(base, w), capture_output=True, timeout=60)
    same = sum(1 for v in results.values() if v)
    print("TOTAL identical=%d (strict-same=%d order-only=%d) real=%d" % (same, results_kind.get("SAME", 0), results_kind.get("ORDER", 0), len(results) - same))
def one_seq(base, pre, args):
    return one(0, base, pre, args, False)
if __name__ == "__main__":
    main()
