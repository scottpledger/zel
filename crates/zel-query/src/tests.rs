// Copyright 2026 The Bazel Authors. All rights reserved.
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

//! End-to-end tests of `run_query` over an in-memory workspace.

use crate::format::{FormatOpts, OrderOutput};
use crate::post_analysis::{run_post_analysis, PostOpts, PostOutcome};
use crate::target_env::QueryOpts;
use crate::{run_query, QueryOutcome};
use std::sync::Arc;
use zel_analysis::BuildConfigurationKey;
use zel_packages::{LoaderConfig, PackageLoader, PatternResolver, SimpleRepoResolver};
use zel_rules::{AnalysisConfig, TargetResolver};
use zel_vfs::{FileSystem, InMemoryFileSystem, PathFragment};

pub(crate) const SH_STUB: &str = r#"def _lib(ctx):
    return [DefaultInfo(files = depset(ctx.files.srcs), runfiles = ctx.runfiles(files = ctx.files.srcs + ctx.files.data))]

def _bin(ctx):
    exe = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.symlink(output = exe, target_file = ctx.files.srcs[0], is_executable = True)
    return [DefaultInfo(executable = exe, files = depset([exe]), runfiles = ctx.runfiles(files = ctx.files.srcs + ctx.files.data))]

_attrs = {
    "srcs": attr.label_list(allow_files = True),
    "deps": attr.label_list(),
    "data": attr.label_list(allow_files = True),
}

sh_library = rule(implementation = _lib, attrs = _attrs)
sh_binary = rule(implementation = _bin, executable = True, attrs = _attrs)
sh_test = rule(implementation = _bin, test = True, attrs = _attrs)
"#;

fn uses_shell_rules(c: &str) -> bool {
    ["sh_binary(", "sh_test(", "sh_library("].iter().any(|n| c.contains(n))
}

/// Prepends `load()`s for the `sh_*` rules a BUILD file uses.
pub(crate) fn shell_loads(path: &str, content: &str) -> String {
    if !(path == "BUILD" || path.ends_with("/BUILD")) || !uses_shell_rules(content) {
        return content.to_string();
    }
    let mut used: Vec<&str> = ["sh_binary", "sh_test", "sh_library"].into_iter().filter(|n| content.contains(&format!("{n}("))).collect();
    used.sort();
    let names: Vec<String> = used.iter().map(|n| format!("\"{n}\"")).collect();
    format!("load(\"//shstub:sh.bzl\", {})\n{content}", names.join(", "))
}

pub(crate) struct Ws {
    pub fs: Arc<InMemoryFileSystem>,
    pub root: PathFragment,
}

impl Ws {
    pub(crate) fn new(files: &[(&str, &str)]) -> Self {
        let fs = Arc::new(InMemoryFileSystem::default());
        let root = PathFragment::create("/ws");
        fs.create_directory_and_parents(&root).unwrap();
        let ws = Ws { fs, root };
        let bt_root = PathFragment::create("/embedded_tools");
        ws.fs.create_directory_and_parents(&bt_root.get_relative_str("tools/genrule")).unwrap();
        ws.fs.write_file(&bt_root.get_relative_str("tools/genrule/BUILD"), b"exports_files(['genrule-setup.sh'])\n").unwrap();
        ws.fs.write_file(&bt_root.get_relative_str("tools/genrule/genrule-setup.sh"), b"#!/bin/sh\n").unwrap();
        ws.fs.write_file(&bt_root.get_relative_str("WORKSPACE"), b"").unwrap();
        for (p, c) in files {
            let path = ws.root.get_relative_str(p);
            ws.fs.create_directory_and_parents(&path.parent_directory().unwrap()).unwrap();
            let content = shell_loads(p, c);
            ws.fs.write_file(&path, content.as_bytes()).unwrap();
        }
        if files.iter().any(|(p, c)| (*p == "BUILD" || p.ends_with("/BUILD")) && uses_shell_rules(c)) {
            // Bazel 9 has no native `sh_*` rules; in-memory workspaces get a stand-in for `@rules_shell`.
            let d = ws.root.get_relative_str("shstub");
            ws.fs.create_directory_and_parents(&d).unwrap();
            ws.fs.write_file(&d.get_relative_str("BUILD"), b"").unwrap();
            ws.fs.write_file(&d.get_relative_str("sh.bzl"), SH_STUB.as_bytes()).unwrap();
        }
        ws
    }

    pub(crate) fn resolver(&self) -> PatternResolver {
        let mut repos = SimpleRepoResolver::new(self.root.clone());
        let bt_root = PathFragment::create("/embedded_tools");
        repos.external.insert("bazel_tools".to_string(), bt_root);
        repos.apparent.insert("bazel_tools".to_string(), zel_cmdline::RepositoryName::bazel_tools());
        let cfg = LoaderConfig::new(self.fs.clone(), Arc::new(repos));
        PatternResolver::new(PackageLoader::new(cfg), PathFragment::empty())
    }

    pub(crate) fn post(&self, expr: &str, configure: impl FnOnce(&mut QueryOpts, &mut FormatOpts, &mut PostOpts)) -> PostOutcome {
        let resolver = self.resolver();
        let tr = TargetResolver::new(resolver.loader().clone(), AnalysisConfig::default());
        let mut qopts = QueryOpts::default();
        let mut fopts = FormatOpts::default();
        let mut popts = PostOpts::default();
        configure(&mut qopts, &mut fopts, &mut popts);
        run_post_analysis(&resolver, tr, BuildConfigurationKey::default(), expr, &qopts, &fopts, &popts)
    }
}

pub(crate) fn tree() -> Ws {
    Ws::new(&[
        (
            "lib/BUILD",
            "genrule(name = 'l', srcs = ['data.txt'], outs = ['l.out'], cmd = 'cat $(SRCS) > $@', visibility = ['//visibility:public'])\n\
             filegroup(name = 'fg', srcs = ['data.txt', ':l'], tags = ['manual', 'x'])\n\
             exports_files(['data.txt'])\n",
        ),
        ("lib/data.txt", "hi"),
        (
            "app/BUILD",
            "genrule(name = 'app', srcs = ['//lib:l', 'in.txt'], outs = ['app.out'], cmd = 'cat $(SRCS) > $@')\n\
             genrule(name = 'other', srcs = [':app'], outs = ['o.out'], cmd = 'echo o > $@', testonly = True)\n\
             sh_test(name = 'app_test', srcs = ['t.sh'], deps = [':app'])\n\
             sh_test(name = 'slow_test', srcs = ['t.sh'], tags = ['slow'])\n\
             test_suite(name = 'suite', tests = [':app_test', ':slow_test'], tags = ['-slow'])\n\
             test_suite(name = 'all_tests')\n",
        ),
        ("app/in.txt", "x"),
        ("app/t.sh", "#!/bin/sh"),
    ])
}

pub(crate) fn q(ws: &Ws, expr: &str, extra: impl FnOnce(&mut QueryOpts, &mut FormatOpts)) -> QueryOutcome {
    let resolver = ws.resolver();
    let mut o = QueryOpts::default();
    let mut f = FormatOpts::default();
    extra(&mut o, &mut f);
    run_query(&resolver, expr, &o, &f)
}

pub(crate) fn lines(o: &QueryOutcome) -> Vec<String> {
    String::from_utf8_lossy(&o.stdout).lines().map(|s| s.to_string()).collect()
}

fn ql(ws: &Ws, expr: &str) -> Vec<String> {
    let o = q(ws, expr, |_, _| {});
    assert_eq!(o.exit_code, 0, "{expr}: {:?}", o.stderr);
    lines(&o)
}

#[test]
fn labels_and_sorted_default_order() {
    let ws = tree();
    assert_eq!(ql(&ws, "//app:all"), vec!["//app:all_tests", "//app:app", "//app:app_test", "//app:other", "//app:slow_test", "//app:suite"]);
    assert_eq!(ql(&ws, "//lib:l + //app:app"), vec!["//app:app", "//lib:l"]);
    // Residue words joined by the command: operators without quotes.
    assert_eq!(ql(&ws, "//lib:all - //lib:l"), vec!["//lib:fg"]);
}

#[test]
fn deps_cross_package_and_depth() {
    let ws = tree();
    assert_eq!(
        ql(&ws, "deps(//app:other)"),
        vec!["//app:app", "//app:in.txt", "//app:other", "//lib:data.txt", "//lib:l", "@@bazel_tools//tools/genrule:genrule-setup.sh"]
    );
    assert_eq!(ql(&ws, "deps(//app:other, 0)"), vec!["//app:other"]);
    assert_eq!(ql(&ws, "deps(//app:other, 1)"), vec!["//app:app", "//app:other", "@@bazel_tools//tools/genrule:genrule-setup.sh"]);
    assert_eq!(
        ql(&ws, "deps(//app:other, 2)"),
        vec!["//app:app", "//app:in.txt", "//app:other", "//lib:l", "@@bazel_tools//tools/genrule:genrule-setup.sh"]
    );
}

#[test]
fn rdeps_and_allrdeps() {
    let ws = tree();
    assert_eq!(
        ql(&ws, "rdeps(//app:other, //lib:l)"),
        vec!["//app:app", "//app:other", "//lib:l"]
    );
    assert_eq!(ql(&ws, "rdeps(//app:other, //lib:l, 1)"), vec!["//app:app", "//lib:l"]);
    let o = q(&ws, "allrdeps(//lib:l)", |o, _| o.infer_universe_scope = true);
    assert_eq!(lines(&o), vec!["//lib:l"], "the universe is inferred from the expression's patterns");
    let o = q(&ws, "allrdeps(//lib:l)", |_, _| {});
    assert_eq!(o.exit_code, 2, "allrdeps only exists in SkyQuery mode (universe scope + streamed output)");
    let o = q(&ws, "allrdeps(//lib:l)", |o, _| o.universe_scope = vec!["//app:all".into()]);
    assert_eq!(lines(&o), vec!["//app:all_tests", "//app:app", "//app:app_test", "//app:other", "//app:suite", "//lib:l"]);
    let o = q(&ws, "allrdeps(//lib:l, 1)", |o, _| o.universe_scope = vec!["//...".into()]);
    assert_eq!(lines(&o), vec!["//app:app", "//lib:fg", "//lib:l"]);
}

#[test]
fn paths() {
    let ws = tree();
    assert_eq!(
        lines(&q(&ws, "somepath(//app:other, //lib:l)", |_, f| f.order = OrderOutput::No)),
        vec!["//app:other", "//app:app", "//lib:l"]
    );
    assert_eq!(ql(&ws, "somepath(//app:other, //lib:l)"), vec!["//app:other", "//app:app", "//lib:l"]);
    assert_eq!(ql(&ws, "somepath(//lib:l, //app:other)"), Vec::<String>::new());
    assert_eq!(
        ql(&ws, "allpaths(//app:other, //lib:data.txt)"),
        vec!["//app:app", "//app:other", "//lib:data.txt", "//lib:l"]
    );
}

#[test]
fn filters() {
    let ws = tree();
    assert_eq!(ql(&ws, "kind('genrule rule', //app:all)"), vec!["//app:app", "//app:other"]);
    assert_eq!(ql(&ws, "kind('source file', deps(//app:app))"), vec!["//app:in.txt", "//lib:data.txt", "@@bazel_tools//tools/genrule:genrule-setup.sh"]);
    assert_eq!(ql(&ws, "filter('_test$', //app:all)"), vec!["//app:app_test", "//app:slow_test"]);
    // test_suite and Starlark test rules (`sh_test` from @rules_shell) are testonly by default.
    assert_eq!(
        ql(&ws, "attr(testonly, 1, //app:all)"),
        vec!["//app:all_tests", "//app:app_test", "//app:other", "//app:slow_test", "//app:suite"]
    );
    assert_eq!(ql(&ws, "attr(tags, 'manual', //lib:all)"), vec!["//lib:fg"]);
    assert_eq!(ql(&ws, "attr(tags, '\\bslow\\b', //app:all)"), vec!["//app:slow_test", "//app:suite"]);
    assert_eq!(ql(&ws, "attr(srcs, 'in\\.txt', //app:all)"), vec!["//app:app"]);
}

#[test]
fn regex_errors_report_java_message() {
    let ws = tree();
    let o = q(&ws, "kind('(', //app:all)", |_, _| {});
    assert_eq!(o.exit_code, 7);
    assert_eq!(
        o.stderr[0],
        "ERROR: Evaluation of query \"kind('(', //app:all)\" failed: illegal 'kind' pattern regexp '(': Unclosed group near index 1\n("
    );
}

#[test]
fn labels_function_and_some() {
    let ws = tree();
    assert_eq!(ql(&ws, "labels(srcs, //app:app)"), vec!["//app:in.txt", "//lib:l"]);
    assert_eq!(ql(&ws, "labels(deps, //app:app_test)"), vec!["//app:app"]);
    assert_eq!(ql(&ws, "some(//app:all)").len(), 1);
    let o = q(&ws, "some(//app:all - //app:all)", |_, _| {});
    assert_eq!(o.exit_code, 7);
    assert!(o.stderr[0].contains("failed (did you want to use --keep_going?): argument set is empty") || o.stderr[0].contains("argument set is empty"), "{:?}", o.stderr);
}

#[test]
fn tests_function_expands_suites() {
    let ws = tree();
    assert_eq!(ql(&ws, "tests(//app:suite)"), vec!["//app:app_test"]);
    assert_eq!(ql(&ws, "tests(//app:all_tests)"), vec!["//app:app_test", "//app:slow_test"]);
    assert_eq!(ql(&ws, "tests(//app:all)"), vec!["//app:app_test", "//app:slow_test"]);
}

#[test]
fn siblings_and_same_pkg_direct_rdeps() {
    let ws = tree();
    assert_eq!(ql(&ws, "siblings(//lib:l)"), vec!["//lib:BUILD", "//lib:data.txt", "//lib:fg", "//lib:l", "//lib:l.out"]);
    assert_eq!(ql(&ws, "same_pkg_direct_rdeps(//app:app)"), vec!["//app:app.out", "//app:app_test", "//app:other"]);
}

#[test]
fn buildfiles_and_visible() {
    let ws = tree();
    // `sh_*` rules are loaded from a .bzl (the `@rules_shell` stand-in `//shstub:sh.bzl`).
    assert_eq!(ql(&ws, "buildfiles(//app:app)"), vec!["//app:BUILD", "//shstub:BUILD", "//shstub:sh.bzl"]);
    assert!(ql(&ws, "visible(//app:app, //lib:all)").contains(&"//lib:l".to_string()));
}

#[test]
fn let_set_and_operators() {
    let ws = tree();
    assert_eq!(ql(&ws, "let x = //lib:l in deps(//app:app) - $x"), vec!["//app:app", "//app:in.txt", "//lib:data.txt", "@@bazel_tools//tools/genrule:genrule-setup.sh"]);
    assert_eq!(ql(&ws, "set(//lib:l //app:app)"), vec!["//app:app", "//lib:l"]);
    assert_eq!(ql(&ws, "//lib:all ^ //lib:l"), vec!["//lib:l"]);
    assert_eq!(ql(&ws, "//lib:l union //app:app except //app:app"), vec!["//lib:l"]);
    let o = q(&ws, "deps($x)", |_, _| {});
    assert_eq!(o.exit_code, 7);
    assert!(o.stderr[0].contains("undefined variable 'x'"), "{:?}", o.stderr);
}

#[test]
fn syntax_errors_exit_2() {
    let ws = tree();
    let o = q(&ws, "deps(//app:app", |_, _| {});
    assert_eq!(o.exit_code, 2);
    assert_eq!(o.stderr, vec!["ERROR: Error while parsing 'deps(//app:app': premature end of input"]);
    let o = q(&ws, "kind(//a)", |_, _| {});
    assert_eq!(o.exit_code, 2);
}

#[test]
fn missing_targets_report_errors() {
    let ws = tree();
    let o = q(&ws, "//app:nope", |_, _| {});
    assert_eq!(o.exit_code, 7);
    assert!(o.stderr[0].starts_with("ERROR: no such target '//app:nope'"), "{:?}", o.stderr);
    assert!(o.stderr[0].contains("no such target '//app:nope'"), "{:?}", o.stderr);
    // The previously buggy behaviour: an empty result must not print the literal.
    let o = q(&ws, "//app:all - //app:all", |_, _| {});
    assert_eq!(o.exit_code, 0);
    assert!(o.stdout.is_empty());
    assert_eq!(o.stderr, vec!["INFO: Empty results"]);
}

#[test]
fn keep_going_exit_3() {
    let ws = Ws::new(&[("a/BUILD", "filegroup(name='a', srcs=['//b:missing'])"), ("b/BUILD", "filegroup(name='b')")]);
    let o = q(&ws, "deps(//a:a)", |o, _| o.keep_going = true);
    assert_eq!(o.exit_code, 3, "{:?}", o.stderr);
    assert_eq!(lines(&o), vec!["//a:a"]);
    assert!(o.stderr.iter().any(|l| l.contains("--keep_going specified, ignoring errors")), "{:?}", o.stderr);
    let o = q(&ws, "deps(//a:a)", |_, _| {});
    assert_eq!(o.exit_code, 7);
}

#[test]
fn label_kind_package_location_and_null_terminator() {
    let ws = tree();
    let o = q(&ws, "//lib:all", |_, f| f.output = "label_kind".into());
    assert_eq!(lines(&o), vec!["filegroup rule //lib:fg", "genrule rule //lib:l"]);
    let o = q(&ws, "deps(//app:app)", |_, f| f.output = "package".into());
    assert_eq!(lines(&o), vec!["@@bazel_tools//tools/genrule", "app", "lib"]);
    let o = q(&ws, "//lib:l", |_, f| f.output = "location".into());
    assert_eq!(lines(&o), vec!["/ws/lib/BUILD:1:8: genrule rule //lib:l"]);
    let o = q(&ws, "//lib:l", |_, f| f.relative_locations = true);
    assert_eq!(lines(&o), vec!["//lib:l"]);
    let o = q(&ws, "//lib:all", |_, f| f.line_terminator = "\0".into());
    assert_eq!(o.stdout, b"//lib:fg\0//lib:l\0".to_vec());
    let o = q(&ws, "buildfiles(//lib:l)", |_, f| f.output = "location".into());
    assert_eq!(o.exit_code, 2);
}

#[test]
fn order_output_deps_and_full() {
    let ws = tree();
    let full = q(&ws, "deps(//app:other)", |_, f| f.order = OrderOutput::Full);
    assert_eq!(lines(&full), vec!["//app:other", "//app:app", "//lib:l", "@@bazel_tools//tools/genrule:genrule-setup.sh", "//lib:data.txt", "//app:in.txt"]);
    let deps = q(&ws, "deps(//app:other)", |_, f| f.order = OrderOutput::Deps);
    let l = lines(&deps);
    assert_eq!(l.first().map(String::as_str), Some("//app:other"));
    assert!(l.iter().position(|x| x == "//app:app") < l.iter().position(|x| x == "//lib:l"));
}

#[test]
fn minrank_maxrank() {
    let ws = tree();
    let o = q(&ws, "deps(//app:other)", |_, f| f.output = "minrank".into());
    assert_eq!(lines(&o), vec!["0 //app:other", "1 //app:app", "1 @@bazel_tools//tools/genrule:genrule-setup.sh", "2 //lib:l", "2 //app:in.txt", "3 //lib:data.txt"]);
    let o = q(&ws, "deps(//app:other)", |_, f| f.output = "maxrank".into());
    assert_eq!(lines(&o), vec!["0 //app:other", "1 //app:app", "2 //lib:l", "2 //app:in.txt", "3 //lib:data.txt", "3 @@bazel_tools//tools/genrule:genrule-setup.sh"]);
}

#[test]
fn graph_output() {
    let ws = tree();
    let o = q(&ws, "deps(//app:app)", |_, f| {
        f.output = "graph".into();
        f.graph_factored = false;
        f.order = OrderOutput::Full;
    });
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "digraph mygraph {\n  node [shape=box];\n  \"//app:app\"\n  \"//app:app\" -> \"//app:in.txt\"\n  \"//app:app\" -> \"//lib:l\"\n  \"//app:app\" -> \"@@bazel_tools//tools/genrule:genrule-setup.sh\"\n  \"//lib:l\"\n  \"//lib:l\" -> \"//lib:data.txt\"\n  \"//lib:l\" -> \"@@bazel_tools//tools/genrule:genrule-setup.sh\"\n  \"@@bazel_tools//tools/genrule:genrule-setup.sh\"\n  \"//lib:data.txt\"\n  \"//app:in.txt\"\n}\n"
    );
}

fn rich() -> Ws {
    Ws::new(&[
        ("BUILD", ""),
        (
            "defs.bzl",
            "load('//tools:helper.bzl', 'helper')\ndef _impl(ctx):\n    out = ctx.actions.declare_file(ctx.label.name + '.txt')\n    ctx.actions.write(out, 'x')\n    return [DefaultInfo(files = depset([out]))]\nmy_rule = rule(implementation = _impl, attrs = {'deps': attr.label_list(), '_tool': attr.label(default = '//tools:t')})\n",
        ),
        ("tools/helper.bzl", "def helper(): pass\n"),
        ("tools/BUILD", "filegroup(name = 't')\n"),
        (
            "pkg/BUILD",
            "load('//:defs.bzl', 'my_rule')\n\
             package_group(name = 'friends', packages = ['//app/...', '-//app/secret'])\n\
             config_setting(name = 'opt', values = {'compilation_mode': 'opt'})\n\
             my_rule(name = 'r', deps = [':s'], visibility = [':friends'])\n\
             my_rule(name = 's')\n\
             filegroup(name = 'fg', srcs = select({':opt': ['a.txt'], '//conditions:default': ['b.txt']}) + ['c.txt'], testonly = True)\n\
             sh_binary(name = 'bin', srcs = ['b.sh'])\n",
        ),
        ("pkg/a.txt", ""),
        ("pkg/b.txt", ""),
        ("pkg/c.txt", ""),
        ("pkg/b.sh", ""),
    ])
}

#[test]
fn implicit_deps_flag_hides_default_label_attributes() {
    let ws = rich();
    assert_eq!(ql(&ws, "deps(//pkg:s)"), vec!["//pkg:s", "//tools:t"]);
    let o = q(&ws, "deps(//pkg:s)", |o, _| o.implicit_deps = false);
    assert_eq!(lines(&o), vec!["//pkg:s"]);
    assert_eq!(ql(&ws, "deps(//pkg:r)"), vec!["//pkg:friends", "//pkg:r", "//pkg:s", "//tools:t"]);
}

#[test]
fn select_branches_all_count_as_dependencies() {
    let ws = rich();
    assert_eq!(
        ql(&ws, "deps(//pkg:fg)"),
        vec!["//pkg:a.txt", "//pkg:b.txt", "//pkg:c.txt", "//pkg:fg", "//pkg:opt"]
    );
    assert_eq!(ql(&ws, "attr(srcs, 'a.txt', //pkg:fg)"), vec!["//pkg:fg"]);
    assert_eq!(ql(&ws, "attr(testonly, 1, //pkg:all)"), vec!["//pkg:fg"]);
}

#[test]
fn build_file_target_and_buildfiles_loadfiles() {
    let ws = rich();
    assert_eq!(ql(&ws, "//pkg:BUILD"), vec!["//pkg:BUILD"]);
    assert_eq!(ql(&ws, "loadfiles(//pkg:r)"), vec!["//:defs.bzl", "//shstub:sh.bzl", "//tools:helper.bzl"]);
    assert_eq!(ql(&ws, "buildfiles(//pkg:r)"), vec!["//:BUILD", "//:defs.bzl", "//pkg:BUILD", "//shstub:BUILD", "//shstub:sh.bzl", "//tools:BUILD", "//tools:helper.bzl"]);
    let o = q(&ws, "rbuildfiles(tools/helper.bzl)", |o, _| o.universe_scope = vec!["//...".into()]);
    assert_eq!(lines(&o), vec!["//pkg:BUILD"], "{:?}", o.stderr);
    assert_eq!(ql(&ws, "executables(//pkg:all)"), vec!["//pkg:bin"]);
}

#[test]
fn visibility_and_visible_function() {
    let ws = rich();
    assert_eq!(ql(&ws, "visible(//tools:t, //pkg:r)").len(), 0);
    let ws2 = Ws::new(&[
        ("a/BUILD", "filegroup(name='a', visibility=['//b:__pkg__'])\nfilegroup(name='priv', visibility=['//visibility:private'])\nfilegroup(name='pub', visibility=['//visibility:public'])"),
        ("b/BUILD", "filegroup(name='b')"),
        ("c/BUILD", "filegroup(name='c')"),
    ]);
    assert_eq!(ql(&ws2, "visible(//b:b, //a:all)"), vec!["//a:a", "//a:pub"]);
    assert_eq!(ql(&ws2, "visible(//c:c, //a:all)"), vec!["//a:pub"]);
    assert_eq!(ql(&ws2, "visible(//c:c + //b:b, //a:all)"), vec!["//a:pub"]);
}

#[test]
fn build_output_formats_selects_and_package_groups() {
    let ws = rich();
    let o = q(&ws, "//pkg:fg + //pkg:friends", |_, f| {
        f.output = "build".into();
        f.package_group_build_output = true;
    });
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(out.contains("filegroup(\n  name = \"fg\",\n  testonly = True,\n  srcs = select({\"//pkg:opt\": [\"//pkg:a.txt\"], \"//conditions:default\": [\"//pkg:b.txt\"]}) + [\"//pkg:c.txt\"],\n)"), "{out}");
    assert!(out.contains("package_group(\n  name = \"friends\",\n  packages = [\n    \"//app/...\",\n    \"-//app/secret\",\n  ],\n)\n\n"), "{out}");
}

#[test]
fn xml_output_has_java_layout() {
    let ws = rich();
    let o = q(&ws, "//pkg:fg + //pkg:a.txt + //pkg:friends", |_, f| f.output = "xml".into());
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(out.starts_with("<?xml version=\"1.1\" encoding=\"UTF-8\" standalone=\"no\"?>\n<query version=\"2\">\n") || out.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n<query version=\"2\">\n"), "{out}");
    assert!(out.contains("    <rule class=\"filegroup\" location=\"/ws/pkg/BUILD:7:10\" name=\"//pkg:fg\">\n        <string name=\"name\" value=\"fg\"/>\n        <boolean name=\"testonly\" value=\"true\"/>\n        <list name=\"srcs\">"), "{out}");
    assert!(out.contains("<source-file location=\"/ws/pkg/a.txt:1:1\" name=\"//pkg:a.txt\"/>") || out.contains("name=\"//pkg:a.txt\""), "{out}");
    assert!(out.contains("<package-group location=\"/ws/pkg/BUILD:3:14\" name=\"//pkg:friends\">"), "{out}");
    assert!(out.trim_end().ends_with("</query>"), "{out}");
}

#[test]
fn proto_family_is_consistent() {
    let ws = rich();
    let bin = q(&ws, "//pkg:s", |_, f| f.output = "proto".into());
    assert_eq!(bin.exit_code, 0, "{:?}", bin.stderr);
    let s = String::from_utf8_lossy(&bin.stdout).to_string();
    assert!(s.contains("//pkg:s") && s.contains("my_rule") && s.contains("//tools:t"), "{s:?}");
    let streamed = q(&ws, "//pkg:s + //pkg:r", |_, f| f.output = "streamed_proto".into());
    // Each message is length-delimited: first byte is the varint length of the Target message.
    let b = &streamed.stdout;
    let len = b[0] as usize;
    assert!(len < 0x80 || b[0] & 0x80 != 0);
    let js = q(&ws, "//pkg:s", |_, f| f.output = "streamed_jsonproto".into());
    let line = String::from_utf8_lossy(&js.stdout).to_string();
    assert!(line.starts_with("{\"type\":\"RULE\",\"rule\":{\"name\":\"//pkg:s\",\"ruleClass\":\"my_rule\""), "{line}");
    assert!(line.contains("\"ruleInput\":[\"//tools:t\"]"), "{line}");
    assert!(line.contains("{\"name\":\"$tool\",\"type\":\"LABEL\",\"stringValue\":\"//tools:t\",\"explicitlySpecified\":false,\"nodep\":false}"), "{line}");
    let no_defaults = q(&ws, "//pkg:s", |_, f| {
        f.output = "streamed_jsonproto".into();
        f.proto_default_values = false;
        f.proto_rule_inputs_and_outputs = false;
        f.proto_locations = false;
    });
    let l = String::from_utf8_lossy(&no_defaults.stdout).to_string();
    assert!(!l.contains("location") && !l.contains("ruleInput") && !l.contains("$tool"), "{l}");
}

#[test]
fn order_no_and_null_and_keep_going_and_universe() {
    let ws = tree();
    let o = q(&ws, "deps(//app:app)", |_, f| f.order = OrderOutput::No);
    assert_eq!(o.exit_code, 0);
    assert_eq!(lines(&o).len(), 5);
    let o = q(&ws, "allrdeps(//lib:data.txt)", |o, _| o.infer_universe_scope = true);
    // Inferred universe = patterns of the expression: just data.txt.
    assert_eq!(lines(&o), vec!["//lib:data.txt"]);
    let o = q(&ws, "allrdeps(//lib:data.txt)", |o, _| o.universe_scope = vec!["//lib:all".into(), "//app:all".into()]);
    assert_eq!(
        lines(&o),
        vec!["//app:all_tests", "//app:app", "//app:app_test", "//app:other", "//app:suite", "//lib:data.txt", "//lib:fg", "//lib:l"]
    );
}

#[test]
fn rdeps_requires_target_in_universe() {
    let ws = tree();
    assert_eq!(ql(&ws, "rdeps(//lib:l, //app:app)"), Vec::<String>::new());
    assert_eq!(ql(&ws, "rdeps(//lib:l + //app:app, //app:app + //lib:l)"), vec!["//app:app", "//lib:l"]);
}

#[test]
fn query_expression_error_texts() {
    let ws = tree();
    let o = q(&ws, "foo(//app:app)", |_, _| {});
    assert_eq!(o.exit_code, 2);
    assert!(o.stderr[0].starts_with("ERROR: Error while parsing 'foo(//app:app)': unknown function 'foo' at 'foo ( //app:app'; expected one of ['allpaths', 'attr', 'buildfiles', 'deps', 'executables', 'filter', 'kind', 'labels', 'loadfiles', 'rdeps', 'same_pkg_direct_rdeps', 'siblings', 'some', 'somepath', 'tests', 'visible']"), "{:?}", o.stderr);
    let o = q(&ws, "'unclosed", |_, _| {});
    assert_eq!(o.stderr, vec!["ERROR: Error while parsing ''unclosed': unclosed quotation"]);
    let o = q(&ws, "deps(//app:app) deps(//app:app)", |_, _| {});
    assert_eq!(o.exit_code, 2);
    assert!(o.stderr[0].contains("unexpected token 'deps' after query expression 'deps(//app:app)'"), "{:?}", o.stderr);
}

#[test]
fn missing_dependency_reports_loading_error_then_closure_error() {
    let ws = Ws::new(&[("foo/BUILD", "filegroup(name='a', srcs=[':b', '//other:doesnotexist'])\nfilegroup(name='b')")]);
    let o = q(&ws, "deps(//foo:a)", |_, _| {});
    assert_eq!(o.exit_code, 7, "{:?}", o.stderr);
    // Without --keep_going the Skyframe preload throws before any located edge error is reported.
    assert!(
        o.stderr.last().unwrap().starts_with("ERROR: Evaluation of query \"deps(//foo:a)\" failed: preloading transitive closure failed:"),
        "{:?}",
        o.stderr
    );
    let o = q(&ws, "deps(//foo:a) + //foo:b", |_, _| {});
    assert!(o.stderr.last().unwrap().starts_with("ERROR: Evaluation of subquery \"deps(//foo:a)\" failed (did you want to use --keep_going?): preloading transitive closure failed:"), "{:?}", o.stderr);
    for expr in ["somepath(//foo:a, //foo:b)", "deps(//foo:a)", "rdeps(//foo:a, //foo:b)", "allpaths(//foo:a, //foo:b)"] {
        let o = q(&ws, expr, |o, f| {
            o.keep_going = true;
            f.output = "label_kind".into();
        });
        assert_eq!(o.exit_code, 3, "{expr}: {:?}", o.stderr);
        let out = String::from_utf8_lossy(&o.stdout).to_string();
        assert!(out.contains("filegroup rule //foo:a") && out.contains("filegroup rule //foo:b"), "{expr}: {out}");
        assert!(o.stderr.iter().any(|l| l.contains("errors were encountered while computing transitive closure")), "{expr}: {:?}", o.stderr);
    }
}

#[test]
fn universe_scope_is_strict_in_sky_mode() {
    let ws = Ws::new(&[("a/BUILD", "filegroup(name='a')"), ("b/BUILD", "filegroup(name='b')")]);
    let o = q(&ws, "//a:a + //b:b", |o, _| {
        o.universe_scope = vec!["//a:a".into()];
        o.infer_universe_scope = true;
    });
    // Literals are not restricted to the universe (`getTargetsMatchingPattern`); only their forward
    // deps are "missing from graph" (see `round9_out_of_universe_targets_warn_instead_of_failing`).
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    assert_eq!(lines(&o), vec!["//a:a", "//b:b"]);
    let o = q(&ws, "deps(//b:b)", |o, _| o.universe_scope = vec!["//a:a".into()]);
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    assert!(o.stderr.iter().any(|l| l.contains("Targets were missing from graph: [//b:b]")), "{:?}", o.stderr);
    let o = q(&ws, "//a:a + //b:b", |o, _| o.infer_universe_scope = true);
    assert_eq!(lines(&o), vec!["//a:a", "//b:b"]);
}

#[test]
fn cquery_show_config_fragments_direct_and_transitive() {
    let ws = tree();
    let o = ws.post("//app:app", |_, _, popts| {
        popts.show_config_fragments = "direct".into();
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("[BazelRuleClassProvider$StrictActionEnvConfiguration, CoreOptions, PlatformConfiguration, PlatformOptions, ShellConfiguration]"), "{out}");

    // Plain Starlark rule has universal fragments
    let ws2 = Ws::new(&[
        ("BUILD", ""),
        ("defs.bzl", "def _impl(ctx): pass\nmy_rule = rule(implementation = _impl)\n"),
        ("pkg/BUILD", "load('//:defs.bzl', 'my_rule')\nmy_rule(name = 'r')\n"),
    ]);
    let o2 = ws2.post("//pkg:r", |_, _, popts| {
        popts.show_config_fragments = "direct".into();
    });
    assert_eq!(o2.exit_code, 0, "{:?}", o2.stderr);
    let out2 = String::from_utf8_lossy(&o2.stdout);
    assert!(out2.contains("[BazelRuleClassProvider$StrictActionEnvConfiguration, CoreOptions, PlatformConfiguration, PlatformOptions, ShellConfiguration]"), "{out2}");
}

#[test]
fn cquery_config_semantics_rejects_host_and_accepts_target_null() {
    let ws = tree();
    let o = ws.post("config(//lib:data.txt, host)", |_, _, _| {});
    assert_eq!(o.exit_code, 1, "{:?}", o.stderr);
    assert!(o.stderr.iter().any(|l| l.contains("'host' configuration no longer exists")), "{:?}", o.stderr);

    let o = ws.post("config(//lib:data.txt, null)", |_, _, _| {});
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("//lib:data.txt (null)"), "{out}");

    let o = ws.post("config(//lib:l, target)", |_, _, _| {});
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);

    let o = ws.post("config(//lib:l, null)", |_, _, _| {});
    assert_eq!(o.exit_code, 1, "{:?}", o.stderr);
    assert!(o.stderr.iter().any(|l| l.contains("No target (in) //lib:l could be found in the 'null' configuration")), "{:?}", o.stderr);

    let o = ws.post("config(//lib:l, exec)", |_, _, _| {});
    assert_eq!(o.exit_code, 1, "{:?}", o.stderr);
    assert!(o.stderr.iter().any(|l| l.contains("Unknown configuration ID 'exec'.")), "{:?}", o.stderr);
    assert!(o.stderr.iter().any(|l| l.contains("A short ID is any prefix of a full ID. cquery shows short IDs.")), "{:?}", o.stderr);
}

#[test]
fn cquery_consistent_labels_printer_installed() {
    let ws = tree();
    let o = ws.post("//app:app", |_, fopts, _| {
        fopts.printer.consistent = true;
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("@@//app:app"), "{out}");
}

#[test]
fn aquery_output_commands_escapes_newlines_and_unicode_like_bazel() {
    let ws = Ws::new(&[
        (
            "pkg/BUILD",
            "genrule(name = 'g', outs = ['g.out'], cmd = 'echo a\\necho b > $@')\n",
        ),
    ]);
    let o = ws.post("//pkg:g", |_, _, popts| {
        popts.aquery = true;
        popts.output = "commands".into();
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("\\u000A"), "commands output must contain \\u000A for newlines: {out}");
}

#[test]
fn cquery_starlark_real_target_object_and_providers() {
    let ws = tree();
    // Test indexing provider: target[DefaultInfo] and string indexing target["DefaultInfo"]
    let o = ws.post("//app:app", |_, _, popts| {
        popts.output = "starlark".into();
        popts.starlark_expr = "str(target['DefaultInfo'] != None) + ':' + str('DefaultInfo' in target) + ':' + str(target.label.name)".into();
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(out.trim(), "True:True:app");

    // Test returning non-string directly (Label)
    let o = ws.post("//app:app", |_, _, popts| {
        popts.output = "starlark".into();
        popts.starlark_expr = "target.label".into();
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(out.trim(), "@@//app:app");

    // Test target.files is depset
    let o = ws.post("//app:app", |_, _, popts| {
        popts.output = "starlark".into();
        popts.starlark_expr = "type(target.files) + ':' + str(len(target.files.to_list()))".into();
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(out.trim(), "depset:1");

    // Test str(target) prints label
    let o = ws.post("//app:app", |_, _, popts| {
        popts.output = "starlark".into();
        popts.starlark_expr = "str(target)".into();
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(out.trim(), "<target //app:app>");

    // Test target.rule does NOT exist
    let o = ws.post("//app:app", |_, _, popts| {
        popts.output = "starlark".into();
        popts.starlark_expr = "target.rule".into();
    });
    // Continues without aborting and logs to stderr
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.trim().is_empty());
}

#[test]
fn query_nodep_deps_filtering() {
    let ws = Ws::new(&[
        (
            "pkg/BUILD",
            "filegroup(name = 'g', srcs = [':dep'], visibility = ['//vis:friends'])\n\
             filegroup(name = 'dep')\n",
        ),
        (
            "vis/BUILD",
            "package_group(name = 'friends')\n",
        ),
    ]);
    // With nodep_deps = true (default), visibility dependency //vis:friends is traversed
    let o = q(&ws, "deps(//pkg:g)", |_, _| {});
    assert_eq!(lines(&o), vec!["//pkg:dep", "//pkg:g", "//vis:friends"]);

    // With nodep_deps = false, visibility is skipped
    let o = q(&ws, "deps(//pkg:g)", |o, f| {
        o.nodep_deps = false;
        f.nodep_deps = false;
    });
    assert_eq!(lines(&o), vec!["//pkg:dep", "//pkg:g"]);
}

#[test]
fn aquery_nestedset_sharing_in_proto() {
    let ws = Ws::new(&[
        ("BUILD", ""),
        ("tool.sh", "#!/bin/sh\n"),
        ("t/BUILD", "sh_binary(name = 'tool', srcs = ['//:tool.sh'])\n"),
        ("in1.txt", "1"),
        ("in2.txt", "2"),
        (
            "pkg/BUILD",
            "genrule(name = 'g1', srcs = ['//:in1.txt'], outs = ['g1.out'], tools = ['//t:tool'], cmd = './$(location //t:tool) < $< > $@')\n\
             genrule(name = 'g2', srcs = ['//:in2.txt'], outs = ['g2.out'], tools = ['//t:tool'], cmd = './$(location //t:tool) < $< > $@')\n",
        ),
    ]);
    let o = ws.post("//pkg:g1 + //pkg:g2", |_, _, popts| {
        popts.aquery = true;
        popts.output = "textproto".into();
        popts.include_artifacts = true;
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("transitive_dep_set_ids:"), "{out}");
}

#[test]
fn macro_instantiation_and_definition_stacks_parity() {
    let ws = Ws::new(&[
        ("BUILD", ""),
        (
            "defs.bzl",
            "def _impl(ctx):\n    pass\nmy_rule = rule(implementation = _impl)\n",
        ),
        (
            "inc.bzl",
            "load('//:defs.bzl', 'my_rule')\ndef g(name):\n    my_rule(name = name)\ndef f(name):\n    g(name)\n",
        ),
        (
            "pkg/BUILD",
            "load('//:inc.bzl', 'f')\nf(name = 'm_gen')\n",
        ),
    ]);
    let o = q(&ws, "//pkg:m_gen", |_, f| f.output = "build".into());
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(out.contains("# /ws/pkg/BUILD:2:2\nmy_rule(\n  name = \"m_gen\","), "Header must be BUILD call site: {out}");
    assert!(out.contains("generator_location = \"pkg/BUILD:2:2\""), "generator_location must be relative: {out}");
    assert!(out.contains("generator_function = \"f\""), "{out}");
    assert!(out.contains("# Rule m_gen instantiated at (most recent call last):\n#   /ws/pkg/BUILD:2:2 in <toplevel>\n#   /ws/inc.bzl:5:6   in f\n#   /ws/inc.bzl:3:12  in g"), "Instantiation stack must have correct function names: {out}");
    assert!(out.contains("# Rule my_rule defined at (most recent call last):\n#   /ws/defs.bzl:3:15 in <toplevel>"), "Definition stack must contain line:col: {out}");

    let o_loc = q(&ws, "//pkg:m_gen", |_, f| f.output = "location".into());
    let loc_out = String::from_utf8_lossy(&o_loc.stdout).to_string();
    assert!(loc_out.contains("/ws/pkg/BUILD:2:2: my_rule rule //pkg:m_gen"), "{loc_out}");

    let o_proto = q(&ws, "//pkg:m_gen", |_, f| {
        f.output = "proto".into();
        f.proto_instantiation_stack = true;
        f.proto_definition_stack = true;
    });
    let proto_out = String::from_utf8_lossy(&o_proto.stdout).to_string();
    assert!(proto_out.contains("pkg/BUILD:2:2: <toplevel>"), "{proto_out}");
    assert!(proto_out.contains("inc.bzl:5:6: f"), "{proto_out}");
    assert!(proto_out.contains("inc.bzl:3:12: g"), "{proto_out}");
    assert!(proto_out.contains("defs.bzl:3:15: <toplevel>"), "{proto_out}");
}

#[test]
fn cquery_output_files_honors_output_groups() {
    let ws = Ws::new(&[
        ("BUILD", ""),
        (
            "pkg/defs.bzl",
            "def _impl(ctx):\n    f1 = ctx.actions.declare_file('f1.txt')\n    ctx.actions.write(f1, '1')\n    f2 = ctx.actions.declare_file('f2.txt')\n    ctx.actions.write(f2, '2')\n    return [DefaultInfo(files = depset([f1])), OutputGroupInfo(custom = depset([f2]))]\nmy_rule = rule(implementation = _impl)\n",
        ),
        (
            "pkg/BUILD",
            "load('//pkg:defs.bzl', 'my_rule')\nmy_rule(name = 'r')\n",
        ),
    ]);
    let o = ws.post("//pkg:r", |_, _, popts| {
        popts.output = "files".into();
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("f1.txt"), "{out}");
    assert!(!out.contains("f2.txt"), "{out}");

    let o2 = ws.post("//pkg:r", |_, _, popts| {
        popts.output = "files".into();
        popts.output_groups = vec!["custom".into()];
    });
    assert_eq!(o2.exit_code, 0, "{:?}", o2.stderr);
    let out2 = String::from_utf8_lossy(&o2.stdout);
    assert!(!out2.contains("f1.txt"), "{out2}");
    assert!(out2.contains("f2.txt"), "{out2}");
}

#[test]
fn cquery_transitions_exact_format_and_names() {
    let ws = tree();
    let o = ws.post("//app:app", |_, _, popts| {
        popts.output = "transitions".into();
        popts.transitions = "full".into();
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    // Same config dependency has NoTransition and nothing after ->
    assert!(out.contains("#NoTransition ->\n") || out.contains("#NoTransition -> \n"), "{out}");
    // Null config dependency (source file) has (null transition) and nothing after ->
    assert!(out.contains("#(null transition) ->\n") || out.contains("#(null transition) -> \n"), "{out}");
    // Check diff format does NOT contain double brackets [[...]]
    assert!(!out.contains("[["), "Must not contain double brackets: {out}");
}

#[test]
fn aspects_in_query_and_post_analysis_graph() {
    let ws = Ws::new(&[
        ("BUILD", ""),
        (
            "t/BUILD",
            "sh_binary(name = 'tool', srcs = ['tool.sh'])\n",
        ),
        ("t/tool.sh", "#!/bin/sh\n"),
        (
            "pkg/defs.bzl",
            "MyInfo = provider()\ndef _asp_impl(target, ctx):\n    out = ctx.actions.declare_file('asp.txt')\n    ctx.actions.write(out, 'asp')\n    return [MyInfo(f = out)]\nmy_asp = aspect(implementation = _asp_impl, attrs = {'_tool': attr.label(default = '//t:tool')})\ndef _rule_impl(ctx):\n    pass\ntop_rule = rule(implementation = _rule_impl, attrs = {'deps': attr.label_list(aspects = [my_asp])})\nleaf_rule = rule(implementation = _rule_impl)\n",
        ),
        (
            "pkg/BUILD",
            "load('//pkg:defs.bzl', 'top_rule', 'leaf_rule')\ntop_rule(name = 'top', deps = [':leaf'])\nleaf_rule(name = 'leaf')\n",
        ),
    ]);

    // 1. query deps(//pkg:top) includes //t:tool
    let o = q(&ws, "deps(//pkg:top)", |_, _| {});
    assert!(lines(&o).contains(&"//t:tool".to_string()), "{:?}", lines(&o));

    // 2. query with aspect_deps=off omits //t:tool
    let o_off = q(&ws, "deps(//pkg:top)", |o, _| o.aspect_deps = "off".into());
    assert!(!lines(&o_off).contains(&"//t:tool".to_string()), "{:?}", lines(&o_off));

    // 3. xml output includes rule-input name="//t:tool"
    let o_xml = q(&ws, "//pkg:top", |_, f| f.output = "xml".into());
    let xml_out = String::from_utf8_lossy(&o_xml.stdout);
    assert!(xml_out.contains("<rule-input name=\"//t:tool\"/>"), "{xml_out}");

    // 4. cquery deps(//pkg:top) includes //t:tool
    let o_cq = ws.post("deps(//pkg:top)", |_, _, _| {});
    eprintln!("CQ_STDERR: {:?}", o_cq.stderr);
    let cq_out = String::from_utf8_lossy(&o_cq.stdout);
    eprintln!("CQ_OUT: {cq_out}");
    assert!(cq_out.contains("//t:tool"), "{cq_out}");

    // 5. aquery text contains AspectDescriptors: [//pkg:defs.bzl%my_asp()]
    let o_aq = ws.post("deps(//pkg:top)", |_, _, popts| {
        popts.aquery = true;
        popts.output = "text".into();
    });
    let aq_out = String::from_utf8_lossy(&o_aq.stdout);
    eprintln!("AQ_OUT: {aq_out}");
    assert!(aq_out.contains("AspectDescriptors: [//pkg:defs.bzl%my_asp()]"), "{aq_out}");
}

#[test]
fn keep_going_skips_bad_patterns_before_other_output() {
    let ws = Ws::new(&[("foo/BUILD", "filegroup(name='a', srcs=['x.txt'])")]);
    let o = q(&ws, "//nope:all + //foo:a", |o, _| o.keep_going = true);
    assert_eq!(o.exit_code, 3, "{:?}", o.stderr);
    assert!(o.stderr[0].starts_with("ERROR: Skipping '//nope:all': no such package 'nope'"), "{:?}", o.stderr);
    let w = o.stderr.iter().position(|l| l.starts_with("WARNING: --keep_going specified")).unwrap();
    assert_eq!(w, 1, "{:?}", o.stderr);
    assert_eq!(lines(&o), vec!["//foo:a"]);
    // Without --keep_going the preload failure is reported verbatim.
    let o = q(&ws, "//nope:all + //foo:a", |_, _| {});
    assert_eq!(o.exit_code, 7);
    assert!(o.stderr[0].starts_with("ERROR: no such package 'nope'"), "{:?}", o.stderr);
}

#[test]
fn referenced_missing_source_file_is_a_target() {
    let ws = Ws::new(&[("foo/BUILD", "filegroup(name='a', srcs=['missing.txt'])")]);
    let o = q(&ws, "//foo:missing.txt", |_, _| {});
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    assert_eq!(lines(&o), vec!["//foo:missing.txt"]);
    let o = q(&ws, "deps(//foo:a)", |_, _| {});
    assert_eq!(lines(&o), vec!["//foo:a", "//foo:missing.txt"]);
    let o = q(&ws, "//foo:other.txt", |_, _| {});
    assert_eq!(o.exit_code, 7);
}

#[test]
fn closure_errors_are_reported_by_label_visitor_and_path_visitor() {
    let ws = Ws::new(&[(
        "foo/BUILD",
        "filegroup(name='bad', srcs=['//gone:x'])\nfilegroup(name='top', srcs=[':bad'])",
    )]);
    let loc_err = |l: &String| l.starts_with("ERROR: /ws/foo/BUILD:1:") && l.contains("and referenced by '//foo:bad'");
    // deps: the Skyframe preload and the label visitor both report the edge.
    let o = q(&ws, "deps(//foo:top)", |o, _| o.keep_going = true);
    assert_eq!(o.stderr.iter().filter(|l| loc_err(l)).count(), 2, "{:?}", o.stderr);
    // allpaths (graphless): preload + the path visitor, which blames the discoverer of `bad`.
    let o = q(&ws, "allpaths(//foo:top, //foo:bad)", |o, _| o.keep_going = true);
    assert!(o.stderr.iter().any(|l| l.starts_with("ERROR: /ws/foo/BUILD:2:") && l.ends_with("and referenced by '//foo:top'")), "{:?}", o.stderr);
    assert!(o.stderr.iter().any(|l| l.contains("failed: errors were encountered while computing transitive closure")), "{:?}", o.stderr);
}

// ---- round 6A -------------------------------------------------------------------------------

#[test]
fn package_defaults_are_not_explicitly_specified() {
    let ws = Ws::new(&[(
        "p/BUILD",
        "package(default_testonly = False, default_deprecation = 'old', default_visibility = ['//visibility:public'])\n\
         filegroup(name = 'a', srcs = [])\n",
    ), (
        "t/BUILD",
        "package(default_testonly = True, features = ['pf'])\nfilegroup(name = 'b', srcs = [], features = ['own'])\nfilegroup(name = 'c', srcs = [])\n",
    )]);
    // `Rule.isAttributeValueExplicitlySpecified` is false for package() defaults: build output omits them.
    let o = q(&ws, "//p:a", |_, f| f.output = "build".into());
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(!out.contains("testonly") && !out.contains("deprecation") && !out.contains("visibility"), "{out}");
    let o = q(&ws, "//t:b", |_, f| f.output = "build".into());
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(!out.contains("testonly"), "{out}");
    // features: only the rule's own value is shown (package features merge elsewhere).
    assert!(out.contains("features = [\"own\"]"), "{out}");
    let o = q(&ws, "//t:c", |_, f| f.output = "build".into());
    assert!(!String::from_utf8_lossy(&o.stdout).contains("features"), "{:?}", o.stdout);
}

#[test]
fn package_group_packages_are_ordered_by_category() {
    let ws = Ws::new(&[(
        "BUILD",
        "package_group(name = 'pg', packages = ['//a/...', '//b', '-//c', '-//d/...', '//e'])\n",
    )]);
    let o = q(&ws, "//:pg", |_, f| {
        f.output = "build".into();
        f.package_group_build_output = true;
    });
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    let pos = |s: &str| out.find(s).unwrap_or_else(|| panic!("{s} missing in {out}"));
    // single positives, `...` positives, single negatives, `...` negatives, public
    assert!(pos("\"//b\"") < pos("\"//e\"") && pos("\"//e\"") < pos("\"//a/...\""), "{out}");
    assert!(pos("\"//a/...\"") < pos("\"-//c\"") && pos("\"-//c\"") < pos("\"-//d/...\""), "{out}");
}

#[test]
fn deps_without_depth_reports_located_missing_package_error() {
    let ws = Ws::new(&[("foo/BUILD", "filegroup(name='bad', srcs=['//gone:x'])\n")]);
    let o = q(&ws, "deps(//foo:bad)", |_, _| {});
    assert_eq!(o.exit_code, 7, "{:?}", o.stderr);
    assert!(o.stderr.iter().any(|l| l.starts_with("ERROR: /ws/foo/BUILD:1:") && l.ends_with("and referenced by '//foo:bad'")), "{:?}", o.stderr);
    assert!(o.stderr.iter().any(|l| l.contains("preloading transitive closure failed")), "{:?}", o.stderr);
}

#[test]
fn visible_treats_external_public_visibility_as_public() {
    // `//visibility:public` is the special label in every repository: here the main repository.
    let ws = Ws::new(&[
        ("a/BUILD", "filegroup(name='x', srcs=[], visibility=['//visibility:public'])\n"),
        ("b/BUILD", "filegroup(name='y', srcs=['//a:x'])\n"),
    ]);
    assert_eq!(ql(&ws, "visible(//b:y, //a:x)"), vec!["//a:x"]);
}

#[test]
fn run_genquery_formats_expression_with_opts() {
    let ws = tree();
    let loader = ws.resolver().loader().clone();
    let out = crate::run_genquery(&loader, "deps(//app:app)", &[], false, &[]).unwrap();
    let text = String::from_utf8_lossy(&out).to_string();
    assert!(text.lines().any(|l| l == "//app:app"), "{text}");
    assert!(text.lines().any(|l| l == "//lib:l"), "{text}");
    let kinds = crate::run_genquery(&loader, "//lib:l", &[], false, &["--output=label_kind".to_string()]).unwrap();
    assert_eq!(String::from_utf8_lossy(&kinds), "genrule rule //lib:l\n");
    // options that `GenQuery.java` rejects
    assert!(crate::run_genquery(&loader, "//lib:l", &[], false, &["--keep_going".to_string()]).is_err());
    assert!(crate::run_genquery(&loader, "//lib:l", &[], false, &["--universe_scope=//lib:l".to_string()]).is_err());
    assert!(crate::run_genquery(&loader, "nonsense(", &[], false, &[]).is_err());
}

#[test]
fn aquery_expands_top_level_test_suites_without_analysing_them() {
    // `//:s` depends on a private test of another package: analysing the suite would be a visibility
    // error, but aquery expands the suite into its tests and never analyses it.
    let ws = Ws::new(&[
        ("BUILD", "test_suite(name = 's', tests = ['//p:t'])\n"),
        ("p/BUILD", "sh_test(name = 't', srcs = ['t.sh'])\n"),
        ("p/t.sh", "#!/bin/sh"),
    ]);
    let o = ws.post("//:s", |_, _, popts| {
        popts.aquery = true;
        popts.output = "text".into();
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    assert!(!String::from_utf8_lossy(&o.stdout).contains("Visibility error"));
    assert!(o.stderr.iter().all(|l| !l.contains("Visibility error")), "{:?}", o.stderr);
}

#[test]
fn round9_out_of_universe_targets_warn_instead_of_failing() {
    let ws = tree();
    // `SkyQueryEnvironment.getFwdDeps`: targets outside the universe have no graph node.
    let o = q(&ws, "deps(//lib:l)", |o, _| o.universe_scope = vec!["//app:app".into()]);
    // //lib:l is inside the closure of //app:app; nothing is missing.
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let o = q(&ws, "deps(//lib:fg)", |o, _| o.universe_scope = vec!["//app:app".into()]);
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    assert_eq!(lines(&o), vec!["//lib:fg"]);
    assert!(o.stderr.iter().any(|l| l == "WARNING: Targets were missing from graph: [//lib:fg]"), "{:?}", o.stderr);
}

#[test]
fn round9_environment_group_is_a_target() {
    let ws = Ws::new(&[(
        "pkgs/BUILD",
        "environment(name = 'a')\nenvironment(name = 'b')\nenvironment_group(name = 'envs', environments = [':a', ':b'], defaults = [':a'])\n",
    )]);
    assert_eq!(ql(&ws, "//pkgs:envs"), vec!["//pkgs:envs"]);
    let o = q(&ws, "//pkgs:envs", |_, f| f.output = "label_kind".into());
    assert_eq!(lines(&o), vec!["environment group //pkgs:envs"], "{:?}", o.stderr);
    let o = q(&ws, "//pkgs:envs", |_, f| f.output = "xml".into());
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(out.contains("<environment-group location=") && out.contains("name=\"//pkgs:envs\""), "{out}");
    assert!(out.contains("<list name=\"environments\">") && out.contains("<label value=\"//pkgs:a\"/>"), "{out}");
    assert!(out.contains("<list name=\"defaults\">"), "{out}");
    assert_eq!(ql(&ws, "deps(//pkgs:envs)"), vec!["//pkgs:envs"]);
}

#[test]
fn round9_labels_crossing_a_subpackage_boundary_are_not_assumed_input_files() {
    let ws = Ws::new(&[
        ("deep/x/BUILD", "filegroup(name = 'x', srcs = ['y/f', '//deep/x/y:g'])\n"),
        ("deep/x/y/BUILD", "exports_files(['f', 'g'])\n"),
        ("deep/x/y/f", ""),
        ("deep/x/y/g", ""),
    ]);
    let o = q(&ws, "deps(//deep/x:x)", |o, _| o.keep_going = true);
    let l = lines(&o);
    assert!(!l.iter().any(|t| t == "//deep/x:y/f"), "{l:?} {:?}", o.stderr);
    assert!(l.contains(&"//deep/x/y:g".to_string()), "{l:?}");
}
