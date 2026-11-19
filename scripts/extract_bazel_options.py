#!/usr/bin/env python3
"""
scripts/extract_bazel_options.py

Generates crates/zel-options/src/flag_tables.rs directly from Bazel's Java source tree.
No calls to the real bazel binary or protoc; fully deterministic.

The generator models the same structure Bazel itself uses at runtime:

  * every `@Option` is attributed to its *innermost enclosing options class*
    (`OptionsBase` subclass), found with a real (brace/paren aware) Java tokenizer;
  * the options of a command are computed exactly like
    `BlazeCommandUtils.getOptions(commandClass, suppliers, ruleClassProvider)`:
      common options (`UiOptions`, `CommonCommandOptions`, ... + every module's
      `getCommonCommandOptions()`), the command's `@Command(options = ...)`, the fragment
      registry's options when `usesConfigurationOptions`, every module's
      `getCommandOptions(<command name>)`, and (recursively) `inheritsOptionsFrom`;
  * startup options are `BlazeCommandUtils.getStartupOptions`: `BlazeServerStartupOptions`,
    `HostJvmStartupOptions` + every module's `getStartupOptions()`;
  * the fragment registry options are the `@RequiresOptions` of every fragment registered by
    the Bazel rule sets, plus `addConfigurationOptions` classes.

The per-option `commands` list is the list of commands whose option set contains the option's
declaring class (this is what `help flags-as-proto` reports), NOT a name-based heuristic.
"""

import argparse
import os
import re
import sys

# --------------------------------------------------------------------------------------------
# Java tokenizer
# --------------------------------------------------------------------------------------------

ID_START = re.compile(r"[A-Za-z_$\u0080-\uffff]")
ID_PART = re.compile(r"[A-Za-z0-9_$\u0080-\uffff]")


def _decode_escapes(s):
    out = []
    i = 0
    while i < len(s):
        c = s[i]
        if c != "\\":
            out.append(c)
            i += 1
            continue
        i += 1
        if i >= len(s):
            break
        e = s[i]
        i += 1
        if e == "n":
            out.append("\n")
        elif e == "t":
            out.append("\t")
        elif e == "r":
            out.append("\r")
        elif e == "b":
            out.append("\b")
        elif e == "f":
            out.append("\f")
        elif e == "s":
            out.append(" ")
        elif e == "0" and not (i < len(s) and s[i] in "01234567"):
            out.append("\0")
        elif e in "01234567":
            j = i
            while j < len(s) and j < i + 2 and s[j] in "01234567":
                j += 1
            out.append(chr(int(e + s[i:j], 8)))
            i = j
        elif e == "u":
            while i < len(s) and s[i] == "u":
                i += 1
            out.append(chr(int(s[i:i + 4], 16)))
            i += 4
        elif e == "\n":
            pass  # line continuation (text blocks)
        else:
            out.append(e)  # \" \' \\
    return "".join(out)


def _text_block(raw):
    """JLS 3.10.6 text block: raw is the text after the opening delimiter's line terminator."""
    lines = raw.split("\n")
    # significant lines: non-blank lines plus the last line (the closing delimiter line).
    sig = [l for l in lines[:-1] if l.strip()] + [lines[-1]]
    indent = min(len(l) - len(l.lstrip(" \t")) for l in sig) if sig else 0
    stripped = []
    for idx, l in enumerate(lines):
        l2 = l[indent:] if len(l) >= indent else l.lstrip(" \t")
        if idx != len(lines) - 1 or l2.strip():
            l2 = l2.rstrip(" \t")
        else:
            l2 = ""
        stripped.append(l2)
    return _decode_escapes("\n".join(stripped))


def tokenize(src):
    """Returns a list of (kind, value, pos) with kind in id/str/char/num/sym."""
    toks = []
    i = 0
    n = len(src)
    while i < n:
        c = src[i]
        if c in " \t\r\n\f":
            i += 1
        elif src.startswith("//", i):
            j = src.find("\n", i)
            i = n if j < 0 else j
        elif src.startswith("/*", i):
            j = src.find("*/", i + 2)
            i = n if j < 0 else j + 2
        elif src.startswith('"""', i):
            nl = src.find("\n", i + 3)
            body_start = nl + 1
            j = body_start
            while True:
                k = src.find('"""', j)
                if k < 0:
                    raise ValueError("unterminated text block")
                b = 0
                while src[k - 1 - b] == "\\":
                    b += 1
                if b % 2 == 0:
                    break
                j = k + 1
            toks.append(("str", _text_block(src[body_start:k]), i))
            i = k + 3
        elif c == '"':
            j = i + 1
            while src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            toks.append(("str", _decode_escapes(src[i + 1:j]), i))
            i = j + 1
        elif c == "'":
            j = i + 1
            while src[j] != "'":
                j += 2 if src[j] == "\\" else 1
            toks.append(("char", _decode_escapes(src[i + 1:j]), i))
            i = j + 1
        elif ID_START.match(c):
            j = i + 1
            while j < n and ID_PART.match(src[j]):
                j += 1
            toks.append(("id", src[i:j], i))
            i = j
        elif c.isdigit():
            j = i + 1
            while j < n and (src[j].isalnum() or src[j] in "._"):
                j += 1
            toks.append(("num", src[i:j], i))
            i = j
        else:
            toks.append(("sym", c, i))
            i += 1
    return toks


def match_close(toks, i, open_c, close_c):
    """toks[i] is open_c; returns the index of the matching close."""
    depth = 0
    while i < len(toks):
        t = toks[i]
        if t[0] == "sym":
            if t[1] == open_c:
                depth += 1
            elif t[1] == close_c:
                depth -= 1
                if depth == 0:
                    return i
        i += 1
    raise ValueError("unbalanced " + open_c)


def split_top(toks, sep=","):
    """Splits a token list on top-level separators."""
    out, cur, depth = [], [], 0
    for t in toks:
        if t[0] == "sym" and t[1] in "({[":
            depth += 1
        elif t[0] == "sym" and t[1] in ")}]":
            depth -= 1
        if depth == 0 and t[0] == "sym" and t[1] == sep:
            out.append(cur)
            cur = []
        else:
            cur.append(t)
    if cur:
        out.append(cur)
    return out


# --------------------------------------------------------------------------------------------
# Java index
# --------------------------------------------------------------------------------------------

MODIFIERS = {"public", "protected", "private", "abstract", "static", "final", "default",
             "synchronized", "native", "transient", "volatile", "strictfp", "sealed"}


class ClassInfo:
    def __init__(self, name, kind, package, outer, path):
        self.name = name
        self.kind = kind
        self.package = package
        self.outer = outer  # ClassInfo or None
        self.path = path
        self.extends = []  # list of token-lists (type refs)
        self.implements = []
        self.imports = {}
        self.static_imports = {}
        self.options = []  # OptionDecl
        self.parsed_options = []
        self.constants = {}  # NAME -> token list of the initializer
        self.annotations = {}  # annotation name -> arg tokens
        self.toks = None
        self.body_start = 0
        self.body_end = 0

    @property
    def qname(self):
        return (self.outer.qname + "." if self.outer else "") + self.name

    @property
    def fqn(self):
        return (self.package + "." if self.package else "") + self.qname

    @property
    def binary_name(self):
        return (self.outer.binary_name + "$" if self.outer else "") + self.name

    @property
    def java_class(self):
        return (self.package + "." if self.package else "") + self.binary_name


class OptionDecl:
    pass


class Index:
    def __init__(self):
        self.classes = []
        self.by_fqn = {}
        self.by_simple = {}

    def add(self, c):
        self.classes.append(c)
        self.by_fqn[c.fqn] = c
        self.by_simple.setdefault(c.name, []).append(c)

    def resolve(self, chain, ctx):
        """Resolves a (possibly qualified) class reference like ['Foo','Bar'] from class ctx."""
        if not chain:
            return None
        first = chain[0]
        dotted = ".".join(chain)
        c = ctx
        while c is not None:
            cand = self.by_fqn.get(c.fqn + "." + dotted)
            if cand:
                return cand
            c = c.outer
        cand = self.by_fqn.get(dotted)
        if cand:
            return cand
        imp = ctx.imports.get(first) if ctx else None
        if imp:
            cand = self.by_fqn.get(imp + ("." + ".".join(chain[1:]) if len(chain) > 1 else ""))
            if cand:
                return cand
        if ctx:
            cand = self.by_fqn.get((ctx.package + "." if ctx.package else "") + dotted)
            if cand:
                return cand
        cands = list(self.by_simple.get(first, []))
        if len(chain) > 1:
            cands = [self.by_fqn[k.fqn + "." + ".".join(chain[1:])] for k in cands
                     if (k.fqn + "." + ".".join(chain[1:])) in self.by_fqn]
        if len(cands) == 1:
            return cands[0]
        if len(cands) > 1:
            for k in cands:
                if ctx and k.package == ctx.package:
                    return k
            return cands[0]
        return None

    def lookup_const(self, chain, ctx):
        """Returns (class, tokens) for a constant reference."""
        if len(chain) == 1:
            c = ctx
            while c is not None:
                if chain[0] in c.constants:
                    return c, c.constants[chain[0]]
                for ext in c.extends + c.implements:
                    sup = self.resolve(type_ref(ext), c)
                    if sup and chain[0] in sup.constants:
                        return sup, sup.constants[chain[0]]
                c = c.outer
            if ctx and chain[0] in ctx.static_imports:
                oc = self.by_fqn.get(ctx.static_imports[chain[0]])
                if oc and chain[0] in oc.constants:
                    return oc, oc.constants[chain[0]]
            return None
        owner = self.resolve(chain[:-1], ctx)
        if owner and chain[-1] in owner.constants:
            return owner, owner.constants[chain[-1]]
        return None


def type_ref(toks):
    """Reduces a type token list to a dotted chain of identifiers (dropping generic args)."""
    chain = []
    depth = 0
    for t in toks:
        if t[0] == "sym" and t[1] == "<":
            depth += 1
        elif t[0] == "sym" and t[1] == ">":
            depth -= 1
        elif depth == 0 and t[0] == "id":
            chain.append(t[1])
    return chain


def _angle_depth(toks):
    d = 0
    for t in toks:
        if t[0] == "sym" and t[1] == "<":
            d += 1
        elif t[0] == "sym" and t[1] == ">":
            d -= 1
    return d


def parse_file(path, idx, wanted_pred):
    with open(path, "r", encoding="utf-8", errors="ignore") as fh:
        src = fh.read()
    if not wanted_pred(src):
        return
    toks = tokenize(src)
    package = ""
    imports = {}
    static_imports = {}
    i = 0
    n = len(toks)
    while i < n:
        t = toks[i]
        if t[0] == "id" and t[1] == "package":
            j = i + 1
            parts = []
            while toks[j][1] != ";":
                parts.append(toks[j][1])
                j += 1
            package = "".join(parts)
            i = j + 1
        elif t[0] == "id" and t[1] == "import":
            j = i + 1
            static = False
            if toks[j][1] == "static":
                static = True
                j += 1
            parts = []
            while toks[j][1] != ";":
                parts.append(toks[j][1])
                j += 1
            full = "".join(parts)
            simple = full.rsplit(".", 1)[-1]
            if static:
                static_imports[simple] = full.rsplit(".", 1)[0]
            else:
                imports[simple] = full
            i = j + 1
        else:
            break

    stack = []  # (ClassInfo, body_depth)
    depth = 0
    pending_ann = []
    while i < n:
        t = toks[i]
        k, v = t[0], t[1]
        if k == "sym" and v == "{":
            depth += 1
            i += 1
            continue
        if k == "sym" and v == "}":
            depth -= 1
            while stack and stack[-1][1] > depth:
                c, _ = stack.pop()
                c.body_end = i
            pending_ann = []
            i += 1
            continue
        if k == "sym" and v == "@" and i + 1 < n and toks[i + 1][0] == "id" and toks[i + 1][1] != "interface":
            j = i + 1
            name_parts = [toks[j][1]]
            j += 1
            while j + 1 < n and toks[j][1] == "." and toks[j + 1][0] == "id":
                name_parts.append(toks[j + 1][1])
                j += 2
            args = []
            if j < n and toks[j][0] == "sym" and toks[j][1] == "(":
                e = match_close(toks, j, "(", ")")
                args = toks[j + 1:e]
                j = e + 1
            ann_name = name_parts[-1]
            pending_ann.append((ann_name, args))
            i = j
            if ann_name == "Option":
                cur = stack[-1][0] if stack else None
                _parse_option(toks, i, args, cur, path)
                pending_ann = []
            continue
        if k == "id" and v in ("class", "interface", "enum", "record") and not (
                i > 0 and toks[i - 1][0] == "sym" and toks[i - 1][1] in ".@"):
            j = i + 1
            if j < n and toks[j][0] == "id":
                name = toks[j][1]
                j += 1
                hdr_start = j
                paren = 0
                while j < n and not (toks[j][0] == "sym" and toks[j][1] == "{" and paren == 0):
                    if toks[j][0] == "sym" and toks[j][1] == "(":
                        paren += 1
                    elif toks[j][0] == "sym" and toks[j][1] == ")":
                        paren -= 1
                    j += 1
                hdr = toks[hdr_start:j]
                outer = stack[-1][0] if stack else None
                c = ClassInfo(name, v, package, outer, path)
                c.imports = imports
                c.static_imports = static_imports
                c.annotations = {a[0]: a[1] for a in pending_ann}
                mode = None
                cur = []
                groups = {"extends": [], "implements": []}
                for t2 in hdr:
                    if t2[0] == "id" and t2[1] in ("extends", "implements", "permits"):
                        if cur and mode:
                            groups[mode].append(cur)
                        cur = []
                        mode = t2[1] if t2[1] != "permits" else None
                        continue
                    if mode is None:
                        continue
                    if t2[0] == "sym" and t2[1] == "," and _angle_depth(cur) == 0:
                        groups[mode].append(cur)
                        cur = []
                    else:
                        cur.append(t2)
                if cur and mode:
                    groups[mode].append(cur)
                c.extends = groups["extends"]
                c.implements = groups["implements"]
                c.toks = toks
                c.body_start = j
                idx.add(c)
                stack.append((c, depth + 1))
                pending_ann = []
                i = j
                continue
        if stack and depth == stack[-1][1] and k == "id" and v in ("String", "int", "long") and i + 2 < n and \
                toks[i + 1][0] == "id" and toks[i + 2][0] == "sym" and toks[i + 2][1] == "=":
            j = i + 3
            expr = []
            d2 = 0
            while j < n:
                tj = toks[j]
                if tj[0] == "sym" and tj[1] in "({[":
                    d2 += 1
                elif tj[0] == "sym" and tj[1] in ")}]":
                    d2 -= 1
                elif tj[0] == "sym" and tj[1] == ";" and d2 == 0:
                    break
                expr.append(tj)
                j += 1
            stack[-1][0].constants[toks[i + 1][1]] = expr
            i = j
            continue
        if k == "sym" and v == ";":
            pending_ann = []
        i += 1
    while stack:
        c, _ = stack.pop()
        c.body_end = n


def _parse_option(toks, i, args, cur, path):
    """toks[i:] begins right after the @Option(...) annotation."""
    n = len(toks)
    j = i
    while j < n and toks[j][0] == "sym" and toks[j][1] == "@":
        j += 2
        while j + 1 < n and toks[j][1] == "." and toks[j + 1][0] == "id":
            j += 2
        if j < n and toks[j][0] == "sym" and toks[j][1] == "(":
            j = match_close(toks, j, "(", ")") + 1
    decl = []
    while j < n and not (toks[j][0] == "sym" and toks[j][1] in "(;={"):
        decl.append(toks[j])
        j += 1
    ids = [t for t in decl if t[0] == "id"]
    name = ids[-1][1]
    type_toks = [t for t in decl[:-1] if not (t[0] == "id" and t[1] in MODIFIERS)]
    od = OptionDecl()
    od.cls = cur
    od.args = args
    od.member = name
    od.type_toks = type_toks
    od.path = path
    if cur is None:
        return
    cur.options.append(od)


# --------------------------------------------------------------------------------------------
# Expression evaluation (string constants)
# --------------------------------------------------------------------------------------------

class Unresolved(Exception):
    pass


def eval_str(toks, ctx, idx, depth=0):
    """Evaluates a Java string-concatenation expression made of literals/constants."""
    if depth > 8:
        raise Unresolved("depth")
    out = []
    i = 0
    n = len(toks)
    expect_operand = True
    while i < n:
        t = toks[i]
        if t[0] == "sym" and t[1] == "(" and expect_operand:
            e = match_close(toks, i, "(", ")")
            out.append(eval_str(toks[i + 1:e], ctx, idx, depth + 1))
            i = e + 1
            expect_operand = False
            continue
        if t[0] == "sym" and t[1] == "+":
            expect_operand = True
            i += 1
            continue
        if t[0] == "sym" and t[1] == "*" and out:
            # integer multiplication of the previous and the next operand
            rhs = eval_str(toks[i + 1:i + 2], ctx, idx, depth + 1)
            out[-1] = str(int(out[-1]) * int(rhs))
            i += 2
            continue
        if t[0] in ("str", "num", "char"):
            out.append(t[1])
            i += 1
            expect_operand = False
            continue
        if t[0] == "id":
            chain = [t[1]]
            j = i + 1
            while j + 1 < n and toks[j][1] == "." and toks[j + 1][0] == "id":
                chain.append(toks[j + 1][1])
                j += 2
            if j < n and toks[j][1] == "(":
                raise Unresolved("call " + ".".join(chain))
            r = idx.lookup_const(chain, ctx)
            if not r:
                raise Unresolved("const " + ".".join(chain))
            owner, expr = r
            out.append(eval_str(expr, owner, idx, depth + 1))
            i = j
            expect_operand = False
            continue
        raise Unresolved("token %r" % (t,))
    return "".join(out)


def arg_map(args):
    """Parses `a = x, b = {..}` annotation args into {a: tokens}."""
    out = {}
    for part in split_top(args):
        if len(part) >= 2 and part[0][0] == "id" and part[1][0] == "sym" and part[1][1] == "=":
            out[part[0][1]] = part[2:]
        elif part:
            out["value"] = part
    return out


def strip_braces(toks):
    if toks and toks[0][0] == "sym" and toks[0][1] == "{":
        return toks[1:-1]
    return toks


def class_refs(toks, ctx, idx):
    out = []
    for part in split_top(strip_braces(toks)):
        chain = [t[1] for t in part if t[0] == "id" and t[1] != "class"]
        if not chain:
            continue
        c = idx.resolve(chain, ctx)
        if c is None:
            raise Unresolved("class " + ".".join(chain))
        out.append(c)
    return out


def enum_refs(toks, prefix):
    return [t[1] for t in toks if t[0] == "id" and t[1] != prefix]


# --------------------------------------------------------------------------------------------
# Option extraction
# --------------------------------------------------------------------------------------------

DEFAULT_CONVERTER_BY_TYPE = {
    "String": "StringConverter",
    "int": "IntegerConverter",
    "long": "LongConverter",
    "double": "DoubleConverter",
    "boolean": "BooleanConverter",
    "TriState": "TriStateConverter",
    "Duration": "DurationConverter",
    "Void": "VoidConverter",
}

DEFAULT_TYPE_DESCRIPTION = {
    "String": "a string", "int": "an integer", "long": "a long integer", "double": "a double",
    "boolean": "a boolean", "Duration": "An immutable length of time.", "Void": "",
    "TriState": "a tri-state (auto, yes, no)",
}


def convert_option(od, idx, unresolved):
    cls = od.cls
    am = arg_map(od.args)

    def s(key, default=""):
        if key not in am:
            return default
        try:
            return eval_str(am[key], cls, idx)
        except Unresolved as e:
            unresolved.append((cls.fqn, od.member, key, str(e)))
            return default

    o = {}
    o["name"] = s("name")
    o["old_name"] = s("oldName")
    ab = am.get("abbrev")
    o["abbrev"] = ab[0][1] if ab and ab[0][0] == "char" else None
    o["default_value"] = s("defaultValue", None)
    o["help"] = s("help")
    o["value_help"] = s("valueHelp")
    o["deprecation_warning"] = s("deprecationWarning")
    o["allow_multiple"] = bool(am.get("allowMultiple") and am["allowMultiple"][0][1] == "true")
    o["documentation_category"] = (enum_refs(am["documentationCategory"], "OptionDocumentationCategory")[0]
                                   if "documentationCategory" in am else "UNCATEGORIZED")
    o["effect_tags"] = enum_refs(am.get("effectTags", []), "OptionEffectTag")
    o["metadata_tags"] = enum_refs(am.get("metadataTags", []), "OptionMetadataTag")
    for key, field in (("expansion", "expansion"), ("implicitRequirements", "implicit_requirements")):
        vals = []
        if key in am:
            for part in split_top(strip_braces(am[key])):
                try:
                    vals.append(eval_str(part, cls, idx))
                except Unresolved as e:
                    unresolved.append((cls.fqn, od.member, key, str(e)))
        o[field] = vals
    ids = [t[1] for t in od.type_toks if t[0] == "id"]
    o["ret_type"] = ids[0] if ids else ""
    if o["allow_multiple"]:
        o["element_type"] = ids[1] if len(ids) > 1 else "String"
    else:
        o["element_type"] = o["ret_type"]
    conv_cls = None
    o["converter_simple"] = None
    if "converter" in am:
        chain = [t[1] for t in am["converter"] if t[0] == "id" and t[1] != "class"]
        conv_cls = idx.resolve(chain, cls)
        o["converter_simple"] = chain[-1] if chain else None
    if conv_cls is None and o["converter_simple"] is None:
        dc = DEFAULT_CONVERTER_BY_TYPE.get(o["element_type"])
        if dc:
            conv_cls = idx.by_fqn.get("com.google.devtools.common.options.Converters." + dc)
    o["converter_cls"] = conv_cls
    o["effective_converter"] = o["converter_simple"] or DEFAULT_CONVERTER_BY_TYPE.get(o["element_type"])
    o["declaring"] = cls
    return o


class Model:
    pass


def is_subtype(cls, target_names, idx, seen=None):
    if seen is None:
        seen = set()
    if cls is None or cls.fqn in seen:
        return False
    seen.add(cls.fqn)
    for ref in cls.extends + cls.implements:
        chain = type_ref(ref)
        if not chain:
            continue
        if chain[-1] in target_names:
            return True
        sup = idx.resolve(chain, cls)
        if sup and is_subtype(sup, target_names, idx, seen):
            return True
    return False


def find_method_body(cls, name):
    """Finds `name(...) { body }` directly inside cls (not nested classes)."""
    toks = cls.toks
    i = cls.body_start + 1
    end = cls.body_end
    depth = 1
    while i < end:
        t = toks[i]
        if t[0] == "sym" and t[1] == "{":
            depth += 1
        elif t[0] == "sym" and t[1] == "}":
            depth -= 1
        elif depth == 1 and t[0] == "id" and t[1] == name and i + 1 < end and toks[i + 1][1] == "(":
            e = match_close(toks, i + 1, "(", ")")
            j = e + 1
            while j < end and not (toks[j][0] == "sym" and toks[j][1] in "{;"):
                j += 1
            if j < end and toks[j][1] == "{":
                be = match_close(toks, j, "{", "}")
                return toks[i + 2:e], toks[j + 1:be]
            return toks[i + 2:e], None
        i += 1
    return None


def walk_supers(cls, idx):
    seen = set()
    c = cls
    while c is not None and c.fqn not in seen:
        seen.add(c.fqn)
        yield c
        if not c.extends:
            return
        c = idx.resolve(type_ref(c.extends[0]), c)


def string_return(body, ctx, idx):
    if not body:
        return None
    if body[0][0] == "id" and body[0][1] == "return":
        expr = body[1:]
        if expr and expr[-1][1] == ";":
            expr = expr[:-1]
        try:
            return eval_str(expr, ctx, idx)
        except Unresolved:
            return None
    return None


def enum_constants(enum_cls):
    toks = enum_cls.toks
    i = enum_cls.body_start + 1
    end = enum_cls.body_end
    consts = []
    while i < end:
        t = toks[i]
        if t[0] == "sym" and t[1] == "@":
            i += 2
            if i < end and toks[i][1] == "(":
                i = match_close(toks, i, "(", ")") + 1
            continue
        if t[0] == "id":
            name = t[1]
            i += 1
            args = []
            if i < end and toks[i][1] == "(":
                e = match_close(toks, i, "(", ")")
                args = toks[i + 1:e]
                i = e + 1
            if i < end and toks[i][1] == "{":
                i = match_close(toks, i, "{", "}") + 1
            consts.append((name, args))
            if i < end and toks[i][1] == ",":
                i += 1
                continue
            break
        break
    return consts


def enum_tostring_values(enum_cls):
    """Models Enum.toString() of each constant: name() unless toString is overridden."""
    consts = enum_constants(enum_cls)
    m = find_method_body(enum_cls, "toString")
    names = [c[0] for c in consts]
    if not m or not m[1]:
        return names
    text = "".join(t[1] if t[0] != "str" else '"' + t[1] + '"' for t in m[1])
    if "toLowerCase(name())" in text or "name().toLowerCase" in text or "toLowerCase(this.name())" in text:
        return [x.lower() for x in names]
    cases = dict(re.findall(r'case(\w+)->"([^"]*)"', text))
    if cases and all(n in cases for n in names):
        return [cases[n] for n in names]  # `switch (this) { case X -> "x"; ... }`
    if re.match(r"return(this\.)?\w+;", text):
        return [(a[0][1] if a and a[0][0] == "str" else nm) for nm, a in consts]
    return names


def join_english_list(vals):
    if not vals:
        return "nothing"
    buf = ""
    for k, v in enumerate(vals):
        if buf:
            buf += ", " if k < len(vals) - 1 else " or "
        buf += v
    return buf


def ctor_super_args(cls):
    """Argument token lists of the `super(...)` call in cls's constructor (None if there is none)."""
    toks = cls.toks
    depth = 0
    for i in range(cls.body_start, cls.body_end):
        t = toks[i]
        if t[0] == "sym" and t[1] == "{":
            depth += 1
        elif t[0] == "sym" and t[1] == "}":
            depth -= 1
        elif t[0] == "id" and t[1] == "super" and i + 1 < cls.body_end and toks[i + 1][1] == "(" and depth == 2:
            e = match_close(toks, i + 1, "(", ")")
            return split_top(toks[i + 2:e])
    return None


INT_CONSTS = {"Integer.MAX_VALUE": 2147483647, "Integer.MIN_VALUE": -2147483648}


def eval_int(toks, ctx, idx, depth=0):
    """Evaluates a small Java int expression (literals, `Integer.MAX_VALUE`, constants, + - * >> <<)."""
    if depth > 6:
        raise Unresolved("depth")
    out = []
    i = 0
    while i < len(toks):
        t = toks[i]
        if t[0] == "num":
            out.append(re.sub(r"[lL]$", "", t[1]))
        elif t[0] == "sym":
            out.append(t[1])
        elif t[0] == "id":
            chain = [t[1]]
            j = i + 1
            while j + 1 < len(toks) and toks[j][1] == "." and toks[j + 1][0] == "id":
                chain.append(toks[j + 1][1])
                j += 2
            name = ".".join(chain)
            if name in INT_CONSTS:
                out.append("(%d)" % INT_CONSTS[name])
            else:
                r = idx.lookup_const(chain, ctx)
                if not r:
                    raise Unresolved("int const " + name)
                out.append("(%d)" % eval_int(r[1], r[0], idx, depth + 1))
            i = j
            continue
        else:
            raise Unresolved("int token %r" % (t,))
        i += 1
    expr = "".join(out)
    if not re.fullmatch(r"[0-9()+\-*<> ]+", expr):
        raise Unresolved("int expr " + expr)
    return int(eval(expr, {"__builtins__": {}}, {}))  # noqa: S307 - restricted to digits/operators above


def dynamic_type_description(conv_cls, idx):
    """Models the `getTypeDescription()` of the converter families whose description is computed from constructor
    arguments (Converters.RangeConverter, the Separated*Converter family, ResourceConverter.IntegerConverter, enum
    filter converters, String.format based descriptions). Returns None when not modelled."""
    chain = list(walk_supers(conv_cls, idx))
    names = [c.fqn.rsplit(".", 2)[-2] + "." + c.name if c.outer else c.name for c in chain]
    full = [c.fqn for c in chain]
    for c in chain:
        ctor = ctor_super_args(c)
        if ctor is not None:
            break
    else:
        ctor, c = None, conv_cls
    # explicit `String.format("...", CONST...)` in the converter itself
    m = find_method_body(conv_cls, "getTypeDescription")
    if m and m[1] and m[1][0][1] == "return" and len(m[1]) > 4 and m[1][1][1] == "String" and m[1][3][1] == "format":
        args = split_top(m[1][5:-2])
        fmt = eval_str(args[0], conv_cls, idx)
        vals = [eval_str(a, conv_cls, idx) if not (len(a) == 1 and a[0][0] == "id") else
                eval_str(a, idx.by_fqn.get("com.google.devtools.build.lib.util.ResourceConverter") or conv_cls, idx)
                for a in args[1:]]
        return fmt % tuple(vals)
    body = m[1] if m and m[1] else []
    # `Joiner.on(", ").join(SomeEnum.values())`
    if len(body) > 12 and [t[1] for t in body[:5]] == ["return", "Joiner", ".", "on", "("] and body[5][0] == "str":
        ids = [t[1] for t in body if t[0] == "id"]
        ref = idx.resolve([ids[ids.index("join") + 1]], conv_cls) if "join" in ids else None
        if ref is not None and ref.kind == "enum":
            return body[5][1].join(enum_tostring_values(ref))
    # `"..." + (LEVELS.size() - 1)` with LEVELS = ImmutableList.of(<n entries>)
    if any(t[1] == "LEVELS" for t in body):
        toks = conv_cls.toks
        for i in range(conv_cls.body_start, conv_cls.body_end):
            if toks[i][1] == "LEVELS" and toks[i + 1][1] == "=":
                j = i + 2
                while toks[j][1] != "(":
                    j += 1
                n = len(split_top(toks[j + 1:match_close(toks, j, "(", ")")]))
                lit = [t[1] for t in body if t[0] == "str"][0]
                return lit + str(n - 1)
    # ResourceConverter.AssignmentConverter: "a named double, ..." + resource.getTypeDescription()
    if conv_cls.fqn.endswith("ResourceConverter.AssignmentConverter") and body and any(t[0] == "str" for t in body):
        rc = idx.by_fqn["com.google.devtools.build.lib.util.ResourceConverter"]
        toks = conv_cls.toks
        kws = []
        for i in range(conv_cls.body_start, conv_cls.body_end):
            if toks[i][1] == "ImmutableMap" and toks[i + 1][1] == "." and toks[i + 2][1] == "of":
                e = match_close(toks, i + 3, "(", ")")
                for part in split_top(toks[i + 4:e])[::2]:
                    kws.append(eval_str(part, rc, idx))
                break
        prefix = [t[1] for t in body if t[0] == "str"][0]
        return prefix + ('an integer, or a keyword ("%s"), optionally followed by an operation ([-|*]<float>) eg. '
                         '"%s", "HOST_CPUS*.5"') % ('", "'.join(kws), kws[0])
    if any(f.endswith("Converters.RangeConverter") for f in full) and ctor and len(ctor) == 2:
        mn, mx = eval_int(ctor[0], c, idx), eval_int(ctor[1], c, idx)
        if mn == -2147483648:
            return "an integer" if mx == 2147483647 else "an integer, <= %d" % mx
        if mx == 2147483647:
            return "an integer, >= %d" % mn
        return "an integer in %s-%d range" % ("(%d)" % mn if mn < 0 else str(mn), mx)
    for sep_cls in ("SeparatedOptionSetConverter", "SeparatedOptionListConverter"):
        if any(f.endswith("Converters." + sep_cls) for f in full):
            for c2 in chain:
                a = ctor_super_args(c2)
                if a is not None and len(a) >= 3 and a[1] and a[1][0][0] == "str":
                    kind = "list" if sep_cls.endswith("ListConverter") else "set"
                    return "%s-separated %s of options" % (a[1][0][1], kind)
                if a is not None and len(a) >= 2 and a[0] and a[0][0][0] == "str":
                    kind = "list" if sep_cls.endswith("ListConverter") else "set"
                    return "%s-separated %s of options" % (a[0][0][1], kind)
    if any(f.endswith("ResourceConverter.IntegerConverter") for f in full) and ctor is not None:
        first = ctor[0]
        if first and first[0][1] == "ImmutableMap":
            kws = [eval_str(p, c, idx) if p[0][0] != "id" else
                   eval_str(p, idx.by_fqn["com.google.devtools.build.lib.util.ResourceConverter"], idx)
                   for p in split_top(first[4:-1])[::2]]
        else:
            kws = ["auto", "HOST_CPUS", "HOST_RAM"]
        return ('an integer, or a keyword ("%s"), optionally followed by an operation ([-|*]<float>) eg. "%s", '
                '"HOST_CPUS*.5"') % ('", "'.join(kws), kws[0])
    if any(f.endswith("EnumFilterConverter") for f in full) and ctor is not None and ctor[0]:
        ref = idx.resolve([t[1] for t in ctor[0] if t[0] == "id" and t[1] != "class"], c)
        if ref is not None and ref.kind == "enum":
            return "comma-separated list of values: " + join_english_list(enum_tostring_values(ref)).lower() \
                if False else "comma-separated list of values: " + join_english_and_list(enum_tostring_values(ref)).lower()
    return None


def join_english_and_list(vals):
    """`StringUtil.joinEnglishList(values)` (default conjunction \"and\")."""
    if not vals:
        return "nothing"
    if len(vals) == 1:
        return vals[0]
    return ", ".join(vals[:-1]) + " and " + vals[-1]


def converter_info(conv_cls, idx):
    info = {"type_description": None, "enum_values": None, "boolean_syntax": False}
    if conv_cls is None:
        return info
    if is_subtype(conv_cls, {"BoolOrEnumConverter", "BooleanStyleOption"}, idx):
        info["boolean_syntax"] = True
    if is_subtype(conv_cls, {"EnumConverter"}, idx):
        enum_cls = None
        for c in walk_supers(conv_cls, idx):
            if c.extends:
                ids = [t[1] for t in c.extends[0] if t[0] == "id"]
                if len(ids) > 1:
                    cand = idx.resolve(ids[1:], c)
                    if cand is not None and cand.kind == "enum":
                        enum_cls = cand
                        break
        if enum_cls is not None and enum_cls.kind == "enum":
            vals = enum_tostring_values(enum_cls)
            info["enum_values"] = vals
            info["type_description"] = join_english_list(vals).lower()
        for c in walk_supers(conv_cls, idx):
            if c.name == "EnumConverter":
                break
            m = find_method_body(c, "getTypeDescription")
            if m and m[1]:
                r = string_return(m[1], c, idx)
                if r is not None:
                    info["type_description"] = r
                break
        return info
    for c in walk_supers(conv_cls, idx):
        m = find_method_body(c, "getTypeDescription")
        if m and m[1]:
            r = string_return(m[1], c, idx)
            if r is not None:
                info["type_description"] = r
            break
    if info["type_description"] is None:
        try:
            info["type_description"] = dynamic_type_description(conv_cls, idx)
        except (Unresolved, ValueError, IndexError, KeyError, TypeError):
            info["type_description"] = None
    return info


# --------------------------------------------------------------------------------------------
# Rust emission helpers
# --------------------------------------------------------------------------------------------

def escape_rust_string(s):
    if s is None:
        return ""
    return (s.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")
            .replace("\r", "\\r").replace("\t", "\\t").replace("\0", "\\0"))


def to_pascal_case(s):
    return "".join(p.capitalize() for p in s.split("_"))


DOC_CATS = {
    "UNCATEGORIZED": "Uncategorized", "UNDOCUMENTED": "Undocumented",
    "BAZEL_CLIENT_OPTIONS": "BazelClientOptions", "LOGGING": "Logging",
    "EXECUTION_STRATEGY": "ExecutionStrategy", "BUILD_TIME_OPTIMIZATION": "BuildTimeOptimization",
    "OUTPUT_SELECTION": "OutputSelection", "OUTPUT_PARAMETERS": "OutputParameters",
    "INPUT_STRICTNESS": "InputStrictness", "SIGNING": "Signing",
    "STARLARK_SEMANTICS": "StarlarkSemantics", "TOOLCHAIN": "Toolchain", "TESTING": "Testing",
    "QUERY": "Query", "MOD_COMMAND": "ModCommand", "BZLMOD": "Bzlmod",
    "BUILD_VERBOSITY": "BuildVerbosity", "GENERIC_INPUTS": "GenericInputs", "REMOTE": "Remote",
}


def map_doc_category(cat):
    if cat not in DOC_CATS:
        raise ValueError("unknown documentation category " + cat)
    return "OptionDocumentationCategory::" + DOC_CATS[cat]


def map_effect_tag(tag):
    return "OptionEffectTag::" + to_pascal_case(tag)


def map_metadata_tag(tag):
    return "OptionMetadataTag::" + to_pascal_case(tag)


# Rust-side `OptionType` shape of each option. The *parsing* semantics of a few resource-style
# options are modelled by dedicated Rust types (see zel-util's resource converter); everything else is
# derived from the converter/return type found in the Java source.
RESOURCE_SPECS = {
    "jobs": "OptionType::Resource(IntegerSpec::JOBS)",
    "local_test_jobs": "OptionType::Resource(IntegerSpec::LOCAL_TEST_JOBS)",
    "loading_phase_threads": "OptionType::Resource(IntegerSpec::THREADS_MIN1)",
    "experimental_fsvc_threads": "OptionType::Resource(IntegerSpec::THREADS_MIN1)",
        "local_resources": "OptionType::ResourceAssignment",
}

CUSTOM_CONVERTERS = {
    "worker_max_instances": "OptionType::Custom(\"MultiResourceConverter\")",
    "gc_thrashing_threshold": "OptionType::Custom(\"PercentageConverter\")",
    "progress_report_interval": "OptionType::Custom(\"ProgressReportIntervalConverter\")",
    "experimental_ui_max_stdouterr_bytes": "OptionType::Custom(\"MaxStdoutErrBytesConverter\")",
    "remote_timeout": "OptionType::Custom(\"RemoteDurationConverter\")",
    "repo_contents_cache_gc_idle_delay": "OptionType::Duration",
    "repo_contents_cache_gc_max_age": "OptionType::Duration",
    "http_connector_retry_max_timeout": "OptionType::Duration",
    "experimental_cpu_load_scheduling_window_size": "OptionType::Duration",
    "spawn_strategy": "OptionType::Custom(\"CommaSeparatedNonEmptyOptionListConverter\")",
    "strategy": "OptionType::Custom(\"StringToStringListConverter\")",
    "dynamic_local_strategy": "OptionType::Custom(\"StringToStringListConverter\")",
    "dynamic_remote_strategy": "OptionType::Custom(\"StringToStringListConverter\")",
    "define": "OptionType::Custom(\"AssignmentConverter\")",
    "test_timeout": "OptionType::Custom(\"TestTimeoutConverter\")",
    "package_path": "OptionType::Custom(\"ColonSeparatedOptionListConverter\")",
}

ENUM_SPEC_OPTIONS = {
    "color": ("ENUM_SPEC_COLOR", "--color setting", ["yes", "no", "auto"], None, None),
    "subcommands": ("ENUM_SPEC_SUBCOMMANDS", "subcommand option", ["true", "pretty_print", "false"], "true", "false"),
    "test_output": ("ENUM_SPEC_TEST_OUTPUT", "test output", ["summary", "errors", "all", "streamed"], None, None),
    "compilation_mode": ("ENUM_SPEC_COMPILATION_MODE", "compilation mode", ["fastbuild", "dbg", "opt"], None, None),
    "strip": ("ENUM_SPEC_STRIP", "strip mode", ["always", "sometimes", "never"], None, None),
    "skyframe": ("ENUM_SPEC_SKYFRAME", "Skyframe Dump option",
                 ["off", "summary", "count", "value", "deps", "rdeps", "function_graph", "active_directories",
                  "active_directories_frontier_deps"], None, None),
    "info_output_type": ("ENUM_SPEC_INFO_OUTPUT_TYPE", "InfoItem output type", ["stdout", "response_proto"], None, None),
    "experimental_convenience_symlinks": ("ENUM_SPEC_EXPERIMENTAL_CONVENIENCE_SYMLINKS", "convenience symlinks mode",
                                          ["normal", "clean", "ignore", "log_only"], "normal", "ignore"),
    "strict_proto_deps": ("ENUM_SPEC_STRICT_PROTO_DEPS", "strict dependency checking level", ["off", "warn", "error", "strict", "default"], None, None),
    "order_output": ("ENUM_SPEC_ORDER_OUTPUT", "Order output setting", ["no", "deps", "auto", "full"], None, None),
}


# Java enum behind each ENUM_SPEC_OPTIONS entry (flag -> (source file under lib/, enum simple name)).
# The value lists in ENUM_SPEC_OPTIONS are NOT trusted: derive_enum_values() re-derives them from the
# Bazel source (EnumConverter lower-cases the constant names) and rewrites the table, so the generated
# tables always follow the checked-out tree.
ENUM_JAVA_SOURCES = {
    "color": ("runtime/UiOptions.java", "UseColor"),
    "subcommands": ("actions/ActionExecutionContext.java", "ShowSubcommands"),
    "test_output": ("exec/ExecutionOptions.java", "TestOutputFormat"),
    "compilation_mode": ("analysis/config/CompilationMode.java", "CompilationMode"),
    "strip": ("rules/cpp/CppConfiguration.java", "StripMode"),
    "skyframe": ("runtime/commands/DumpCommand.java", "SkyframeDumpOption"),
    "info_output_type": ("runtime/commands/info/InfoItemHandler.java", "InfoItemOutputType"),
    "experimental_convenience_symlinks": ("buildtool/BuildRequestOptions.java", "ConvenienceSymlinksMode"),
    "strict_proto_deps": ("analysis/config/CoreOptionConverters.java", "StrictDepsMode"),
    "order_output": ("query2/query/output/QueryOptions.java", "OrderOutput"),
}


def java_enum_constants(bazel_dir, rel_file, enum_name):
    path = os.path.join(bazel_dir, "src/main/java/com/google/devtools/build/lib", rel_file)
    with open(path, encoding="utf-8") as f:
        text = f.read()
    m = re.search(r"enum\s+" + enum_name + r"\s*(?:implements[^{]*)?\{(.*?)(?:;|\n  \}|\n\})", text, re.S)
    if not m:
        raise SystemExit("enum %s not found in %s" % (enum_name, path))
    body = re.sub(r"/\*.*?\*/|//[^\n]*", "", m.group(1), flags=re.S)
    body = re.sub(r"\([^)]*\)", "", body)
    return [x.strip().lower() for x in body.split(",") if x.strip()]


def derive_enum_values(bazel_dir):
    for flag, (rel, enum) in ENUM_JAVA_SOURCES.items():
        spec = ENUM_SPEC_OPTIONS[flag]
        vals = java_enum_constants(bazel_dir, rel, enum)
        ENUM_SPEC_OPTIONS[flag] = (spec[0], spec[1], vals, spec[3], spec[4])
    missing = set(ENUM_SPEC_OPTIONS) - set(ENUM_JAVA_SOURCES)
    if missing:
        raise SystemExit("ENUM_SPEC_OPTIONS entries without a Java enum source: %s" % sorted(missing))


def map_option_type(o):
    name = o["name"]
    if name in RESOURCE_SPECS:
        return RESOURCE_SPECS[name]
    if name in CUSTOM_CONVERTERS:
        return CUSTOM_CONVERTERS[name]
    if name in ENUM_SPEC_OPTIONS:
        return "OptionType::Enum(&%s)" % ENUM_SPEC_OPTIONS[name][0]
    conv = o["converter_simple"]
    ret_type = o["element_type"]
    allows_mult = o["allow_multiple"]
    if conv in ("Boolean", "BooleanConverter") or (conv is None and ret_type == "boolean"):
        return "OptionType::Boolean"
    if conv in ("TriState", "TriStateConverter") or (conv is None and ret_type == "TriState"):
        return "OptionType::TriState"
    if conv in ("Integer", "Int", "IntegerConverter") or (conv is None and ret_type == "int"):
        return "OptionType::Int"
    if conv in ("Long", "LongConverter") or (conv is None and ret_type == "long"):
        return "OptionType::Long"
    if conv in ("Double", "Float", "DoubleConverter") or (conv is None and ret_type in ("double", "float")):
        return "OptionType::Double"
    if conv in ("Duration", "DurationConverter") or (conv is None and ret_type == "Duration"):
        return "OptionType::Duration"
    if conv in ("ByteSize", "ByteSizeConverter"):
        return "OptionType::ByteSize"
    if conv in ("Label", "LabelConverter") or (conv is None and ret_type == "Label"):
        return "OptionType::ListLabel" if allows_mult else "OptionType::Label"
    if conv in ("Void", "VoidConverter") or (conv is None and ret_type == "Void"):
        return "OptionType::Void"
    if conv in ("String", "StringConverter") or (conv is None and ret_type == "String"):
        return "OptionType::ListString" if allows_mult else "OptionType::String"
    if conv:
        return 'OptionType::Custom("%s")' % conv
    if allows_mult:
        return "OptionType::ListString"
    return "OptionType::String"


# --------------------------------------------------------------------------------------------
# Main model construction
# --------------------------------------------------------------------------------------------

def build_index(bazel_src):
    java_root = os.path.join(bazel_src, "src/main/java")
    if not os.path.isdir(java_root):
        raise ValueError("Directory not found: " + java_root)
    idx = Index()
    wanted = re.compile(r"@Option\(|@Command\(|getCommandOptions|getCommonCommandOptions|getStartupOptions|"
                        r"RequiresOptions|ConfigurationFragment|getTypeDescription|BAZEL_MODULES|"
                        r"static final String [A-Z_]+ =|enum |class ")
    files = []
    for root, dirs, fs in os.walk(java_root):
        dirs.sort()
        for f in sorted(fs):
            if f.endswith(".java"):
                files.append(os.path.join(root, f))
    for p in files:
        parse_file(p, idx, lambda src: bool(wanted.search(src)))
    return idx


def collect(bazel_src):
    idx = build_index(bazel_src)
    unresolved = []

    for c in idx.classes:
        c.parsed_options = [convert_option(od, idx, unresolved) for od in c.options]

    def class_options(c):
        """Options visible in an options class: its own plus those of OptionsBase superclasses."""
        res = list(c.parsed_options)
        for ref in c.extends:
            sup = idx.resolve(type_ref(ref), c)
            if sup and (sup.parsed_options or sup.extends):
                res.extend(class_options(sup))
        return res

    cmd_classes = {}
    for c in idx.classes:
        if "Command" in c.annotations and "src/main/java" in c.path:
            am = arg_map(c.annotations["Command"])
            if "name" in am:
                cmd_classes[eval_str(am["name"], c, idx)] = c
    cmd_info = {}
    for name, c in cmd_classes.items():
        am = arg_map(c.annotations["Command"])
        cmd_info[name] = {
            "cls": c,
            "options": class_refs(am["options"], c, idx) if "options" in am else [],
            "inherits": class_refs(am["inheritsOptionsFrom"], c, idx) if "inheritsOptionsFrom" in am else [],
            "uses_config": "usesConfigurationOptions" in am and am["usesConfigurationOptions"][0][1] == "true",
            "hidden": "hidden" in am and am["hidden"][0][1] == "true",
        }
    cmd_by_class = {info["cls"].fqn: n for n, info in cmd_info.items()}

    bazel_cls = [c for c in idx.classes if c.name == "Bazel" and c.package.endswith("lib.bazel")][0]
    mods_tok = None
    for i, t in enumerate(bazel_cls.toks):
        if t[0] == "id" and t[1] == "BAZEL_MODULES":
            j = i
            while bazel_cls.toks[j][1] != "(":
                j += 1
            e = match_close(bazel_cls.toks, j, "(", ")")
            mods_tok = bazel_cls.toks[j + 1:e]
            break
    modules = []
    for part in split_top(mods_tok):
        chain = [t[1] for t in part if t[0] == "id" and t[1] != "class"]
        m = idx.by_fqn.get(".".join(chain))
        if m is None:
            m = idx.resolve(chain, bazel_cls)
        if m is None:
            raise ValueError("cannot resolve module " + ".".join(chain))
        modules.append(m)

    def module_method(m, mname):
        for c in walk_supers(m, idx):
            r = find_method_body(c, mname)
            if r and r[1] is not None:
                return c, r
        return None

    def method_classes(owner, body, command):
        """Option classes returned by a supplier method; handles `cmd.equals("x") ? A : B`."""
        q = [k for k, t in enumerate(body) if t[0] == "sym" and t[1] == "?"
             and not (k > 0 and body[k - 1][1] == "<") and not (k + 1 < len(body) and body[k + 1][1] in ("extends", ">", "super"))]
        seg = body
        if q:
            colon = [k for k, t in enumerate(body) if t[0] == "sym" and t[1] == ":" and k > q[0]][0]
            lit = [t[1] for t in body[:q[0]] if t[0] == "str"]
            seg = body[q[0] + 1:colon] if (command is not None and lit and lit[0] == command) else body[colon + 1:]
        refs = []
        for k in range(len(seg)):
            if seg[k][0] == "id" and seg[k][1] == "class" and k > 0 and seg[k - 1][1] == ".":
                chain = []
                j = k - 2
                while j >= 0 and seg[j][0] == "id":
                    chain.append(seg[j][1])
                    if j - 1 >= 0 and seg[j - 1][1] == ".":
                        j -= 2
                    else:
                        break
                chain.reverse()
                r = idx.resolve(chain, owner)
                if r is None:
                    raise ValueError("cannot resolve %s in module %s" % (chain, owner.fqn))
                refs.append(r)
        return refs

    def options_class_accessor(m):
        """`Class<Foo> optionsClass()` of a (subclass of a) generic module."""
        for c in walk_supers(m, idx):
            toks = c.toks
            for k in range(c.body_start, c.body_end):
                if toks[k][1] == "optionsClass" and toks[k + 1][1] == "(" and toks[k - 1][1] == ">":
                    ids = []
                    j = k - 2
                    while toks[j][1] != "<":
                        if toks[j][0] == "id":
                            ids.append(toks[j][1])
                        j -= 1
                    if ids and "abstract" not in [t[1] for t in toks[k - 6:k]]:
                        ref = idx.resolve([ids[0]], c)
                        if ref:
                            return ref
        return None

    def supplier(mname, command=None):
        res = []
        for m in modules:
            r = module_method(m, mname)
            if not r:
                continue
            owner, (_params, body) = r
            refs = method_classes(owner, body, command)
            if any(t[0] == "id" and t[1] == "optionsClass" for t in body):
                oc = options_class_accessor(m)
                if oc:
                    refs.append(oc)
            res.extend(refs)
        return res

    def by_simple(n):
        cands = [c for c in idx.by_simple[n] if "src/main/java" in c.path]
        return cands[0]

    startup_classes = [by_simple(n) for n in ("BlazeServerStartupOptions", "HostJvmStartupOptions")] + \
        supplier("getStartupOptions")
    # BlazeCommandUtils.COMMON_COMMAND_OPTIONS, read from the source (differs between releases).
    _bcu = open(os.path.join(bazel_src, "src/main/java/com/google/devtools/build/lib/runtime/BlazeCommandUtils.java")).read()
    _m = re.search(r"COMMON_COMMAND_OPTIONS\s*=\s*ImmutableList\.of\((.*?)\);", _bcu, re.S)
    _names = re.findall(r"(\w+)\.class", re.sub(r"//[^\n]*", "", _m.group(1)))
    common_classes = [by_simple(n) for n in _names] + supplier("getCommonCommandOptions")

    # fragment registry (BazelRuleClassProvider.RULE_SETS)
    rule_files = [c for c in idx.classes if any(k in c.path for k in (
        "/lib/bazel/rules/", "/lib/rules/core/", "/lib/rules/config/", "/lib/rules/platform/"))]
    seen_frag = set()
    frag_classes = []
    opts_added = []
    for c in rule_files:
        toks = c.toks
        for k in range(c.body_start, min(c.body_end, len(toks))):
            if toks[k][0] == "id" and toks[k][1] in ("addConfigurationFragment", "addUniversalConfigurationFragment",
                                                      "addConfigurationOptions") and toks[k + 1][1] == "(":
                e = match_close(toks, k + 1, "(", ")")
                chain = [t[1] for t in toks[k + 2:e] if t[0] == "id" and t[1] != "class"]
                ref = idx.resolve(chain, c)
                if ref is None:
                    raise ValueError("cannot resolve fragment " + ".".join(chain))
                if toks[k][1] == "addConfigurationOptions":
                    opts_added.append(ref)
                elif ref.fqn not in seen_frag:
                    seen_frag.add(ref.fqn)
                    frag_classes.append(ref)
    fragment_option_classes = []
    for f in frag_classes:
        ro = f.annotations.get("RequiresOptions")
        if ro:
            am = arg_map(ro)
            if "options" in am:
                fragment_option_classes.extend(class_refs(am["options"], f, idx))
    fragment_option_classes.extend(opts_added)
    fragment_option_classes = list({c.fqn: c for c in fragment_option_classes}.values())

    def command_classes(name):
        info = cmd_info[name]
        res = list(common_classes) + list(info["options"])
        if info["uses_config"]:
            res.extend(fragment_option_classes)
        res.extend(supplier("getCommandOptions", name))
        for base in info["inherits"]:
            res.extend(command_classes(cmd_by_class[base.fqn]))
        return list({c.fqn: c for c in res}.values())

    model = Model()
    model.idx = idx
    model.unresolved = unresolved
    model.cmd_info = cmd_info
    model.startup_classes = list({c.fqn: c for c in startup_classes}.values())
    model.common_classes = list({c.fqn: c for c in common_classes}.values())
    model.fragment_option_classes = fragment_option_classes
    model.command_classes = {n: command_classes(n) for n in cmd_info}
    model.class_options = class_options
    model.modules = modules
    model.supplier_command_options = {n: supplier("getCommandOptions", n) for n in cmd_info}
    return model


# --------------------------------------------------------------------------------------------
# Rust writer
# --------------------------------------------------------------------------------------------

HEADER = """// Copyright 2026 The Bazel Authors. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Static flag schema tables extracted directly from the Bazel Java source tree.
//!
//! GENERATED by `scripts/extract_bazel_options.py` -- do not edit by hand
//! (`scripts/check_flag_tables.sh` verifies this file is byte-identical to the generator output).

use crate::definition::OptionDefinition;
use crate::tags::{OptionDocumentationCategory, OptionEffectTag, OptionMetadataTag};
use crate::types::{EnumSpec, OptionType};
use std::collections::HashMap;
use zel_util::resource_converter::IntegerSpec;

"""

FOOTER = """
/// Returns a map from option name (and old names) to option definition. When several options classes declare
/// the same name (for different commands), the first declaration wins; use [`is_command_allowed`] to pick the
/// one that applies to a command.
pub fn get_command_options_map() -> HashMap<&'static str, &'static OptionDefinition> {
    let mut map = HashMap::with_capacity(ALL_COMMAND_OPTIONS.len() * 2);
    for def in ALL_COMMAND_OPTIONS {
        map.entry(def.name).or_insert(def);
        if let Some(old) = def.old_name {
            map.entry(old).or_insert(def);
        }
    }
    map
}

/// Returns a map from startup option name to option definition.
pub fn get_startup_options_map() -> HashMap<&'static str, &'static OptionDefinition> {
    let mut map = HashMap::with_capacity(ALL_STARTUP_OPTIONS.len() * 2);
    for def in ALL_STARTUP_OPTIONS {
        map.insert(def.name, def);
    }
    map
}

/// Returns abbreviation map: abbrev char -> option definition.
pub fn get_command_abbrev_map() -> HashMap<char, &'static OptionDefinition> {
    let mut map = HashMap::new();
    for def in ALL_COMMAND_OPTIONS {
        if let Some(c) = def.abbrev {
            map.entry(c).or_insert(def);
        }
    }
    map
}

/// Checks if an option definition applies to a given command (the declaring options class is part of the
/// command's option set, `BlazeCommandUtils.getOptions`).
pub fn is_command_allowed(command: &str, def: &OptionDefinition) -> bool {
    def.commands.contains(&command)
}

/// Legacy helper for allowed classes; options now track commands directly.
pub fn get_command_allowed_classes(_command: &str) -> Option<std::collections::HashSet<&'static str>> {
    None
}
"""


def rust_str_slice(items):
    return "&[" + ", ".join('"%s"' % escape_rust_string(x) for x in items) + "]"


def finish_option(o, idx):
    ci = converter_info(o["converter_cls"], idx)
    eff = o["effective_converter"]
    et = o["element_type"]
    if not o["allow_multiple"] and o["ret_type"] in ("boolean", "TriState"):
        ci["boolean_syntax"] = True  # `getType()` is boolean.class / TriState.class
    o["conv_info"] = ci
    # HelpCommand.createFlagInfo: the converter's simple class name when it ends with "Converter"
    # (a class literally named `Converter` yields the empty string); otherwise unset.
    o["type_converter"] = eff[: -len("Converter")] if eff and eff.endswith("Converter") else None
    o["boolean_syntax"] = ci["boolean_syntax"]


def emit_option(out, o, commands):
    abbrev = "Some('%s')" % o["abbrev"] if o["abbrev"] else "None"
    dv = o["default_value"]
    special_null = dv == "null" and o["element_type"] not in ("boolean", "int", "long", "double")
    dv_str = 'Some("%s")' % escape_rust_string(dv) if dv is not None and not special_null else "None"
    exp = o["expansion"]
    exp_str = "Some(&[%s])" % ", ".join('"%s"' % escape_rust_string(e) for e in exp) if exp else "None"
    old = 'Some("%s")' % escape_rust_string(o["old_name"]) if o["old_name"] else "None"
    dep = 'Some("%s")' % escape_rust_string(o["deprecation_warning"]) if o["deprecation_warning"] else "None"
    ci = o["conv_info"]
    cls = o["declaring"]
    out.write("    OptionDefinition::new(\n")
    out.write('        "%s",\n' % escape_rust_string(o["name"]))
    out.write("        %s,\n" % abbrev)
    out.write("        %s,\n" % map_option_type(o))
    out.write("        %s,\n" % dv_str)
    out.write("        %s,\n" % ("true" if o["allow_multiple"] else "false"))
    out.write("        %s,\n" % exp_str)
    out.write("        %s,\n" % rust_str_slice(o["implicit_requirements"]))
    out.write("        %s,\n" % old)
    out.write("        %s,\n" % dep)
    out.write('        "",\n')
    out.write("        %s,\n" % map_doc_category(o["documentation_category"]))
    out.write("        &[%s],\n" % ", ".join(map_effect_tag(t) for t in o["effect_tags"]))
    out.write("        &[%s],\n" % ", ".join(map_metadata_tag(t) for t in o["metadata_tags"]))
    out.write('        "%s",\n' % escape_rust_string(o["help"]))
    out.write('        "%s",\n' % escape_rust_string(cls.binary_name))
    out.write("        %s,\n" % ('Some("%s")' % escape_rust_string(ci["type_description"]) if ci["type_description"] is not None else "None"))
    out.write("        %s,\n" % rust_str_slice(commands))
    out.write("        %s,\n" % ('Some("%s")' % escape_rust_string(o["type_converter"]) if o["type_converter"] is not None else "None"))
    out.write("        %s,\n" % rust_str_slice(ci["enum_values"] or []))
    out.write("        %s,\n" % ("true" if o["boolean_syntax"] else "false"))
    out.write('        "%s",\n' % escape_rust_string(o["value_help"]))
    out.write('        "%s",\n' % escape_rust_string(cls.java_class))
    out.write("    ),\n")


def emit_flags_json(model, sorted_cmds, path):
    """`HelpCommand.emitFlagsAsProtoHelp`: non-internal options of the startup options and of every command
    (sorted by name), merged by option name; the first visit creates the FlagInfo."""
    import json
    flags = {}
    order = []

    def visit(o, command):
        if "INTERNAL" in o["metadata_tags"]:
            return
        f = flags.get(o["name"])
        if f is None:
            dv = o["default_value"]
            special_null = dv == "null" and o["element_type"] not in ("boolean", "int", "long", "double")
            ci = o["conv_info"]
            f = {
                "name": o["name"],
                "has_negative_flag": "true" if o["boolean_syntax"] else "false",
                "documentation": o["help"],
                "commands": [],
                "allows_multiple": "true" if o["allow_multiple"] else "false",
                "effect_tags": list(o["effect_tags"]),
                "metadata_tags": list(o["metadata_tags"]),
                "documentation_category": o["documentation_category"],
                "requires_value": "false" if (o["boolean_syntax"] or o["ret_type"] == "Void") else "true",
            }
            if o["abbrev"]:
                f["abbreviation"] = o["abbrev"]
            if o["old_name"]:
                f["old_name"] = o["old_name"]
            if o["deprecation_warning"]:
                f["deprecation_warning"] = o["deprecation_warning"]
            if dv is not None and not special_null:
                f["default_value"] = dv
            if o["expansion"]:
                f["option_expansions"] = list(o["expansion"])
            if o["type_converter"] is not None:
                f["type_converter"] = o["type_converter"]
            if ci["enum_values"]:
                f["enum_values"] = list(ci["enum_values"])
            flags[o["name"]] = f
            order.append(o["name"])
        f["commands"].append(command)

    seen = set()
    startup = []
    for c in model.startup_classes:
        for o in model.class_options(c):
            if (o["declaring"].fqn, o["name"]) not in seen:
                seen.add((o["declaring"].fqn, o["name"]))
                startup.append(o)
    for o in sorted(startup, key=lambda o: o["name"]):
        visit(o, "startup")
    for cname in sorted_cmds:
        opts = {}
        for c in model.command_classes[cname]:
            for o in model.class_options(c):
                opts.setdefault((o["declaring"].fqn, o["name"]), o)
        for o in sorted(opts.values(), key=lambda o: o["name"]):
            visit(o, cname)
    with open(path, "w") as fh:
        json.dump([flags[n] for n in order], fh)


def cmd_name_of(model, cls):
    for n, info in model.cmd_info.items():
        if info["cls"].fqn == cls.fqn:
            return n
    raise ValueError("no command for class " + cls.fqn)


def main():
    parser = argparse.ArgumentParser(description="Extract Bazel option tables from Java source")
    parser.add_argument("--bazel-dir", default=os.environ.get("BAZEL_SRC_DIR", os.path.expanduser("~/Development/bazel")),
                        help="Path to Bazel source tree")
    parser.add_argument("--out", default=os.environ.get("OUT_FILE"),
                        help="Path to generated flag_tables.rs output file")
    parser.add_argument("--emit-flags-json", default=None,
                        help="Instead of flag_tables.rs, write the `help flags-as-proto` model (one record per "
                             "flag, HelpCommand.emitFlagsAsProtoHelp semantics) as JSON to this path")
    args = parser.parse_args()

    bazel_src = os.path.abspath(args.bazel_dir)
    if not os.path.isdir(bazel_src):
        print("Error: Bazel source tree not found at %s" % bazel_src, file=sys.stderr)
        sys.exit(1)
    repo_root = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
    out_file = os.path.abspath(args.out) if args.out else os.path.join(
        repo_root, "crates/zel-options/src/flag_tables.rs")

    print("Scanning Java options in %s..." % bazel_src)
    model = collect(bazel_src)
    idx = model.idx
    if model.unresolved:
        for u in model.unresolved[:50]:
            print("  unresolved: %s.%s %s: %s" % u, file=sys.stderr)
        print("Warning: %d unresolved option attribute expressions" % len(model.unresolved), file=sys.stderr)

    for c in idx.classes:
        for o in c.parsed_options:
            finish_option(o, idx)

    sorted_cmds = sorted(model.cmd_info)

    if args.emit_flags_json:
        emit_flags_json(model, sorted_cmds, args.emit_flags_json)
        return

    seen = {}
    order = []
    for cname in sorted_cmds:
        for c in model.command_classes[cname]:
            for o in model.class_options(c):
                key = (o["declaring"].fqn, o["name"])
                if key not in seen:
                    seen[key] = (o, [])
                    order.append(key)
                if cname not in seen[key][1]:
                    seen[key][1].append(cname)
    order.sort(key=lambda k: (k[1], k[0]))
    cmd_entries = [seen[k] for k in order]

    startup_entries = []
    seen_s = set()
    for c in model.startup_classes:
        for o in model.class_options(c):
            key = (o["declaring"].fqn, o["name"])
            if key not in seen_s:
                seen_s.add(key)
                startup_entries.append(o)
    startup_entries.sort(key=lambda o: o["name"])

    print("Parsed %d command options and %d startup options from Bazel source." % (
        len(cmd_entries), len(startup_entries)))
    print("Parsed %d options." % len({e[0]["name"] for e in cmd_entries} | {o["name"] for o in startup_entries}))

    derive_enum_values(bazel_src)
    enum_specs = sorted(ENUM_SPEC_OPTIONS.values(), key=lambda x: x[0])

    print("Writing %s..." % out_file)
    with open(out_file, "w", encoding="utf-8") as out:
        out.write(HEADER)
        out.write("// Static EnumSpecs\n")
        for spec_const, type_name, vals, tv, fv in enum_specs:
            out.write("static %s: EnumSpec = EnumSpec {\n" % spec_const)
            out.write('    type_name: "%s",\n' % escape_rust_string(type_name))
            out.write("    values: &[%s],\n" % ", ".join('"%s"' % v for v in vals))
            out.write("    true_value: %s,\n" % ('Some("%s")' % tv if tv else "None"))
            out.write("    false_value: %s,\n" % ('Some("%s")' % fv if fv else "None"))
            out.write("};\n")

        out.write("\n/// Every command option declared by an options class used by at least one command; `commands` lists the\n"
                  "/// commands whose option set (`BlazeCommandUtils.getOptions`) contains the declaring options class.\n")
        out.write("pub static ALL_COMMAND_OPTIONS: &[OptionDefinition] = &[\n")
        for o, cmds in cmd_entries:
            emit_option(out, o, cmds)
        out.write("];\n\n")

        out.write("/// Startup options (`BlazeCommandUtils.getStartupOptions`).\n")
        out.write("pub static ALL_STARTUP_OPTIONS: &[OptionDefinition] = &[\n")
        for o in startup_entries:
            emit_option(out, o, ["startup"])
        out.write("];\n\n")

        out.write("/// `@Command` wiring: `(name, inheritsOptionsFrom, usesConfigurationOptions, hidden)`.\n")
        out.write("pub static COMMAND_INHERITANCE: &[(&str, &[&str], bool, bool)] = &[\n")
        for cname in sorted_cmds:
            info = model.cmd_info[cname]
            inh = [cmd_name_of(model, b) for b in info["inherits"]]
            out.write('    ("%s", %s, %s, %s),\n' % (cname, rust_str_slice(inh),
                                                    "true" if info["uses_config"] else "false",
                                                    "true" if info["hidden"] else "false"))
        out.write("];\n\n")

        out.write("/// Declaring options classes (fully qualified binary names, `OptionDefinition::java_class`) per command: `(command, direct classes, all classes)`. `direct` is\n"
                  "/// `@Command(options)` + module `getCommandOptions(command)` (what `everything-as-html` lists), `all` is\n"
                  "/// `BlazeCommandUtils.getOptions`.\n")
        out.write("pub static COMMAND_OPTION_CLASSES: &[(&str, &[&str], &[&str])] = &[\n")
        for cname in sorted_cmds:
            info = model.cmd_info[cname]
            # `HelpCommand` builds an OptionsParser from the direct classes, which also exposes the options
            # declared by their superclasses (e.g. `AqueryOptions extends CommonQueryOptions`).
            direct_classes = info["options"] + model.supplier_command_options[cname]
            direct = list(dict.fromkeys(
                [c.java_class for c in direct_classes]
                + [oc["declaring"].java_class for c in direct_classes for oc in model.class_options(c)]))
            allc = list(dict.fromkeys(oc["declaring"].java_class for c in model.command_classes[cname]
                                      for oc in model.class_options(c)))
            out.write('    ("%s", %s, %s),\n' % (cname, rust_str_slice(direct), rust_str_slice(allc)))
        out.write("];\n\n")
        out.write("/// Options classes common to all commands (`BlazeCommandUtils.getCommonOptions`).\n")
        out.write("pub static COMMON_OPTION_CLASSES: &[&str] = %s;\n\n" % rust_str_slice(
            [c.java_class for c in model.common_classes]))
        out.write("/// Options classes of the fragment registry (used by commands with `usesConfigurationOptions`).\n")
        out.write("pub static FRAGMENT_OPTION_CLASSES: &[&str] = %s;\n" % rust_str_slice(
            [c.java_class for c in model.fragment_option_classes]))
        out.write(FOOTER)
    print("Done generating flag_tables.rs")


if __name__ == "__main__":
    main()
