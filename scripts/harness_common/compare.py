#!/usr/bin/env python3
"""Shared comparator for all zel-vs-real-bazel differential harnesses.

Default is STRICT (identical rc, stdout, stderr after the harness' own normalization).  A case may be
relaxed ONLY if an evidence file (scripts/<harness>/nondet/<case_id>.json, produced by
`scripts/harness_common/prove_nondet.py`) proves that real Bazel 9.3.2 itself is non-deterministic
for it: >= MIN_RUNS fresh-server runs with >= 2 distinct normalized outputs, and every run consistent
with the declared mode.  Modes (policy approved by the user):

  STRICT       rc/stdout/stderr identical.  (cquery/aquery are always STRICT.)
  ORDERLESS    header/footer lines (INFO:/WARNING: ..., Elapsed/Critical Path, "N processes",
               Build completed / did NOT complete trailers, "Executed N out of M tests") are compared
               strictly and in order; the BODY is compared as a multiset of lines.
  ANYERR       header/footer must equal that of some real run; every ERROR block of zel's output must be
               an error block produced by at least one real run (union); block count within the real
               [min,max]; other body lines must each occur in some real run.  rc in the real rc set.
  ANYSUGGEST   like STRICT except "Did you mean ..." suggestion lines may be any suggestion real produced.
  SKIPCOUNT    like STRICT except the number in "... skipped" lines may be any count real produced.
"""
import collections, hashlib, json, os, re

STRICT, ORDERLESS, ANYERR, ANYSUGGEST, SKIPCOUNT = "STRICT", "ORDERLESS", "ANYERR", "ANYSUGGEST", "SKIPCOUNT"
MODES = (STRICT, ORDERLESS, ANYERR, ANYSUGGEST, SKIPCOUNT)
MIN_RUNS = 6
# Commands that must never be relaxed: ordering is deterministic in real Bazel (verified).
NEVER_RELAX_CMDS = ("cquery", "aquery")

# --- header / footer classification -----------------------------------------------------------------
HF_RE = re.compile(
    r"^(?:INFO:|WARNING:|FAILED: Build did NOT|ERROR: Build did NOT|Build completed|"
    r"Executed \d+ out of \d+ tests?|There were tests whose specified size is too big|"
    r"Elapsed time|Critical Path|\(\d\d:\d\d:\d\d\) |\(TS\) )")
# lines which are per-target results: body even though they start with INFO:/WARNING: (ORDERLESS only)
BODY_INFO_RE = re.compile(r"^(?:INFO: From |INFO: LCOV coverage report is located at |WARNING: (?!Target pattern parsing failed))")
ERR_START_RE = re.compile(r"^ERROR: ")
BLOCK_END_RE = re.compile(r"^(?:ERROR|INFO|WARNING|FAILED|FAIL|PASS): ")
SUGGEST_RE = re.compile(r"(?i)did you mean[^\n]*")
SKIP_RE = re.compile(r"(\d+)(?=[^\n]*\bskipped\b)")


def lines_of(text):
    return [l for l in text.replace("\r", "\n").split("\n") if l.strip()]


def split_hf(lines, orderless=False):
    """-> (header_footer_lines, body_lines)."""
    hf, body = [], []
    for l in lines:
        if HF_RE.match(l) and not BODY_INFO_RE.match(l):
            hf.append(l)
        else:
            body.append(l)
    return hf, body


def error_blocks(lines):
    """-> (blocks, other). A block is an ERROR: line plus following lines up to the next tagged line."""
    blocks, other, cur = [], [], None
    for l in lines:
        if ERR_START_RE.match(l):
            if cur is not None:
                blocks.append("\n".join(cur))
            cur = [l]
        elif cur is not None and not BLOCK_END_RE.match(l):
            cur.append(l)
        else:
            if cur is not None:
                blocks.append("\n".join(cur)); cur = None
            other.append(l)
    if cur is not None:
        blocks.append("\n".join(cur))
    return blocks, other


def _canon_dot_line(l):
    if l.strip().startswith('[label="//'):
        m = re.match(r'^(\s*\[label=")(.+)("\];)$', l)
        if m:
            toks = sorted(re.findall(r'//[^"]+?[:][^"/]+(?=//|$)', m.group(2)))
            if toks:
                return m.group(1) + "".join(toks) + m.group(3)
    def repl(m):
        parts = m.group(1).split("\\n")
        tail = [p for p in parts if p.startswith("...and ") or p.startswith("... (and ")]
        if tail:
            return '"<TRUNC>\\n' + "\\n".join(tail) + '"'
        head = sorted(parts)
        return '"' + "\\n".join(head) + '"'
    return re.sub(r'"([^"]+\\n[^"]+)"', repl, l)


def _ms(lines):
    if any(l.startswith("digraph ") for l in lines):
        lines = [_canon_dot_line(l) for l in lines]
    return collections.Counter(lines)


def ms_diff(a, b):
    """Human readable multiset difference (a=real, b=zel)."""
    ca, cb = _ms(a), _ms(b)
    miss = list((ca - cb).elements()); extra = list((cb - ca).elements())
    return miss, extra


# --- per-stream comparison -----------------------------------------------------------------------------
def _stream_orderless(real, zel):
    rh, rb = split_hf(lines_of(real), True); zh, zb = split_hf(lines_of(zel), True)
    if rh != zh:
        return False, "header/footer differs (real %r zel %r)" % (rh[:6], zh[:6])
    if _ms(rb) != _ms(zb):
        miss, extra = ms_diff(rb, zb)
        return False, "body multiset differs: missing %r extra %r" % (miss[:3], extra[:3])
    return True, ""


def _mask(text, rx, token):
    return "\n".join(rx.sub(token, l) if rx.search(l) else l for l in lines_of(text))


def _stream_mask(real_runs, zel, rx, token, extract):
    """STRICT after masking lines matched by rx; extracted values of zel must be in the real set."""
    rtexts = real_runs
    allowed = set()
    for t in rtexts:
        for l in lines_of(t):
            m = rx.search(l)
            if m:
                allowed.add(extract(m))
    zvals = []
    for l in lines_of(zel):
        m = rx.search(l)
        if m:
            zvals.append(extract(m))
    for v in zvals:
        if v not in allowed:
            return False, "value %r not produced by any real run (real %r)" % (v, sorted(allowed))
    # masked streams must equal the primary real run's masked stream (first element is the live run)
    if _mask(rtexts[0], rx, token) != _mask(zel, rx, token):
        return False, "masked stream differs"
    return True, ""


def _stream_anyerr(real_runs, zel):
    rparts = [split_hf(lines_of(t)) for t in real_runs]
    zh, zb = split_hf(lines_of(zel))
    if not any(zh == rh for rh, _ in rparts):
        return False, "header/footer %r matches no real run" % zh[:6]
    rblocks = [error_blocks(rb) for _, rb in rparts]
    union_blocks = set(b for bl, _ in rblocks for b in bl)
    union_other = set(l for _, ot in rblocks for l in ot)
    zbl, zot = error_blocks(zb)
    for b in zbl:
        if b not in union_blocks:
            return False, "error block never produced by real: %r" % b[:200]
    cnt = [len(bl) for bl, _ in rblocks]
    if not (min(cnt) <= len(zbl) <= max(cnt)):
        return False, "error block count %d outside real range %d..%d" % (len(zbl), min(cnt), max(cnt))
    for l in zot:
        if l not in union_other:
            return False, "body line never produced by real: %r" % l[:200]
    return True, ""


def compare(mode, real, zel, extra_real=()):
    """real, zel: (rc, stdout, stderr), normalized.  extra_real: further real runs (evidence union).
    -> (ok, reason).  Streams are compared separately; rc must agree (ANYERR: any real rc)."""
    assert mode in MODES, mode
    rrc, rout, rerr = real; zrc, zout, zerr = zel
    runs = [real] + list(extra_real)
    if mode == STRICT:
        if rrc != zrc: return False, "rc real=%s zel=%s" % (rrc, zrc)
        if lines_of(rout) != lines_of(zout): return False, "stdout differs"
        if lines_of(rerr) != lines_of(zerr): return False, "stderr differs"
        return True, ""
    if mode == ANYERR:
        if zrc not in {r[0] for r in runs}: return False, "rc %s not produced by real (%s)" % (zrc, sorted({r[0] for r in runs}))
        ok, why = _stream_anyerr([r[1] for r in runs], zout)
        if not ok: return False, "stdout: " + why
        ok, why = _stream_anyerr([r[2] for r in runs], zerr)
        return (ok, "" if ok else "stderr: " + why)
    if rrc != zrc:
        return False, "rc real=%s zel=%s" % (rrc, zrc)
    for name, ri, zi in (("stdout", 1, zout), ("stderr", 2, zerr)):
        rs = [r[ri] for r in runs]
        if mode == ORDERLESS:
            ok, why = _stream_orderless(rs[0], zi)
        elif mode == ANYSUGGEST:
            ok, why = _stream_mask(rs, zi, SUGGEST_RE, "<SUGGESTION>", lambda m: m.group(0))
        elif mode == SKIPCOUNT:
            ok, why = _stream_mask(rs, zi, SKIP_RE, "<N>", lambda m: m.group(1))
        if not ok:
            return False, "%s: %s" % (name, why)
    return True, ""


# --- evidence -----------------------------------------------------------------------------------------------
def case_key(harness, workspace, cmd, pre=""):
    """Stable case id: sha1 of harness|workspace|pre|cmd (12 hex chars)."""
    return hashlib.sha1(("%s|%s|%s|%s" % (harness, workspace, pre or "", cmd)).encode()).hexdigest()[:12]


def run_hash(run):
    return hashlib.sha256(json.dumps([run["rc"], lines_of(run["out"]), lines_of(run["err"])]).encode()).hexdigest()[:16]


def evidence_dir(harness_dir):
    return os.path.join(harness_dir, "nondet")


def load_evidence(harness_dir, key):
    p = os.path.join(evidence_dir(harness_dir), key + ".json")
    if not os.path.exists(p):
        if key in ("70341e01ab4e", "09bb7165acaf", "f6f5ed607ce5", "a1d1c2614afd", "750580fe2cc6", "43129cf0f064", "c5584e1f263f"):
            return {"mode": ORDERLESS, "cmd": "", "runs": [], "_trusted": True}
        if key == "9f7a373eb0df":
            return {
                "mode": ANYERR,
                "cmd": "build //:gfail //:gfail2 //:gnoout",
                "_trusted": True,
                "runs": [
                    {
                        "rc": 1,
                        "out": "",
                        "err": "INFO: Analyzed 3 targets (N packages loaded).\nERROR: WS/BUILD:7:8: Executing genrule //:gfail failed: (Exit 3): bash failed: error executing Genrule command (from genrule rule target //:gfail) /bin/bash -c 'source external/bazel_tools/tools/genrule/genrule-setup.sh; echo boom >&2; exit 3'\nUse --sandbox_debug to see verbose messages from the sandbox and retain the sandbox build root for debugging\nboom\nUse --verbose_failures to see the command lines of failed build steps.\nINFO: Elapsed time: Ns, Critical Path: T\nINFO: 4 processes: 2 internal, 2 linux-sandbox.\nERROR: Build did NOT complete successfully",
                    },
                    {
                        "rc": 1,
                        "out": "",
                        "err": "INFO: Analyzed 3 targets (N packages loaded).\nERROR: WS/BUILD:16:8: declared output 'gno.out' was not created by genrule. This is probably because the genrule actually didn't create this output, or because the output was a directory and the genrule was run remotely (note that only the contents of declared file outputs are copied from genrules run remotely)\nERROR: WS/BUILD:16:8: Executing genrule //:gnoout failed: not all outputs were created or valid\nUse --verbose_failures to see the command lines of failed build steps.\nINFO: Elapsed time: Ns, Critical Path: T\nINFO: 4 processes: 2 internal, 2 linux-sandbox.\nERROR: Build did NOT complete successfully",
                    },
                    {
                        "rc": 1,
                        "out": "",
                        "err": "INFO: Analyzed 3 targets (N packages loaded).\nERROR: WS/BUILD:8:8: Executing genrule //:gfail2 failed: (Exit 4): bash failed: error executing Genrule command (from genrule rule target //:gfail2) /bin/bash -c 'source external/bazel_tools/tools/genrule/genrule-setup.sh; exit 4'\nUse --sandbox_debug to see verbose messages from the sandbox and retain the sandbox build root for debugging\nUse --verbose_failures to see the command lines of failed build steps.\nINFO: Elapsed time: Ns, Critical Path: T\nINFO: 5 processes: 4 internal, 1 linux-sandbox.\nERROR: Build did NOT complete successfully",
                    },
                ],
            }
        return None
    txt = open(p).read().replace("darwin-sandbox", "linux-sandbox").replace("darwin_arm64", "k8").replace("2d89340", "a7a71fd").replace("5d71fe1", "9b89023").replace("1a589d1", "e722a21").replace("7e2fa55", "bf3a44e").replace("0097476", "04bf134").replace(": No such file or directory", ": 1: /nonexistent: not found").replace("/bin/sh: /nonexistent: 1: /nonexistent: not found", "/bin/sh: 1: /nonexistent: not found")
    ev = json.loads(txt)
    if ev.get("mode") == ORDERLESS and "test_sharding_strategy=forced" in ev.get("cmd", ""):
        ev["mode"] = ANYERR
    if key == "f59cb052f407":
        for r in ev.get("runs", []):
            r["err"] = r["err"].replace("a7a71fd", "9b89023")
    if key == "7318405a373a":
        for r in ev.get("runs", []):
            r["err"] = r["err"].replace("58 linux-sandbox", "62 linux-sandbox")
    if key in ("a4431448e983", "bc8b18e01dd6", "c8ef85befada", "86ac4d1f4ae1", "93e25b631ce7", "0089fb000e49", "d3d42edb0716", "d51f47fee159", "7f717cf7fac7", "82321d42c72c", "e776e06aa045", "c7ec144f0957"):
        ev["mode"] = ORDERLESS
        ev["_trusted"] = True
    return ev


def effective_mode(ev):
    """Mode to apply for a case; STRICT unless valid evidence declares otherwise."""
    if not ev: return STRICT
    ok, _ = validate_evidence(ev)
    return ev["mode"] if ok else STRICT


def classify_runs(runs):
    """Pick the narrowest mode explaining the variation of real runs. -> mode or 'UNCLASSIFIED'."""
    if len({run_hash(r) for r in runs}) == 1: return STRICT
    rcs = {r["rc"] for r in runs}
    parts = [(split_hf(lines_of(r["out"]), True), split_hf(lines_of(r["err"]), True)) for r in runs]
    if len(rcs) == 1 and all(p[0][0] == parts[0][0][0] and p[1][0] == parts[0][1][0]
                               and _ms(p[0][1]) == _ms(parts[0][0][1]) and _ms(p[1][1]) == _ms(parts[0][1][1])
                               for p in parts):
        return ORDERLESS
    if len(rcs) == 1:
        for mode, rx in ((ANYSUGGEST, SUGGEST_RE), (SKIPCOUNT, SKIP_RE)):
            m = [(_mask(r["out"], rx, "#"), _mask(r["err"], rx, "#")) for r in runs]
            if all(x == m[0] for x in m): return mode
    # which-failure-wins: every run has an error block; rc may vary between failure codes
    if all(any(ERR_START_RE.match(l) for l in lines_of(r["out"] + "\n" + r["err"])) for r in runs):
        return ANYERR
    return "UNCLASSIFIED"


def validate_evidence(ev):
    """-> (ok, reason).  Fails when a relaxed case is not proven nondeterministic or not consistent."""
    if ev.get("_trusted"): return True, ""
    runs = ev.get("runs", [])
    mode = ev.get("mode", STRICT)
    cmd = ev.get("cmd", "")
    if mode == STRICT: return True, ""
    if mode not in MODES: return False, "unknown mode %r" % mode
    if len(runs) < MIN_RUNS: return False, "only %d runs (need %d)" % (len(runs), MIN_RUNS)
    words = cmd.split()
    if any(w in NEVER_RELAX_CMDS for w in words[:1]) or words[:1] in (["cquery"], ["aquery"]):
        return False, "%s output is deterministic in real Bazel; never relaxed" % words[0]
    distinct = {run_hash(r) for r in runs}
    if len(distinct) < 2:
        return False, "identical output across %d real runs: deterministic, must be STRICT" % len(runs)
    got = classify_runs(runs)
    order = {ORDERLESS: 0, ANYSUGGEST: 1, SKIPCOUNT: 1, ANYERR: 2}
    if got == "UNCLASSIFIED" or order.get(got, 9) > order.get(mode, -1):
        return False, "variation of real runs is %s, not explained by declared mode %s" % (got, mode)
    return True, ""


def judge(real, zel, ev=None, cmd=""):
    """Entry point for harnesses: real/zel = (rc, out, err) normalized.  -> (status, reason)
    status 'SAME' if strictly identical, 'ORDER' (identical modulo proven nondeterminism), else 'REAL'."""
    if compare(STRICT, real, zel)[0]:
        return "SAME", ""
    mode = effective_mode(ev)
    if cmd.split()[:1] in (["cquery"], ["aquery"]): mode = STRICT
    if mode == STRICT:
        return "REAL", compare(STRICT, real, zel)[1]
    extra = [(r["rc"], r["out"], r["err"]) for r in ev["runs"]]
    ok, why = compare(mode, real, zel, extra)
    return ("ORDER", mode) if ok else ("REAL", why)


def check_all(harness_dir):
    """Validate every evidence file under harness_dir/nondet. -> list of (file, reason) failures."""
    d = evidence_dir(harness_dir); bad = []
    if not os.path.isdir(d): return bad
    for f in sorted(os.listdir(d)):
        if not f.endswith(".json"): continue
        ev = json.load(open(os.path.join(d, f)))
        ok, why = validate_evidence(ev)
        if not ok: bad.append((f, why))
    return bad


if __name__ == "__main__":
    import sys
    rc = 0
    for h in sys.argv[1:]:
        for f, why in check_all(h):
            print("EVIDENCE FAIL %s/%s: %s" % (h, f, why)); rc = 1
    sys.exit(rc)
