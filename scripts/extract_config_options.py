#!/usr/bin/env python3
"""
scripts/extract_config_options.py

Generates crates/zel-analysis/src/build_options_table.rs: the per-FragmentOptions option
schema (class, option name, Java field type, converter, default value, allowMultiple) of the
option classes that make up a `BuildOptions` in Bazel's Bazel-flavoured rule set.  Those are
exactly the inputs of `BuildOptions.checksum()` (see zel-analysis `build_options.rs`).

Source of truth: the Bazel 9.3.2 Java sources, read straight from the checkout (the spec is the
9.3.2 tree, which equals the reference binary; no per-option overrides, no git).
  --bazel-src  ~/Development/bazel

Usage: scripts/extract_config_options.py [--check]   (--check: fail if the table is stale)
"""
import argparse, os, re, subprocess, sys

LIB = "src/main/java/com/google/devtools/build/lib/"
# (source file, FQ class prefix) — fragment option classes registered by the Bazel rule set.
CLASS_FILES = [
    "analysis/PlatformOptions",
    "analysis/ShellConfiguration",
    "analysis/config/CoreOptions",
    "analysis/test/CoverageConfiguration",
    "analysis/test/TestConfiguration",
    "bazel/rules/BazelRuleClassProvider",
    "bazel/rules/python/BazelPythonConfiguration",
    "rules/android/AndroidConfiguration",
    "rules/android/BazelAndroidConfiguration",
    "rules/apple/AppleCommandLineOptions",
    "rules/config/ConfigFeatureFlagOptions",
    "rules/cpp/CppOptions",
    "rules/java/JavaOptions",
    "rules/objc/J2ObjcCommandLineOptions",
    "rules/objc/ObjcCommandLineOptions",
    "rules/proto/ProtoConfiguration",
    "rules/python/PythonOptions",
]


# Configuration fragment classes (`extends Fragment`) whose `@RequiresOptions` define which
# options classes `bazel config` lists per fragment ("Fragments:" line).
FRAGMENT_FILES = [
    "analysis/PlatformConfiguration", "analysis/ShellConfiguration",
    "analysis/test/CoverageConfiguration", "analysis/test/TestConfiguration",
    "bazel/rules/BazelRuleClassProvider", "bazel/rules/python/BazelPythonConfiguration",
    "rules/android/AndroidConfiguration", "rules/android/BazelAndroidConfiguration",
    "rules/apple/AppleConfiguration", "rules/config/ConfigFeatureFlagConfiguration",
    "rules/cpp/CppConfiguration", "rules/java/JavaConfiguration",
    "rules/objc/J2ObjcConfiguration", "rules/objc/ObjcConfiguration",
    "rules/proto/ProtoConfiguration", "rules/python/PythonConfiguration",
]


def parse_fragments(src, rev, option_classes):
    out = []
    for rel in FRAGMENT_FILES:
        text = git_show(src, rev, LIB + rel + ".java")
        pkg = re.search(r"^package ([\w.]+);", text, re.M).group(1)
        outer = os.path.basename(rel)
        for m in re.finditer(r"@RequiresOptions\(\s*options\s*=\s*(\{[^}]*\}|[\w.]+\.class)", text):
            cm = re.search(r"class\s+(\w+)\s+extends\s+Fragment\b", text[m.end():])
            cname = cm.group(1)
            fq = pkg + "." + (outer if cname == outer else outer + "$" + cname)
            toks = re.findall(r"([\w.]+)\.class", m.group(1))
            opts = []
            for t in toks:
                parts = t.split(".")
                simple = parts[-1]
                cands = [c for c in option_classes if re.split(r"[.$]", c)[-1] == simple
                         and (len(parts) == 1 or re.split(r"[.$]", c)[-2] == parts[-2])]
                if len(cands) != 1:
                    raise ValueError(f"cannot resolve {t} in {rel}: {cands}")
                opts.append(cands[0])
            out.append((fq, sorted(opts)))
    return sorted(out)


def git_show(src, rev, path):
    """Reads `path` from the checkout at `src` (the 9.3.2 tree); `rev` is informational only."""
    with open(os.path.join(src, path)) as f:
        return f.read()


def matching_paren(s, i):
    """Index of the ')' matching the '(' at s[i], string/comment aware."""
    depth = 0
    n = len(s)
    while i < n:
        c = s[i]
        if s.startswith('"""', i):
            i = s.index('"""', i + 3) + 3
            continue
        if c == '"':
            i += 1
            while s[i] != '"':
                i += 2 if s[i] == "\\" else 1
        elif s.startswith("//", i):
            i = s.index("\n", i)
            continue
        elif c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    raise ValueError("unbalanced")


def unescape_java(lit):
    return (lit.replace('\\"', '"').replace("\\\\", "\\").replace("\\n", "\n").replace("\\t", "\t"))


_EXT_CACHE = {}
GLOBAL_SRC = {"src": None, "rev": None}


def external_const(ident):
    """Resolve `Outer.NAME` / `NAME` String constants declared in other Bazel source files."""
    short = ident.split(".")[-1]
    if short in _EXT_CACHE:
        return _EXT_CACHE[short]
    res = ""
    for rev in (GLOBAL_SRC["rev"], "master"):
        res = subprocess.run(["git", "-C", GLOBAL_SRC["src"], "grep", "-h", "-A3", "-w", "-e",
                              "static final String " + short, rev, "--", "src/main/java"],
                             capture_output=True, text=True).stdout
        if res:
            break
    m = re.search(r'static final String ' + short + r'\s*=\s*((?:"(?:[^"\\]|\\.)*"\s*\+?\s*)+);', res.replace("\n", " "))
    v = java_string_expr(m.group(1), {}) if m else None
    _EXT_CACHE[short] = v
    return v


def java_string_expr(expr, consts):
    """Evaluate `"a" + "b" + CONST` into a python string (None if unsupported)."""
    parts = re.findall(r'"((?:[^"\\]|\\.)*)"|([A-Za-z_][\w.]*)', expr)
    out = ""
    for lit, ident in parts:
        if ident:
            if ident in consts:
                out += consts[ident]
            else:
                v = external_const(ident)
                if v is None:
                    return None
                out += v
        else:
            out += unescape_java(lit)
    return out


def parse_consts(src):
    consts = {}
    for m in re.finditer(r'static final String (\w+)\s*=\s*((?:"(?:[^"\\]|\\.)*"\s*\+?\s*)+);', src):
        v = java_string_expr(m.group(2), consts)
        if v is not None:
            consts[m.group(1)] = v
    return consts


def parse_file(rel, text):
    consts = parse_consts(text)
    # Classes that declare options (extends FragmentOptions / *Options).
    classes = [(m.start(), m.group(1)) for m in
               re.finditer(r"\bclass\s+(\w+)\s+extends\s+(?:FragmentOptions|\w*Options)\b", text)]
    pkg = re.search(r"^package ([\w.]+);", text, re.M).group(1)
    outer = os.path.basename(rel)
    opts = []
    for m in re.finditer(r"@Option\(", text):
        start = m.end() - 1
        end = matching_paren(text, start)
        body = text[start + 1:end]
        decl = re.match(r"\s*(?://[^\n]*\n\s*)*(?:@\w+(?:\([^)]*\))?\s*)*public\s+(?:abstract\s+)?([\w<>?,.\[\] ]+?)\s+(\w+)\s*(?:\(\))?\s*(?:=|;)", text[end + 1:])
        if not decl:
            raise ValueError(f"cannot parse field after option in {rel}: {text[end+1:end+120]!r}")
        ftype, field = decl.group(1).strip(), decl.group(2)

        def arg(name):
            mm = re.search(r"\b" + name + r"\s*=\s*", body)
            if not mm:
                return None
            j = mm.end()
            depth = 0
            k = j
            while k < len(body):
                ch = body[k]
                if body.startswith('"""', k):
                    k = body.index('"""', k + 3) + 3
                    continue
                if ch == '"':
                    k += 1
                    while body[k] != '"':
                        k += 2 if body[k] == "\\" else 1
                elif ch in "({":
                    depth += 1
                elif ch in ")}":
                    depth -= 1
                elif ch == "," and depth == 0:
                    break
                k += 1
            return body[j:k].strip()

        name = java_string_expr(arg("name"), consts)
        dv_expr = arg("defaultValue")
        dv = java_string_expr(dv_expr, consts) if dv_expr is not None else None
        if name is None or dv is None:
            raise ValueError(f"unresolved name/default in {rel}: {body[:200]!r}")
        conv = arg("converter")
        multi = (arg("allowMultiple") or "false") == "true"
        cls = [c for p, c in classes if p < m.start()]
        cname = cls[-1] if cls else outer
        fq = pkg + "." + (outer if cname == outer else outer + "$" + cname)
        opts.append(dict(cls=fq, name=name, ftype=ftype, field=field,
                         conv=(conv or "").replace(".class", ""), default=dv, multi=multi))
    return opts


def rust_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n").replace("\t", "\\t") + '"'


def generate(src, rev):
    allopts = []
    for rel in CLASS_FILES:
        allopts.extend(parse_file(rel, git_show(src, rev, LIB + rel + ".java")))
    by_class = {}
    for o in allopts:
        by_class.setdefault(o["cls"], []).append(o)
    out = ["// @generated by scripts/extract_config_options.py from Bazel sources (rev %s). DO NOT EDIT." % rev,
           "//",
           "// Schema of every FragmentOptions class that participates in `BuildOptions.checksum()`.",
           "",
           "use crate::build_options::ConfigOption;",
           "",
           "/// Fragment option classes, sorted by fully-qualified class name (BuildOptions' fragment order),",
           "/// each with its options sorted by option name (OptionsParser definition order).",
           "pub static CONFIG_OPTION_CLASSES: &[(&str, &[ConfigOption])] = &["]
    for cls in sorted(by_class):
        out.append(f"    ({rust_str(cls)}, &[")
        for o in sorted(by_class[cls], key=lambda o: o["name"]):
            out.append("        ConfigOption { name: %s, java_type: %s, converter: %s, default: %s, multiple: %s }," % (
                rust_str(o["name"]), rust_str(re.sub(r"\s+", "", o["ftype"])), rust_str(o["conv"]),
                rust_str(o["default"]), "true" if o["multi"] else "false"))
        out.append("    ]),")
    out.append("];")
    out += ["", "/// `@RequiresOptions` of every configuration fragment: (fragment class, options classes),",
            "/// sorted by fragment class name.",
            "pub static CONFIG_FRAGMENTS: &[(&str, &[&str])] = &["]
    for fq, opts in parse_fragments(src, rev, sorted(by_class)):
        out.append(f"    ({rust_str(fq)}, &[{', '.join(rust_str(o) for o in opts)}]),")
    out.append("];")
    # Exec transition (`builtin_exec_platforms.bzl`): every option listed in a `propagate = [...]`
    # copies from the source configuration; all other native options take their defaults.
    bzl = git_show(src, rev, "src/main/starlark/builtins_bzl/common/builtin_exec_platforms.bzl")
    prop = set()
    for m in re.finditer(r"propagate\s*=\s*\[([^\]]*)\]", bzl):
        prop.update(re.findall(r'"//command_line_option:([^"]+)"', m.group(1)))
    out += ["", "/// Options the exec transition propagates from the source configuration (sorted).",
            "pub static EXEC_PROPAGATED: &[&str] = &["]
    out += [f"    {rust_str(n)}," for n in sorted(prop)]
    out.append("];")
    return "\n".join(out) + "\n", len(allopts)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bazel-src", default=os.path.expanduser("~/Development/bazel"))
    ap.add_argument("--rev", default="9.3.2", help="label recorded in the generated header")
    ap.add_argument("--check", action="store_true")
    a = ap.parse_args()
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    dest = os.path.join(root, "crates/zel-analysis/src/build_options_table.rs")
    GLOBAL_SRC.update(src=a.bazel_src, rev=a.rev)
    text, n = generate(a.bazel_src, a.rev)
    if a.check:
        if open(dest).read() != text:
            print("build_options_table.rs is stale"); sys.exit(1)
        print(f"ok ({n} options)")
        return
    open(dest, "w").write(text)
    print(f"wrote {dest} ({n} options)")


if __name__ == "__main__":
    main()
