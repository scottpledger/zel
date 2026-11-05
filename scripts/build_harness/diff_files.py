#!/usr/bin/env python3
"""Structural differential check of the files a build writes: `--execution_log_json_file`,
`--build_event_json_file` and `--profile` of real bazel vs zel.

usage: diff_files.py <outdir> [FX]     (cases are defined in CASES; run through scripts/heavy.sh)

For each case both tools run in their own copy of the fixture with a fresh `clean`.  The files are
reduced to structure + key fields (paths / hashes / timings / uuids normalized) and compared:
  * exec log: sorted list of (mnemonic, target_label, command_args, env keys, output paths,
    exit_code, status, runner, cache_hit) of every SpawnExec;
  * BEP: ordered list of event id kinds plus selected payload fields (exit code, success, test
    status, completed target labels/files, build finished);
  * profile: multiset of (category, name-prefix) of the trace events + presence of required
    metadata (`otherData`, process/thread names).
"""
import json, os, re, shutil, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
ZEL = os.environ.get("ZEL_BIN", "/tmp/zel-target/release/zel")
BAZEL = os.environ.get("BAZEL_BIN", "/opt/homebrew/bin/bazel")
FX = sys.argv[2] if len(sys.argv) > 2 else os.path.join(HERE, "fixtures", "ws3")

CASES = [
    "build //:ga",
    "build //:gc",
    "build //:gfail",
    "build //:hello",
    "test //:tp",
    "test //:tf",
    "test //:tshard",
    "build //:ga //:gb --keep_going",
    "build //:nope",
    "build //:gwarn",
    "build //:gout",
    "build //:gmiss",
    "build --nobuild //:ga",
    "build --keep_going //:gfail //:ga",
    "build //:nope //:ga --keep_going",
    "test //:tfl",
    "test //:tflaky",
    "test //:tsmall",
    "test //:tenv",
    "test //:suite_mixed --keep_going",
    "test //:tp //:tf --keep_going",
    "test --runs_per_test=2 //:tp",
    "test --test_sharding_strategy=forced=3 //:tshard",
    "test --nocache_test_results //:tp",
    "coverage //:tp",
    "run //:hello",
    "query //:ga",
    "cquery //:ga",
]


def norm_path(s, ws, ob):
    if not isinstance(s, str):
        return s
    s = s.replace("/private/tmp", "/tmp").replace(ws, "WS").replace(ob, "OB")
    s = re.sub(r"[0-9a-f]{32,64}", "HASH", s)
    s = re.sub(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", "UUID", s)
    return s


def load_jsonl(path):
    """A stream of JSON values (one per line, or pretty-printed objects back to back)."""
    if not os.path.exists(path):
        return None
    text = open(path, errors="replace").read()
    dec = json.JSONDecoder()
    out, i = [], 0
    while True:
        while i < len(text) and text[i].isspace():
            i += 1
        if i >= len(text):
            break
        try:
            obj, j = dec.raw_decode(text, i)
        except Exception:
            out.append({"_unparsed": text[i:i + 80]})
            break
        out.append(obj)
        i = j
    return out


def reduce_execlog(path, ws, ob):
    recs = load_jsonl(path)
    if recs is None:
        return "MISSING"
    out = []
    for r in recs:
        env = sorted(e.get("name", "") for e in r.get("environmentVariables", []) 
        outs = sorted(norm_path(o if isinstance(o, str) else o.get("path", ""), ws, ob) for o in r.get("listedOutputs", []))
        out.append((
            r.get("mnemonic", ""), r.get("targetLabel", ""),
            tuple(norm_path(a, ws, ob) for a in r.get("commandArgs", [])), tuple(env), tuple(outs),
            r.get("exitCode", 0), r.get("status", ""), r.get("runner", ""), r.get("cacheHit", False),
            len(r.get("inputs", [])),
        ))
    return sorted(out)


def reduce_bep(path, ws, ob):
    recs = load_jsonl(path)
    if recs is None:
        return "MISSING"
    out = []
    for r in recs:
        eid = r.get("id", {})
        kind = next(iter(eid), "?")
        item = {"kind": kind, "children": [next(iter(c), "?") for c in r.get("children", [])]}
        if kind in ("buildFinished",):
            f = r.get("finished", {})
            item["exit"] = (f.get("exitCode") or {}).get("name"), f.get("overallSuccess", False)
        elif kind == "targetCompleted":
            c = r.get("completed", {})
            item["label"] = eid[kind].get("label"); item["success"] = c.get("success", False)
        elif kind == "testResult":
            t = r.get("testResult", {})
            item["label"] = eid[kind].get("label"); item["status"] = t.get("status")
        elif kind == "testSummary":
            item["label"] = eid[kind].get("label"); item["status"] = r.get("testSummary", {}).get("overallStatus")
        elif kind == "pattern":
            item["pattern"] = eid[kind].get("pattern")
        elif kind == "configured":
            item["target_kind"] = r.get("configured", {}).get("targetKind")
        if kind == "progress" and not item["children"]:
            continue  # the final progress event races with the build tool logs (nondeterministic position)
        out.append(json.dumps(item, sort_keys=True))  # ordered: event order is part of the contract
    return out


def reduce_profile(path):
    if not os.path.exists(path):
        return "MISSING"
    try:
        d = json.load(open(path))
    except Exception as e:
        return "UNPARSEABLE %s" % e
    evs = d.get("traceEvents", [])
    ms = sorted(set(("M", e.get("name")) for e in evs if e.get("ph") == "M"))
    cats = {}
    for e in evs:
        if e.get("ph") == "M":
            continue
        k = (e.get("cat", ""), re.sub(r"[0-9a-f]{6,}|[0-9]+", "N", str(e.get("name", "")))[:40])
        cats[k] = cats.get(k, 0) + 1
    return {"meta": ms, "other": sorted(d.get("otherData", {}).keys()), "cats": sorted(cats)}


CORE_CATS = {"action processing", "complete action execution", "critical path component"}


def reduce_profile_core(path):
    """The contract of the profile that zel reproduces: metadata, otherData keys, phase marker
    names in order, counter lane names and the action-level categories. Bazel's JVM-internal
    categories (`bazel module processing`, `general information`, ...) are not compared."""
    if not os.path.exists(path):
        return "MISSING"
    try:
        d = json.load(open(path))
    except Exception as e:
        return "UNPARSEABLE %s" % e
    evs = d.get("traceEvents", [])
    return {
        "meta": sorted(set(str(e.get("name")) for e in evs if e.get("ph") == "M")),
        "other": sorted(d.get("otherData", {}).keys()),
        "phases": [e.get("name") for e in evs if e.get("cat") == "build phase marker"],
        # the CPU/memory/load lanes are sampled on a timer, so short builds have none in bazel:
        # only the action-count lanes are compared
        "counters": sorted(set(e.get("name") for e in evs if e.get("ph") == "C" and str(e.get("name")).startswith("action count"))),
        "cats": sorted(set(e.get("cat") for e in evs if e.get("cat") in CORE_CATS)),
    }


def run(binp, ws, ob, args, files):
    cmd = [binp, "--output_base=" + ob, "--max_idle_secs=60", args[0]] + [
        "--execution_log_json_file=" + files["el"], "--build_event_json_file=" + files["bep"], "--profile=" + files["prof"],
    ] + args[1:]
    p = subprocess.run(cmd, cwd=ws, capture_output=True, timeout=300, stdin=subprocess.DEVNULL)
    return p.returncode


def main():
    out = os.path.abspath(sys.argv[1])
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(out)
    same = {"exec_log": 0, "bep": 0, "profile": 0, "profile_core": 0}
    total = 0
    for i, case in enumerate(CASES):
        args = case.split()
        res = {}
        for tool, binp in (("b", BAZEL), ("z", ZEL)):
            ws = os.path.join(out, "ws" + tool); ob = os.path.join(out, "ob" + tool)
            if not os.path.exists(ws):
                shutil.copytree(FX, ws, symlinks=True)
            os.makedirs(ob, exist_ok=True)
            subprocess.run([binp, "--output_base=" + ob, "--max_idle_secs=60", "clean"], cwd=ws, capture_output=True, timeout=200)
            files = {k: os.path.join(out, "%s%d.%s" % (tool, i, k)) for k in ("el", "bep", "prof")}
            rc = run(binp, ws, ob, args, files)
            res[tool] = (rc, reduce_execlog(files["el"], ws, ob), reduce_bep(files["bep"], ws, ob), reduce_profile(files["prof"]), reduce_profile_core(files["prof"]))
        total += 1
        line = [case, "rc %s/%s" % (res["b"][0], res["z"][0])]
        for idx, name in ((1, "exec_log"), (2, "bep"), (3, "profile"), (4, "profile_core")):
            ok = res["b"][idx] == res["z"][idx]
            same[name] += ok
            line.append("%s=%s" % (name, "SAME" if ok else "DIFF"))
            if not ok:
                open(os.path.join(out, "c%d.%s.b" % (i, name)), "w").write(json.dumps(res["b"][idx], indent=1, default=str))
                open(os.path.join(out, "c%d.%s.z" % (i, name)), "w").write(json.dumps(res["z"][idx], indent=1, default=str))
        print(*line, flush=True)
    for tool, binp in (("b", BAZEL), ("z", ZEL)):
        subprocess.run([binp, "--output_base=" + os.path.join(out, "ob" + tool), "shutdown"], capture_output=True, timeout=100)
    print("TOTAL", total, same)


if __name__ == "__main__":
    main()
