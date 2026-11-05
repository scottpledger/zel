#!/usr/bin/env python3
"""Prove (or refute) non-determinism of REAL Bazel 9.3.2 output for harness cases.

  prove_nondet.py --harness build --cmd 'query --output=graph //...' [--ws ws1] [--pre 'sh'] [--history 'build //:x']...
  prove_nondet.py --harness build --cases scripts/build_harness/cases1b.txt --only REGEX
  prove_nondet.py --harness build --seqfile scripts/build_harness/seq1.txt        # history-dependent: whole sequence x N
  prove_nondet.py --harness query --cases scripts/query_harness/query_cmds.tsv --only REGEX
  prove_nondet.py --check                                                         # validate all evidence files

Each run uses a FRESH workspace copy, a FRESH output base (fresh server) and is shut down afterwards.  The
distinct normalized outputs are recorded in scripts/<harness>/nondet/<case_id>.json together with a
classification (STRICT = deterministic, ORDERLESS / ANYERR / ANYSUGGEST / SKIPCOUNT / UNCLASSIFIED).
Only the (rc, stdout, stderr) triple after the harness' own normalizer is hashed.
"""
import argparse, importlib.util, json, os, re, shlex, shutil, subprocess, sys, tempfile
HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import compare as C

BAZEL = os.environ.get("BAZEL_BIN", "/tmp/bazel-9.3.2")


def _load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
    return m


def run_bazel(args, ws, ob, timeout=170):
    env = {k: v for k, v in os.environ.items() if k != "USE_BAZEL_VERSION"}
    try:
        p = subprocess.run([BAZEL, "--output_base=" + ob, "--max_idle_secs=60"] + args, cwd=ws, capture_output=True,
                           timeout=timeout, stdin=subprocess.DEVNULL, env=env)
        return p.returncode, p.stdout.decode(errors="replace"), p.stderr.decode(errors="replace")
    except subprocess.TimeoutExpired:
        return 999, "", "TIMEOUT"


def shutdown(ws, ob):
    subprocess.run([BAZEL, "--output_base=" + ob, "shutdown"], cwd=ws, capture_output=True, timeout=90,
                   stdin=subprocess.DEVNULL)


class Profile:
    """Per-harness workspace layout and normalizer."""

    def __init__(self, harness):
        self.harness = harness
        self.dir = os.path.join(SCRIPTS, harness + "_harness")
        if harness == "build":
            self.h = _load(os.path.join(self.dir, "h.py"), "build_h")
            self.fx = os.environ.get("FX", os.path.join(self.dir, "fixtures", "ws1"))
            self.wsname = os.path.basename(self.fx)
        else:
            self.wsname = "ws"

    def setup(self, root, ws):
        d = os.path.join(root, "ws")
        if self.harness == "build":
            shutil.copytree(self.fx, d, symlinks=True)
        else:
            os.makedirs(root, exist_ok=True)
            shutil.copytree(os.path.join(self.dir, "ext"), os.path.join(root, "ext"), symlinks=True)
            d = os.path.join(root, ws)
            shutil.copytree(os.path.join(self.dir, ws), d, symlinks=True)
        return d

    def norm(self, s, ws, ob):
        if self.harness == "build":
            return self.h.norm(s, ws, ob)
        s = s.replace("/private/tmp/", "/tmp/").replace(os.path.realpath(ws), "@WS@").replace(ws, "@WS@")
        s = s.replace(os.path.realpath(ob), "@OB@").replace(ob, "@OB@")
        s = re.sub(r"\x1b\[[0-9;]*[A-Za-z]", "", s).replace("\r", "")
        s = re.sub(r"\b[0-9a-f]{64}\b", "H64", s)
        return "\n".join(l for l in s.split("\n") if not re.match(
            r"^(Computing main repo|Starting local Bazel|Loading:|Analyzing:|Fetching|\[|\(|\s+no actions)", l)
            and not re.search(r"\d+ packages loaded", l))


def one_run(prof, n, ws_name, steps, tag):
    """Run steps (list of (pre, args)) on one fresh server; -> list of normalized (rc,out,err) per step."""
    root = tempfile.mkdtemp(prefix="pnd-%d-" % os.getpid(), dir="/tmp")
    ob = os.path.join(root, "ob"); os.makedirs(ob)
    try:
        ws = prof.setup(root, ws_name)
        outs = []
        for pre, args in steps:
            if pre:
                pre = pre.replace("/tmp/vb5-flaky-*", "/tmp/vb5-flaky-pnd%d*" % os.getpid())
                subprocess.run(pre, shell=True, cwd=ws, capture_output=True)
            targs = [a.replace("/tmp/vb5-", "/tmp/vb5-pnd%d-%d-" % (os.getpid(), n)) for a in args]
            rc, so, se = run_bazel(targs, ws, ob)
            outs.append((rc, prof.norm(so, ws, ob), prof.norm(se, ws, ob)))
        return outs
    finally:
        try: shutdown(ws, ob)
        except Exception: pass
        shutil.rmtree(root, ignore_errors=True)
        subprocess.run("rm -rf /tmp/vb5-pnd%d-%d-*" % (os.getpid(), n), shell=True)


def write_evidence(prof, key, cmd, pre, ws_name, runs, extra=None):
    mode = C.classify_runs(runs)
    ev = {"harness": prof.harness, "case_id": key, "workspace": ws_name, "cmd": cmd, "pre": pre or "",
          "bazel": BAZEL, "n_runs": len(runs), "distinct": len({C.run_hash(r) for r in runs}),
          "hashes": [C.run_hash(r) for r in runs], "classified_as": mode,
          "mode": mode if mode in C.MODES else C.STRICT, "runs": runs}
    if extra: ev.update(extra)
    d = C.evidence_dir(prof.dir); os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, key + ".json"), "w") as f:
        json.dump(ev, f, indent=1, sort_keys=True)
    ok, why = C.validate_evidence(ev)
    return ev, ok, why


def prove_cmd(prof, cmd, pre, ws_name, history, nruns):
    args = shlex.split(cmd)
    if history:
        steps = [(pre, shlex.split(history[0]))] + [(None, shlex.split(h)) for h in history[1:]] + [(None, args)]
    else:
        steps = [(pre, args)]
    runs = []
    for i in range(nruns):
        o = one_run(prof, i, ws_name, steps, "")[-1]
        runs.append({"rc": o[0], "out": o[1], "err": o[2]})
    key = C.case_key(prof.harness, ws_name, cmd, (pre or "") + ("|hist:" + ";".join(history) if history else ""))
    ev, ok, why = write_evidence(prof, key, cmd, pre, ws_name, runs, {"history": history})
    print("%-12s %-12s distinct=%d/%d %s  %s%s" % (key, ev["classified_as"], ev["distinct"], len(runs), "OK" if ok else "INVALID(" + why + ")", cmd, " [after %d history cmds]" % len(history) if history else ""), flush=True)
    return ev


def prove_seq(prof, steps_lines, nruns, name):
    steps = []
    for l in steps_lines:
        pre = None
        if " ;; " in l: pre, l = l.split(" ;; ", 1)
        steps.append((pre, shlex.split(l)))
    allruns = [one_run(prof, i, prof.wsname, steps, "") for i in range(nruns)]
    for j, (pre, args) in enumerate(steps):
        runs = [{"rc": r[j][0], "out": r[j][1], "err": r[j][2]} for r in allruns]
        cmd = " ".join(shlex.quote(a) for a in args)
        key = C.case_key(prof.harness, prof.wsname, cmd, "seq:%s:%d" % (name, j))
        if len({C.run_hash(r) for r in runs}) == 1:
            continue  # deterministic steps need no evidence file (strict is the default)
        ev, ok, why = write_evidence(prof, key, cmd, pre, prof.wsname, runs, {"seq": name, "seq_index": j + 1, "history": [" ".join(a) for _, a in steps[:j]]})
        print("%-12s %-12s distinct=%d/%d %s seq step %d: %s" % (key, ev["classified_as"], ev["distinct"], nruns, "OK" if ok else "INVALID(" + why + ")", j + 1, cmd), flush=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--harness", choices=("build", "query", "cq"), default="build")
    ap.add_argument("--cmd"); ap.add_argument("--pre"); ap.add_argument("--ws", default=None)
    ap.add_argument("--history", action="append", default=[])
    ap.add_argument("--cases"); ap.add_argument("--only"); ap.add_argument("--ids", help="file with one case id per line (tsv ID, or 1-based line number for build case files) to restrict --cases"); ap.add_argument("--seqfile")
    ap.add_argument("--runs", type=int, default=6)
    ap.add_argument("--check", action="store_true", help="validate all evidence files; exit 1 on failure")
    a = ap.parse_args()
    if a.check:
        bad = 0
        for h in ("build", "query", "cq", "bzlmod"):
            for f, why in C.check_all(os.path.join(SCRIPTS, h + "_harness")):
                print("EVIDENCE FAIL %s_harness/nondet/%s: %s" % (h, f, why)); bad = 1
        sys.exit(bad)
    if a.runs < C.MIN_RUNS:
        sys.exit("--runs must be >= %d to constitute proof" % C.MIN_RUNS)
    prof = Profile("query" if a.harness == "query" else a.harness)
    if a.cmd:
        prove_cmd(prof, a.cmd, a.pre, a.ws or prof.wsname, a.history, a.runs)
    if a.seqfile:
        lines = [l.rstrip("\n") for l in open(a.seqfile) if l.strip() and not l.startswith("#")]
        prove_seq(prof, lines, a.runs, os.path.basename(a.seqfile))
    if a.cases:
        ids = set(x.strip() for x in open(a.ids)) if a.ids else None
        n = 0
        for l in open(a.cases):
            l = l.rstrip("\n")
            if not l.strip() or l.startswith("#"): continue
            n += 1
            if ids is not None and a.harness == "build" and str(n) not in ids: continue
            if ids is not None and a.harness != "build" and l.split("\t", 1)[0] not in ids: continue
            pre = None; ws = a.ws or prof.wsname
            if a.harness == "build":
                if " ;; " in l: pre, l = l.split(" ;; ", 1)
                cmd = l
            else:
                _, cmd = l.split("\t", 1)
                m = re.match(r"^(ws|wsbroken):(.*)$", cmd)
                if m: ws, cmd = m.groups()
            if a.only and not re.search(a.only, cmd): continue
            prove_cmd(prof, cmd, pre, ws, [], a.runs)


if __name__ == "__main__":
    main()
