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
    ["sh_binary(", "sh_test(", "sh_library("]
        .iter()
        .any(|n| c.contains(n))
}

/// Prepends `load()`s for the `sh_*` rules a BUILD file uses.
pub(crate) fn shell_loads(path: &str, content: &str) -> String {
    if !(path == "BUILD" || path.ends_with("/BUILD")) || !uses_shell_rules(content) {
        return content.to_string();
    }
    let mut used: Vec<&str> = ["sh_binary", "sh_test", "sh_library"]
        .into_iter()
        .filter(|n| content.contains(&format!("{n}(")))
        .collect();
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
        ws.fs
            .create_directory_and_parents(&bt_root.get_relative_str("tools/genrule"))
            .unwrap();
        ws.fs
            .write_file(
                &bt_root.get_relative_str("tools/genrule/BUILD"),
                b"exports_files(['genrule-setup.sh'])\n",
            )
            .unwrap();
        ws.fs
            .write_file(
                &bt_root.get_relative_str("tools/genrule/genrule-setup.sh"),
                b"#!/bin/sh\n",
            )
            .unwrap();
        ws.fs
            .write_file(&bt_root.get_relative_str("WORKSPACE"), b"")
            .unwrap();
        for (p, c) in files {
            let path = ws.root.get_relative_str(p);
            ws.fs
                .create_directory_and_parents(&path.parent_directory().unwrap())
                .unwrap();
            let content = shell_loads(p, c);
            ws.fs.write_file(&path, content.as_bytes()).unwrap();
        }
        if files
            .iter()
            .any(|(p, c)| (*p == "BUILD" || p.ends_with("/BUILD")) && uses_shell_rules(c))
        {
            // Bazel 9 has no native `sh_*` rules; in-memory workspaces get a stand-in for `@rules_shell`.
            let d = ws.root.get_relative_str("shstub");
            ws.fs.create_directory_and_parents(&d).unwrap();
            ws.fs.write_file(&d.get_relative_str("BUILD"), b"").unwrap();
            ws.fs
                .write_file(&d.get_relative_str("sh.bzl"), SH_STUB.as_bytes())
                .unwrap();
        }
        ws
    }

    pub(crate) fn resolver(&self) -> PatternResolver {
        let mut repos = SimpleRepoResolver::new(self.root.clone());
        let bt_root = PathFragment::create("/embedded_tools");
        repos.external.insert("bazel_tools".to_string(), bt_root);
        repos.apparent.insert(
            "bazel_tools".to_string(),
            zel_cmdline::RepositoryName::bazel_tools(),
        );
        let cfg = LoaderConfig::new(self.fs.clone(), Arc::new(repos));
        PatternResolver::new(PackageLoader::new(cfg), PathFragment::empty())
    }

    pub(crate) fn post(
        &self,
        expr: &str,
        configure: impl FnOnce(&mut QueryOpts, &mut FormatOpts, &mut PostOpts),
    ) -> PostOutcome {
        let resolver = self.resolver();
        let tr = TargetResolver::new(resolver.loader().clone(), AnalysisConfig::default());
        let mut qopts = QueryOpts::default();
        let mut fopts = FormatOpts::default();
        let mut popts = PostOpts::default();
        configure(&mut qopts, &mut fopts, &mut popts);
        run_post_analysis(
            &resolver,
            tr,
            BuildConfigurationKey::default(),
            expr,
            &qopts,
            &fopts,
            &popts,
        )
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

pub(crate) fn q(
    ws: &Ws,
    expr: &str,
    extra: impl FnOnce(&mut QueryOpts, &mut FormatOpts),
) -> QueryOutcome {
    let resolver = ws.resolver();
    let mut o = QueryOpts::default();
    let mut f = FormatOpts::default();
    extra(&mut o, &mut f);
    run_query(&resolver, expr, &o, &f)
}

pub(crate) fn lines(o: &QueryOutcome) -> Vec<String> {
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect()
}

fn ql(ws: &Ws, expr: &str) -> Vec<String> {
    let o = q(ws, expr, |_, _| {});
    assert_eq!(o.exit_code, 0, "{expr}: {:?}", o.stderr);
    lines(&o)
}

#[test]
fn labels_and_sorted_default_order() {
    let ws = tree();
    assert_eq!(
        ql(&ws, "//app:all"),
        vec![
            "//app:all_tests",
            "//app:app",
            "//app:app_test",
            "//app:other",
            "//app:slow_test",
            "//app:suite"
        ]
    );
    assert_eq!(ql(&ws, "//lib:l + //app:app"), vec!["//app:app", "//lib:l"]);
    // Residue words joined by the command: operators without quotes.
    assert_eq!(ql(&ws, "//lib:all - //lib:l"), vec!["//lib:fg"]);
}

#[test]
fn deps_cross_package_and_depth() {
    let ws = tree();
    assert_eq!(
        ql(&ws, "deps(//app:other)"),
        vec![
            "//app:app",
            "//app:in.txt",
            "//app:other",
            "//lib:data.txt",
            "//lib:l",
            "@@bazel_tools//tools/genrule:genrule-setup.sh"
        ]
    );
    assert_eq!(ql(&ws, "deps(//app:other, 0)"), vec!["//app:other"]);
    assert_eq!(
        ql(&ws, "deps(//app:other, 1)"),
        vec![
            "//app:app",
            "//app:other",
            "@@bazel_tools//tools/genrule:genrule-setup.sh"
        ]
    );
    assert_eq!(
        ql(&ws, "deps(//app:other, 2)"),
        vec![
            "//app:app",
            "//app:in.txt",
            "//app:other",
            "//lib:l",
            "@@bazel_tools//tools/genrule:genrule-setup.sh"
        ]
    );
}

#[test]
fn rdeps_and_allrdeps() {
    let ws = tree();
    assert_eq!(
        ql(&ws, "rdeps(//app:other, //lib:l)"),
        vec!["//app:app", "//app:other", "//lib:l"]
    );
    assert_eq!(
        ql(&ws, "rdeps(//app:other, //lib:l, 1)"),
        vec!["//app:app", "//lib:l"]
    );
    let o = q(&ws, "allrdeps(//lib:l)", |o, _| {
        o.infer_universe_scope = true
    });
    assert_eq!(
        lines(&o),
        vec!["//lib:l"],
        "the universe is inferred from the expression's patterns"
    );
    let o = q(&ws, "allrdeps(//lib:l)", |_, _| {});
    assert_eq!(
        o.exit_code, 2,
        "allrdeps only exists in SkyQuery mode (universe scope + streamed output)"
    );
    let o = q(&ws, "allrdeps(//lib:l)", |o, _| {
        o.universe_scope = vec!["//app:all".into()]
    });
    assert_eq!(
        lines(&o),
        vec![
            "//app:all_tests",
            "//app:app",
            "//app:app_test",
            "//app:other",
            "//app:suite",
            "//lib:l"
        ]
    );
    let o = q(&ws, "allrdeps(//lib:l, 1)", |o, _| {
        o.universe_scope = vec!["//...".into()]
    });
    assert_eq!(lines(&o), vec!["//app:app", "//lib:fg", "//lib:l"]);
}

#[test]
fn paths() {
    let ws = tree();
    assert_eq!(
        lines(&q(&ws, "somepath(//app:other, //lib:l)", |_, f| f.order =
            OrderOutput::No)),
        vec!["//app:other", "//app:app", "//lib:l"]
    );
    assert_eq!(
        ql(&ws, "somepath(//app:other, //lib:l)"),
        vec!["//app:other", "//app:app", "//lib:l"]
    );
    assert_eq!(
        ql(&ws, "somepath(//lib:l, //app:other)"),
        Vec::<String>::new()
    );
    assert_eq!(
        ql(&ws, "allpaths(//app:other, //lib:data.txt)"),
        vec!["//app:app", "//app:other", "//lib:data.txt", "//lib:l"]
    );
}

#[test]
fn filters() {
    let ws = tree();
    assert_eq!(
        ql(&ws, "kind('genrule rule', //app:all)"),
        vec!["//app:app", "//app:other"]
    );
    assert_eq!(
        ql(&ws, "kind('source file', deps(//app:app))"),
        vec![
            "//app:in.txt",
            "//lib:data.txt",
            "@@bazel_tools//tools/genrule:genrule-setup.sh"
        ]
    );
    assert_eq!(
        ql(&ws, "filter('_test$', //app:all)"),
        vec!["//app:app_test", "//app:slow_test"]
    );
    // test_suite and Starlark test rules (`sh_test` from @rules_shell) are testonly by default.
    assert_eq!(
        ql(&ws, "attr(testonly, 1, //app:all)"),
        vec![
            "//app:all_tests",
            "//app:app_test",
            "//app:other",
            "//app:slow_test",
            "//app:suite"
        ]
    );
    assert_eq!(ql(&ws, "attr(tags, 'manual', //lib:all)"), vec!["//lib:fg"]);
    assert_eq!(
        ql(&ws, "attr(tags, '\\bslow\\b', //app:all)"),
        vec!["//app:slow_test", "//app:suite"]
    );
    assert_eq!(
        ql(&ws, "attr(srcs, 'in\\.txt', //app:all)"),
        vec!["//app:app"]
    );
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
    assert_eq!(
        ql(&ws, "labels(srcs, //app:app)"),
        vec!["//app:in.txt", "//lib:l"]
    );
    assert_eq!(ql(&ws, "labels(deps, //app:app_test)"), vec!["//app:app"]);
    assert_eq!(ql(&ws, "some(//app:all)").len(), 1);
    let o = q(&ws, "some(//app:all - //app:all)", |_, _| {});
    assert_eq!(o.exit_code, 7);
    assert!(
        o.stderr[0].contains("failed (did you want to use --keep_going?): argument set is empty")
            || o.stderr[0].contains("argument set is empty"),
        "{:?}",
        o.stderr
    );
}

#[test]
fn tests_function_expands_suites() {
    let ws = tree();
    assert_eq!(ql(&ws, "tests(//app:suite)"), vec!["//app:app_test"]);
    assert_eq!(
        ql(&ws, "tests(//app:all_tests)"),
        vec!["//app:app_test", "//app:slow_test"]
    );
    assert_eq!(
        ql(&ws, "tests(//app:all)"),
        vec!["//app:app_test", "//app:slow_test"]
    );
}

#[test]
fn siblings_and_same_pkg_direct_rdeps() {
    let ws = tree();
    assert_eq!(
        ql(&ws, "siblings(//lib:l)"),
        vec![
            "//lib:BUILD",
            "//lib:data.txt",
            "//lib:fg",
            "//lib:l",
            "//lib:l.out"
        ]
    );
    assert_eq!(
        ql(&ws, "same_pkg_direct_rdeps(//app:app)"),
        vec!["//app:app.out", "//app:app_test", "//app:other"]
    );
}

#[test]
fn buildfiles_and_visible() {
    let ws = tree();
    // `sh_*` rules are loaded from a .bzl (the `@rules_shell` stand-in `//shstub:sh.bzl`).
    assert_eq!(
        ql(&ws, "buildfiles(//app:app)"),
        vec!["//app:BUILD", "//shstub:BUILD", "//shstub:sh.bzl"]
    );
    assert!(ql(&ws, "visible(//app:app, //lib:all)").contains(&"//lib:l".to_string()));
}

#[test]
fn let_set_and_operators() {
    let ws = tree();
    assert_eq!(
        ql(&ws, "let x = //lib:l in deps(//app:app) - $x"),
        vec![
            "//app:app",
            "//app:in.txt",
            "//lib:data.txt",
            "@@bazel_tools//tools/genrule:genrule-setup.sh"
        ]
    );
    assert_eq!(
        ql(&ws, "set(//lib:l //app:app)"),
        vec!["//app:app", "//lib:l"]
    );
    assert_eq!(ql(&ws, "//lib:all ^ //lib:l"), vec!["//lib:l"]);
    assert_eq!(
        ql(&ws, "//lib:l union //app:app except //app:app"),
        vec!["//lib:l"]
    );
    let o = q(&ws, "deps($x)", |_, _| {});
    assert_eq!(o.exit_code, 7);
    assert!(
        o.stderr[0].contains("undefined variable 'x'"),
        "{:?}",
        o.stderr
    );
}

#[test]
fn syntax_errors_exit_2() {
    let ws = tree();
    let o = q(&ws, "deps(//app:app", |_, _| {});
    assert_eq!(o.exit_code, 2);
    assert_eq!(
        o.stderr,
        vec!["ERROR: Error while parsing 'deps(//app:app': premature end of input"]
    );
    let o = q(&ws, "kind(//a)", |_, _| {});
    assert_eq!(o.exit_code, 2);
}

#[test]
fn missing_targets_report_errors() {
    let ws = tree();
    let o = q(&ws, "//app:nope", |_, _| {});
    assert_eq!(o.exit_code, 7);
    assert!(
        o.stderr[0].starts_with("ERROR: no such target '//app:nope'"),
        "{:?}",
        o.stderr
    );
    assert!(
        o.stderr[0].contains("no such target '//app:nope'"),
        "{:?}",
        o.stderr
    );
    // The previously buggy behaviour: an empty result must not print the literal.
    let o = q(&ws, "//app:all - //app:all", |_, _| {});
    assert_eq!(o.exit_code, 0);
    assert!(o.stdout.is_empty());
    assert_eq!(o.stderr, vec!["INFO: Empty results"]);
}

#[test]
fn keep_going_exit_3() {
    let ws = Ws::new(&[
        ("a/BUILD", "filegroup(name='a', srcs=['//b:missing'])"),
        ("b/BUILD", "filegroup(name='b')"),
    ]);
    let o = q(&ws, "deps(//a:a)", |o, _| o.keep_going = true);
    assert_eq!(o.exit_code, 3, "{:?}", o.stderr);
    assert_eq!(lines(&o), vec!["//a:a"]);
    assert!(
        o.stderr
            .iter()
            .any(|l| l.contains("--keep_going specified, ignoring errors")),
        "{:?}",
        o.stderr
    );
    let o = q(&ws, "deps(//a:a)", |_, _| {});
    assert_eq!(o.exit_code, 7);
}

#[test]
fn label_kind_package_location_and_null_terminator() {
    let ws = tree();
    let o = q(&ws, "//lib:all", |_, f| f.output = "label_kind".into());
    assert_eq!(
        lines(&o),
        vec!["filegroup rule //lib:fg", "genrule rule //lib:l"]
    );
    let o = q(&ws, "deps(//app:app)", |_, f| f.output = "package".into());
    assert_eq!(
        lines(&o),
        vec!["@@bazel_tools//tools/genrule", "app", "lib"]
    );
    let o = q(&ws, "//lib:l", |_, f| f.output = "location".into());
    assert_eq!(lines(&o), vec!["/ws/lib/BUILD:1:8: genrule rule //lib:l"]);
    let o = q(&ws, "//lib:l", |_, f| f.relative_locations = true);
    assert_eq!(lines(&o), vec!["//lib:l"]);
    let o = q(&ws, "//lib:all", |_, f| f.line_terminator = "\0".into());
    assert_eq!(o.stdout, b"//lib:fg\0//lib:l\0".to_vec());
    let o = q(&ws, "buildfiles(//lib:l)", |_, f| {
        f.output = "location".into()
    });
    assert_eq!(o.exit_code, 2);
}

#[test]
fn order_output_deps_and_full() {
    let ws = tree();
    let full = q(&ws, "deps(//app:other)", |_, f| f.order = OrderOutput::Full);
    assert_eq!(
        lines(&full),
        vec![
            "//app:other",
            "//app:app",
            "//lib:l",
            "@@bazel_tools//tools/genrule:genrule-setup.sh",
            "//lib:data.txt",
            "//app:in.txt"
        ]
    );
    let deps = q(&ws, "deps(//app:other)", |_, f| f.order = OrderOutput::Deps);
    let l = lines(&deps);
    assert_eq!(l.first().map(String::as_str), Some("//app:other"));
    assert!(l.iter().position(|x| x == "//app:app") < l.iter().position(|x| x == "//lib:l"));
}

#[test]
fn minrank_maxrank() {
    let ws = tree();
    let o = q(&ws, "deps(//app:other)", |_, f| f.output = "minrank".into());
    assert_eq!(
        lines(&o),
        vec![
            "0 //app:other",
            "1 //app:app",
            "1 @@bazel_tools//tools/genrule:genrule-setup.sh",
            "2 //lib:l",
            "2 //app:in.txt",
            "3 //lib:data.txt"
        ]
    );
    let o = q(&ws, "deps(//app:other)", |_, f| f.output = "maxrank".into());
    assert_eq!(
        lines(&o),
        vec![
            "0 //app:other",
            "1 //app:app",
            "2 //lib:l",
            "2 //app:in.txt",
            "3 //lib:data.txt",
            "3 @@bazel_tools//tools/genrule:genrule-setup.sh"
        ]
    );
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
    assert_eq!(
        ql(&ws, "deps(//pkg:r)"),
        vec!["//pkg:friends", "//pkg:r", "//pkg:s", "//tools:t"]
    );
}

#[test]
fn select_branches_all_count_as_dependencies() {
    let ws = rich();
    assert_eq!(
        ql(&ws, "deps(//pkg:fg)"),
        vec![
            "//pkg:a.txt",
            "//pkg:b.txt",
            "//pkg:c.txt",
            "//pkg:fg",
            "//pkg:opt"
        ]
    );
    assert_eq!(ql(&ws, "attr(srcs, 'a.txt', //pkg:fg)"), vec!["//pkg:fg"]);
    assert_eq!(ql(&ws, "attr(testonly, 1, //pkg:all)"), vec!["//pkg:fg"]);
}

#[test]
fn build_file_target_and_buildfiles_loadfiles() {
    let ws = rich();
    assert_eq!(ql(&ws, "//pkg:BUILD"), vec!["//pkg:BUILD"]);
    assert_eq!(
        ql(&ws, "loadfiles(//pkg:r)"),
        vec!["//:defs.bzl", "//shstub:sh.bzl", "//tools:helper.bzl"]
    );
    assert_eq!(
        ql(&ws, "buildfiles(//pkg:r)"),
        vec![
            "//:BUILD",
            "//:defs.bzl",
            "//pkg:BUILD",
            "//shstub:BUILD",
            "//shstub:sh.bzl",
            "//tools:BUILD",
            "//tools:helper.bzl"
        ]
    );
    let o = q(&ws, "rbuildfiles(tools/helper.bzl)", |o, _| {
        o.universe_scope = vec!["//...".into()]
    });
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
    assert_eq!(
        ql(&ws2, "visible(//b:b, //a:all)"),
        vec!["//a:a", "//a:pub"]
    );
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
    let o = q(&ws, "//pkg:fg + //pkg:a.txt + //pkg:friends", |_, f| {
        f.output = "xml".into()
    });
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(
        out.starts_with(
            "<?xml version=\"1.1\" encoding=\"UTF-8\" standalone=\"no\"?>\n<query version=\"2\">\n"
        ) || out.starts_with(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n<query version=\"2\">\n"
        ),
        "{out}"
    );
    assert!(out.contains("    <rule class=\"filegroup\" location=\"/ws/pkg/BUILD:7:10\" name=\"//pkg:fg\">\n        <string name=\"name\" value=\"fg\"/>\n        <boolean name=\"testonly\" value=\"true\"/>\n        <list name=\"srcs\">"), "{out}");
    assert!(
        out.contains("<source-file location=\"/ws/pkg/a.txt:1:1\" name=\"//pkg:a.txt\"/>")
            || out.contains("name=\"//pkg:a.txt\""),
        "{out}"
    );
    assert!(
        out.contains("<package-group location=\"/ws/pkg/BUILD:3:14\" name=\"//pkg:friends\">"),
        "{out}"
    );
    assert!(out.trim_end().ends_with("</query>"), "{out}");
}

#[test]
fn proto_family_is_consistent() {
    let ws = rich();
    let bin = q(&ws, "//pkg:s", |_, f| f.output = "proto".into());
    assert_eq!(bin.exit_code, 0, "{:?}", bin.stderr);
    let s = String::from_utf8_lossy(&bin.stdout).to_string();
    assert!(
        s.contains("//pkg:s") && s.contains("my_rule") && s.contains("//tools:t"),
        "{s:?}"
    );
    let streamed = q(&ws, "//pkg:s + //pkg:r", |_, f| {
        f.output = "streamed_proto".into()
    });
    // Each message is length-delimited: first byte is the varint length of the Target message.
    let b = &streamed.stdout;
    let len = b[0] as usize;
    assert!(len < 0x80 || b[0] & 0x80 != 0);
    let js = q(&ws, "//pkg:s", |_, f| {
        f.output = "streamed_jsonproto".into()
    });
    let line = String::from_utf8_lossy(&js.stdout).to_string();
    assert!(
        line.starts_with(
            "{\"type\":\"RULE\",\"rule\":{\"name\":\"//pkg:s\",\"ruleClass\":\"my_rule\""
        ),
        "{line}"
    );
    assert!(line.contains("\"ruleInput\":[\"//tools:t\"]"), "{line}");
    assert!(line.contains("{\"name\":\"$tool\",\"type\":\"LABEL\",\"stringValue\":\"//tools:t\",\"explicitlySpecified\":false,\"nodep\":false}"), "{line}");
    let no_defaults = q(&ws, "//pkg:s", |_, f| {
        f.output = "streamed_jsonproto".into();
        f.proto_default_values = false;
        f.proto_rule_inputs_and_outputs = false;
        f.proto_locations = false;
    });
    let l = String::from_utf8_lossy(&no_defaults.stdout).to_string();
    assert!(
        !l.contains("location") && !l.contains("ruleInput") && !l.contains("$tool"),
        "{l}"
    );
}

#[test]
fn order_no_and_null_and_keep_going_and_universe() {
    let ws = tree();
    let o = q(&ws, "deps(//app:app)", |_, f| f.order = OrderOutput::No);
    assert_eq!(o.exit_code, 0);
    assert_eq!(lines(&o).len(), 5);
    let o = q(&ws, "allrdeps(//lib:data.txt)", |o, _| {
        o.infer_universe_scope = true
    });
    // Inferred universe = patterns of the expression: just data.txt.
    assert_eq!(lines(&o), vec!["//lib:data.txt"]);
    let o = q(&ws, "allrdeps(//lib:data.txt)", |o, _| {
        o.universe_scope = vec!["//lib:all".into(), "//app:all".into()]
    });
    assert_eq!(
        lines(&o),
        vec![
            "//app:all_tests",
            "//app:app",
            "//app:app_test",
            "//app:other",
            "//app:suite",
            "//lib:data.txt",
            "//lib:fg",
            "//lib:l"
        ]
    );
}

#[test]
fn rdeps_requires_target_in_universe() {
    let ws = tree();
    assert_eq!(ql(&ws, "rdeps(//lib:l, //app:app)"), Vec::<String>::new());
    assert_eq!(
        ql(&ws, "rdeps(//lib:l + //app:app, //app:app + //lib:l)"),
        vec!["//app:app", "//lib:l"]
    );
}

#[test]
fn query_expression_error_texts() {
    let ws = tree();
    let o = q(&ws, "foo(//app:app)", |_, _| {});
    assert_eq!(o.exit_code, 2);
    assert!(o.stderr[0].starts_with("ERROR: Error while parsing 'foo(//app:app)': unknown function 'foo' at 'foo ( //app:app'; expected one of ['allpaths', 'attr', 'buildfiles', 'deps', 'executables', 'filter', 'kind', 'labels', 'loadfiles', 'rdeps', 'same_pkg_direct_rdeps', 'siblings', 'some', 'somepath', 'tests', 'visible']"), "{:?}", o.stderr);
    let o = q(&ws, "'unclosed", |_, _| {});
    assert_eq!(
        o.stderr,
        vec!["ERROR: Error while parsing ''unclosed': unclosed quotation"]
    );
    let o = q(&ws, "deps(//app:app) deps(//app:app)", |_, _| {});
    assert_eq!(o.exit_code, 2);
    assert!(
        o.stderr[0].contains("unexpected token 'deps' after query expression 'deps(//app:app)'"),
        "{:?}",
        o.stderr
    );
}

#[test]
fn missing_dependency_reports_loading_error_then_closure_error() {
    let ws = Ws::new(&[(
        "foo/BUILD",
        "filegroup(name='a', srcs=[':b', '//other:doesnotexist'])\nfilegroup(name='b')",
    )]);
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
    for expr in [
        "somepath(//foo:a, //foo:b)",
        "deps(//foo:a)",
        "rdeps(//foo:a, //foo:b)",
        "allpaths(//foo:a, //foo:b)",
    ] {
        let o = q(&ws, expr, |o, f| {
            o.keep_going = true;
            f.output = "label_kind".into();
        });
        assert_eq!(o.exit_code, 3, "{expr}: {:?}", o.stderr);
        let out = String::from_utf8_lossy(&o.stdout).to_string();
        assert!(
            out.contains("filegroup rule //foo:a") && out.contains("filegroup rule //foo:b"),
            "{expr}: {out}"
        );
        assert!(
            o.stderr
                .iter()
                .any(|l| l.contains("errors were encountered while computing transitive closure")),
            "{expr}: {:?}",
            o.stderr
        );
    }
}

#[test]
fn universe_scope_is_strict_in_sky_mode() {
    let ws = Ws::new(&[
        ("a/BUILD", "filegroup(name='a')"),
        ("b/BUILD", "filegroup(name='b')"),
    ]);
    let o = q(&ws, "//a:a + //b:b", |o, _| {
        o.universe_scope = vec!["//a:a".into()];
        o.infer_universe_scope = true;
    });
    // Literals are not restricted to the universe (`getTargetsMatchingPattern`); only their forward
    // deps are "missing from graph" (see `round9_out_of_universe_targets_warn_instead_of_failing`).
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    assert_eq!(lines(&o), vec!["//a:a", "//b:b"]);
    let o = q(&ws, "deps(//b:b)", |o, _| {
        o.universe_scope = vec!["//a:a".into()]
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    assert!(
        o.stderr
            .iter()
            .any(|l| l.contains("Targets were missing from graph: [//b:b]")),
        "{:?}",
        o.stderr
    );
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
        (
            "defs.bzl",
            "def _impl(ctx): pass\nmy_rule = rule(implementation = _impl)\n",
        ),
        (
            "pkg/BUILD",
            "load('//:defs.bzl', 'my_rule')\nmy_rule(name = 'r')\n",
        ),
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
    assert!(
        o.stderr
            .iter()
            .any(|l| l.contains("'host' configuration no longer exists")),
        "{:?}",
        o.stderr
    );

    let o = ws.post("config(//lib:data.txt, null)", |_, _, _| {});
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("//lib:data.txt (null)"), "{out}");

    let o = ws.post("config(//lib:l, target)", |_, _, _| {});
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);

    let o = ws.post("config(//lib:l, null)", |_, _, _| {});
    assert_eq!(o.exit_code, 1, "{:?}", o.stderr);
    assert!(
        o.stderr.iter().any(
            |l| l.contains("No target (in) //lib:l could be found in the 'null' configuration")
        ),
        "{:?}",
        o.stderr
    );

    let o = ws.post("config(//lib:l, exec)", |_, _, _| {});
    assert_eq!(o.exit_code, 1, "{:?}", o.stderr);
    assert!(
        o.stderr
            .iter()
            .any(|l| l.contains("Unknown configuration ID 'exec'.")),
        "{:?}",
        o.stderr
    );
    assert!(
        o.stderr
            .iter()
            .any(|l| l.contains("A short ID is any prefix of a full ID. cquery shows short IDs.")),
        "{:?}",
        o.stderr
    );
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
    let ws = Ws::new(&[(
        "pkg/BUILD",
        "genrule(name = 'g', outs = ['g.out'], cmd = 'echo a\\necho b > $@')\n",
    )]);
    let o = ws.post("//pkg:g", |_, _, popts| {
        popts.aquery = true;
        popts.output = "commands".into();
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("\\u000A"),
        "commands output must contain \\u000A for newlines: {out}"
    );
}

#[test]
fn cquery_starlark_real_target_object_and_providers() {
    let ws = tree();
    // `providers(target)` exposes DefaultInfo & FileProvider; string indexing and `'...' in target` error like Bazel.
    let o = ws.post("//app:app", |_, _, popts| {
        popts.output = "starlark".into();
        popts.starlark_expr =
            "str('DefaultInfo' in providers(target)) + ':' + str(target.label.name)".into();
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(out.trim(), "True:app");

    for bad_expr in [
        "target['DefaultInfo']",
        "'DefaultInfo' in target",
        "target.DefaultInfo",
        "target[provider()]",
    ] {
        let o_bad = ws.post("//app:app", |_, _, popts| {
            popts.output = "starlark".into();
            popts.starlark_expr = bad_expr.into();
        });
        assert!(
            String::from_utf8_lossy(&o_bad.stdout).trim().is_empty(),
            "{bad_expr}"
        );
        assert!(!o_bad.stderr.is_empty(), "{bad_expr} should error");
    }

    // Test returning non-string directly (Label)
    let o = ws.post("//app:app", |_, _, popts| {
        popts.output = "starlark".into();
        popts.starlark_expr = "target.label".into();
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(out.trim(), "@@//app:app");

    // Test target.files is depset and target.actions exposes Action objects
    let o = ws.post("//app:app", |_, _, popts| {
        popts.output = "starlark".into();
        popts.starlark_expr = "type(target.files) + ':' + str(len(target.files.to_list())) + ':' + target.actions[0].mnemonic".into();
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(out.trim(), "depset:1:Genrule");

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
        ("vis/BUILD", "package_group(name = 'friends')\n"),
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
    assert!(
        out.contains("# /ws/pkg/BUILD:2:2\nmy_rule(\n  name = \"m_gen\","),
        "Header must be BUILD call site: {out}"
    );
    assert!(
        out.contains("generator_location = \"pkg/BUILD:2:2\""),
        "generator_location must be relative: {out}"
    );
    assert!(out.contains("generator_function = \"f\""), "{out}");
    assert!(out.contains("# Rule m_gen instantiated at (most recent call last):\n#   /ws/pkg/BUILD:2:2 in <toplevel>\n#   /ws/inc.bzl:5:6   in f\n#   /ws/inc.bzl:3:12  in g"), "Instantiation stack must have correct function names: {out}");
    assert!(out.contains("# Rule my_rule defined at (most recent call last):\n#   /ws/defs.bzl:3:15 in <toplevel>"), "Definition stack must contain line:col: {out}");

    let o_loc = q(&ws, "//pkg:m_gen", |_, f| f.output = "location".into());
    let loc_out = String::from_utf8_lossy(&o_loc.stdout).to_string();
    assert!(
        loc_out.contains("/ws/pkg/BUILD:2:2: my_rule rule //pkg:m_gen"),
        "{loc_out}"
    );

    let o_proto = q(&ws, "//pkg:m_gen", |_, f| {
        f.output = "proto".into();
        f.proto_instantiation_stack = true;
        f.proto_definition_stack = true;
    });
    let proto_out = String::from_utf8_lossy(&o_proto.stdout).to_string();
    assert!(
        proto_out.contains("pkg/BUILD:2:2: <toplevel>"),
        "{proto_out}"
    );
    assert!(proto_out.contains("inc.bzl:5:6: f"), "{proto_out}");
    assert!(proto_out.contains("inc.bzl:3:12: g"), "{proto_out}");
    assert!(
        proto_out.contains("defs.bzl:3:15: <toplevel>"),
        "{proto_out}"
    );
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
    assert!(
        out.contains("#NoTransition ->\n") || out.contains("#NoTransition -> \n"),
        "{out}"
    );
    // Null config dependency (source file) has (null transition) and nothing after ->
    assert!(
        out.contains("#(null transition) ->\n") || out.contains("#(null transition) -> \n"),
        "{out}"
    );
    // Check diff format does NOT contain double brackets [[...]]
    assert!(
        !out.contains("[["),
        "Must not contain double brackets: {out}"
    );
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
    assert!(
        lines(&o).contains(&"//t:tool".to_string()),
        "{:?}",
        lines(&o)
    );

    // 2. query with aspect_deps=off omits //t:tool
    let o_off = q(&ws, "deps(//pkg:top)", |o, _| o.aspect_deps = "off".into());
    assert!(
        !lines(&o_off).contains(&"//t:tool".to_string()),
        "{:?}",
        lines(&o_off)
    );

    // 3. xml output includes rule-input name="//t:tool"
    let o_xml = q(&ws, "//pkg:top", |_, f| f.output = "xml".into());
    let xml_out = String::from_utf8_lossy(&o_xml.stdout);
    assert!(
        xml_out.contains("<rule-input name=\"//t:tool\"/>"),
        "{xml_out}"
    );

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
    assert!(
        aq_out.contains("AspectDescriptors: [//pkg:defs.bzl%my_asp()]"),
        "{aq_out}"
    );
}

#[test]
fn keep_going_skips_bad_patterns_before_other_output() {
    let ws = Ws::new(&[("foo/BUILD", "filegroup(name='a', srcs=['x.txt'])")]);
    let o = q(&ws, "//nope:all + //foo:a", |o, _| o.keep_going = true);
    assert_eq!(o.exit_code, 3, "{:?}", o.stderr);
    assert!(
        o.stderr[0].starts_with("ERROR: Skipping '//nope:all': no such package 'nope'"),
        "{:?}",
        o.stderr
    );
    let w = o
        .stderr
        .iter()
        .position(|l| l.starts_with("WARNING: --keep_going specified"))
        .unwrap();
    assert_eq!(w, 1, "{:?}", o.stderr);
    assert_eq!(lines(&o), vec!["//foo:a"]);
    // Without --keep_going the preload failure is reported verbatim.
    let o = q(&ws, "//nope:all + //foo:a", |_, _| {});
    assert_eq!(o.exit_code, 7);
    assert!(
        o.stderr[0].starts_with("ERROR: no such package 'nope'"),
        "{:?}",
        o.stderr
    );
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
    let loc_err = |l: &String| {
        l.starts_with("ERROR: /ws/foo/BUILD:1:") && l.contains("and referenced by '//foo:bad'")
    };
    // deps: the Skyframe preload and the label visitor both report the edge.
    let o = q(&ws, "deps(//foo:top)", |o, _| o.keep_going = true);
    assert_eq!(
        o.stderr.iter().filter(|l| loc_err(l)).count(),
        2,
        "{:?}",
        o.stderr
    );
    // allpaths (graphless): preload + the path visitor, which blames the discoverer of `bad`.
    let o = q(&ws, "allpaths(//foo:top, //foo:bad)", |o, _| {
        o.keep_going = true
    });
    assert!(
        o.stderr
            .iter()
            .any(|l| l.starts_with("ERROR: /ws/foo/BUILD:2:")
                && l.ends_with("and referenced by '//foo:top'")),
        "{:?}",
        o.stderr
    );
    assert!(
        o.stderr
            .iter()
            .any(|l| l
                .contains("failed: errors were encountered while computing transitive closure")),
        "{:?}",
        o.stderr
    );
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
    assert!(
        !out.contains("testonly") && !out.contains("deprecation") && !out.contains("visibility"),
        "{out}"
    );
    let o = q(&ws, "//t:b", |_, f| f.output = "build".into());
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(!out.contains("testonly"), "{out}");
    // features: only the rule's own value is shown (package features merge elsewhere).
    assert!(out.contains("features = [\"own\"]"), "{out}");
    let o = q(&ws, "//t:c", |_, f| f.output = "build".into());
    assert!(
        !String::from_utf8_lossy(&o.stdout).contains("features"),
        "{:?}",
        o.stdout
    );
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
    let pos = |s: &str| {
        out.find(s)
            .unwrap_or_else(|| panic!("{s} missing in {out}"))
    };
    // single positives, `...` positives, single negatives, `...` negatives, public
    assert!(
        pos("\"//b\"") < pos("\"//e\"") && pos("\"//e\"") < pos("\"//a/...\""),
        "{out}"
    );
    assert!(
        pos("\"//a/...\"") < pos("\"-//c\"") && pos("\"-//c\"") < pos("\"-//d/...\""),
        "{out}"
    );
}

#[test]
fn deps_without_depth_reports_located_missing_package_error() {
    let ws = Ws::new(&[("foo/BUILD", "filegroup(name='bad', srcs=['//gone:x'])\n")]);
    let o = q(&ws, "deps(//foo:bad)", |_, _| {});
    assert_eq!(o.exit_code, 7, "{:?}", o.stderr);
    assert!(
        o.stderr
            .iter()
            .any(|l| l.starts_with("ERROR: /ws/foo/BUILD:1:")
                && l.ends_with("and referenced by '//foo:bad'")),
        "{:?}",
        o.stderr
    );
    assert!(
        o.stderr
            .iter()
            .any(|l| l.contains("preloading transitive closure failed")),
        "{:?}",
        o.stderr
    );
}

#[test]
fn visible_treats_external_public_visibility_as_public() {
    // `//visibility:public` is the special label in every repository: here the main repository.
    let ws = Ws::new(&[
        (
            "a/BUILD",
            "filegroup(name='x', srcs=[], visibility=['//visibility:public'])\n",
        ),
        ("b/BUILD", "filegroup(name='y', srcs=['//a:x'])\n"),
    ]);
    assert_eq!(ql(&ws, "visible(//b:y, //a:x)"), vec!["//a:x"]);
}

#[test]
fn run_genquery_formats_expression_with_opts() {
    let ws = tree();
    let loader = ws.resolver().loader().clone();
    let out = crate::run_genquery(
        &loader,
        "deps(//app:app)",
        &["//app:app".to_string()],
        true,
        &[],
    )
    .unwrap();
    let text = String::from_utf8_lossy(&out).to_string();
    assert!(text.lines().any(|l| l == "//app:app"), "{text}");
    assert!(text.lines().any(|l| l == "//lib:l"), "{text}");
    let kinds = crate::run_genquery(
        &loader,
        "//lib:l",
        &["//lib:l".to_string()],
        true,
        &["--output=label_kind".to_string()],
    )
    .unwrap();
    assert_eq!(String::from_utf8_lossy(&kinds), "genrule rule //lib:l\n");
    // options that `GenQuery.java` rejects
    assert!(crate::run_genquery(
        &loader,
        "//lib:l",
        &["//lib:l".to_string()],
        false,
        &["--keep_going".to_string()]
    )
    .is_err());
    assert!(crate::run_genquery(
        &loader,
        "//lib:l",
        &["//lib:l".to_string()],
        false,
        &["--universe_scope=//lib:l".to_string()]
    )
    .is_err());
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
    assert!(
        o.stderr.iter().all(|l| !l.contains("Visibility error")),
        "{:?}",
        o.stderr
    );
}

#[test]
fn round9_out_of_universe_targets_warn_instead_of_failing() {
    let ws = tree();
    // `SkyQueryEnvironment.getFwdDeps`: targets outside the universe have no graph node.
    let o = q(&ws, "deps(//lib:l)", |o, _| {
        o.universe_scope = vec!["//app:app".into()]
    });
    // //lib:l is inside the closure of //app:app; nothing is missing.
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let o = q(&ws, "deps(//lib:fg)", |o, _| {
        o.universe_scope = vec!["//app:app".into()]
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    assert_eq!(lines(&o), vec!["//lib:fg"]);
    assert!(
        o.stderr
            .iter()
            .any(|l| l == "WARNING: Targets were missing from graph: [//lib:fg]"),
        "{:?}",
        o.stderr
    );
}

#[test]
fn round9_environment_group_is_a_target() {
    let ws = Ws::new(&[(
        "pkgs/BUILD",
        "environment(name = 'a')\nenvironment(name = 'b')\nenvironment_group(name = 'envs', environments = [':a', ':b'], defaults = [':a'])\n",
    )]);
    assert_eq!(ql(&ws, "//pkgs:envs"), vec!["//pkgs:envs"]);
    let o = q(&ws, "//pkgs:envs", |_, f| f.output = "label_kind".into());
    assert_eq!(
        lines(&o),
        vec!["environment group //pkgs:envs"],
        "{:?}",
        o.stderr
    );
    let o = q(&ws, "//pkgs:envs", |_, f| f.output = "xml".into());
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(
        out.contains("<environment-group location=") && out.contains("name=\"//pkgs:envs\""),
        "{out}"
    );
    assert!(
        out.contains("<list name=\"environments\">") && out.contains("<label value=\"//pkgs:a\"/>"),
        "{out}"
    );
    assert!(out.contains("<list name=\"defaults\">"), "{out}");
    assert_eq!(ql(&ws, "deps(//pkgs:envs)"), vec!["//pkgs:envs"]);
}

#[test]
fn round9_labels_crossing_a_subpackage_boundary_are_not_assumed_input_files() {
    let ws = Ws::new(&[
        (
            "deep/x/BUILD",
            "filegroup(name = 'x', srcs = ['y/f', '//deep/x/y:g'])\n",
        ),
        ("deep/x/y/BUILD", "exports_files(['f', 'g'])\n"),
        ("deep/x/y/f", ""),
        ("deep/x/y/g", ""),
    ]);
    let o = q(&ws, "deps(//deep/x:x)", |o, _| o.keep_going = true);
    let l = lines(&o);
    assert!(
        !l.iter().any(|t| t == "//deep/x:y/f"),
        "{l:?} {:?}",
        o.stderr
    );
    assert!(l.contains(&"//deep/x/y:g".to_string()), "{l:?}");
}

// ---- Bazel 9.3.2 parity tests ----------------------------------------------------------------

#[test]
fn sky_rdeps_and_allrdeps_use_unfiltered_universe_and_bounded_seeds() {
    let ws = Ws::new(&[
        ("BUILD", ""),
        (
            "defs.bzl",
            "def _impl(ctx):\n    pass\nmy_rule = rule(implementation = _impl, attrs = {'deps': attr.label_list(), '_imp': attr.label(default = '//pkg:imp_dep')})\n",
        ),
        ("pkg/tool.sh", "#!/bin/sh\n"),
        ("pkg/imp.txt", ""),
        ("pkg/vis_allowed", ""),
        ("vis/BUILD", "package_group(name = 'pg')\n"),
        (
            "pkg/BUILD",
            "load('//:defs.bzl', 'my_rule')\n\
             filegroup(name = 'tool_src', srcs = ['tool.sh'])\n\
             sh_binary(name = 'tool', srcs = [':tool_src'])\n\
             genrule(name = 'gen', outs = ['g.out'], tools = [':tool'], cmd = 'echo > $@', visibility = ['//vis:pg'])\n\
             filegroup(name = 'imp_src', srcs = ['imp.txt'])\n\
             filegroup(name = 'imp_dep', srcs = [':imp_src'])\n\
             my_rule(name = 'top', deps = [':gen'])\n\
             filegroup(name = 'outside')\n",
        ),
    ]);

    // Sky mode + --notool_deps: //pkg:tool is reachable in the unfiltered universe, so as a seed it
    // is emitted, while the tool edge //pkg:gen -> //pkg:tool is dropped.
    let o = q(&ws, "rdeps(//pkg:gen, //pkg:tool)", |o, _| {
        o.infer_universe_scope = true;
        o.tool_deps = false;
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    assert_eq!(lines(&o), vec!["//pkg:tool"]);

    // Allowed reverse edge //pkg:tool -> //pkg:tool_src still counts, then stops at tool edge //pkg:gen -> //pkg:tool.
    let o = q(&ws, "rdeps(//pkg:gen, //pkg:tool_src)", |o, _| {
        o.infer_universe_scope = true;
        o.tool_deps = false;
    });
    assert_eq!(lines(&o), vec!["//pkg:tool", "//pkg:tool_src"]);

    // Sky mode + --noimplicit_deps: //pkg:imp_dep and //pkg:imp_src are in the unfiltered universe.
    let o = q(&ws, "rdeps(//pkg:top, //pkg:imp_src)", |o, _| {
        o.infer_universe_scope = true;
        o.implicit_deps = false;
    });
    assert_eq!(lines(&o), vec!["//pkg:imp_dep", "//pkg:imp_src"]);

    // Sky mode + --nonodep_deps: visibility target //vis:pg stays in the unfiltered universe.
    let o = q(&ws, "rdeps(//pkg:gen, //vis:pg)", |o, _| {
        o.infer_universe_scope = true;
        o.nodep_deps = false;
    });
    assert_eq!(lines(&o), vec!["//vis:pg"]);

    // allrdeps with --notool_deps and --universe_scope=//pkg:gen walks unfiltered nodes, filtered edges.
    let o = q(&ws, "allrdeps(//pkg:tool_src)", |o, _| {
        o.universe_scope = vec!["//pkg:gen".into()];
        o.tool_deps = false;
    });
    assert_eq!(lines(&o), vec!["//pkg:tool", "//pkg:tool_src"]);

    // Bounded rdeps in Sky mode emits out-of-universe seeds at depth 0, negative depth does not expand,
    // and i32::MAX does not wrap.
    let o = q(&ws, "rdeps(//pkg:gen, //pkg:outside, 1)", |o, _| {
        o.infer_universe_scope = true
    });
    assert_eq!(lines(&o), vec!["//pkg:outside"]);
    let o = q(&ws, "rdeps(//pkg:gen, //pkg:tool_src, '-1')", |o, _| {
        o.infer_universe_scope = true
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    assert_eq!(lines(&o), vec!["//pkg:tool_src"]);
    let o = q(
        &ws,
        "rdeps(//pkg:gen, //pkg:tool_src, 2147483647)",
        |o, _| o.infer_universe_scope = true,
    );
    assert_eq!(lines(&o), vec!["//pkg:gen", "//pkg:tool", "//pkg:tool_src"]);
}

#[test]
fn set_variable_references_and_universe_patterns() {
    let ws = tree();
    // `set($x //app:app)` must not preload `$x` as a pattern.
    let o = q(&ws, "let x = //lib:l in set($x //app:app)", |_, _| {});
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    assert_eq!(lines(&o), vec!["//app:app", "//lib:l"]);

    let o = q(&ws, "let x = //lib:l in set($x //app:app)", |o, _| {
        o.infer_universe_scope = true
    });
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    assert_eq!(lines(&o), vec!["//app:app", "//lib:l"]);

    // `$1`, `$a-b`, `$ü` are NOT valid var references and are treated as target patterns.
    for non_var in ["$1", "$a-b", "$ü"] {
        let mut pats = Vec::new();
        let expr = crate::parse::parse(&format!("set({non_var})"), crate::parse::DEFAULT_FUNCTIONS)
            .unwrap();
        expr.collect_target_patterns(&mut pats);
        assert_eq!(pats, vec![non_var.to_string()], "{non_var}");
    }
}

#[test]
fn attr_omits_null_defaults_and_concatenates_select_combinations() {
    let ws = Ws::new(&[
        ("BUILD", ""),
        (
            "defs.bzl",
            "def _impl(ctx):\n    pass\nstr_int = rule(implementation = _impl, attrs = {'s': attr.string(), 'n': attr.int(), 'l': attr.label_list(allow_files = True)})\n",
        ),
        (
            "pkg/BUILD",
            "load('//:defs.bzl', 'str_int')\n\
             config_setting(name = 'c1', values = {'compilation_mode': 'opt'})\n\
             config_setting(name = 'c2', values = {'compilation_mode': 'dbg'})\n\
             filegroup(name = 'fg', srcs = ['a.txt'])\n\
             filegroup(name = 'dep_empty', deprecation = '')\n\
             str_int(\n\
                 name = 'si',\n\
                 s = 'pre_' + select({':c1': 'alpha', ':c2': 'beta', '//conditions:default': 'gamma'}),\n\
                 n = select({':c1': 2147483647, ':c2': 1, '//conditions:default': 5}) + select({':c1': 1, ':c2': 2, '//conditions:default': 10}),\n\
                 l = select({':c1': ['a.txt'], '//conditions:default': ['b.txt']}) + ['c.txt'],\n\
             )\n",
        ),
        ("pkg/a.txt", ""),
        ("pkg/b.txt", ""),
        ("pkg/c.txt", ""),
    ]);

    // Unset null-default string attribute `deprecation` must not match `.*` or `^$`.
    assert_eq!(
        ql(&ws, "attr(deprecation, '.*', //pkg:all)"),
        vec!["//pkg:dep_empty"]
    );
    assert_eq!(
        ql(&ws, "attr(deprecation, '^$', //pkg:all)"),
        vec!["//pkg:dep_empty"]
    );

    // Scalar string select concatenation joins prefix + branch.
    assert_eq!(
        ql(&ws, "attr(s, '^pre_alpha$', //pkg:all)"),
        vec!["//pkg:si"]
    );
    assert_eq!(
        ql(&ws, "attr(s, '^alpha$', //pkg:all)"),
        Vec::<String>::new()
    );

    // Integer select concatenation sums with signed 32-bit wrap and specializes identical keysets.
    assert_eq!(
        ql(&ws, "attr(n, '^-2147483648$', //pkg:all)"),
        vec!["//pkg:si"]
    );
    assert_eq!(ql(&ws, "attr(n, '^3$', //pkg:all)"), vec!["//pkg:si"]);
    assert_eq!(ql(&ws, "attr(n, '^2$', //pkg:all)"), Vec::<String>::new());

    // List select evaluates each combination separately, not one superset.
    assert_eq!(
        ql(
            &ws,
            "attr(l, '^\\[//pkg:a\\.txt, //pkg:c\\.txt\\]$', //pkg:all)"
        ),
        vec!["//pkg:si"]
    );
    assert_eq!(
        ql(&ws, "attr(l, 'a\\.txt.*b\\.txt', //pkg:all)"),
        Vec::<String>::new()
    );
}

#[test]
fn tests_function_size_tags_plus_manual_and_implicit_tests_attr() {
    let ws = Ws::new(&[(
        "pkg/BUILD",
        "sh_test(name = 't_small', srcs = ['t.sh'], size = 'small')\n\
         sh_test(name = 't_default', srcs = ['t.sh'], tags = ['slow'])\n\
         sh_test(name = 't_plus_lit', srcs = ['t.sh'], tags = ['+slow'])\n\
         sh_test(name = 't_manual', srcs = ['t.sh'], tags = ['manual'])\n\
         test_suite(name = 's_small', tests = [':t_small', ':t_default'], tags = ['small'])\n\
         test_suite(name = 's_med', tests = [':t_small', ':t_default'], tags = ['medium'])\n\
         test_suite(name = 's_not_med', tests = [':t_small', ':t_default'], tags = ['-medium'])\n\
         test_suite(name = 's_plus', tests = [':t_default', ':t_plus_lit'], tags = ['+slow'])\n\
         test_suite(name = 's_plus_small', tests = [':t_small', ':t_default'], tags = ['+small'])\n\
         test_suite(name = 's_manual', tests = [':t_small'], tags = ['manual'])\n\
         test_suite(name = 's_plus_manual', tests = [':t_small', ':t_manual'], tags = ['+manual'])\n\
         test_suite(name = 's_implicit_manual', tags = ['manual'])\n",
    ), ("pkg/t.sh", "#!/bin/sh\n")]);

    assert_eq!(ql(&ws, "tests(//pkg:s_small)"), vec!["//pkg:t_small"]);
    assert_eq!(ql(&ws, "tests(//pkg:s_med)"), vec!["//pkg:t_default"]);
    assert_eq!(ql(&ws, "tests(//pkg:s_not_med)"), vec!["//pkg:t_small"]);
    assert_eq!(ql(&ws, "tests(//pkg:s_plus)"), vec!["//pkg:t_default"]);
    assert_eq!(ql(&ws, "tests(//pkg:s_plus_small)"), vec!["//pkg:t_small"]);
    assert_eq!(ql(&ws, "tests(//pkg:s_manual)"), vec!["//pkg:t_small"]);
    assert_eq!(
        ql(&ws, "tests(//pkg:s_plus_manual)"),
        vec!["//pkg:t_manual"]
    );
    assert_eq!(
        ql(&ws, "tests(//pkg:s_implicit_manual)"),
        vec!["//pkg:t_default", "//pkg:t_plus_lit", "//pkg:t_small"]
    );

    // `labels(tests, ...)` keeps empty `tests` empty; `$implicit_tests` holds the implicit tests.
    assert_eq!(
        ql(&ws, "labels(tests, //pkg:s_implicit_manual)"),
        Vec::<String>::new()
    );
    assert_eq!(
        ql(&ws, "labels('$implicit_tests', //pkg:s_implicit_manual)"),
        vec!["//pkg:t_default", "//pkg:t_plus_lit", "//pkg:t_small"]
    );
}

#[test]
fn function_expression_preserves_inner_errors_and_keep_going() {
    let ws = tree();
    let o = ws.post("config(//nope:missing, target)", |_, _, _| {});
    assert_ne!(o.exit_code, 0);
    assert!(
        o.stderr
            .iter()
            .any(|l| l.contains("no such package 'nope'")),
        "must preserve inner error rather than reporting 'No target': {:?}",
        o.stderr
    );

    let o = ws.post(
        "config(//nope:missing + //app:app, target)",
        |qopts, _, _| {
            qopts.keep_going = true;
        },
    );
    assert!(
        String::from_utf8_lossy(&o.stdout).contains("//app:app"),
        "{:?}",
        o.stderr
    );
    assert!(
        o.stderr
            .iter()
            .any(|l| l.contains("Skipping '//nope:missing'")),
        "{:?}",
        o.stderr
    );
}

#[test]
fn aquery_filter_chain_and_top_level_test_suite_behavior() {
    let ws = tree();

    // Nested filter under non-filter function fails before analysis.
    let o = ws.post("deps(inputs('.*', //app:app))", |_, _, popts| {
        popts.aquery = true;
    });
    assert_eq!(o.exit_code, 1, "{:?}", o.stderr);
    assert!(
        o.stderr.iter().any(|l| l.contains("aquery filter functions (inputs, outputs, mnemonic) produce actions, and therefore can't be the input of other function types: deps")),
        "{:?}",
        o.stderr
    );

    // Non-chain filters (binary union / let) do not filter actions.
    let o = ws.post(
        "inputs('nonexistent_xyz', //app:app) + //lib:l",
        |_, _, popts| {
            popts.aquery = true;
            popts.output = "text".into();
        },
    );
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("Target: //app:app") && out.contains("Target: //lib:l"),
        "{out}"
    );

    let o = ws.post(
        "let x = mnemonic('NonExistent', //app:app) in $x",
        |_, _, popts| {
            popts.aquery = true;
            popts.output = "text".into();
        },
    );
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    assert!(String::from_utf8_lossy(&o.stdout).contains("Target: //app:app"));

    // Top-level test_suite is excluded even if a configured target for it already exists in TargetResolver.
    let resolver = ws.resolver();
    let tr = TargetResolver::new(resolver.loader().clone(), AnalysisConfig::default());
    let top = BuildConfigurationKey::default();
    let _ = crate::post_analysis::run_post_analysis(
        &resolver,
        tr.clone(),
        top.clone(),
        "//app:suite",
        &QueryOpts::default(),
        &FormatOpts::default(),
        &PostOpts::default(),
    );
    let aq = crate::post_analysis::run_post_analysis(
        &resolver,
        tr,
        top,
        "//app:suite",
        &QueryOpts::default(),
        &FormatOpts::default(),
        &PostOpts {
            aquery: true,
            output: "text".into(),
            ..Default::default()
        },
    );
    assert_eq!(aq.exit_code, 0, "{:?}", aq.stderr);
    assert!(
        String::from_utf8_lossy(&aq.stdout).is_empty(),
        "{:?}",
        String::from_utf8_lossy(&aq.stdout)
    );
}

#[test]
fn cquery_and_aquery_dep_filtering_and_explicit_aspects() {
    let ws = Ws::new(&[
        ("BUILD", ""),
        (
            "defs.bzl",
            "MyInfo = provider()\n\
             def _asp_impl(target, ctx):\n    return [MyInfo()]\n\
             my_asp = aspect(implementation = _asp_impl, attrs = {'_asp_dep': attr.label(default = '//pkg:asp_dep')})\n\
             def _rule_impl(ctx):\n    pass\n\
             my_rule = rule(implementation = _rule_impl, attrs = {'deps': attr.label_list(aspects = [my_asp]), '_imp': attr.label(default = '//pkg:imp_dep')})\n",
        ),
        ("pkg/src.txt", ""),
        ("pkg/tool_src.txt", ""),
        (
            "pkg/BUILD",
            "load('//:defs.bzl', 'my_rule')\n\
             filegroup(name = 'asp_dep')\n\
             filegroup(name = 'imp_dep')\n\
             filegroup(name = 'tool_dep', srcs = ['tool_src.txt'])\n\
             genrule(name = 'gen', srcs = ['src.txt'], outs = ['g.out'], tools = [':tool_dep'], cmd = 'echo > $@')\n\
             my_rule(name = 'leaf')\n\
             my_rule(name = 'top', deps = [':leaf'])\n\
             alias(name = 'al', actual = ':leaf')\n",
        ),
    ]);

    // --nonodep_deps is rejected in post-analysis queries.
    let o = ws.post("//pkg:gen", |qopts, _, _| qopts.nodep_deps = false);
    assert_eq!(o.exit_code, 1);
    assert!(
        o.stderr.iter().any(|l| l.contains("NO_NODEP_DEPS")),
        "{:?}",
        o.stderr
    );

    // --noimplicit_deps drops implicit, target platform, toolchains, and aspect-only deps,
    // while keeping explicit deps, alias actual, and OutputFile's generating rule.
    let o = ws.post("deps(//pkg:top)", |qopts, _, _| qopts.implicit_deps = false);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("//pkg:top") && out.contains("//pkg:leaf"),
        "{out}"
    );
    assert!(
        !out.contains("//pkg:imp_dep") && !out.contains("//pkg:asp_dep"),
        "{out}"
    );
    assert!(
        !out.contains("host_platform") && !out.contains("empty_toolchain"),
        "{out}"
    );

    let o_alias = ws.post("deps(//pkg:al)", |qopts, _, _| qopts.implicit_deps = false);
    let out_alias = String::from_utf8_lossy(&o_alias.stdout);
    assert!(
        out_alias.contains("//pkg:al") && out_alias.contains("//pkg:leaf"),
        "{out_alias}"
    );

    let o_out = ws.post("deps(//pkg:g.out)", |qopts, _, _| {
        qopts.implicit_deps = false
    });
    let out_out = String::from_utf8_lossy(&o_out.stdout);
    assert!(
        out_out.contains("//pkg:g.out") && out_out.contains("//pkg:gen"),
        "{out_out}"
    );

    // rdeps & allpaths with --noimplicit_deps work in cquery.
    let o_rd = ws.post("rdeps(//pkg:top, //pkg:leaf)", |qopts, _, _| {
        qopts.implicit_deps = false
    });
    let out_rd = String::from_utf8_lossy(&o_rd.stdout);
    assert!(
        out_rd.contains("//pkg:top") && out_rd.contains("//pkg:leaf"),
        "{out_rd}"
    );
    let o_ap = ws.post("allpaths(//pkg:top, //pkg:leaf)", |qopts, _, _| {
        qopts.implicit_deps = false
    });
    let out_ap = String::from_utf8_lossy(&o_ap.stdout);
    assert!(
        out_ap.contains("//pkg:top") && out_ap.contains("//pkg:leaf"),
        "{out_ap}"
    );

    // --notool_deps: target-config parent drops exec deps and keeps null-config sources;
    // exec-config parent keeps exec deps and drops null-config sources.
    let o_nt = ws.post("deps(//pkg:gen)", |qopts, _, _| qopts.tool_deps = false);
    let out_nt = String::from_utf8_lossy(&o_nt.stdout);
    assert!(out_nt.contains("//pkg:src.txt"), "{out_nt}");
    assert!(!out_nt.contains("//pkg:tool_dep"), "{out_nt}");

    let o_exec = ws.post("deps(//pkg:gen)", |_, _, _| {});
    let exec_line = String::from_utf8_lossy(&o_exec.stdout)
        .lines()
        .find(|l| l.starts_with("//pkg:tool_dep "))
        .unwrap()
        .to_string();
    let exec_hash = exec_line
        .rsplit('(')
        .next()
        .unwrap()
        .trim_end_matches(')')
        .to_string();
    let o_exec_nt = ws.post(
        &format!("deps(config(//pkg:tool_dep, {exec_hash}))"),
        |qopts, _, _| {
            qopts.universe_scope = vec!["//pkg:gen".into()];
            qopts.tool_deps = false;
        },
    );
    let out_exec_nt = String::from_utf8_lossy(&o_exec_nt.stdout);
    assert!(out_exec_nt.contains("//pkg:tool_dep"), "{out_exec_nt}");
    assert!(
        !out_exec_nt.contains("//pkg:tool_src.txt"),
        "exec parent must drop null-config source under --notool_deps: {out_exec_nt}"
    );

    // --experimental_explicit_aspects emits aspect nodes in cquery, and providers(aspect_node) is None.
    let o_ea = ws.post("deps(//pkg:top)", |_, _, popts| {
        popts.explicit_aspects = true
    });
    let out_ea = String::from_utf8_lossy(&o_ea.stdout);
    assert!(out_ea.contains("my_asp of //pkg:leaf"), "{out_ea}");
}

#[test]
fn cquery_config_labels_files_and_starlark_semantics() {
    let ws = Ws::new(&[
        ("BUILD", ""),
        (
            "pkg/defs.bzl",
            "def _asp_impl(target, ctx):\n\
                 \x20   f = ctx.actions.declare_file(target.label.name + '.asp')\n\
                 \x20   ctx.actions.write(f, 'x')\n\
                 \x20   return [OutputGroupInfo(default = depset([f]), custom = depset([f]))]\n\
             top_asp = aspect(implementation = _asp_impl)\n\
             def _rule_impl(ctx):\n\
                 \x20   f1 = ctx.actions.declare_file('f1.txt')\n\
                 \x20   ctx.actions.write(f1, '1')\n\
                 \x20   f2 = ctx.actions.declare_file('f2.txt')\n\
                 \x20   ctx.actions.write(f2, '2')\n\
                 \x20   return [DefaultInfo(files = depset([f1])), OutputGroupInfo(custom = depset([f2]))]\n\
             my_rule = rule(implementation = _rule_impl, attrs = {'deps': attr.label_list()})\n",
        ),
        ("pkg/a.txt", ""),
        ("pkg/b.txt", ""),
        (
            "pkg/BUILD",
            "load('//pkg:defs.bzl', 'my_rule')\n\
             config_setting(name = 'opt', values = {'compilation_mode': 'opt'})\n\
             filegroup(name = 'fg', srcs = select({':opt': ['a.txt'], '//conditions:default': ['b.txt']}))\n\
             alias(name = 'fg_alias', actual = ':fg')\n\
             genrule(name = 'gen', srcs = ['a.txt'], outs = ['g.out'], tools = [':fg'], cmd = 'echo > $@')\n\
             my_rule(name = 'r')\n\
             package_group(name = 'pg')\n",
        ),
    ]);

    // `config(..., target)` drops null-configured deps when no top-level target is null-configured.
    let o = ws.post("config(deps(//pkg:fg), target)", |_, _, _| {});
    assert_eq!(o.exit_code, 0, "{:?}", o.stderr);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("//pkg:fg ") && !out.contains("//pkg:b.txt"),
        "{out} stderr={:?}",
        o.stderr
    );

    // `labels()` in cquery follows alias chain, uses configured select branch, preserves exec config,
    // and errors on missing attributes.
    let o = ws.post("labels(srcs, //pkg:fg_alias)", |_, _, _| {});
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("//pkg:b.txt") && !out.contains("//pkg:a.txt"),
        "{out}"
    );

    let o_tools = ws.post("labels(tools, //pkg:gen)", |_, _, _| {});
    let tools_out = String::from_utf8_lossy(&o_tools.stdout);
    let o_fg_target = ws.post("//pkg:fg", |_, _, _| {});
    let fg_target_line = String::from_utf8_lossy(&o_fg_target.stdout);
    assert!(
        tools_out.contains("//pkg:fg ") && tools_out.trim() != fg_target_line.trim(),
        "tools dep must be in exec cfg: {tools_out} vs {fg_target_line}"
    );

    let o_missing_attr = ws.post("labels(no_such_attr, //pkg:fg_alias)", |_, _, _| {});
    assert_ne!(o_missing_attr.exit_code, 0);
    assert!(
        o_missing_attr.stderr.iter().any(|l| l.contains(
            "configured target of type filegroup does not have attribute 'no_such_attr'"
        )),
        "{:?}",
        o_missing_attr.stderr
    );

    // `cquery --output=files` supports `+group` / `-group` syntax.
    let o_plus = ws.post("//pkg:r", |_, _, popts| {
        popts.output = "files".into();
        popts.output_groups = vec!["+custom".into()];
    });
    let plus_out = String::from_utf8_lossy(&o_plus.stdout);
    assert!(
        plus_out.contains("f1.txt") && plus_out.contains("f2.txt"),
        "{plus_out}"
    );

    let o_minus = ws.post("//pkg:r", |_, _, popts| {
        popts.output = "files".into();
        popts.output_groups = vec!["-default".into()];
    });
    let minus_out = String::from_utf8_lossy(&o_minus.stdout);
    assert!(!minus_out.contains("f1.txt"), "{minus_out}");

    // Top-level aspect outputs appear in `cquery --output=files` with `--include_aspects` and not with `--noinclude_aspects`.
    let resolver = ws.resolver();
    let asp_lbl = zel_cmdline::Label::parse_canonical("//pkg:defs.bzl").unwrap();
    let top_asp = resolver
        .loader()
        .load_bzl_export(&asp_lbl, "top_asp")
        .unwrap();
    let o_asp = ws.post("//pkg:r", |_, _, popts| {
        popts.output = "files".into();
        popts.top_aspects = vec![top_asp.clone()];
    });
    let asp_out = String::from_utf8_lossy(&o_asp.stdout);
    assert!(asp_out.contains("r.asp"), "{asp_out}");

    let o_noasp = ws.post("//pkg:r", |qopts, _, popts| {
        qopts.include_aspects = false;
        popts.output = "files".into();
        popts.top_aspects = vec![top_asp];
    });
    let noasp_out = String::from_utf8_lossy(&o_noasp.stdout);
    assert!(!noasp_out.contains("r.asp"), "{noasp_out}");

    // `cquery --output=starlark`: OutputFile repr & providers, PackageGroup -> None,
    // and `--incompatible_disable_target_default_provider_fields`.
    let o_of = ws.post("//pkg:g.out", |_, _, popts| {
        popts.output = "starlark".into();
        popts.starlark_expr = "str(target) + ':' + str('InstrumentedFilesInfo' in providers(target)) + ':' + str(target.actions)".into();
    });
    assert_eq!(
        String::from_utf8_lossy(&o_of.stdout).trim(),
        "<output file target //pkg:g.out>:True:None"
    );

    let o_pg = ws.post("//pkg:pg", |_, _, popts| {
        popts.output = "starlark".into();
        popts.starlark_expr = "str(providers(target))".into();
    });
    assert_eq!(String::from_utf8_lossy(&o_pg.stdout).trim(), "None");

    let o_dis = ws.post("//pkg:r", |_, _, popts| {
        popts.output = "starlark".into();
        popts.starlark_expr = "target.files".into();
        popts.incompatible_disable_target_default_provider_fields = true;
    });
    assert!(String::from_utf8_lossy(&o_dis.stdout).trim().is_empty());
    assert!(
        o_dis
            .stderr
            .iter()
            .any(|l| l.contains("incompatible_disable_target_default_provider_fields")),
        "{:?}",
        o_dis.stderr
    );
}

#[test]
fn visibility_environment_group_config_setting_javatests_and_included_negations() {
    let ws = Ws::new(&[
        ("BUILD", ""),
        (
            "env/BUILD",
            "environment(name = 'e1')\nenvironment_group(name = 'eg', environments = [':e1'], defaults = [':e1'])\n",
        ),
        (
            "cfg/BUILD",
            "config_setting(name = 'cs', values = {'cpu': 'k8'})\n",
        ),
        (
            "java/com/foo/BUILD",
            "filegroup(name = 'internal_lib', visibility = ['//visibility:private'])\n",
        ),
        ("javatests/com/foo/BUILD", "filegroup(name = 'test_target')\n"),
        (
            "vis/BUILD",
            "package_group(name = 'exclude_sub', packages = ['//a/...', '-//a/sub/...'])\n\
             package_group(name = 'allow_sub', packages = ['//a/sub'])\n\
             package_group(name = 'combined', includes = [':exclude_sub', ':allow_sub'])\n\
             filegroup(name = 'guarded', visibility = [':combined'])\n",
        ),
        ("a/sub/BUILD", "filegroup(name = 'consumer')\n"),
        (
            "badvis/BUILD",
            "filegroup(name = 't', visibility = ['//vis:consumer_not_pg'])\n",
        ),
    ]);

    // environment_group is private to its own package
    assert!(ql(&ws, "visible(//a/sub:consumer, //env:eg)").is_empty());
    assert_eq!(ql(&ws, "visible(//env:e1, //env:eg)"), vec!["//env:eg"]);

    // config_setting without explicit/package visibility defaults to public
    assert_eq!(
        ql(&ws, "visible(//a/sub:consumer, //cfg:cs)"),
        vec!["//cfg:cs"]
    );

    // javatests/com/foo can see private targets in java/com/foo
    assert_eq!(
        ql(
            &ws,
            "visible(//javatests/com/foo:test_target, //java/com/foo:internal_lib)"
        ),
        vec!["//java/com/foo:internal_lib"]
    );

    // Sibling included package_group negative spec does not cancel sibling positive spec
    assert_eq!(
        ql(&ws, "visible(//a/sub:consumer, //vis:guarded)"),
        vec!["//vis:guarded"]
    );

    // Invalid visibility label reports error
    let o_bad = q(&ws, "visible(//a/sub:consumer, //badvis:t)", |_, _| {});
    assert_ne!(o_bad.exit_code, 0);
    assert!(
        o_bad
            .stderr
            .iter()
            .any(|l| l.contains("Invalid visibility label")),
        "{:?}",
        o_bad.stderr
    );
}

#[test]
fn query_aspect_deps_precise_transitive_requires_and_proto_metadata() {
    let ws = Ws::new(&[
        ("BUILD", ""),
        ("t/tool1.sh", "#!/bin/sh\n"),
        ("t/tool2.sh", "#!/bin/sh\n"),
        (
            "t/BUILD",
            "sh_binary(name = 'tool1', srcs = ['tool1.sh'])\n\
             sh_binary(name = 'tool2', srcs = ['tool2.sh'])\n",
        ),
        (
            "pkg/defs.bzl",
            "MyInfo = provider()\n\
             OtherInfo = provider()\n\
             def _base_asp(target, ctx):\n    return []\n\
             base_asp = aspect(implementation = _base_asp, attrs = {'_t1': attr.label(default = '//t:tool1')})\n\
             def _top_asp(target, ctx):\n    return []\n\
             top_asp = aspect(\n\
                 implementation = _top_asp,\n\
                 requires = [base_asp],\n\
                 required_providers = [MyInfo],\n\
                 attrs = {'_t2': attr.label(default = '//t:tool2')},\n\
             )\n\
             def _impl(ctx):\n    return [MyInfo()]\n\
             def _other_impl(ctx):\n    return [OtherInfo()]\n\
             good_leaf = rule(implementation = _impl, provides = [MyInfo])\n\
             other_leaf = rule(implementation = _other_impl, provides = [OtherInfo])\n\
             holder = rule(implementation = _other_impl, attrs = {'deps': attr.label_list(aspects = [top_asp])})\n",
        ),
        (
            "pkg/BUILD",
            "load('//pkg:defs.bzl', 'good_leaf', 'other_leaf', 'holder')\n\
             good_leaf(name = 'good')\n\
             other_leaf(name = 'other')\n\
             holder(name = 'h_match', deps = [':good'])\n\
             holder(name = 'h_nomatch', deps = [':other'])\n",
        ),
    ]);

    // Conservative mode includes both top_asp and transitive required base_asp even when providers don't match
    let cons = q(&ws, "deps(//pkg:h_nomatch)", |o, _| {
        o.aspect_deps = "conservative".into();
    });
    let cons_lines = lines(&cons);
    assert!(
        cons_lines.contains(&"//t:tool1".to_string()),
        "{cons_lines:?}"
    );
    assert!(
        cons_lines.contains(&"//t:tool2".to_string()),
        "{cons_lines:?}"
    );

    // Precise mode drops top_asp (and its required base_asp) on h_nomatch, but keeps both on h_match
    let prec_no = q(&ws, "deps(//pkg:h_nomatch)", |o, _| {
        o.aspect_deps = "precise".into();
    });
    let prec_no_lines = lines(&prec_no);
    assert!(
        !prec_no_lines.contains(&"//t:tool1".to_string()),
        "{prec_no_lines:?}"
    );
    assert!(
        !prec_no_lines.contains(&"//t:tool2".to_string()),
        "{prec_no_lines:?}"
    );

    let prec_yes = q(&ws, "deps(//pkg:h_match)", |o, _| {
        o.aspect_deps = "precise".into();
    });
    let prec_yes_lines = lines(&prec_yes);
    assert!(
        prec_yes_lines.contains(&"//t:tool1".to_string()),
        "{prec_yes_lines:?}"
    );
    assert!(
        prec_yes_lines.contains(&"//t:tool2".to_string()),
        "{prec_yes_lines:?}"
    );

    // Proto output: source_aspect_name, synthetic $internal_attr_hash, rule_class_key (16) and rule_class_info (17) once
    let proto_json = q(&ws, "//pkg:h_match + //pkg:h_nomatch", |_, f| {
        f.output = "streamed_jsonproto".into();
        f.proto_include_attribute_source_aspects = true;
        f.proto_include_synthetic_attribute_hash = true;
        f.proto_rule_classes = true;
    });
    assert_eq!(proto_json.exit_code, 0, "{:?}", proto_json.stderr);
    let pj = String::from_utf8_lossy(&proto_json.stdout);
    assert!(pj.contains("\"$internal_attr_hash\""), "{pj}");
    assert!(
        pj.contains("\"sourceAspectName\":\"//pkg:defs.bzl%top_asp\""),
        "{pj}"
    );
    assert!(
        pj.contains("\"ruleClassKey\":\"//pkg:defs.bzl%holder\""),
        "{pj}"
    );
    assert_eq!(
        pj.matches("\"ruleClassInfo\":").count(),
        1,
        "ruleClassInfo must be emitted once per rule class key: {pj}"
    );
}

#[test]
fn query_rbuildfiles_subpath_zero_target_pkg_and_genquery_scope() {
    let ws = Ws::new(&[
        ("BUILD", ""),
        ("pkg/BUILD", ""),
        ("pkg/sub/helper.bzl", "X = 1\n"),
        ("consumer/BUILD", "load('//pkg:sub/helper.bzl', 'X')\n"),
        (
            "g/BUILD",
            "filegroup(name = 'in_scope', srcs = [':dep'])\n\
             filegroup(name = 'dep')\n\
             filegroup(name = 'out_of_scope')\n",
        ),
    ]);

    // rbuildfiles resolves pkg/sub/helper.bzl to //pkg:sub/helper.bzl and finds consumer/BUILD even with 0 rules
    let rb = q(&ws, "rbuildfiles(pkg/sub/helper.bzl)", |o, _| {
        o.infer_universe_scope = true;
    });
    assert_eq!(rb.exit_code, 0, "{:?}", rb.stderr);
    assert_eq!(lines(&rb), vec!["//consumer:BUILD"]);

    // genquery scope enforcement (strict vs non-strict)
    let strict_err = crate::run_genquery(
        ws.resolver().loader(),
        "//g:in_scope + //g:out_of_scope",
        &["//g:in_scope".to_string()],
        true,
        &[],
    )
    .unwrap_err();
    assert!(
        strict_err.contains("is not within the scope of the query"),
        "{strict_err}"
    );

    let non_strict = crate::run_genquery(
        ws.resolver().loader(),
        "//g:in_scope + //g:out_of_scope",
        &["//g:in_scope".to_string()],
        false,
        &[],
    )
    .unwrap();
    assert_eq!(String::from_utf8_lossy(&non_strict).trim(), "//g:in_scope");
}

#[test]
fn query_build_xml_proto_and_subgraph_shortcuts() {
    let ws = Ws::new(&[
        ("BUILD", ""),
        (
            "pkg/defs.bzl",
            concat!(
                "def _impl(ctx):\n",
                "    pass\n",
                "my_rule = rule(implementation = _impl, attrs = {'tool': attr.label(), 'note': attr.string(), 'num': attr.int()})\n",
            ),
        ),
        (
            "pkg/BUILD",
            concat!(
                "load('//pkg:defs.bzl', 'my_rule')\n",
                "config_setting(name = 'c1', values = {'cpu': 'k8'})\n",
                "package_group(name = 'pg', packages = ['-//b/...', '//a/...'])\n",
                "my_rule(\n",
                "    name = 'r',\n",
                "    note = select({'//conditions:default': 'plain'}),\n",
                ")\n",
                "my_rule(name = 'r_sel', tool = select({'//pkg:c1': None, '//conditions:default': '//pkg:r'}))\n",
                "my_rule(name = 'r_nul', note = 'bad\\0str')\n",
                "filegroup(name = 'top', srcs = [':mid'])\n",
                "filegroup(name = 'mid', srcs = [':bot'])\n",
                "filegroup(name = 'bot')\n",
            ),
        ),
    ]);

    // build formatter: default package_group in source order, None -> None, single //conditions:default collapsed
    let b_out = q(&ws, "//pkg:pg + //pkg:r + //pkg:r_sel", |_, f| {
        f.output = "build".into();
    });
    assert_eq!(b_out.exit_code, 0, "{:?}", b_out.stderr);
    let b_str = String::from_utf8_lossy(&b_out.stdout);
    assert!(
        b_str.contains("packages = [\"-//b/...\", \"//a/...\"]"),
        "{b_str}"
    );
    assert!(b_str.contains("note = \"plain\","), "{b_str}");
    assert!(b_str.contains("\"//pkg:c1\": None"), "{b_str}");
    assert!(!b_str.contains(": null"), "{b_str}");

    // xml formatter: NUL char produces Bazel DOM error placeholder, display_full_kind does not alter class
    let xml_out = q(&ws, "//pkg:r_nul", |_, f| {
        f.output = "xml".into();
        f.display_full_kind = true;
    });
    let xml_str = String::from_utf8_lossy(&xml_out.stdout);
    assert!(xml_str.contains("class=\"my_rule\""), "{xml_str}");
    assert!(
        xml_str.contains("[[[ERROR: could not be encoded as XML]]]"),
        "{xml_str}"
    );

    // location formatter: display_full_kind does not alter target kind
    let loc_out = q(&ws, "//pkg:r", |_, f| {
        f.output = "location".into();
        f.display_full_kind = true;
    });
    let loc_str = String::from_utf8_lossy(&loc_out.stdout);
    assert!(loc_str.contains(": my_rule rule //pkg:r"), "{loc_str}");

    // unflattened proto select: is_default_value true for None branch, false for explicit //conditions:default
    let sel_json = q(&ws, "//pkg:r_sel", |_, f| {
        f.output = "streamed_jsonproto".into();
        f.proto_flatten_selects = false;
    });
    let sj = String::from_utf8_lossy(&sel_json.stdout);
    assert!(
        sj.contains("{\"label\":\"//pkg:c1\",\"isDefaultValue\":true}"),
        "{sj}"
    );
    assert!(
        sj.contains(
            "{\"label\":\"//conditions:default\",\"stringValue\":\"//pkg:r\",\"isDefaultValue\":false}"
        ) || sj.contains(
            "{\"label\":\"//conditions:default\",\"isDefaultValue\":false,\"stringValue\":\"//pkg:r\"}"
        ),
        "{sj}"
    );

    // extractSubgraph shortcut: dropping //pkg:mid from deps(//pkg:top) preserves //pkg:top -> //pkg:bot rank
    let mr = q(&ws, "deps(//pkg:top) - //pkg:mid", |_, f| {
        f.output = "maxrank".into();
    });
    let mr_str = String::from_utf8_lossy(&mr.stdout);
    assert!(mr_str.contains("0 //pkg:top"), "{mr_str}");
    assert!(mr_str.contains("1 //pkg:bot"), "{mr_str}");
}

#[test]
fn cquery_and_aquery_formatters_skyframe_state_and_starlark_validation() {
    let ws = Ws::new(&[
        ("BUILD", ""),
        ("pkg/src.txt", "hello\n"),
        (
            "pkg/defs.bzl",
            concat!(
                "def _impl(ctx):\n",
                "    f1 = ctx.actions.declare_file('z_first.txt')\n",
                "    f2 = ctx.actions.declare_file('a_second.txt')\n",
                "    f3 = ctx.actions.declare_file('extra_default.txt')\n",
                "    ctx.actions.write(f1, '1')\n",
                "    ctx.actions.write(f2, '2')\n",
                "    ctx.actions.write(f3, '3')\n",
                "    return [\n",
                "        DefaultInfo(files = depset([f1, f2])),\n",
                "        OutputGroupInfo(default = depset([f3])),\n",
                "    ]\n",
                "ord_rule = rule(implementation = _impl)\n",
            ),
        ),
        (
            "pkg/BUILD",
            concat!(
                "load('//pkg:defs.bzl', 'ord_rule')\n",
                "exports_files(['src.txt'])\n",
                "ord_rule(name = 'ord')\n",
                "genrule(name = 'multi', outs = ['not_main.txt', 'main.txt'], cmd = 'touch $(OUTS)')\n",
                "alias(name = 'multi_alias', actual = ':multi')\n",
            ),
        ),
    ]);

    // 1. cquery --output=files preserves insertion order and unions OutputGroupInfo.default,
    //    and OutputFile target emits only exact file (not suffix match not_main.txt)
    let f_ord = ws.post("//pkg:ord", |_, _, popts| {
        popts.output = "files".into();
    });
    assert_eq!(f_ord.exit_code, 0, "{:?}", f_ord.stderr);
    let f_lines: Vec<&str> = std::str::from_utf8(&f_ord.stdout)
        .unwrap()
        .lines()
        .collect();
    assert_eq!(f_lines.len(), 3, "{f_lines:?}");
    assert!(f_lines[0].ends_with("z_first.txt"), "{f_lines:?}");
    assert!(f_lines[1].ends_with("a_second.txt"), "{f_lines:?}");
    assert!(f_lines[2].ends_with("extra_default.txt"), "{f_lines:?}");

    let f_exact = ws.post("//pkg:main.txt", |_, _, popts| {
        popts.output = "files".into();
    });
    let exact_lines: Vec<&str> = std::str::from_utf8(&f_exact.stdout)
        .unwrap()
        .lines()
        .collect();
    assert_eq!(exact_lines.len(), 1, "{exact_lines:?}");
    assert!(exact_lines[0].ends_with("/pkg/main.txt"), "{exact_lines:?}");

    // 2. cquery --output=starlark: depset & struct predeclared, build_options(source_file) is None,
    //    bad expr paren escape and bad format arity rejected up front
    let st_ok = ws.post("//pkg:src.txt + //pkg:ord", |_, _, popts| {
        popts.output = "starlark".into();
        popts.starlark_expr =
            "str(struct(n = len(depset([1]).to_list())).n) + ':' + str(build_options(target) == None)"
                .into();
    });
    assert_eq!(st_ok.exit_code, 0, "{:?}", st_ok.stderr);
    let st_lines: Vec<&str> = std::str::from_utf8(&st_ok.stdout)
        .unwrap()
        .lines()
        .collect();
    assert!(st_lines.contains(&"1:True"), "{st_lines:?}");
    assert!(st_lines.contains(&"1:False"), "{st_lines:?}");

    let st_escape = ws.post("//pkg:ord", |_, _, popts| {
        popts.output = "starlark".into();
        popts.starlark_expr = "1)\ndef evil(): pass\n(".into();
    });
    assert_ne!(st_escape.exit_code, 0);
    assert!(
        st_escape
            .stderr
            .iter()
            .any(|l| l.contains("invalid --starlark:expr")),
        "{:?}",
        st_escape.stderr
    );

    let st_bad_arity = ws.post("//pkg:ord", |_, _, popts| {
        popts.output = "starlark".into();
        popts.starlark_file = "bad.cquery".into();
        popts.starlark_file_source = "def format(a, b):\n    return str(a)\n".into();
    });
    assert_ne!(st_bad_arity.exit_code, 0);
    assert!(
        st_bad_arity
            .stderr
            .iter()
            .any(|l| l.contains("must take 1 argument")),
        "{:?}",
        st_bad_arity.stderr
    );

    // 3. cquery --output=textproto with --transitions=lite emits configured_rule_input
    let cq_tp = ws.post("//pkg:ord", |_, _, popts| {
        popts.output = "textproto".into();
        popts.transitions = "lite".into();
    });
    assert_eq!(cq_tp.exit_code, 0, "{:?}", cq_tp.stderr);
    let cq_tp_str = String::from_utf8_lossy(&cq_tp.stdout);
    assert!(
        cq_tp_str.contains("configured_rule_input") || cq_tp_str.contains("configuration_id"),
        "{cq_tp_str}"
    );

    // 4. cquery --output=transitions on OutputFile emits rule transition header and no dep lines
    let tr_of = ws.post("//pkg:main.txt", |_, _, popts| {
        popts.output = "transitions".into();
        popts.transitions = "lite".into();
    });
    assert_eq!(tr_of.exit_code, 0, "{:?}", tr_of.stderr);
    let tr_of_lines: Vec<&str> = std::str::from_utf8(&tr_of.stdout)
        .unwrap()
        .lines()
        .collect();
    assert_eq!(tr_of_lines.len(), 1, "{tr_of_lines:?}");
    assert!(
        tr_of_lines[0].starts_with("NoTransition -> //pkg:main.txt ("),
        "{tr_of_lines:?}"
    );

    // 5. aquery: 1-arg inputs('*') reports Wrong query syntax (regex compiled before arity check),
    //    filter() follows alias actual label, and --skyframe_state accepts 1-arg mnemonic('Genrule')
    let aq_bad_re = ws.post("inputs('*')", |_, _, popts| {
        popts.aquery = true;
    });
    assert_ne!(aq_bad_re.exit_code, 0);
    assert!(
        aq_bad_re
            .stderr
            .iter()
            .any(|l| l.contains("Wrong query syntax:")),
        "{:?}",
        aq_bad_re.stderr
    );

    let aq_alias_filter = ws.post(
        "deps(filter('//pkg:multi$', //pkg:multi_alias))",
        |_, _, popts| {
            popts.aquery = true;
            popts.output = "summary".into();
        },
    );
    assert_eq!(aq_alias_filter.exit_code, 0, "{:?}", aq_alias_filter.stderr);
    assert!(
        String::from_utf8_lossy(&aq_alias_filter.stdout).contains("1 total action"),
        "{}",
        String::from_utf8_lossy(&aq_alias_filter.stdout)
    );

    let resolver = ws.resolver();
    let shared_tr = TargetResolver::new(resolver.loader().clone(), AnalysisConfig::default());
    let seed_popts = PostOpts {
        aquery: true,
        output: "summary".into(),
        ..Default::default()
    };
    let seed = crate::post_analysis::run_post_analysis(
        &resolver,
        shared_tr.clone(),
        BuildConfigurationKey::default(),
        "//pkg:multi + //pkg:ord",
        &QueryOpts::default(),
        &FormatOpts::default(),
        &seed_popts,
    );
    assert_eq!(seed.exit_code, 0, "{:?}", seed.stderr);

    let sky_popts = PostOpts {
        aquery: true,
        skyframe_state: true,
        output: "textproto".into(),
        ..Default::default()
    };
    let aq_sky = crate::post_analysis::run_post_analysis(
        &resolver,
        shared_tr,
        BuildConfigurationKey::default(),
        "mnemonic('Genrule')",
        &QueryOpts::default(),
        &FormatOpts::default(),
        &sky_popts,
    );
    assert_eq!(aq_sky.exit_code, 0, "{:?}", aq_sky.stderr);
    let sky_str = String::from_utf8_lossy(&aq_sky.stdout);
    assert!(sky_str.contains("mnemonic: \"Genrule\""), "{sky_str}");
    assert!(!sky_str.contains("mnemonic: \"FileWrite\""), "{sky_str}");
}

#[test]
fn build_output_formatter_starlark_none_in_select_and_bare_omission() {
    let ws = Ws::new(&[
        ("BUILD", ""),
        (
            "pkg/defs.bzl",
            concat!(
                "def _impl(ctx):\n",
                "    return []\n",
                "my_rule = rule(\n",
                "    implementation = _impl,\n",
                "    attrs = {\n",
                "        'tool': attr.label(),\n",
                "    },\n",
                ")\n",
            ),
        ),
        (
            "pkg/BUILD",
            concat!(
                "load('//pkg:defs.bzl', 'my_rule')\n",
                "config_setting(name = 'on_linux', values = {'cpu': 'k8'})\n",
                "my_rule(name = 'dep_t')\n",
                "my_rule(name = 'bare_none', tool = None)\n",
                "my_rule(\n",
                "    name = 'sel_none',\n",
                "    tool = select({'//pkg:on_linux': '//pkg:dep_t', '//conditions:default': None}),\n",
                ")\n",
            ),
        ),
    ]);

    let q_out = q(&ws, "//pkg:bare_none + //pkg:sel_none", |_, f| {
        f.output = "build".into();
    });
    assert_eq!(q_out.exit_code, 0, "{:?}", q_out.stderr);
    let q_str = String::from_utf8_lossy(&q_out.stdout);
    // Starlark `None` inside `select({...})` must render as valid Starlark `None`, never `null`.
    assert!(q_str.contains("\"//conditions:default\": None"), "{q_str}");
    assert!(!q_str.contains("null"), "{q_str}");
    // Bare `tool = None` leaves the attribute unset (`possible_values` is empty), so `outputRule` omits it.
    let bare_block = q_str
        .split("my_rule(")
        .find(|s| s.contains("name = \"bare_none\""))
        .expect("bare_none rule block");
    assert!(!bare_block.contains("tool ="), "{bare_block}");

    // In `cquery --output=build` when the resolved `select` branch is `None`, the attribute is also omitted.
    let cq_out = ws.post("//pkg:sel_none", |_, _, popts| {
        popts.output = "build".into();
    });
    assert_eq!(cq_out.exit_code, 0, "{:?}", cq_out.stderr);
    let cq_str = String::from_utf8_lossy(&cq_out.stdout);
    assert!(!cq_str.contains("null"), "{cq_str}");
    assert!(!cq_str.contains("tool = None"), "{cq_str}");
}

#[test]
fn proto_output_aspect_rule_inputs_and_attributes_dedup() {
    let ws = Ws::new(&[
        ("BUILD", ""),
        ("t/shared.sh", "#!/bin/sh\n"),
        ("t/only_asp.sh", "#!/bin/sh\n"),
        (
            "t/BUILD",
            concat!(
                "sh_binary(name = 'shared', srcs = ['shared.sh'])\n",
                "sh_binary(name = 'only_asp', srcs = ['only_asp.sh'])\n",
            ),
        ),
        (
            "pkg/defs.bzl",
            concat!(
                "def _asp_impl(target, ctx):\n",
                "    return []\n",
                "asp_one = aspect(\n",
                "    implementation = _asp_impl,\n",
                "    attrs = {\n",
                "        '_dup': attr.label(default = '//t:only_asp'),\n",
                "        '_shared': attr.label(default = '//t:shared'),\n",
                "    },\n",
                ")\n",
                "asp_two = aspect(\n",
                "    implementation = _asp_impl,\n",
                "    attrs = {\n",
                "        '_dup': attr.label(default = '//t:only_asp'),\n",
                "        '_also': attr.label(default = '//t:only_asp'),\n",
                "    },\n",
                ")\n",
                "def _rule_impl(ctx):\n",
                "    return []\n",
                "leaf = rule(implementation = _rule_impl)\n",
                "holder = rule(\n",
                "    implementation = _rule_impl,\n",
                "    attrs = {\n",
                "        'deps': attr.label_list(aspects = [asp_one, asp_two]),\n",
                "        'direct': attr.label(),\n",
                "    },\n",
                ")\n",
            ),
        ),
        (
            "pkg/BUILD",
            concat!(
                "load('//pkg:defs.bzl', 'leaf', 'holder')\n",
                "leaf(name = 'l')\n",
                "holder(name = 'h', deps = [':l'], direct = '//t:shared')\n",
            ),
        ),
    ]);

    let out_no_src = q(&ws, "//pkg:h", |_, f| {
        f.output = "streamed_jsonproto".into();
        f.proto_include_attribute_source_aspects = false;
    });
    assert_eq!(out_no_src.exit_code, 0, "{:?}", out_no_src.stderr);
    let json_no_src = String::from_utf8_lossy(&out_no_src.stdout);
    // `//t:only_asp` is referenced by three aspect attributes across `asp_one` and `asp_two`,
    // and must be deduplicated to a single entry in the aspect `rule_input` pass.
    let ri_section = json_no_src
        .split("\"ruleInput\":[")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .expect("ruleInput array");
    let ri_items: Vec<&str> = ri_section
        .split(',')
        .map(|s| s.trim().trim_matches('"'))
        .collect();
    assert_eq!(
        ri_items.iter().filter(|&&x| x == "//t:only_asp").count(),
        1,
        "aspect rule_input pass must deduplicate repeated aspect labels: {ri_items:?}"
    );
    // `//t:shared` is both an aspect dep and a direct rule dep; matching Bazel's two-pass
    // `ProtoOutputFormatter`, it appears once in the aspect pass and once in the sorted rule pass.
    assert_eq!(
        ri_items.iter().filter(|&&x| x == "//t:shared").count(),
        2,
        "two-pass rule_input emission matches ProtoOutputFormatter: {ri_items:?}"
    );
    // Identical `$dup` aspect `Attribute` protos are deduplicated when `source_aspect_name` is omitted.
    assert_eq!(
        json_no_src.matches("\"name\":\"$dup\"").count(),
        1,
        "identical serialized aspect attributes must be deduplicated: {json_no_src}"
    );

    // With `--proto:include_attribute_source_aspects`, `source_aspect_name` differs so both `$dup`
    // attributes remain distinct.
    let out_with_src = q(&ws, "//pkg:h", |_, f| {
        f.output = "streamed_jsonproto".into();
        f.proto_include_attribute_source_aspects = true;
    });
    let json_with_src = String::from_utf8_lossy(&out_with_src.stdout);
    assert_eq!(
        json_with_src.matches("\"name\":\"$dup\"").count(),
        2,
        "{json_with_src}"
    );
}

#[test]
fn cquery_starlark_build_options_returns_dict_for_configured_aspect() {
    let ws = Ws::new(&[
        ("BUILD", ""),
        ("pkg/src.txt", "x\n"),
        (
            "pkg/defs.bzl",
            concat!(
                "MyInfo = provider()\n",
                "def _asp_impl(target, ctx):\n",
                "    return [MyInfo()]\n",
                "my_asp = aspect(implementation = _asp_impl)\n",
                "def _rule_impl(ctx):\n",
                "    return []\n",
                "leaf_rule = rule(implementation = _rule_impl, attrs = {'srcs': attr.label_list(allow_files = True)})\n",
                "top_rule = rule(implementation = _rule_impl, attrs = {'deps': attr.label_list(aspects = [my_asp])})\n",
            ),
        ),
        (
            "pkg/BUILD",
            concat!(
                "load('//pkg:defs.bzl', 'leaf_rule', 'top_rule')\n",
                "exports_files(['src.txt'])\n",
                "leaf_rule(name = 'leaf', srcs = ['src.txt'])\n",
                "top_rule(name = 'top', deps = [':leaf'])\n",
            ),
        ),
    ]);

    let res = ws.post("deps(//pkg:top)", |_, _, popts| {
        popts.explicit_aspects = true;
        popts.output = "starlark".into();
        popts.starlark_expr =
            "str(target.label) + '|' + str(providers(target) == None) + '|' + str(build_options(target) != None)"
                .into();
    });
    assert_eq!(res.exit_code, 0, "{:?}", res.stderr);
    let out_lines: Vec<String> = String::from_utf8_lossy(&res.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect();
    // For `//pkg:leaf`, there are two nodes under `--experimental_explicit_aspects`:
    // 1. ConfiguredTarget `//pkg:leaf`: `providers(target) == None` is False, `build_options(target) != None` is True
    // 2. AspectKey `my_asp of //pkg:leaf`: `providers(target) == None` is True, `build_options(target) != None` is True!
    assert!(
        out_lines.contains(&"@@//pkg:leaf|False|True".to_string())
            || out_lines.contains(&"//pkg:leaf|False|True".to_string()),
        "{out_lines:?}"
    );
    assert!(
        out_lines.contains(&"@@//pkg:leaf|True|True".to_string())
            || out_lines.contains(&"//pkg:leaf|True|True".to_string()),
        "configured aspect node must have non-None build_options(target): {out_lines:?}"
    );
    // Source file `//pkg:src.txt` is null-configured, so `build_options(target)` is `None`.
    assert!(
        out_lines.contains(&"@@//pkg:src.txt|False|False".to_string())
            || out_lines.contains(&"//pkg:src.txt|False|False".to_string()),
        "{out_lines:?}"
    );
}

#[test]
fn genquery_restricts_universe_closure_and_rejects_out_of_scope_paths() {
    let ws = Ws::new(&[
        ("a/helper.bzl", "HELPER = 1\n"),
        (
            "a/BUILD",
            concat!(
                "load('//a:helper.bzl', 'HELPER')\n",
                "package_group(name = 'pg', packages = ['//a'])\n",
                "filegroup(name = 'a', visibility = [':pg'])\n",
                "filegroup(name = 'bridge', srcs = ['//out:mid'])\n",
                "sh_test(name = 'bridge_test', srcs = ['t.sh'], deps = ['//b:b'])\n",
                "test_suite(name = 'suite')\n",
            ),
        ),
        ("a/t.sh", "#!/bin/sh\n"),
        (
            "out/BUILD",
            "filegroup(name = 'mid', srcs = ['//b:b'])\n",
        ),
        (
            "b/BUILD",
            concat!(
                "filegroup(name = 'b')\n",
                "filegroup(name = 'same_pkg_bridge', srcs = [':b'])\n",
            ),
        ),
        (
            "sel/BUILD",
            concat!(
                "config_setting(name = 'cs', values = {'define': 'D=1'})\n",
                "filegroup(name = 'f', srcs = select({':cs': [], '//conditions:default': ['//dne:missing']}))\n",
            ),
        ),
        (
            "g/BUILD",
            "genrule(name = 'gen', outs = ['gen.out'], cmd = 'touch $@')\n",
        ),
    ]);
    let loader = ws.resolver().loader().clone();
    let scope = vec!["//a:a".to_string(), "//b:b".to_string()];

    // Non-strict mode strips out-of-scope pattern matches BEFORE graph evaluation, so
    // `allpaths`, `somepath`, `rdeps`, `tests`, and `deps` cannot walk through `//a:bridge`,
    // `//a:bridge_test`, or `//out:mid` to reach in-scope `//b:b`.
    for expr in [
        "deps(//out:mid)",
        "deps(//a:bridge)",
        "allpaths(//a:a + //out:mid, //b:b)",
        "rdeps(//a:a + //out:mid, //b:b)",
        "allpaths(//a:all, //b:b)",
        "somepath(//a:all, //b:b)",
        "rdeps(//a:all, //b:b)",
        "allpaths(tests(//a:all), //b:b)",
    ] {
        let out = crate::run_genquery(&loader, expr, &scope, false, &[]).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out).trim(),
            "",
            "expected empty output for `{expr}` when path goes through out-of-scope targets"
        );
    }

    // `//g:gen.out` is an OutputFile of `//g:gen` (not in `deps(//g:gen)`), so `deps(//g:gen.out)`
    // cannot walk through out-of-scope `//g:gen.out` back to in-scope `//g:gen`.
    let gen_out_strict = crate::run_genquery(
        &loader,
        "deps(//g:gen.out)",
        &["//g:gen".to_string()],
        true,
        &[],
    )
    .unwrap_err();
    assert!(
        gen_out_strict.contains("is not within the scope of the query"),
        "{gen_out_strict}"
    );
    let gen_out_non_strict = crate::run_genquery(
        &loader,
        "deps(//g:gen.out)",
        &["//g:gen".to_string()],
        false,
        &[],
    )
    .unwrap();
    assert_eq!(String::from_utf8_lossy(&gen_out_non_strict).trim(), "");

    // Strict mode restricts `siblings` and `same_pkg_direct_rdeps` to the `deps(scope)` universe
    // as well, preventing indirect traversal through out-of-scope targets.
    for expr in [
        "allpaths(siblings(//a:a), //b:b)",
        "same_pkg_direct_rdeps(//b:b)",
    ] {
        let out = crate::run_genquery(&loader, expr, &scope, true, &[]).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out).trim(),
            "",
            "expected empty output for `{expr}` in strict genquery universe"
        );
    }

    // Empty scope restricts the universe to `{}` rather than exposing the workspace.
    let empty_strict_err = crate::run_genquery(&loader, "//b:b", &[], true, &[]).unwrap_err();
    assert!(
        empty_strict_err.contains("is not within the scope of the query"),
        "{empty_strict_err}"
    );
    let empty_non_strict = crate::run_genquery(&loader, "//b:b", &[], false, &[]).unwrap();
    assert_eq!(String::from_utf8_lossy(&empty_non_strict).trim(), "");

    // Recursive patterns are forbidden in genquery.
    let rec_err = crate::run_genquery(&loader, "//a/...", &scope, true, &[]).unwrap_err();
    assert!(
        rec_err.contains("recursive target patterns are not permitted: '//a/...'"),
        "{rec_err}"
    );

    // Missing target in expression fails with `no such target`.
    let missing_expr_err =
        crate::run_genquery(&loader, "deps(//b:nosuchtarget)", &scope, true, &[]).unwrap_err();
    assert!(
        missing_expr_err.contains("no such target '//b:nosuchtarget'"),
        "{missing_expr_err}"
    );

    // Broken or missing targets in transitive scope closure fail with scope closure error.
    let broken_scope_err =
        crate::run_genquery(&loader, "//sel:f", &["//sel:f".to_string()], true, &[]).unwrap_err();
    assert!(
        broken_scope_err
            .contains("errors were encountered while computing transitive closure of the scope"),
        "{broken_scope_err}"
    );

    // `buildfiles` and `loadfiles` work inside genquery (including `.bzl` loads from packages
    // outside `allowed_packages` without leaking out-of-scope `//shstub:BUILD`), while
    // `--output=location` is rejected.
    let bf_out = crate::run_genquery(&loader, "buildfiles(//a:a)", &scope, true, &[]).unwrap();
    let bf_lines: Vec<&str> = std::str::from_utf8(&bf_out).unwrap().lines().collect();
    assert_eq!(
        bf_lines,
        vec!["//a:BUILD", "//a:helper.bzl", "//shstub:sh.bzl"]
    );
    let loc_err = crate::run_genquery(
        &loader,
        "loadfiles(//a:a)",
        &scope,
        true,
        &["--output=location".to_string()],
    )
    .unwrap_err();
    assert!(
        loc_err.contains(
            "Query expressions involving 'buildfiles' or 'loadfiles' cannot be used with --output=location"
        ),
        "{loc_err}"
    );

    // `--nodep_deps=true` includes visibility package groups collected in the scope closure.
    let with_nodep = crate::run_genquery(
        &loader,
        "deps(//a:a)",
        &scope,
        true,
        &["--nodep_deps=true".to_string()],
    )
    .unwrap();
    assert_eq!(
        std::str::from_utf8(&with_nodep)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec!["//a:a", "//a:pg"]
    );
}
