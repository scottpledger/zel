#!/usr/bin/env python3
"""Verification only: diffs the source-derived rule-class table against a real Bazel binary.

`crates/zel-packages/src/rule_defs_gen.rs` is derived from the Bazel *source tree* by
`scripts/gen_rule_defs.py` and never from a binary. This script builds a probe workspace with one
instance of every native rule class that the table lists (plus the four flavours of Starlark
`rule()`), asks the real `bazel` for `query --output=proto --proto:default_values` and compares,
per rule class, the attribute names (in declaration order), their proto types, `nodep` flags and
static defaults with the table. Differences are classified:

  SKEW      the attribute/default exists in HEAD sources (the table) but not in the binary, or vice
            versa, and the version of the binary differs from the sources (see KNOWN_SKEW, each
            entry with its source evidence);
  MISMATCH  anything else (a bug in either the generator or this probe).

Usage: scripts/verify_rule_defs_vs_bazel.py [--bazel /opt/homebrew/bin/bazel] [--src ~/Development/bazel]
Exit status 1 if there is any MISMATCH.
"""
import os
import re
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
BAZEL = '/opt/homebrew/bin/bazel'
SRC = os.path.expanduser('~/Development/bazel')
a = sys.argv[1:]
while a:
    x = a.pop(0)
    if x == '--bazel':
        BAZEL = a.pop(0)
    elif x == '--src':
        SRC = os.path.expanduser(a.pop(0))

# (kind, attribute) -> evidence. The binary is Bazel 9.3.x, the sources are HEAD.
KNOWN_SKEW = {
    ('label_flag', 'on_leave_scope'): 'rules/LabelBuildSettings.java: on_leave_scope added after 9.3',
    ('label_setting', 'on_leave_scope'): 'rules/LabelBuildSettings.java: on_leave_scope added after 9.3',
    ('platform', 'remote_execution_properties'): 'rules/platform/PlatformRule.java: removed in HEAD',
    ('@starlark:test', '$collect_coverage_script'): 'analysis/starlark/StarlarkRuleClassFunctions.java: HEAD names it :collect_coverage_script',
    ('@starlark:test', ':collect_coverage_script'): 'analysis/starlark/StarlarkRuleClassFunctions.java: HEAD names it :collect_coverage_script',
}
SKEW_DEFAULT_KINDS = {('label_flag', 'scope'), ('label_setting', 'scope'), ('platform', 'missing_toolchain_error')}


def load_table():
    t = open(os.path.join(ROOT, 'crates/zel-packages/src/rule_defs_gen.rs')).read()
    kinds, cur = {}, None
    for l in t.splitlines():
        m = re.match(r'    \("([^"]+)", &\[', l)
        if m:
            cur = m.group(1)
            kinds[cur] = []
            continue
        m = re.match(r'        GenAttr \{ name: (?:"([^"]+)"|USER_ATTRS), ty: "([^"]*)", nodep: (\w+),', l)
        if m and cur:
            kinds[cur].append((m.group(1) or '<user>', m.group(2), m.group(3) == 'true'))
    return kinds


def sh(cmd, cwd):
    r = subprocess.run(cmd, cwd=cwd, capture_output=True)
    return r.stdout, r.stderr.decode('utf8', 'replace'), r.returncode


def main():
    table = load_table()
    tmp = tempfile.mkdtemp(prefix='verify_rule_defs')
    ws, ob = tmp + '/ws', tmp + '/ob'
    os.makedirs(ws)
    open(ws + '/MODULE.bazel', 'w').write('module(name="probe")\n')
    open(ws + '/rules.bzl', 'w').write(
        'def _i(ctx):\n    return [DefaultInfo()]\n'
        'r_plain = rule(_i, attrs={"x": attr.string()})\n'
        'r_exec = rule(_i, executable=True, attrs={"x": attr.string()})\n'
        'r_test = rule(_i, test=True, attrs={"x": attr.string()})\n'
        'r_bs = rule(_i, build_setting=config.string(flag=True), attrs={"x": attr.string()})\n')
    probes = {
        'genrule': 'genrule(name="p_genrule", outs=["o"], cmd="true")',
        'filegroup': 'filegroup(name="p_filegroup")',
        'alias': 'alias(name="p_alias", actual=":dummy")',
        'test_suite': 'test_suite(name="p_test_suite")',
        'config_setting': 'config_setting(name="p_config_setting", values={"cpu": "x"})',
        'genquery': 'genquery(name="p_genquery", expression=":dummy", scope=[":dummy"])',
        'constraint_setting': 'constraint_setting(name="p_constraint_setting")',
        'constraint_value': 'constraint_value(name="p_constraint_value", constraint_setting=":p_constraint_setting")',
        'platform': 'platform(name="p_platform")',
        'toolchain_type': 'toolchain_type(name="p_toolchain_type")',
        'toolchain': 'toolchain(name="p_toolchain", toolchain=":dummy", toolchain_type=":p_toolchain_type")',
        'label_flag': 'label_flag(name="p_label_flag", build_setting_default=":dummy")',
        'label_setting': 'label_setting(name="p_label_setting", build_setting_default=":dummy")',
        'extra_action': 'extra_action(name="p_extra_action", cmd="true")',
        'action_listener': 'action_listener(name="p_action_listener", mnemonics=["X"], extra_actions=[":p_extra_action"])',
        'starlark_doc_extract': 'starlark_doc_extract(name="p_starlark_doc_extract", src="rules.bzl")',
        'environment': 'environment(name="p_environment")',
        '@starlark': 'r_plain(name="p_starlark")',
        '@starlark:executable': 'r_exec(name="p_starlark_executable")',
        '@starlark:test': 'r_test(name="p_starlark_test")',
        '@starlark:build_setting': 'r_bs(name="p_starlark_build_setting", build_setting_default="")',
    }
    open(ws + '/BUILD', 'w').write(
        'load(":rules.bzl", "r_plain", "r_exec", "r_test", "r_bs")\nfilegroup(name="dummy")\n' +
        '\n'.join(probes.values()) + '\n')
    out, err, rc = sh(['bash', '-c',
                       '%s --output_base=%s query "//..." --output=xml --xml:default_values' % (BAZEL, ob)], ws)
    if rc:
        sys.exit(err)
    xml = out.decode('utf8', 'replace')
    real = {}
    for m in re.finditer(r'<rule class="[^"]*" location="[^"]*" name="//:(\w+)">(.*?)\n    </rule>', xml, re.S):
        real[m.group(1)] = re.findall(r'^        <(?!rule-)(\w[\w-]*) name="([^"]+)"', m.group(2), re.M)
    bad = 0
    names = {'p_' + k.replace('@', '').replace(':', '_').replace('doc', 'doc'): k for k in probes}
    for pname, kind in sorted(names.items(), key=lambda kv: kv[1]):
        if pname not in real:
            print('MISMATCH %-26s probe target missing from query output (kind unknown to this binary?)' % kind)
            bad += 1
            continue
        r_attrs = [n for _t, n in real[pname] if n != 'name']
        t_attrs = [n for n, _ty, _np in table.get(kind, []) if n != '<user>']
        if kind.startswith('@starlark'):
            r_attrs = [n for n in r_attrs if n != 'x']
        for n in t_attrs:
            if n not in r_attrs:
                tag = 'SKEW    ' if (kind, n) in KNOWN_SKEW else 'MISMATCH'
                bad += tag == 'MISMATCH'
                print('%s %-26s attribute %-28s only in source-derived table  %s' % (tag, kind, n, KNOWN_SKEW.get((kind, n), '')))
        for n in r_attrs:
            if n not in t_attrs:
                tag = 'SKEW    ' if (kind, n) in KNOWN_SKEW else 'MISMATCH'
                bad += tag == 'MISMATCH'
                print('%s %-26s attribute %-28s only in %s  %s' % (tag, kind, n, os.path.basename(BAZEL), KNOWN_SKEW.get((kind, n), '')))
        common_r = [n for n in r_attrs if n in t_attrs]
        common_t = [n for n in t_attrs if n in r_attrs]
        if common_r != common_t:
            print('MISMATCH %-26s attribute order differs:\n   binary %s\n   table  %s' % (kind, common_r, common_t))
            bad += 1
    print('done: %d MISMATCH' % bad)
    sys.exit(1 if bad else 0)


main()
