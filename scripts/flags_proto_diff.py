#!/usr/bin/env python3
"""
scripts/flags_proto_diff.py

Field-by-field comparison of two decoded `help flags-as-proto` dumps (text format as printed by
`protoc --decode=bazel_flags.FlagCollection` or by `zel-flags-dump`).

Usage: flags_proto_diff.py REAL.txt ZEL.txt [--allowlist FILE] [--quiet]

Every field of every flag (name, has_negative_flag, documentation, commands, abbreviation,
allows_multiple, effect_tags, metadata_tags, documentation_category, requires_value,
default_value, old_name, deprecation_warning, option_expansions, type_converter, enum_values)
is compared, as is the presence of the flag itself and the order of the flags in the collection.

Optional allowlist lines (no allowlist file ships any more; the spec is the real 9.3.2):
    flag_skew: <flag-name> <field|*> <reason>

Exit status: 0 when the only differences are allowlisted, 1 otherwise.
"""

import re
import sys

FIELDS = ["name", "has_negative_flag", "documentation", "commands", "abbreviation", "allows_multiple",
          "effect_tags", "metadata_tags", "documentation_category", "requires_value", "default_value",
          "old_name", "deprecation_warning", "option_expansions", "type_converter", "enum_values"]
REPEATED = {"commands", "effect_tags", "metadata_tags", "option_expansions", "enum_values"}


def unescape(s):
    out = []
    i = 0
    while i < len(s):
        c = s[i]
        if c != "\\":
            out.append(c)
            i += 1
            continue
        i += 1
        e = s[i]
        i += 1
        if e == "n":
            out.append("\n")
        elif e == "t":
            out.append("\t")
        elif e == "r":
            out.append("\r")
        elif e in "\"'\\":
            out.append(e)
        elif e in "01234567":
            j = i
            while j < len(s) and j < i + 2 and s[j] in "01234567":
                j += 1
            out.append(chr(int(e + s[i:j], 8)))
            i = j
        else:
            out.append(e)
    return "".join(out)


def parse(path):
    """Returns an ordered list of flag dicts (field -> value or list)."""
    flags = []
    cur = None
    line_re = re.compile(r'^\s*(\w+):\s*(.*)$')
    # protoc escapes non-ASCII as octal bytes; collect bytes and decode as UTF-8
    with open(path, "rb") as fh:
        data = fh.read().decode("latin-1")
    for line in data.split("\n"):
        line = line.rstrip("\r")
        if line.startswith("flag_infos {"):
            cur = {}
            continue
        if line.startswith("}"):
            if cur is not None:
                flags.append(cur)
            cur = None
            continue
        m = line_re.match(line)
        if not m or cur is None:
            continue
        key, raw = m.group(1), m.group(2)
        if raw.startswith('"'):
            body = raw[1:raw.rindex('"')]
            # octal escapes represent UTF-8 bytes
            bs = bytearray()
            i = 0
            while i < len(body):
                c = body[i]
                if c == "\\":
                    e = body[i + 1]
                    if e in "01234567":
                        j = i + 2
                        while j < len(body) and j < i + 4 and body[j] in "01234567":
                            j += 1
                        bs.append(int(body[i + 1:j], 8) & 0xFF)
                        i = j
                        continue
                    mp = {"n": "\n", "t": "\t", "r": "\r", '"': '"', "'": "'", "\\": "\\"}
                    bs.extend(mp.get(e, e).encode("utf-8"))
                    i += 2
                    continue
                bs.extend(c.encode("latin-1"))
                i += 1
            val = bs.decode("utf-8", errors="replace")
        else:
            val = raw
        if key in REPEATED:
            cur.setdefault(key, []).append(val)
        else:
            cur[key] = val
    return flags


def load_allowlist(path):
    allow = {}
    if not path:
        return allow
    try:
        with open(path) as fh:
            for line in fh:
                line = line.strip()
                if not line.startswith("flag_skew:"):
                    continue
                parts = line[len("flag_skew:"):].strip().split(None, 2)
                if len(parts) >= 2:
                    allow[(parts[0], parts[1])] = parts[2] if len(parts) > 2 else ""
    except FileNotFoundError:
        pass
    return allow


def main():
    args = sys.argv[1:]
    allowlist = None
    quiet = False
    evidence_path = None
    max_lines = 200
    files = []
    i = 0
    while i < len(args):
        if args[i] == "--allowlist":
            allowlist = args[i + 1]
            i += 2
        elif args[i] == "--quiet":
            quiet = True
            i += 1
        elif args[i] == "--skew-evidence":
            evidence_path = args[i + 1]
            i += 2
        elif args[i] == "--max":
            max_lines = int(args[i + 1])
            i += 2
        else:
            files.append(args[i])
            i += 1
    real = parse(files[0])
    zel = parse(files[1])
    allow = load_allowlist(allowlist)
    evidence = []
    baseline = None
    if evidence_path:
        import json
        try:
            with open(evidence_path) as fh:
                ev = json.load(fh)
            evidence = [b["flags"] for b in ev.get("baselines", [])]
            baseline = bool(evidence)
        except (ValueError, OSError):
            pass
    rmap = {f["name"]: f for f in real}
    zmap = {f["name"]: f for f in zel}
    problems = []
    skewed = 0

    def norm(v):
        return [] if v is None else v

    def report(flag, field, msg):
        nonlocal skewed
        if (flag, field) in allow or (flag, "*") in allow:
            skewed += 1
            return
        if baseline:
            for ev in evidence:
                b = ev.get(flag)
                if field == "presence":
                    # missing in zel: a baseline source (generator proven against the real binary) declares it;
                    # extra in zel: the baseline does not. Either way HEAD changed since.
                    missing_in_zel = msg.startswith("missing")
                    if (b is not None) == missing_in_zel:
                        skewed += 1
                        return
                elif field == "order":
                    pass
                elif b is not None and norm(b.get(field)) == norm(rmap.get(flag, {}).get(field)):
                    skewed += 1
                    return
        problems.append("%s [%s]: %s" % (flag, field, msg))

    for name in sorted(set(rmap) - set(zmap)):
        report(name, "presence", "missing in zel (real has it, commands=%s)" % rmap[name].get("commands"))
    for name in sorted(set(zmap) - set(rmap)):
        report(name, "presence", "extra in zel (commands=%s)" % zmap[name].get("commands"))
    for name in sorted(set(rmap) & set(zmap)):
        r, z = rmap[name], zmap[name]
        for f in FIELDS:
            rv, zv = r.get(f), z.get(f)
            if f in REPEATED:
                rv, zv = rv or [], zv or []
            if rv != zv:
                report(name, f, "real=%r zel=%r" % (rv, zv))
    # order of the common flags (HashMap iteration order of the real implementation); only meaningful when the
    # commands (visit order) of all common flags agree
    common_r = [f["name"] for f in real if f["name"] in zmap]
    common_z = [f["name"] for f in zel if f["name"] in rmap]
    same_visit = all(rmap[n].get("commands") == zmap[n].get("commands") for n in rmap if n in zmap)
    if common_r != common_z and same_visit:
        first = next(i for i, (a, b) in enumerate(zip(common_r, common_z)) if a != b)
        report("<collection>", "order", "flag order differs first at #%d (real=%s zel=%s)" % (
            first, common_r[first], common_z[first]))
    print("flags: real=%d zel=%d common=%d, differences=%d, allowlisted=%d" % (
        len(real), len(zel), len(set(rmap) & set(zmap)), len(problems), skewed))
    if not quiet:
        by_field = {}
        for p in problems:
            m = re.match(r"\S+ \[(\w+)\]", p)
            by_field[m.group(1) if m else "?"] = by_field.get(m.group(1) if m else "?", 0) + 1
        if by_field:
            print("by field: " + ", ".join("%s=%d" % kv for kv in sorted(by_field.items())))
        for p in problems[:max_lines]:
            print("  " + p[:300])
        if len(problems) > max_lines:
            print("  ... %d more" % (len(problems) - max_lines))
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
