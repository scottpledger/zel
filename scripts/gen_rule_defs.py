#!/usr/bin/env python3
"""Derives crates/zel-packages/src/rule_defs_gen.rs from the Bazel *source tree*.

No Bazel binary is ever run, neither at build time nor to generate the table: the attribute schema
of every native rule class is computed by interpreting the Java rule definitions of the Bazel 9.3.2
sources the way `RuleClass.Builder` does:

  * `RuleDefinition` classes (`getMetadata()`: name + ancestors; `build()`: the chain of
    `.add(attr(NAME, TYPE)....)`, `.addAttribute`, `.override`, `.removeAttribute`, inlined helper
    methods such as `commonCoreAndStarlarkAttributes(builder)`),
  * the rule sets that register them (`GenericRules`, `CcRules`, `JavaRules`, `ObjcRules`,
    `ToolchainRules`, `CoreRules`, `ConfigRules`, `PlatformRules`, `GenQueryRule.register`),
  * `BaseRuleClasses` (`NativeBuildRule`, `NativeActionCreatingRule`, `MakeVariableExpandingRule`,
    `EmptyRule`), `LateBoundAlias` (`CommonAliasRule`/`AbstractAliasRule`), `RuleClass.Builder`
    (parent attribute merging order, `name` first, implicit `testonly` of test rules, the
    `build_setting_default`/`help` attributes of build settings),
  * `StarlarkRuleClassFunctions` (`baseRule`, `binaryBaseRule`, `getTestBaseRule`) for the four
    flavours of Starlark `rule()`,
  * `Attribute.Builder` semantics: `nonconfigurable`, `orderIndependent`, `mandatory`, `tool`,
    `cfg(ExecutionTransitionFactory|NoConfigTransition)` (`Attribute.isToolDependency`),
  * `ProtoUtils.TYPE_MAP` for the proto discriminator of each attribute type.

Usage:
  scripts/gen_rule_defs.py [--src ~/Development/bazel] [--out FILE] [--check]

`--check` regenerates in memory and fails if the checked-in file differs (see
scripts/check_rule_defs.sh). The Starlark-defined rules of `@bazel_tools` and `@rules_*` are loaded
as ordinary Starlark rules by the loader (their attributes come from their `.bzl` definitions) and
need no table. Comparing the table against a real Bazel binary is a *separate* verification step:
scripts/verify_rule_defs_vs_bazel.py.
"""
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
SRC = os.path.expanduser('~/Development/bazel')
OUT = os.path.join(ROOT, 'crates/zel-packages/src/rule_defs_gen.rs')
CHECK = False
a = sys.argv[1:]
while a:
    x = a.pop(0)
    if x == '--src':
        SRC = os.path.expanduser(a.pop(0))
    elif x == '--out':
        OUT = a.pop(0)
    elif x == '--check':
        CHECK = True
LIB = os.path.join(SRC, 'src/main/java/com/google/devtools/build/lib')


# ------------------------------------------------------------------------------------------
# Java reading helpers
# ------------------------------------------------------------------------------------------
def strip_comments(s):
    out = []
    i = 0
    n = len(s)
    while i < n:
        c = s[i]
        if s.startswith('"""', i):
            j = s.index('"""', i + 3) + 3
            out.append(s[i:j])
            i = j
        elif c == '"' or c == "'":
            j = i + 1
            while s[j] != c:
                j += 2 if s[j] == '\\' else 1
            out.append(s[i:j + 1])
            i = j + 1
        elif s.startswith('//', i):
            j = s.find('\n', i)
            i = n if j < 0 else j
        elif s.startswith('/*', i):
            i = s.index('*/', i + 2) + 2
        else:
            out.append(c)
            i += 1
    return ''.join(out)


def read(rel):
    with open(os.path.join(LIB, rel)) as f:
        return strip_comments(f.read())


def balanced(s, i, open_c='(', close_c=')'):
    """s[i] == open_c; returns index just after the matching close."""
    assert s[i] == open_c, (s[i - 20:i + 20])
    depth = 0
    n = len(s)
    while i < n:
        c = s[i]
        if c == '"':
            i += 1
            while s[i] != '"':
                i += 2 if s[i] == '\\' else 1
        elif c == open_c:
            depth += 1
        elif c == close_c:
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    raise ValueError('unbalanced')


def split_args(s):
    args, depth, cur, i = [], 0, [], 0
    while i < len(s):
        c = s[i]
        if c == '"':
            j = i + 1
            while s[j] != '"':
                j += 2 if s[j] == '\\' else 1
            cur.append(s[i:j + 1])
            i = j + 1
            continue
        if c in '([{<' and not (c == '<' and False):
            depth += 1 if c != '<' else 0
        if c in ')]}':
            depth -= 1
        if c == ',' and depth == 0:
            args.append(''.join(cur).strip())
            cur = []
        else:
            cur.append(c)
        i += 1
    last = ''.join(cur).strip()
    if last:
        args.append(last)
    return args


def class_bodies(src):
    """{simple class name: (extends, implements, body)} for every (nested) class in `src`."""
    out = {}
    for m in re.finditer(r'\bclass\s+(\w+)(?:<[^{]*?>)?(?:\s+extends\s+([\w.]+)(?:<[^{]*?>)?)?'
                         r'(?:\s+implements\s+([\w.,\s<>]+?))?\s*\{', src):
        j = balanced(src, m.end() - 1, '{', '}')
        out[m.group(1)] = (m.group(2), m.group(3) or '', src[m.end():j - 1])
    return out


def method_body(body, name, with_args=False):
    m = re.search(r'\b' + re.escape(name) + r'\s*\(', body)
    if not m:
        return None
    j = balanced(body, m.end() - 1)
    k = body.index('{', j)
    return body[k + 1:balanced(body, k, '{', '}') - 1]


# ------------------------------------------------------------------------------------------
# Constants / types
# ------------------------------------------------------------------------------------------
CONSTS = {}
CUR_SRC = ['']   # source text of the Java file currently interpreted
SCOPE = {}   # string constants of the Java source currently interpreted (shadow CONSTS)


def const(name):
    n = name.split('.')[-1]
    return SCOPE[n] if n in SCOPE else CONSTS[n]


def file_consts(src):
    d = {}
    for m in re.finditer(r'String\s+(\w+)\s*=\s*((?:"(?:[^"\\]|\\.)*"\s*\+?\s*)+);', src):
        d[m.group(1)] = ''.join(qs(x) for x in re.findall(r'"(?:[^"\\]|\\.)*"', m.group(2)))
    return d


def load_consts():
    for rel in ['packages/RuleClass.java', 'analysis/BaseRuleClasses.java', 'rules/Alias.java',
                'rules/LabelBuildSettings.java', 'rules/LateBoundAlias.java']:
        t = read(rel)
        for m in re.finditer(r'static\s+final\s+String\s+(\w+)\s*=\s*"((?:[^"\\]|\\.)*)"', t):
            CONSTS[m.group(1)] = m.group(2)
        for m in re.finditer(r'static\s+final\s+String\s+(\w+)\s*=\s*(\w+(?:\.\w+)*)\s*;', t):
            if m.group(2).split('.')[-1] in CONSTS:
                CONSTS[m.group(1)] = CONSTS[m.group(2).split('.')[-1]]
    # every other `String NAME = "literal"` constant of the rule definitions and their helpers
    for d in ('rules', 'analysis/constraints', 'bazel/rules'):
        for dp, _dn, fns in os.walk(os.path.join(LIB, d)):
            for fn in sorted(fns):
                if fn.endswith('.java'):
                    t = read(os.path.relpath(os.path.join(dp, fn), LIB))
                    for m in re.finditer(r'String\s+(\w+)\s*=\s*"((?:[^"\\]|\\.)*)"', t):
                        CONSTS.setdefault(m.group(1), m.group(2))
    for rel in ('packages/Rule.java', 'packages/RuleClass.java'):
        t = read(rel)
        for m in re.finditer(r'String\s+(\w+)\s*=\s*"((?:[^"\\]|\\.)*)"', t):
            CONSTS[m.group(1)] = m.group(2)


def types_from_source():
    """Parses ProtoUtils.TYPE_MAP (proto discriminators) + BuildType's LabelClass of every label type."""
    pu = read('packages/ProtoUtils.java')
    disc = {}
    for m in re.finditer(r'\.put\((\w+),\s*Discriminator\.(\w+)\)', pu):
        disc[m.group(1)] = m.group(2)
    bt = read('packages/BuildType.java')
    lclass = {}
    for m in re.finditer(r'Type<Label>\s+(\w+)\s*=\s*new\s+LabelType\(LabelClass\.(\w+)\)', bt):
        lclass[m.group(1)] = m.group(2)
    for m in re.finditer(r'ListType<Label>\s+(\w+)\s*=\s*ListType\.create\((\w+)\)', bt):
        lclass[m.group(1)] = lclass.get(m.group(2), 'DEPENDENCY')
    lclass['LABEL'] = 'DEPENDENCY'
    for n in ('LABEL_LIST', 'LABEL_DICT_UNARY', 'LABEL_KEYED_STRING_DICT', 'LABEL_LIST_DICT'):
        lclass[n] = 'DEPENDENCY'
    lclass['OUTPUT'] = lclass['OUTPUT_LIST'] = 'OUTPUT'
    # Type.STRING_NO_INTERN etc. share the discriminator of STRING
    return disc, lclass


DISC, LCLASS = {}, {}
NODEP_SERIALIZED = set()   # Java type constants for which AttributeFormatter sets `Attribute.nodep`


def nodep_serialized_from_source():
    t = read('packages/AttributeFormatter.java')
    out = set()
    for var in ('depTypes', 'noDepTypes'):
        m = re.search(var + r'\s*=\s*ImmutableSet\.of\((.*?)\);', t, re.S)
        out.update(x.strip().split('.')[-1] for x in m.group(1).split(',') if x.strip())
    return out


class Attr:
    def __init__(self, name, jtype):
        self.name = name
        self.jtype = jtype
        self.ty = DISC[jtype]
        # `nodep` of the proto output: attribute type is NODEP_LABEL / NODEP_LABEL_LIST
        self.nodep = LCLASS.get(jtype) == 'NONDEP_REFERENCE'
        self.is_dep = LCLASS.get(jtype) == 'DEPENDENCY'
        self.configurable = True
        self.ordered = False
        self.mandatory = False
        self.tool = False
        self.cfg = None
        self.default = ('none',)
        self.default_src = None

    def copy(self):
        o = Attr.__new__(Attr)
        o.__dict__.update(self.__dict__)
        return o

    def is_tool_dependency(self):
        # Attribute.isToolDependency(): label class DEPENDENCY && (IS_TOOL_DEPENDENCY flag ||
        # transitionFactory.isTool()) (ExecutionTransitionFactory and NoConfigTransition: true).
        return self.is_dep and (self.tool or self.cfg in ('exec', 'noconfig'))


def qs(s):
    return bytes(s[1:-1], 'utf8').decode('unicode_escape')


def eval_value(expr, a):
    """The static default of `.value(expr)`, or ('none',) for computed/late-bound defaults."""
    e = expr.strip()
    if re.fullmatch(r'"(?:[^"\\]|\\.)*"', e):
        return ('str', qs(e))
    if e in ('true', 'false'):
        return ('bool', e == 'true')
    if re.fullmatch(r'[A-Z][A-Z_0-9]*', e) and (e in SCOPE or e in CONSTS):
        return ('str', const(e)) if a.ty == 'STRING' else ('none',)
    if re.fullmatch(r'[A-Z][A-Z_0-9]*', e):
        # `LabelLateBoundDefault.fromTargetConfigurationWithRuleBasedDefault(.., rule ->
        # RawAttributeMapper.of(rule).get(<attr>, ..), ..)`: query evaluates the rule-based default,
        # which is the raw value of another attribute.
        m = re.search(r'\b' + e + r'\s*=\s*LabelLateBoundDefault\s*\.\s*fromTargetConfigurationWithRuleBasedDefault\s*\(', CUR_SRC[0])
        if m:
            end = balanced(CUR_SRC[0], m.end() - 1)
            g = re.search(r'RawAttributeMapper\.of\(\w+\)\s*\.get\(\s*(\w+)\s*,', CUR_SRC[0][m.end():end])
            if g:
                return ('rulebased', const(g.group(1)))
    m = re.fullmatch(r'StarlarkInt\.of\((-?\d+)\)', e)
    if m:
        return ('int', int(m.group(1)))
    if re.fullmatch(r'ImmutableList\.of\(\)|ImmutableMap\.of\(\)|ImmutableSet\.of\(\)', e):
        return ('none',)
    m = re.fullmatch(r'ImmutableList\.of\(((?:"(?:[^"\\]|\\.)*"\s*,?\s*)+)\)', e)
    if m:
        return ('list', [qs(x) for x in split_args(m.group(1))])
    m = re.fullmatch(r'(?:Label\.parseCanonical(?:Unchecked)?)\(("(?:[^"\\]|\\.)*")\)', e)
    if m:
        return ('label', qs(m.group(1)))
    m = re.fullmatch(r'(?:labelCache\.get\()?toolsRepository\s*\+\s*("(?:[^"\\]|\\.)*"|[\w.]+)\)?', e)
    if m:
        v = m.group(1)
        v = qs(v) if v.startswith('"') else const(v)
        return ('label', '@bazel_tools' + v)
    m = re.fullmatch(r'(?:ruleDefinitionEnvironment|env|environment)\.getToolsLabel\(("(?:[^"\\]|\\.)*"|[\w.]+)\)', e)
    if m:
        v = m.group(1)
        return ('label', '@bazel_tools' + (qs(v) if v.startswith('"') else const(v)))
    m = re.fullmatch(r'TriState\.(NO|YES|AUTO)', e)
    if m:
        return ('tristate', {'NO': 0, 'YES': 1, 'AUTO': -1}[m.group(1)])
    m = re.search(r'toolsRepository\s*\+\s*("(?:[^"\\]|\\.)*"|[\w.]+)', e)
    if m and ('coverage' in e.lower()):
        v = m.group(1)
        return ('label', '@bazel_tools' + (qs(v) if v.startswith('"') else const(v)))
    return ('none',)


def helper_head(expr):
    """Attribute-builder helper calls: `Allowlist.getAttributeFromAllowlistName(N)` and
    `X.getAllowlistAttribute(env)` (returns `<that>.value(env.getToolsLabel(CONST))`)."""
    m = re.match(r'(?:\w+\.)?(getAttributeFromAllowlistName|getAllowlistAttribute)\s*\(', expr)
    if not m:
        return None
    j = balanced(expr, m.end() - 1)
    args = split_args(expr[m.end():j - 1])
    if m.group(1) == 'getAttributeFromAllowlistName':
        al = read('analysis/Allowlist.java')
        fm = re.search(r'getAttributeNameFromAllowlistName\(String allowlistName\)\s*\{(.*?)\n  \}', al, re.S)
        fmt = re.search(r'String\.format\("([^"]+)"', fm.group(1)).group(1)   # iterator().next()
        an = args[0]
        an = qs(an) if an.startswith('"') else const(an)
        at = Attr(fmt % an, 'LABEL')
        at.cfg = 'exec'
        return at, j, an
    # getAllowlistAttribute(env): ConfigFeatureFlag
    t = read('rules/config/ConfigFeatureFlag.java')
    mb = re.search(r'getAllowlistAttribute\(RuleDefinitionEnvironment env\)\s*\{(.*?)\n  \}', t, re.S).group(1)
    nm = re.search(r'getAttributeFromAllowlistName\((\w+)\)', mb).group(1)
    consts = dict(re.findall(r'String\s+(\w+)\s*=\s*\s*"([^"]+)"', t))
    at, _j, _ = helper_head(mb.strip()[len('return '):])
    lab = re.search(r'getToolsLabel\((\w+)\)', mb).group(1)
    at.default = ('label', '@bazel_tools' + consts[lab])
    return at, j, nm


def parse_attr_chain(expr):
    """`attr(NAME, TYPE).m1(..).m2(..)` -> Attr (or None for non-attr expressions)."""
    expr = expr.strip()
    hh = helper_head(expr)
    if hh:
        at, j, _ = hh
        return chain(expr, j, at)
    m = re.match(r'(?:Attribute\.)?attr\s*\(', expr)
    if not m:
        return None
    j = balanced(expr, m.end() - 1)
    args = split_args(expr[m.end():j - 1])
    name = args[0]
    name = qs(name) if name.startswith('"') else const(name)
    jtype = args[1].split('.')[-1]
    return chain(expr, j, Attr(name, jtype))


def chain(expr, j, at):
    pos = j
    while pos < len(expr):
        m = re.match(r'\s*\.\s*(\w+)\s*(?=\()', expr[pos:])
        if not m:
            break
        k = pos + m.end()
        e = balanced(expr, k)
        meth, arg = m.group(1), expr[k + 1:e - 1]
        pos = e
        if meth == 'nonconfigurable':
            at.configurable = False
        elif meth == 'orderIndependent':
            at.ordered = True
        elif meth == 'mandatory':
            at.mandatory = True
        elif meth == 'tool':
            at.tool = True
        elif meth == 'cfg':
            if 'ExecutionTransitionFactory' in arg:
                at.cfg = 'exec'
            elif 'NoConfigTransition' in arg:
                at.cfg = 'noconfig'
            else:
                at.cfg = 'other'
        elif meth == 'value':
            at.default = eval_value(arg, at)
            at.default_src = arg.strip()
    return at


# ------------------------------------------------------------------------------------------
# Rule definition interpretation
# ------------------------------------------------------------------------------------------
class RuleDef:
    def __init__(self, name, ancestors, ops, is_test=False, abstract=False, build_setting=None):
        self.name, self.ancestors, self.ops = name, ancestors, ops
        self.is_test, self.abstract = is_test, abstract
        self.build_setting = build_setting


FILES = {}  # simple class name -> (file, extends, implements, body, whole source)


def index_classes(rels):
    for rel in rels:
        src = read(rel)
        for n, (ext, impl, body) in class_bodies(src).items():
            FILES[n] = (rel, ext, impl, body, src)


HELPERS = {}  # method name -> body (static helpers taking a RuleClass.Builder)


def index_helpers(rel):
    src = read(rel)
    for m in re.finditer(r'(?:public|private)?\s*static\s+RuleClass(?:\.Builder)?\s+(\w+)\s*\(', src):
        j = balanced(src, m.end() - 1)
        k = src.index('{', j)
        HELPERS[m.group(1)] = src[k + 1:balanced(src, k, '{', '}') - 1]


def interpret_build(body, rel_helpers=True, test=False, cond_test_blocks=True):
    """The ordered ops of a `build()` method body: ('add'|'override', Attr) / ('remove', name)."""
    ops = []
    # blocks guarded by `builder.getType() == RuleClassType.TEST`
    guarded = []
    for m in re.finditer(r'if\s*\(\s*builder\.getType\(\)\s*==\s*RuleClassType\.TEST\s*\)\s*\{', body):
        e = balanced(body, m.end() - 1, '{', '}')
        guarded.append((m.start(), e))
    tokens = []
    for m in re.finditer(r'\.\s*(add|addAttribute|override|removeAttribute|exemptFromConstraintChecking)\s*\(|\b(\w+)\s*\(\s*builder\b', body):
        tokens.append(m)
    consumed_until = -1
    for m in tokens:
        if m.start() < consumed_until:
            continue
        in_guard = [g for g in guarded if g[0] <= m.start() < g[1]]
        if in_guard and not test:
            continue
        if m.group(1):
            k = m.end() - 1
            e = balanced(body, k)
            arg = body[k + 1:e - 1]
            consumed_until = e
            if m.group(1) == 'exemptFromConstraintChecking':
                # RuleClass.Builder.exemptFromConstraintChecking
                ops.append(('exempt', None))
            elif m.group(1) == 'removeAttribute':
                nm = arg.strip()
                ops.append(('remove', qs(nm) if nm.startswith('"') else CONSTS[nm.split('.')[-1]]))
            else:
                arg = arg.strip()
                arg = re.sub(r'\.build\(\)\s*$', '', arg)
                at = parse_attr_chain(arg)
                if at is None:
                    raise ValueError('cannot parse attribute: ' + arg[:120])
                ops.append(('override' if m.group(1) == 'override' else 'add', at))
        else:
            h = m.group(2)
            if h in HELPERS:
                consumed_until = balanced(body, body.index('(', m.start()))
                ops.extend(interpret_build(HELPERS[h], test=test))
    return ops


def metadata_of(body):
    md = method_body(body, 'getMetadata')
    if md is None:
        return None, [], False
    nm = re.search(r'\.name\(\s*("(?:[^"\\]|\\.)*"|[\w.]+)\s*\)', md)
    name = None
    if nm:
        v = nm.group(1)
        name = qs(v) if v.startswith('"') else v
    anc = []
    am = re.search(r'\.ancestors\(', md)
    if am:
        e = balanced(md, am.end() - 1)
        anc = [x.strip().split('.')[-1][:-len('.class')] if False else re.sub(r'\.class$', '', x.strip()).split('.')[-1]
               for x in split_args(md[am.end():e - 1])]
    is_test = bool(re.search(r'RuleClassType\.TEST', md))
    return name, anc, is_test


def rule_def_for(cls):
    rel, ext, impl, body, src = FILES[cls]
    SCOPE.clear()
    SCOPE.update(file_consts(src))
    SCOPE.update(file_consts(body))   # constants of the class itself shadow the file's
    CUR_SRC[0] = src
    name, anc, is_test = metadata_of(body)
    if name is None and ext:
        # AbstractAliasRule / CommonAliasRule subclasses: name from `super("rule_name", ...)`
        sm = re.search(r'\bsuper\s*\(\s*("(?:[^"\\]|\\.)*")', body)
        name = qs(sm.group(1)) if sm else None
        anc = ['NativeBuildRule']
    if name and (name.split('.')[-1] in SCOPE or name.split('.')[-1] in CONSTS):
        name = const(name)
    bb = method_body(body, 'build')
    if bb is None and ext and ext.split('.')[-1] == 'CommonAliasRule':
        # LateBoundAlias.CommonAliasRule.build: licenses/distribs removed, `:alias` LABEL late bound
        ops = [('remove', 'licenses'), ('remove', 'distribs')]
        at = Attr(':alias', 'LABEL')
        ops.append(('add', at))
    else:
        ops = interpret_build(bb or '')
    return name, anc, ops, is_test, rel


# ------------------------------------------------------------------------------------------
# RuleClass construction (RuleClass.Builder semantics)
# ------------------------------------------------------------------------------------------
RULE_CLASSES = {}   # java class simple name -> ordered list[Attr] (as built)
TEST_ONLY_VARIANTS = {}


def apply_ops(attrs, ops):
    for kind, v in ops:
        if kind == 'remove':
            attrs.pop(v)
        elif kind == 'exempt':
            for n in ('compatible_with', 'restricted_to', 'target_compatible_with'):
                attrs.pop(n, None)
        elif kind == 'add':
            if v.name in attrs:
                raise ValueError('duplicate attribute ' + v.name)
            attrs[v.name] = v
        elif kind == 'override':
            old = attrs[v.name]
            n = v.copy()
            if v.default == ('none',) and old.default != ('none',):
                pass
            attrs[v.name] = n
    return attrs


def build_class(cls, test_variant=False, memo=None):
    memo = {} if memo is None else memo
    key = (cls, test_variant)
    if key in memo:
        return memo[key]
    name, anc, ops, is_test, rel = rule_def_for(cls)
    attrs = {}
    # name attribute at index 0 (RuleClass.NAME_ATTRIBUTE), implicit for every class
    nm = Attr('name', 'STRING_NO_INTERN')
    nm.configurable = False
    nm.mandatory = True
    attrs['name'] = nm
    for p in anc:
        for k, v in build_class(p, False, memo)[0].items():
            attrs[k] = v.copy() if k not in attrs else attrs[k]
    if cls == 'EmptyRule':
        pass
    if is_test or test_variant:
        # RuleClass.Builder: TEST rule classes get `testonly` (default true), nonconfigurable
        t = Attr('testonly', 'BOOLEAN')
        t.configurable = False
        t.default = ('bool', True)
        if 'testonly' in attrs:
            attrs['testonly'] = t
        else:
            attrs['testonly'] = t
    if test_variant:
        ops = interpret_build(method_body(FILES[cls][3], 'build'), test=True)
    attrs = apply_ops(attrs, ops)
    memo[key] = (attrs, name, is_test)
    return memo[key]


def finish(attrs, build_setting_type=None):
    """RuleClass.Builder.build(): build settings get `build_setting_default` + `help` last."""
    if build_setting_type:
        d = Attr('build_setting_default', build_setting_type)
        d.configurable = False
        d.mandatory = True
        attrs['build_setting_default'] = d
        h = Attr('help', 'STRING')
        h.configurable = False
        attrs['help'] = h
    return attrs


def default_expr(a, cname):
    t = a.default
    ty = a.ty
    if a.name == '$implicit_tests':
        return 'D::None'
    if a.name == 'visibility':
        # PackageFunction: PrecomputedValue.DEFAULT_VISIBILITY, --default_visibility defaults to private
        return 'D::List(&["//visibility:private"])'
    if t[0] == 'str' and ty == 'STRING':
        return 'D::Str(%s)' % rs(t[1])
    if t[0] == 'bool':
        return 'D::Bool(%s)' % ('true' if t[1] else 'false')
    if t[0] == 'tristate':
        return 'D::Tristate(%d)' % t[1]
    if t[0] == 'rulebased':
        return 'D::RuleBased(%s)' % rs(t[1])
    if t[0] == 'int':
        return 'D::Int(%d)' % t[1]
    if t[0] == 'label':
        return 'D::Label(%s)' % rs(t[1])
    if t[0] == 'list':
        return 'D::List(&[%s])' % ', '.join(rs(x) for x in t[1])
    if t[0] == 'none':
        # No (static) `.value(..)`: the Type's default value (Type.getDefaultValue()) applies. For
        # attributes with computed / late-bound defaults the default stays unset.
        if a.default_src is not None:
            return computed_default(a)
        return type_default(a)
    return 'D::None'


def computed_default(a):
    src = a.default_src
    if src == 'testonlyDefault':
        return 'D::Bool(false)'      # PackageArgs.defaultTestOnly() (package(default_testonly=False))
    if a.name == '$is_executable':
        return 'D::Bool(false)'      # ComputedDefault: `outs.size() == 1 && executable` at defaults
    if src == 'TIMEOUT_DEFAULT':
        return 'D::Str("moderate")'  # TestSize.getDefaultTimeout() of the default size "medium"
    return 'D::None'


def type_default(a):
    ty = a.ty
    if ty == 'STRING' and a.jtype in ('STRING', 'STRING_NO_INTERN'):
        return 'D::Str("")'
    if ty == 'BOOLEAN':
        return 'D::Bool(false)'
    if ty == 'INTEGER':
        return 'D::Int(0)'
    if ty == 'TRISTATE':
        return 'D::Tristate(-1)'
    if ty == 'LICENSE':
        return 'D::License("NONE")'   # License.NO_LICENSE
    return 'D::None'


def rs(s):
    return '"' + s.replace('\\', '\\\\').replace('"', '\\"') + '"'


# ------------------------------------------------------------------------------------------
def main():
    global DISC, LCLASS
    load_consts()
    DISC, LCLASS = types_from_source()
    NODEP_SERIALIZED.update(nodep_serialized_from_source())
    index_classes([
        'analysis/BaseRuleClasses.java', 'rules/Alias.java', 'rules/LabelBuildSettings.java',
        'rules/LateBoundAlias.java', 'rules/filegroup/FilegroupRule.java',
        'rules/test/TestSuiteRule.java', 'rules/genrule/GenRuleBaseRule.java',
        'bazel/rules/genrule/BazelGenRuleRule.java', 'rules/config/ConfigRuleClasses.java',
        'rules/platform/PlatformRule.java', 'rules/platform/ConstraintSettingRule.java',
        'rules/platform/ConstraintValueRule.java', 'rules/platform/ToolchainRule.java',
        'rules/ToolchainType.java', 'analysis/constraints/EnvironmentRule.java',
        'rules/starlarkdocextract/StarlarkDocExtractRule.java', 'rules/extra/ActionListenerRule.java',
        'rules/extra/ExtraActionRule.java', 'rules/java/JavaPluginsFlagAliasRule.java',
        'rules/cpp/CcToolchainAliasRule.java', 'rules/cpp/CcLibcTopAlias.java',
        'rules/genquery/GenQueryRule.java'])
    for rel in ['analysis/BaseRuleClasses.java', 'rules/LabelBuildSettings.java']:
        index_helpers(rel)
    HELPERS['buildRuleClass'] = HELPERS.get('buildRuleClass', '')
    # `commonCoreAndStarlarkAttributes(builder)` and `buildRuleClass(builder, flag)` are written as
    # `public static RuleClass.Builder f(RuleClass.Builder builder)` / `RuleClass f(...)`.
    for rel, nm in [('analysis/BaseRuleClasses.java', 'commonCoreAndStarlarkAttributes'),
                    ('rules/LabelBuildSettings.java', 'buildRuleClass')]:
        src = read(rel)
        m = re.search(r'\b' + nm + r'\s*\(', src)
        j = balanced(src, m.end() - 1)
        k = src.index('{', j)
        HELPERS[nm] = src[k + 1:balanced(src, k, '{', '}') - 1]

    # registered rule classes (GenericRules, CcRules, ... in BazelRuleClassProvider.RULE_SETS)
    registered = []   # (rule class name or EmptyRule name, java class or None)
    reg_files = ['bazel/rules/GenericRules.java', 'bazel/rules/ToolchainRules.java',
                 'bazel/rules/CcRules.java', 'bazel/rules/JavaRules.java', 'bazel/rules/ObjcRules.java',
                 'rules/core/CoreRules.java', 'rules/config/ConfigRules.java',
                 'rules/platform/PlatformRules.java', 'rules/genquery/GenQueryRule.java']
    for rel in reg_files:
        t = read(rel)
        for m in re.finditer(r'addRuleDefinition\(\s*new\s+([\w.]+)\(\s*("(?:[^"\\]|\\.)*")?[^)]*\)\s*(\{\s*\})?\s*\)', t):
            cls = m.group(1).split('.')[-1]
            if cls == 'EmptyRule':
                registered.append((qs(m.group(2)), 'EmptyRule'))
            else:
                registered.append((None, cls))
    for rel in ['bazel/rules/BazelRuleClassProvider.java']:
        pass

    # which RuleSets are really part of BazelRuleClassProvider.RULE_SETS (informational)
    provider = read('bazel/rules/BazelRuleClassProvider.java')
    sets = re.search(r'RULE_SETS\s*=\s*ImmutableSet\.of\((.*?)\);', provider, re.S).group(1)
    active = set(re.findall(r'(\w+)\.INSTANCE|(\w+_RULES|BAZEL_SETUP)', sets) and
                 [a or b for a, b in re.findall(r'(\w+)\.INSTANCE|(\w+_RULES|BAZEL_SETUP)', sets)])
    for need in ('GenericRules', 'CcRules', 'JavaRules', 'ObjcRules', 'ToolchainRules', 'CoreRules',
                 'ConfigRules', 'PlatformRules'):
        assert need in active, need

    defs = []   # (kind, [Attr])
    seen = set()
    memo = {}
    for ename, cls in registered:
        if cls == 'EmptyRule':
            is_test = bool(re.search(r'(_test|_binary)$', ename)) and ename.endswith('_test')
            attrs, _n, _t = build_class('EmptyRule', test_variant=is_test, memo=memo)
            attrs = {k: v.copy() for k, v in attrs.items()}
            # EmptyRule.$bzl_load_label is a STRING attribute with a `null` default
            if '$bzl_load_label' in attrs:
                pass
            defs.append((ename, finish(attrs)))
            continue
        attrs, name, is_test = build_class(cls, memo=memo)
        _n, anc, _o, _t, _r = rule_def_for(cls)
        if name is None or name.startswith('$'):
            continue
        if name in seen:
            continue
        seen.add(name)
        bs = None
        if cls in ('LabelBuildFlagRule', 'LabelBuildSettingRule'):
            bs = 'NODEP_LABEL'   # BuildSetting.create(flag, NODEP_LABEL)
        defs.append((name, finish({k: v.copy() for k, v in attrs.items()}, bs)))
    # classes registered through helper classes (e.g. `GenRuleBaseRule` is abstract: skipped by `$`)

    # --- Starlark rule() flavours (StarlarkRuleClassFunctions) ------------------------------
    sdefs = starlark_kinds()
    out = emit(defs, sdefs)
    if CHECK:
        cur = open(OUT).read() if os.path.exists(OUT) else ''
        if cur != out:
            sys.stderr.write('%s is out of date: run scripts/gen_rule_defs.py\n' % OUT)
            sys.exit(1)
        print('ok:', OUT, 'matches the generator output')
        return
    with open(OUT, 'w') as f:
        f.write(out)
    print('wrote', OUT, '(%d native kinds, %d starlark kinds)' % (len(defs), len(sdefs)))


def starlark_kinds():
    src = read('analysis/starlark/StarlarkRuleClassFunctions.java')
    base_m = re.search(r'RuleClass\s+baseRule\s*=\s*(.*?);\n\n', src, re.S)
    base = base_m.group(1)
    # expect_failure is added to the builder handed to commonCoreAndStarlarkAttributes
    attrs = {}
    nm = Attr('name', 'STRING_NO_INTERN')
    nm.configurable = False
    nm.mandatory = True
    attrs['name'] = nm
    m = re.search(r'commonCoreAndStarlarkAttributes\(', base)
    e = balanced(base, m.end() - 1)
    inner = base[m.end():e - 1]
    pre = interpret_build(inner)
    apply_ops(attrs, pre)
    apply_ops(attrs, interpret_build(HELPERS['commonCoreAndStarlarkAttributes']))
    rest = base[e:]
    apply_ops(attrs, interpret_build(rest))
    base_attrs = attrs

    bin_m = re.search(r'binaryBaseRule\s*=\s*(.*?);\n\n', src, re.S).group(1)
    bin_attrs = {k: v.copy() for k, v in base_attrs.items()}
    apply_ops(bin_attrs, interpret_build(bin_m))

    test_body = method_body(src, 'getTestBaseRule')
    test_attrs = {k: v.copy() for k, v in base_attrs.items()}
    # `$test_runtime` default comes from getTestRuntimeLabelList(env)
    ops = interpret_build(test_body)
    brc = read('analysis/BaseRuleClasses.java')
    pat = re.search(r'TOOLS_TEST_RUNTIME_TARGET_PATTERN\s*=\s*"([^"]+)"', brc).group(1)
    for kind, at in ops:
        if kind == 'add' and at.name == '$test_runtime':
            # getTestRuntimeLabelList(env): [env.getToolsRepository() + TOOLS_TEST_RUNTIME_TARGET_PATTERN]
            at.default = ('list', ['@bazel_tools' + pat])
    apply_ops(test_attrs, ops)
    # RuleClass.Builder for TEST rule classes: testonly default true is applied by
    # `Builder(.., RuleClassType.TEST ...)` for the *user's* rule class; for Starlark tests the
    # parent has testonly already.  Starlark `rule(test=True)` creates a TEST class:
    t = Attr('testonly', 'BOOLEAN')
    t.configurable = False
    t.default = ('bool', True)
    test_attrs['testonly'] = t

    def with_user(a, bs=None):
        a = {k: v.copy() for k, v in a.items()}
        u = Attr('<user attributes>', 'STRING')
        u.user = True
        a['<user attributes>'] = u
        return finish(a, bs)
    return [('@starlark', with_user(base_attrs)),
            ('@starlark:executable', with_user(bin_attrs)),
            ('@starlark:test', with_user(test_attrs)),
            ('@starlark:build_setting', with_user(base_attrs, 'STRING'))]


HEADER = '''// Copyright 2026 The Bazel Authors. All rights reserved.
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

//! GENERATED by `scripts/gen_rule_defs.py` from the Bazel *source tree* (rule definitions under
//! `src/main/java/com/google/devtools/build/lib`); do not edit by hand and do not regenerate from
//! a Bazel binary. Schema of every native rule class and of each flavour of Starlark `rule()`, in
//! `RuleClass.Builder` declaration order (`Rule.getAttributes()`), with proto type, `nodep`,
//! configurability, tool-dependency flag (`Attribute.isToolDependency`), mandatory flag and the
//! static default value. `scripts/check_rule_defs.sh` verifies this file is up to date.
#![allow(clippy::all)]

/// Default value of a generated attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum D {
    /// The type's zero value (empty string/list/dict, unset label, `false`, `0`).
    None,
    Str(&'static str),
    Int(i64),
    Bool(bool),
    Tristate(i32),
    Label(&'static str),
    List(&'static [&'static str]),
    Dict(&'static [(&'static str, &'static str)]),
    License(&'static str),
    /// Late-bound label default whose rule-based default (what `query` shows) is the raw value of
    /// the named attribute of the rule (`label_flag.:alias` -> `build_setting_default`).
    RuleBased(&'static str),
}

/// One generated attribute. `ty` is the `Build.Attribute.Discriminator` name of the attribute.
#[derive(Debug, Clone, Copy)]
pub struct GenAttr {
    pub name: &'static str,
    pub ty: &'static str,
    pub nodep: bool,
    /// Whether `AttributeFormatter.maybeSetNoDep` serializes the `nodep` field for this attribute's
    /// Java type (`depTypes`/`noDepTypes`).
    pub nodep_set: bool,
    pub configurable: bool,
    pub tool: bool,
    /// `Attribute.orderIndependent()`: plain list values are sorted when converted.
    pub ordered: bool,
    /// `Attribute.Builder.mandatory()`.
    pub mandatory: bool,
    pub default: D,
}

/// The attribute that stands for the user-defined attributes of a Starlark rule: Starlark rule
/// kinds list their user attributes where this placeholder is.
pub const USER_ATTRS: &str = "<user attributes>";

/// `(kind, attributes)`: native rule classes by name, Starlark kinds as `@starlark`,
/// `@starlark:executable`, `@starlark:test`, `@starlark:build_setting`.
pub static RULE_DEFS: &[(&str, &[GenAttr])] = &['''


def emit(defs, sdefs):
    o = [HEADER]
    for kind, attrs in defs + sdefs:
        o.append('    (%s, &[' % rs(kind))
        for n, a in attrs.items():
            if n == 'name':
                continue
            if getattr(a, 'user', False):
                o.append('        GenAttr { name: USER_ATTRS, ty: "", nodep: false, nodep_set: true, configurable: true, tool: false, '
                         'ordered: false, mandatory: false, default: D::None },')
                continue
            o.append('        GenAttr { name: %s, ty: %s, nodep: %s, nodep_set: %s, configurable: %s, tool: %s, ordered: %s, mandatory: %s, default: %s },' % (
                rs(a.name), rs(a.ty), str(a.nodep).lower(), str(a.jtype in NODEP_SERIALIZED).lower(), str(a.configurable and LCLASS.get(a.jtype) != 'OUTPUT').lower(),
                str(a.is_tool_dependency()).lower(), str(a.ordered).lower(), str(a.mandatory).lower(),
                default_expr(a, kind)))
        o.append('    ]),')
    o.append('];\n')
    return '\n'.join(o)


if __name__ == '__main__':
    main()
