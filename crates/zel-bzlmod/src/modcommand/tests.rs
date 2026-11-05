//! Tests for the `mod` command. The first half ports `ModExecutorTest` from Bazel (executor and
//! formatters over hand-built graphs); the second half drives `run_mod` end to end over
//! resolutions built from `file://` registries.

use super::executor::*;
use super::inspector::*;
use super::*;
use crate::model::{AttrVal, ExtensionUsage, Proxy, Tag};
use crate::registry::testutil::*;
use crate::resolve::{resolve, CheckMode, ResolveOptions};
use crate::version::Version;
use std::collections::BTreeMap;
use std::path::Path;

fn key(n: &str, v: &str) -> ModuleKey {
    ModuleKey::new(n, Version::parse(v).unwrap())
}

struct AB(AugmentedModule);

fn aug(k: ModuleKey) -> AB {
    AB(AugmentedModule {
        name: k.name.clone(),
        version: k.version.clone(),
        key: k.clone(),
        repo_name: k.name.clone(),
        dependants: vec![],
        original_dependants: vec![],
        deps: vec![],
        unused_deps: vec![],
        dep_reasons: vec![],
        loaded: true,
    })
}

fn aug_root(name: &str, version: &str) -> AB {
    let mut a = aug(ModuleKey::root());
    a.0.name = name.to_string();
    a.0.version = Version::parse(version).unwrap();
    a.0.repo_name = name.to_string();
    a
}

impl AB {
    fn dep(mut self, name: &str, v: &str) -> AB {
        self.0.deps.push((name.to_string(), key(name, v)));
        self.0.dep_reasons.push((name.to_string(), ResolutionReason::Original));
        self
    }
    fn changed_dep(mut self, name: &str, v: &str, old: &str, reason: ResolutionReason) -> AB {
        self.0.deps.push((name.to_string(), key(name, v)));
        self.0.unused_deps.push((name.to_string(), key(name, old)));
        self.0.dep_reasons.push((name.to_string(), reason));
        self
    }
    fn still_dependant(mut self, k: ModuleKey) -> AB {
        self.0.dependants.push(k.clone());
        self.0.original_dependants.push(k);
        self
    }
    fn dependant(mut self, k: ModuleKey) -> AB {
        self.0.dependants.push(k);
        self
    }
    fn original_dependant(mut self, k: ModuleKey) -> AB {
        self.0.original_dependants.push(k);
        self
    }
}

fn graph_of(mods: Vec<AB>) -> BTreeMap<ModuleKey, AugmentedModule> {
    mods.into_iter().map(|a| (a.0.key.clone(), a.0)).collect()
}

fn insp_of(mods: Vec<AB>) -> Inspector {
    Inspector::assemble(graph_of(mods), vec![], vec![], vec![], BTreeMap::new())
}

/// `ModOptions.getDefaultOptions()` of the Java tests.
fn test_opts() -> ModOptions {
    ModOptions { depth: i32::MAX, cycles: false, ..ModOptions::default() }
}

fn text_out(lines: &[&str]) -> String {
    format!("{}\n\n", lines.join("\n"))
}

fn graph_out(lines: &[&str]) -> String {
    format!("{}\n", lines.join("\n"))
}

fn root() -> ModuleKey {
    ModuleKey::root()
}

/// Compact rendering of a result graph for readable assertion failures.
fn dump(g: &ResultGraph) -> String {
    let mut s = String::new();
    for (k, n) in g {
        s.push_str(&format!("{k}{}:", if n.target { " #" } else { "" }));
        for (c, m) in n.children_sorted_by_edge_type() {
            s.push_str(&format!(
                " {c}[{}{}{}]",
                if m.expanded == IsExpanded::True { "E" } else { "-" },
                if m.indirect == IsIndirect::True { "I" } else { "-" },
                if m.cycle == IsCycle::True { "C" } else { "-" }
            ));
        }
        s.push('\n');
    }
    s
}

// ---------------------------------------------------------------------------------------------
// Executor tests (ported from ModExecutorTest)
// ---------------------------------------------------------------------------------------------

#[test]
fn expand_from_targets_first() {
    let insp = insp_of(vec![
        aug_root("aaa", "1.0").dep("bbb", "1.0"),
        aug(key("bbb", "1.0")).still_dependant(root()).dep("ccc", "1.0"),
        aug(key("ccc", "1.0")).still_dependant(key("bbb", "1.0")).dep("ddd", "1.0"),
        aug(key("ddd", "1.0")).still_dependant(key("ccc", "1.0")),
    ]);
    let opts = test_opts();
    let mut ex = ModExecutor::new(&insp, None, &opts);
    let result = ex.expand_and_prune(&[root(), key("ccc", "1.0")]);
    let mut expected = ResultGraph::new();
    expected.insert(
        root(),
        ResultNode::new()
            .with_child(&key("bbb", "1.0"), IsExpanded::True, IsIndirect::False)
            .with_child(&key("ccc", "1.0"), IsExpanded::True, IsIndirect::True),
    );
    expected.insert(
        key("bbb", "1.0"),
        ResultNode::new().with_child(&key("ccc", "1.0"), IsExpanded::False, IsIndirect::False),
    );
    expected.insert(
        key("ccc", "1.0"),
        ResultNode::new().with_child(&key("ddd", "1.0"), IsExpanded::True, IsIndirect::False),
    );
    expected.insert(key("ddd", "1.0"), ResultNode::new());
    assert_eq!(dump(&result), dump(&expected));
    assert_eq!(result, expected);
}

#[test]
fn paths_depth1_contains_all_targets_with_nested_indirect() {
    // <root> -> bbb -> ccc -> ddd -> eee -> fff -> ggg -> hhh, with eee -> ddd cycle.
    let insp = insp_of(vec![
        aug_root("aaa", "1.0").dep("bbb", "1.0"),
        aug(key("bbb", "1.0")).still_dependant(root()).dep("ccc", "1.0"),
        aug(key("ccc", "1.0")).still_dependant(key("bbb", "1.0")).dep("ddd", "1.0"),
        aug(key("ddd", "1.0"))
            .still_dependant(key("ccc", "1.0"))
            .still_dependant(key("eee", "1.0"))
            .dep("eee", "1.0"),
        aug(key("eee", "1.0")).still_dependant(key("ddd", "1.0")).dep("fff", "1.0").dep("ddd", "1.0"),
        aug(key("fff", "1.0")).still_dependant(key("eee", "1.0")).dep("ggg", "1.0"),
        aug(key("ggg", "1.0")).still_dependant(key("fff", "1.0")).dep("hhh", "1.0"),
        aug(key("hhh", "1.0")).still_dependant(key("ggg", "1.0")),
    ]);
    let opts = ModOptions { cycles: true, depth: 1, ..test_opts() };
    let mut ex = ModExecutor::new(&insp, None, &opts);
    let result = ex.expand_paths_to_targets(&[root()], &[key("eee", "1.0"), key("hhh", "1.0")], false);
    let mut expected = ResultGraph::new();
    expected.insert(root(), ResultNode::new().with_child(&key("bbb", "1.0"), IsExpanded::True, IsIndirect::False));
    expected.insert(
        key("bbb", "1.0"),
        ResultNode::new().with_child(&key("ddd", "1.0"), IsExpanded::True, IsIndirect::True),
    );
    expected.insert(
        key("ddd", "1.0"),
        ResultNode::new().with_child(&key("eee", "1.0"), IsExpanded::True, IsIndirect::False),
    );
    expected.insert(
        key("eee", "1.0"),
        ResultNode::new()
            .with_target(true)
            .with_child(&key("ggg", "1.0"), IsExpanded::True, IsIndirect::True),
    );
    expected.insert(
        key("ggg", "1.0"),
        ResultNode::new().with_child(&key("hhh", "1.0"), IsExpanded::True, IsIndirect::False),
    );
    expected.insert(key("hhh", "1.0"), ResultNode::new().with_target(true));
    assert_eq!(dump(&result), dump(&expected));
    assert_eq!(result, expected);
}

#[test]
fn paths_depth1_target_parent_is_direct_and_indirect_child() {
    let insp = insp_of(vec![
        aug_root("aaa", "1.0").dep("bbb", "1.0"),
        aug(key("bbb", "1.0")).still_dependant(root()).dep("ccc", "1.0").dep("ddd", "1.0"),
        aug(key("ccc", "1.0")).still_dependant(key("bbb", "1.0")).dep("ddd", "1.0"),
        aug(key("ddd", "1.0"))
            .still_dependant(key("bbb", "1.0"))
            .still_dependant(key("ccc", "1.0"))
            .still_dependant(key("eee", "1.0"))
            .dep("eee", "1.0"),
        aug(key("eee", "1.0"))
            .still_dependant(key("ddd", "1.0"))
            .still_dependant(key("eee", "1.0"))
            .dep("ddd", "1.0"),
    ]);
    let opts = ModOptions { cycles: true, depth: 1, ..test_opts() };
    let mut ex = ModExecutor::new(&insp, None, &opts);
    let result = ex.expand_paths_to_targets(&[root()], &[key("eee", "1.0")], false);
    let mut expected = ResultGraph::new();
    expected.insert(root(), ResultNode::new().with_child(&key("bbb", "1.0"), IsExpanded::True, IsIndirect::False));
    expected.insert(
        key("bbb", "1.0"),
        ResultNode::new().with_child(&key("ddd", "1.0"), IsExpanded::True, IsIndirect::False),
    );
    expected.insert(
        key("ddd", "1.0"),
        ResultNode::new().with_child(&key("eee", "1.0"), IsExpanded::True, IsIndirect::False),
    );
    expected.insert(key("eee", "1.0"), ResultNode::new().with_target(true));
    assert_eq!(dump(&result), dump(&expected));
    assert_eq!(result, expected);
}

fn explanation_graph() -> Inspector {
    insp_of(vec![
        aug_root("A", "1.0").dep("B", "1.0").dep("C", "1.0"),
        aug(key("B", "1.0"))
            .still_dependant(root())
            .changed_dep("C", "1.0", "0.1", ResolutionReason::MinimalVersionSelection)
            .changed_dep("E", "", "1.0", ResolutionReason::NonRegistryOverride),
        aug(key("C", "1.0"))
            .still_dependant(root())
            .dependant(key("B", "1.0"))
            .changed_dep("D", "1.5", "1.0", ResolutionReason::SingleVersionOverride),
        aug(key("C", "0.1")).original_dependant(key("B", "1.0")),
        aug(key("D", "1.0")).original_dependant(key("C", "1.0")),
        aug(key("D", "1.5")).dependant(key("C", "1.0")),
        aug(key("E", "1.0")).original_dependant(key("B", "1.0")),
        aug(key("E", "")).dependant(key("B", "1.0")),
    ])
}

#[test]
fn resolution_explanation_most_cases() {
    let insp = explanation_graph();
    let opts = ModOptions { verbose: true, include_unused: true, ..test_opts() };
    let result = ResultGraph::new();
    let imports = ImportsTable::default();
    let ctx = FormatCtx { result: &result, insp: &insp, ext_imports: &imports, opts: &opts };
    let ex = |k: ModuleKey, p: ModuleKey| ctx.extra_resolution_explanation(&k, &p);
    assert_eq!(ex(root(), root()), None);
    assert_eq!(ex(key("B", "1.0"), root()), None);
    assert_eq!(
        ex(key("C", "1.0"), key("B", "1.0")),
        Some(Explanation {
            changed_version: Version::parse("0.1").unwrap(),
            resolution_reason: ResolutionReason::MinimalVersionSelection,
            requested_by_modules: Some(vec![root()]),
        })
    );
    assert_eq!(
        ex(key("C", "0.1"), key("B", "1.0")),
        Some(Explanation {
            changed_version: Version::parse("1.0").unwrap(),
            resolution_reason: ResolutionReason::MinimalVersionSelection,
            requested_by_modules: Some(vec![root()]),
        })
    );
    assert_eq!(
        ex(key("D", "1.0"), key("C", "1.0")),
        Some(Explanation {
            changed_version: Version::parse("1.5").unwrap(),
            resolution_reason: ResolutionReason::SingleVersionOverride,
            requested_by_modules: None,
        })
    );
    assert_eq!(
        ex(key("D", "1.5"), key("C", "1.0")),
        Some(Explanation {
            changed_version: Version::parse("1.0").unwrap(),
            resolution_reason: ResolutionReason::SingleVersionOverride,
            requested_by_modules: None,
        })
    );
    assert_eq!(
        ex(key("E", "1.0"), key("B", "1.0")),
        Some(Explanation {
            changed_version: Version::empty(),
            resolution_reason: ResolutionReason::NonRegistryOverride,
            requested_by_modules: None,
        })
    );
    assert_eq!(
        ex(key("E", ""), key("B", "1.0")),
        Some(Explanation {
            changed_version: Version::parse("1.0").unwrap(),
            resolution_reason: ResolutionReason::NonRegistryOverride,
            requested_by_modules: None,
        })
    );
}

fn unused_and_indirect_mods() -> Vec<AB> {
    vec![
        aug_root("A", "1.0").dep("B", "1.0"),
        aug(key("B", "1.0"))
            .still_dependant(root())
            .changed_dep("C", "1.0", "0.1", ResolutionReason::SingleVersionOverride)
            .changed_dep("Y", "2.0", "1.0", ResolutionReason::MinimalVersionSelection),
        aug(key("C", "0.1")).original_dependant(key("B", "1.0")),
        aug(key("C", "1.0")).dependant(key("B", "1.0")).dep("D", "1.0"),
        aug(key("D", "1.0")).still_dependant(key("C", "1.0")).still_dependant(key("E", "1.0")).dep("E", "1.0"),
        aug(key("E", "1.0")).still_dependant(key("D", "1.0")).dep("F", "1.0").dep("D", "1.0"),
        aug(key("F", "1.0")).still_dependant(key("E", "1.0")).dep("G", "1.0"),
        aug(key("G", "1.0")).still_dependant(key("F", "1.0")).dep("H", "1.0").dep("Y", "2.0"),
        aug(key("H", "1.0")).still_dependant(key("G", "1.0")),
        aug(key("Y", "1.0")).original_dependant(key("B", "1.0")),
        aug(key("Y", "2.0")).dependant(key("B", "1.0")).still_dependant(key("G", "1.0")),
    ]
}

#[test]
fn text_and_graph_output_indirect_and_nested_target_paths_with_unused() {
    let insp = insp_of(unused_and_indirect_mods());
    let targets = [
        key("C", "0.1"),
        key("C", "1.0"),
        key("Y", "1.0"),
        key("Y", "2.0"),
        key("E", "1.0"),
        key("H", "1.0"),
    ];
    let mut opts = ModOptions {
        cycles: true,
        include_unused: true,
        verbose: true,
        depth: 1,
        output: OutputFormat::Text,
        ..test_opts()
    };
    // Double check for human error: the unformatted result graph.
    {
        let mut ex = ModExecutor::new(&insp, None, &opts);
        let result = ex.expand_paths_to_targets(&[root()], &targets, false);
        let mut expected = ResultGraph::new();
        expected.insert(root(), ResultNode::new().with_child(&key("B", "1.0"), IsExpanded::True, IsIndirect::False));
        expected.insert(
            key("B", "1.0"),
            ResultNode::new()
                .with_child(&key("Y", "1.0"), IsExpanded::True, IsIndirect::False)
                .with_child(&key("Y", "2.0"), IsExpanded::True, IsIndirect::False)
                .with_child(&key("C", "1.0"), IsExpanded::True, IsIndirect::False)
                .with_child(&key("C", "0.1"), IsExpanded::True, IsIndirect::False),
        );
        expected.insert(key("C", "0.1"), ResultNode::new().with_target(true));
        expected.insert(
            key("C", "1.0"),
            ResultNode::new().with_target(true).with_child(&key("D", "1.0"), IsExpanded::True, IsIndirect::False),
        );
        expected.insert(
            key("D", "1.0"),
            ResultNode::new().with_child(&key("E", "1.0"), IsExpanded::True, IsIndirect::False),
        );
        expected.insert(
            key("E", "1.0"),
            ResultNode::new().with_target(true).with_child(&key("G", "1.0"), IsExpanded::True, IsIndirect::True),
        );
        expected.insert(
            key("G", "1.0"),
            ResultNode::new()
                .with_child(&key("H", "1.0"), IsExpanded::True, IsIndirect::False)
                .with_child(&key("Y", "2.0"), IsExpanded::True, IsIndirect::False),
        );
        expected.insert(key("H", "1.0"), ResultNode::new().with_target(true));
        expected.insert(key("Y", "1.0"), ResultNode::new().with_target(true));
        expected.insert(key("Y", "2.0"), ResultNode::new().with_target(true));
        assert_eq!(dump(&result), dump(&expected));
    assert_eq!(result, expected);
    }
    let mut ex = ModExecutor::new(&insp, None, &opts);
    ex.all_paths(&[root()], &targets);
    assert_eq!(
        ex.take_output(),
        text_out(&[
            "<root> (A@1.0)",
            "└───B@1.0 ",
            "    ├───C@0.1 # (to 1.0, cause single_version_override)",
            "    ├───C@1.0 # (was 0.1, cause single_version_override)",
            "    │   └───D@1.0 ",
            "    │       └───E@1.0 # ",
            "    │           └╌╌╌G@1.0 ",
            "    │               ├───H@1.0 # ",
            "    │               └───Y@2.0 # ",
            "    ├───Y@1.0 # (to 2.0, cause G@1.0)",
            "    └───Y@2.0 # (was 1.0, cause G@1.0)",
        ])
    );

    opts.output = OutputFormat::Graph;
    let mut ex = ModExecutor::new(&insp, None, &opts);
    ex.all_paths(&[root()], &targets);
    assert_eq!(
        ex.take_output(),
        graph_out(&[
            "digraph mygraph {",
            "  node [ shape=box ]",
            "  edge [ fontsize=8 ]",
            "  \"<root>\" [ label=\"<root> (A@1.0)\" ]",
            "  \"<root>\" -> \"B@1.0\" [  ]",
            "  \"B@1.0\" -> \"C@0.1\" [ label=SVO ]",
            "  \"B@1.0\" -> \"C@1.0\" [ label=SVO ]",
            "  \"B@1.0\" -> \"Y@1.0\" [ label=MVS ]",
            "  \"B@1.0\" -> \"Y@2.0\" [ label=MVS ]",
            "  \"C@0.1\" [ shape=diamond style=dotted ]",
            "  \"C@1.0\" [ shape=diamond style=solid ]",
            "  \"C@1.0\" -> \"D@1.0\" [  ]",
            "  \"Y@1.0\" [ shape=diamond style=dotted ]",
            "  \"Y@2.0\" [ shape=diamond style=solid ]",
            "  \"D@1.0\" -> \"E@1.0\" [  ]",
            "  \"E@1.0\" [ shape=diamond style=solid ]",
            "  \"E@1.0\" -> \"G@1.0\" [ style=dashed ]",
            "  \"G@1.0\" -> \"H@1.0\" [  ]",
            "  \"G@1.0\" -> \"Y@2.0\" [  ]",
            "  \"H@1.0\" [ shape=diamond style=solid ]",
            "}",
        ])
    );
}

#[test]
fn unused_marker_without_verbose_and_ascii_charset() {
    let insp = insp_of(unused_and_indirect_mods());
    let opts = ModOptions {
        cycles: true,
        include_unused: true,
        depth: 1,
        charset: Charset::Ascii,
        ..test_opts()
    };
    let mut ex = ModExecutor::new(&insp, None, &opts);
    ex.all_paths(&[root()], &[key("C", "0.1"), key("C", "1.0"), key("E", "1.0")]);
    assert_eq!(
        ex.take_output(),
        text_out(&[
            "<root> (A@1.0)",
            "|___B@1.0 ",
            "    |___C@0.1 # (unused) ",
            "    |___C@1.0 # ",
            "        |___D@1.0 ",
            "            |___E@1.0 # ",
        ])
    );
}

fn id(pkg: &str, name: &str) -> ExtensionId {
    ExtensionId { bzl_file: format!("@@//{pkg}:{pkg}"), name: name.to_string(), isolation_key: None }
}

fn usage(
    id: &ExtensionId,
    loc: &str,
    imports: &[(&str, &str)],
    tags: Vec<Tag>,
) -> (ExtensionId, ExtensionUsage) {
    (
        id.clone(),
        ExtensionUsage {
            bzl_file: "//extensions:extensions.bzl".into(),
            extension_name: id.name.clone(),
            isolation_key: None,
            proxies: vec![Proxy {
                proxy_name: String::new(),
                dev_dependency: false,
                location: loc.to_string(),
                imports: imports.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
                containing_module_file: "MODULE.bazel".into(),
            }],
            tags,
            repo_overrides: BTreeMap::new(),
        },
    )
}

fn tag(name: &str, attrs: Vec<(&str, AttrVal)>) -> Tag {
    Tag {
        tag_name: name.into(),
        attrs: attrs.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        dev_dependency: false,
        location: "MODULE.bazel:1:1".into(),
    }
}

fn extension_fixture() -> (Inspector, ExtensionId, ExtensionId) {
    let maven = id("extensions", "maven");
    let gradle = id("extensions", "gradle");
    let mut usages = vec![];
    for (k, (i, u)) in [
        (key("C", "1.0"), usage(&maven, "C@1.0/MODULE.bazel:2:23", &[("repo1", "repo1"), ("repo3", "repo3")], vec![])),
        (key("D", "1.0"), usage(&maven, "D@1.0/MODULE.bazel:1:10", &[("repo1", "repo1"), ("repo2", "repo2")], vec![])),
        (key("Y", "2.0"), usage(&gradle, "Y@2.0/MODULE.bazel:2:13", &[("repo2", "repo2")], vec![])),
        (
            key("Y", "2.0"),
            usage(
                &maven,
                "Y@2.0/MODULE.bazel:13:10",
                &[("myrepo", "repo5")],
                vec![
                    tag("dep", vec![("coord", AttrVal::Str("junit".into()))]),
                    tag("dep", vec![("coord", AttrVal::Str("guava".into()))]),
                    tag(
                        "pom",
                        vec![(
                            "pom_xmls",
                            AttrVal::List(vec![AttrVal::Str("//:pom.xml".into()), AttrVal::Str("@bar//:pom.xml".into())]),
                        )],
                    ),
                ],
            ),
        ),
    ] {
        usages.push((i, k, u));
    }
    let repos = vec![
        (maven.clone(), ["repo6", "repo1", "repo2", "repo3", "repo4", "repo5"].iter().map(|s| s.to_string()).collect()),
        (gradle.clone(), ["repo1", "repo2"].iter().map(|s| s.to_string()).collect()),
    ];
    (Inspector::assemble(graph_of(unused_and_indirect_mods()), vec![], usages, repos, BTreeMap::new()), maven, gradle)
}

#[test]
fn extensions_info_text_and_graph() {
    let (insp, maven, _) = extension_fixture();
    let mut opts = ModOptions { output: OutputFormat::Text, extension_info: ExtensionShow::All, ..test_opts() };
    let mut ex = ModExecutor::new(&insp, None, &opts);
    ex.graph(&[root()]);
    assert_eq!(
        ex.take_output(),
        text_out(&[
            "<root> (A@1.0)",
            "└───B@1.0 ",
            "    ├───C@1.0 ",
            "    │   ├───$@@//extensions:extensions%maven ",
            "    │   │   ├───repo1",
            "    │   │   ├───repo3",
            "    │   │   ├╌╌╌repo4",
            "    │   │   └╌╌╌repo6",
            "    │   └───D@1.0 ",
            "    │       ├───$@@//extensions:extensions%maven ... ",
            "    │       │   ├───repo1",
            "    │       │   └───repo2",
            "    │       └───E@1.0 ",
            "    │           └───F@1.0 ",
            "    │               └───G@1.0 ",
            "    │                   ├───Y@2.0 (*) ",
            "    │                   └───H@1.0 ",
            "    └───Y@2.0 ",
            "        ├───$@@//extensions:extensions%gradle ",
            "        │   ├───repo2",
            "        │   └╌╌╌repo1",
            "        └───$@@//extensions:extensions%maven ... ",
            "            └───repo5",
        ])
    );

    opts.output = OutputFormat::Graph;
    let mut ex = ModExecutor::new(&insp, None, &opts);
    ex.graph(&[root()]);
    assert_eq!(
        ex.take_output(),
        graph_out(&[
            "digraph mygraph {",
            "  node [ shape=box ]",
            "  edge [ fontsize=8 ]",
            "  \"<root>\" [ label=\"<root> (A@1.0)\" ]",
            "  \"<root>\" -> \"B@1.0\" [  ]",
            "  \"B@1.0\" -> \"C@1.0\" [  ]",
            "  \"B@1.0\" -> \"Y@2.0\" [  ]",
            "  subgraph \"cluster_@@//extensions:extensions%maven\" {",
            "    label=\"@@//extensions:extensions%maven\"",
            "    \"@@//extensions:extensions%maven%repo1\" [ label=\"repo1\" ]",
            "    \"@@//extensions:extensions%maven%repo2\" [ label=\"repo2\" ]",
            "    \"@@//extensions:extensions%maven%repo3\" [ label=\"repo3\" ]",
            "    \"@@//extensions:extensions%maven%repo5\" [ label=\"repo5\" ]",
            "    \"@@//extensions:extensions%maven%repo4\" [ label=\"repo4\" style=dotted ]",
            "    \"@@//extensions:extensions%maven%repo6\" [ label=\"repo6\" style=dotted ]",
            "  }",
            "  \"C@1.0\" -> \"@@//extensions:extensions%maven%repo1\"",
            "  \"C@1.0\" -> \"@@//extensions:extensions%maven%repo3\"",
            "  \"C@1.0\" -> \"D@1.0\" [  ]",
            "  subgraph \"cluster_@@//extensions:extensions%gradle\" {",
            "    label=\"@@//extensions:extensions%gradle\"",
            "    \"@@//extensions:extensions%gradle%repo2\" [ label=\"repo2\" ]",
            "    \"@@//extensions:extensions%gradle%repo1\" [ label=\"repo1\" style=dotted ]",
            "  }",
            "  \"Y@2.0\" -> \"@@//extensions:extensions%gradle%repo2\"",
            "  \"Y@2.0\" -> \"@@//extensions:extensions%maven%repo5\"",
            "  \"D@1.0\" -> \"@@//extensions:extensions%maven%repo1\"",
            "  \"D@1.0\" -> \"@@//extensions:extensions%maven%repo2\"",
            "  \"D@1.0\" -> \"E@1.0\" [  ]",
            "  \"E@1.0\" -> \"F@1.0\" [  ]",
            "  \"F@1.0\" -> \"G@1.0\" [  ]",
            "  \"G@1.0\" -> \"H@1.0\" [  ]",
            "  \"G@1.0\" -> \"Y@2.0\" [  ]",
            "}",
        ])
    );

    // Extension filter on maven + paths to Y@2.0, depth 1.
    opts.output = OutputFormat::Text;
    opts.depth = 1;
    let mut ex = ModExecutor::new(&insp, Some(MaybeCompleteExtSet::Set(vec![maven])), &opts);
    ex.all_paths(&[root()], &[key("Y", "2.0")]);
    assert_eq!(
        ex.take_output(),
        text_out(&[
            "<root> (A@1.0)",
            "└───B@1.0 ",
            "    ├───C@1.0 # ",
            "    │   ├───$@@//extensions:extensions%maven ",
            "    │   │   ├───repo1",
            "    │   │   ├───repo3",
            "    │   │   ├╌╌╌repo4",
            "    │   │   └╌╌╌repo6",
            "    │   └───D@1.0 # ",
            "    │       ├───$@@//extensions:extensions%maven ... ",
            "    │       │   ├───repo1",
            "    │       │   └───repo2",
            "    │       └╌╌╌G@1.0 ",
            "    │           └───Y@2.0 # ",
            "    │               └───$@@//extensions:extensions%maven ... ",
            "    │                   └───repo5",
            "    └───Y@2.0 # ",
            "        └───$@@//extensions:extensions%maven ... ",
            "            └───repo5",
        ])
    );
}

#[test]
fn extensions_info_usages_repos_and_json() {
    let (insp, _, _) = extension_fixture();
    let mut opts = ModOptions {
        output: OutputFormat::Text,
        extension_info: ExtensionShow::Usages,
        depth: 1,
        ..test_opts()
    };
    let mut ex = ModExecutor::new(&insp, None, &opts);
    ex.graph(&[key("Y", "2.0")]);
    assert_eq!(
        ex.take_output(),
        text_out(&[
            "<root> (A@1.0)",
            "└╌╌╌Y@2.0 ",
            "    ├───$@@//extensions:extensions%gradle ",
            "    └───$@@//extensions:extensions%maven ",
        ])
    );
    opts.extension_info = ExtensionShow::Repos;
    let mut ex = ModExecutor::new(&insp, None, &opts);
    ex.graph(&[key("Y", "2.0")]);
    assert_eq!(
        ex.take_output(),
        text_out(&[
            "<root> (A@1.0)",
            "└╌╌╌Y@2.0 ",
            "    ├───$@@//extensions:extensions%gradle ",
            "    │   └───repo2",
            "    └───$@@//extensions:extensions%maven ",
            "        └───repo5",
        ])
    );
    opts.output = OutputFormat::Json;
    opts.extension_info = ExtensionShow::All;
    let mut ex = ModExecutor::new(&insp, None, &opts);
    ex.graph(&[key("Y", "2.0")]);
    let out = ex.take_output();
    let j = crate::json::Json::parse(&out).unwrap();
    let y = &j.get("indirectDependencies").unwrap().as_arr().unwrap()[0];
    assert_eq!(y.get("key").unwrap().as_str(), Some("Y@2.0"));
    // Y@2.0 is not a dependency of <root> here, so the root refers to it by no name at all...
    assert_eq!(y.get("apparentName"), None);
    let usages = y.get("extensionUsages").unwrap().as_arr().unwrap();
    assert_eq!(usages.len(), 2);
    assert_eq!(usages[0].get("key").unwrap().as_str(), Some("@@//extensions:extensions%gradle"));
    assert_eq!(usages[0].get("unexpanded").unwrap().as_bool(), Some(false));
    assert_eq!(usages[0].get("used_repos").unwrap().as_arr().unwrap()[0].as_str(), Some("repo2"));
    assert_eq!(usages[0].get("unused_repos").unwrap().as_arr().unwrap()[0].as_str(), Some("repo1"));
    assert_eq!(usages[1].get("key").unwrap().as_str(), Some("@@//extensions:extensions%maven"));
}

#[test]
fn path_and_all_paths_in_complex_graph() {
    // <root> -> A -> B -> C -> D;  A -> E -> F -> C;  <root> -> G -> H;  <root> -> I -> D
    let insp = insp_of(vec![
        aug_root("main", "1.0").dep("A", "1.0").dep("G", "1.0").dep("I", "1.0"),
        aug(key("A", "1.0")).still_dependant(root()).dep("B", "1.0").dep("E", "1.0"),
        aug(key("B", "1.0")).still_dependant(key("A", "1.0")).dep("C", "1.0"),
        aug(key("C", "1.0")).still_dependant(key("B", "1.0")).still_dependant(key("F", "1.0")).dep("D", "1.0"),
        aug(key("D", "1.0")).still_dependant(key("C", "1.0")).still_dependant(key("I", "1.0")),
        aug(key("E", "1.0")).still_dependant(key("A", "1.0")).dep("F", "1.0"),
        aug(key("F", "1.0")).still_dependant(key("E", "1.0")).dep("C", "1.0"),
        aug(key("G", "1.0")).still_dependant(root()).dep("H", "1.0"),
        aug(key("H", "1.0")).still_dependant(key("G", "1.0")),
        aug(key("I", "1.0")).still_dependant(root()).dep("D", "1.0"),
    ]);
    let opts = ModOptions { output: OutputFormat::Text, ..test_opts() };
    let run = |f: &dyn Fn(&mut ModExecutor)| {
        let mut ex = ModExecutor::new(&insp, None, &opts);
        f(&mut ex);
        ex.take_output()
    };
    let d = key("D", "1.0");
    let g = key("G", "1.0");
    assert_eq!(
        run(&|ex| ex.all_paths(&[root()], &[d.clone()])),
        text_out(&[
            "<root> (main@1.0)",
            "├───A@1.0 ",
            "│   └───B@1.0 ",
            "│       └───C@1.0 ",
            "│           └───D@1.0 # ",
            "└───I@1.0 ",
            "    └───D@1.0 # ",
        ])
    );
    assert_eq!(
        run(&|ex| ex.path(&[root()], &[d.clone()])),
        text_out(&["<root> (main@1.0)", "└───I@1.0 ", "    └───D@1.0 # "])
    );
    assert_eq!(
        run(&|ex| ex.all_paths(&[root()], &[d.clone(), g.clone()])),
        text_out(&[
            "<root> (main@1.0)",
            "├───A@1.0 ",
            "│   └───B@1.0 ",
            "│       └───C@1.0 ",
            "│           └───D@1.0 # ",
            "├───G@1.0 # ",
            "└───I@1.0 ",
            "    └───D@1.0 # ",
        ])
    );
    assert_eq!(
        run(&|ex| ex.path(&[root()], &[d.clone(), g.clone()])),
        text_out(&["<root> (main@1.0)", "├───G@1.0 # ", "└───I@1.0 ", "    └───D@1.0 # "])
    );
    assert_eq!(
        run(&|ex| ex.all_paths(&[key("E", "1.0")], &[d.clone()])),
        text_out(&[
            "<root> (main@1.0)",
            "└╌╌╌E@1.0 ",
            "    └───F@1.0 ",
            "        └───C@1.0 ",
            "            └───D@1.0 # ",
        ])
    );
    // Messages for empty queries.
    assert_eq!(run(&|ex| ex.path(&[root()], &[])), "No target modules specified.\n");
    assert_eq!(
        run(&|ex| ex.path(&[key("H", "1.0")], &[key("A", "1.0")])),
        "No path found to the specified target modules.\n"
    );
}

#[test]
fn graph_with_cycle() {
    let insp = insp_of(vec![
        aug_root("main", "1.0").dep("A", "1.0"),
        aug(key("A", "1.0")).still_dependant(root()).still_dependant(key("B", "1.0")).dep("B", "1.0"),
        aug(key("B", "1.0")).still_dependant(key("A", "1.0")).dep("A", "1.0"),
    ]);
    let opts = ModOptions { cycles: true, ..test_opts() };
    let mut ex = ModExecutor::new(&insp, None, &opts);
    ex.graph(&[root()]);
    assert_eq!(
        ex.take_output(),
        text_out(&["<root> (main@1.0)", "└───A@1.0 ", "    └───B@1.0 ", "        └───A@1.0 (cycle) "])
    );
    // Without --cycles the back edge is dropped.
    let opts = ModOptions { cycles: false, ..test_opts() };
    let mut ex = ModExecutor::new(&insp, None, &opts);
    ex.graph(&[root()]);
    assert_eq!(ex.take_output(), text_out(&["<root> (main@1.0)", "└───A@1.0 ", "    └───B@1.0 "]));
}

#[test]
fn graph_with_extension_filter_on_root() {
    let maven = id("extensions", "maven");
    let insp = Inspector::assemble(
        graph_of(vec![aug_root("main", "1.0")]),
        vec![],
        vec![{
            let (i, u) = usage(&maven, "MODULE.bazel:1:1", &[("repo1", "repo1")], vec![]);
            (i, root(), u)
        }],
        vec![(maven.clone(), vec!["repo1".to_string()])],
        BTreeMap::new(),
    );
    let opts = ModOptions { output: OutputFormat::Text, extension_info: ExtensionShow::All, ..test_opts() };
    let mut ex = ModExecutor::new(&insp, Some(MaybeCompleteExtSet::Set(vec![maven])), &opts);
    ex.graph(&[root()]);
    assert_eq!(
        ex.take_output(),
        text_out(&["<root> (main@1.0)", "└───$@@//extensions:extensions%maven ", "    └───repo1"])
    );
}

#[test]
fn graph_with_extension_filter_and_cycle() {
    let ext = ExtensionId {
        bzl_file: "@@A+1.0//extensions:extensions".into(),
        name: "ext".into(),
        isolation_key: None,
    };
    let mods = vec![
        aug_root("main", "1.0").dep("A", "1.0").dep("B", "1.0"),
        aug(key("A", "1.0")).still_dependant(root()).still_dependant(key("B", "1.0")).dep("B", "1.0"),
        aug(key("B", "1.0")).still_dependant(root()).still_dependant(key("A", "1.0")).dep("A", "1.0"),
    ];
    let (i1, u1) = usage(&ext, "MODULE.bazel:1:1", &[("repo1", "repo1")], vec![]);
    let (i2, u2) = usage(&ext, "MODULE.bazel:1:1", &[("repo2", "repo2")], vec![]);
    let insp = Inspector::assemble(
        graph_of(mods),
        vec![],
        vec![(i1, key("A", "1.0"), u1), (i2, key("B", "1.0"), u2)],
        vec![(ext.clone(), vec!["repo1".to_string(), "repo2".to_string()])],
        BTreeMap::new(),
    );
    let opts = ModOptions {
        output: OutputFormat::Text,
        extension_info: ExtensionShow::All,
        cycles: true,
        ..test_opts()
    };
    let mut ex = ModExecutor::new(&insp, Some(MaybeCompleteExtSet::Set(vec![ext])), &opts);
    ex.graph(&[root()]);
    assert_eq!(
        ex.take_output(),
        text_out(&[
            "<root> (main@1.0)",
            "├───A@1.0 # ",
            "│   ├───$@@A+1.0//extensions:extensions%ext ",
            "│   │   └───repo1",
            "│   ├───B@1.0 (cycle) ",
            "│   └───B@1.0 # ",
            "│       ├───$@@A+1.0//extensions:extensions%ext ... ",
            "│       │   └───repo2",
            "│       ├───A@1.0 (cycle) ",
            "│       └───A@1.0 (cycle) ",
            "└───B@1.0 # ",
            "    ├───$@@A+1.0//extensions:extensions%ext ... ",
            "    │   └───repo2",
            "    ├───A@1.0 (cycle) ",
            "    └───A@1.0 # ",
            "        ├───$@@A+1.0//extensions:extensions%ext ... ",
            "        │   └───repo1",
            "        ├───B@1.0 (cycle) ",
            "        └───B@1.0 (cycle) ",
        ])
    );
}

#[test]
fn json_output_with_cycle() {
    let insp = insp_of(vec![
        aug_root("main", "1.0").dep("A", "1.0"),
        aug(key("A", "1.0")).still_dependant(root()).still_dependant(key("B", "1.0")).dep("B", "1.0"),
        aug(key("B", "1.0")).still_dependant(key("A", "1.0")).dep("A", "1.0"),
    ]);
    let opts = ModOptions { cycles: true, output: OutputFormat::Json, ..test_opts() };
    let mut ex = ModExecutor::new(&insp, None, &opts);
    ex.graph(&[root()]);
    let expected = r#"{
  "key": "<root>",
  "name": "main",
  "version": "1.0",
  "apparentName": "main",
  "dependencies": [
    {
      "key": "A@1.0",
      "name": "A",
      "version": "1.0",
      "apparentName": "A",
      "dependencies": [
        {
          "key": "B@1.0",
          "name": "B",
          "version": "1.0",
          "apparentName": "B",
          "dependencies": [],
          "indirectDependencies": [],
          "cycles": [
            {
              "key": "A@1.0",
              "name": "A",
              "version": "1.0",
              "apparentName": "A",
              "unexpanded": true
            }
          ]
        }
      ],
      "indirectDependencies": [],
      "cycles": []
    }
  ],
  "indirectDependencies": [],
  "cycles": [],
  "root": true
}
"#;
    assert_eq!(ex.take_output(), expected);
}

// ---------------------------------------------------------------------------------------------
// End-to-end tests over resolutions built from file:// registries
// ---------------------------------------------------------------------------------------------

fn add(reg: &Path, name: &str, ver: &str, body: &str) {
    add_module(
        reg,
        name,
        ver,
        &format!("module(name='{name}', version='{ver}')\n{body}"),
        r#"{"url":"https://example.com/x.tgz","integrity":"sha256-AAAA","strip_prefix":"x-1"}"#,
    );
}

/// Root depends on a, b, c@1.0; a -> c@1.0, b -> c@1.2 (so c@1.0 loses to c@1.2 by MVS).
fn mvs_resolution(tag: &str) -> Resolution {
    let reg = tmpdir(&format!("modreg-{tag}"));
    let url = format!("file://{}", reg.display());
    add(&reg, "a", "1.0", "bazel_dep(name='c', version='1.0')");
    add(&reg, "b", "1.0", "bazel_dep(name='c', version='1.2')\nbazel_dep(name='d', version='1.0')");
    add(&reg, "c", "1.0", "");
    add(&reg, "c", "1.2", "");
    add(&reg, "d", "1.0", "");
    let ws = tmpdir(&format!("modws-{tag}"));
    write(
        &ws,
        "MODULE.bazel",
        "module(name='root')\nbazel_dep(name='a', version='1.0')\nbazel_dep(name='b', version='1.0')\nbazel_dep(name='c', version='1.0')\n",
    );
    let mut o = ResolveOptions::new(&ws);
    o.registries = vec![url];
    o.check_direct_dependencies = CheckMode::Off;
    resolve(&o).unwrap_or_else(|e| panic!("{e}"))
}

fn run(res: &Resolution, sub: &str, args: &[&str], opts: &ModOptions) -> Result<String, ModError> {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    run_mod(res, sub, &args, opts, &NoRepoSpecs, &NoExtensionInfo)
}

#[test]
fn e2e_graph_and_deps() {
    let res = mvs_resolution("graph");
    let opts = ModOptions::default();
    assert_eq!(
        run(&res, "graph", &[], &opts).unwrap(),
        text_out(&[
            "<root> (root@_)",
            "├───a@1.0 ",
            "│   └───c@1.2 (*) ",
            "├───b@1.0 ",
            "│   ├───c@1.2 (*) ",
            "│   └───d@1.0 ",
            "└───c@1.2 ",
        ])
    );
    // deps defaults to depth 2.
    assert_eq!(
        run(&res, "deps", &["b"], &opts).unwrap(),
        text_out(&["<root> (root@_)", "└───b@1.0 ", "    ├───c@1.2 ", "    └───d@1.0 "])
    );
    assert_eq!(
        run(&res, "deps", &[], &opts).unwrap(),
        text_out(&["<root> (root@_)"])
    );
    // An explicit --depth overrides the per-subcommand default.
    let o1 = ModOptions { depth: 1, ..ModOptions::default() };
    assert_eq!(
        run(&res, "graph", &[], &o1).unwrap(),
        text_out(&["<root> (root@_)", "├───a@1.0 ", "├───b@1.0 ", "└───c@1.2 "])
    );
    // --include_unused adds the unused original dependency c@1.0.
    let ou = ModOptions { include_unused: true, depth: 1, ..ModOptions::default() };
    assert_eq!(
        run(&res, "graph", &[], &ou).unwrap(),
        text_out(&["<root> (root@_)", "├───a@1.0 ", "├───b@1.0 ", "├───c@1.0 (unused) ", "└───c@1.2 "])
    );
    // ASCII charset.
    let oa = ModOptions { charset: Charset::Ascii, depth: 1, ..ModOptions::default() };
    assert_eq!(
        run(&res, "graph", &[], &oa).unwrap(),
        text_out(&["<root> (root@_)", "|___a@1.0 ", "|___b@1.0 ", "|___c@1.2 "])
    );
}

#[test]
fn e2e_explain_and_verbose() {
    let res = mvs_resolution("explain");
    let o = ModOptions { verbose: true, ..ModOptions::default() };
    assert_eq!(
        run(&res, "explain", &["c"], &o).unwrap(),
        text_out(&[
            "<root> (root@_)",
            "├───a@1.0 ",
            "│   └───c@1.2 # (was 1.0, cause b@1.0)",
            "├───b@1.0 ",
            "│   └───c@1.2 # ",
            "└───c@1.2 # (was 1.0, cause b@1.0)",
        ])
    );
}

#[test]
fn e2e_explain_with_unused() {
    let res = mvs_resolution("explain-unused");
    let ou = ModOptions { verbose: true, include_unused: true, ..ModOptions::default() };
    assert_eq!(
        run(&res, "all_paths", &["c@1.0"], &ou).unwrap(),
        text_out(&[
            "<root> (root@_)",
            "├───a@1.0 ",
            "│   └───c@1.0 # (to 1.2, cause b@1.0)",
            "└───c@1.0 # (to 1.2, cause b@1.0)",
        ])
    );
}

#[test]
fn e2e_path_and_all_paths() {
    let res = mvs_resolution("paths");
    let o = ModOptions::default();
    assert_eq!(
        run(&res, "all_paths", &["d"], &o).unwrap(),
        text_out(&["<root> (root@_)", "└───b@1.0 ", "    └───d@1.0 # "])
    );
    assert_eq!(run(&res, "path", &["c"], &o).unwrap(), text_out(&["<root> (root@_)", "└───c@1.2 # "]));
    // --from selects the start of the path search.
    let of = ModOptions { from: vec!["b".into()], ..ModOptions::default() };
    assert_eq!(
        run(&res, "path", &["d"], &of).unwrap(),
        text_out(&["<root> (root@_)", "└───b@1.0 ", "    └───d@1.0 # "])
    );
    assert_eq!(run(&res, "path", &[], &o).unwrap(), "No target modules specified.\n");
}

#[test]
fn e2e_graphviz_and_json() {
    let res = mvs_resolution("fmt");
    let og = ModOptions { output: OutputFormat::Graph, verbose: true, depth: 1, ..ModOptions::default() };
    assert_eq!(
        run(&res, "graph", &[], &og).unwrap(),
        graph_out(&[
            "digraph mygraph {",
            "  node [ shape=box ]",
            "  edge [ fontsize=8 ]",
            "  \"<root>\" [ label=\"<root> (root@)\" ]",
            "  \"<root>\" -> \"a@1.0\" [  ]",
            "  \"<root>\" -> \"b@1.0\" [  ]",
            "  \"<root>\" -> \"c@1.2\" [ label=MVS ]",
            "}",
        ])
    );
    let oj = ModOptions { output: OutputFormat::Json, verbose: true, depth: 1, ..ModOptions::default() };
    let out = run(&res, "graph", &[], &oj).unwrap();
    let j = crate::json::Json::parse(&out).unwrap();
    assert_eq!(j.get("root").unwrap().as_bool(), Some(true));
    let deps = j.get("dependencies").unwrap().as_arr().unwrap();
    assert_eq!(deps.len(), 3);
    assert_eq!(deps[2].get("key").unwrap().as_str(), Some("c@1.2"));
    assert_eq!(deps[2].get("originalVersion").unwrap().as_str(), Some("1.0"));
    // Faithful to Bazel: "resolutionReason" carries the changed version.
    assert_eq!(deps[2].get("resolutionReason").unwrap().as_str(), Some("1.0"));
    assert_eq!(deps[2].get("apparentName").unwrap().as_str(), Some("c"));
    assert!(out.ends_with("}\n"));
}

#[test]
fn e2e_errors() {
    let res = mvs_resolution("errors");
    let o = ModOptions::default();
    let e = run(&res, "", &[], &o).unwrap_err();
    assert_eq!(e.exit_code(), 37);
    assert_eq!(
        e.message,
        "No subcommand specified, choose one of : (deps, graph, all_paths, path, explain, show_repo, show_extension, dump_repo_mapping, tidy)."
    );
    let e = run(&res, "bogus", &[], &o).unwrap_err();
    assert_eq!(e.exit_code(), 37);
    assert_eq!(
        e.message,
        "Invalid subcommand, choose one from : (deps, graph, all_paths, path, explain, show_repo, show_extension, dump_repo_mapping, tidy)."
    );
    let e = run(&res, "graph", &["a"], &o).unwrap_err();
    assert_eq!(e.exit_code(), 2);
    assert_eq!(e.message, "the 'graph' command doesn't take extra arguments. Type 'bazel help mod' for syntax and help.");
    let e = run(&res, "deps", &["nope"], &o).unwrap_err();
    assert_eq!(
        e.message,
        "In module argument nope: Module nope does not exist in the dependency graph. Type 'bazel help mod' for syntax and help."
    );
    let e = run(&res, "deps", &["c@1.0"], &o).unwrap_err();
    assert_eq!(
        e.message,
        "In module argument c@1.0: Module version c@1.0 is unused as a result of module resolution. Use the --include_unused flag to include it. Type 'bazel help mod' for syntax and help."
    );
    let e = run(&res, "deps", &["c@9.9"], &o).unwrap_err();
    assert_eq!(
        e.message,
        "In module argument c@9.9: Module version c@9.9 does not exist, available versions: [c@1.2]. Type 'bazel help mod' for syntax and help."
    );
    let e = run(&res, "deps", &["foo@"], &o).unwrap_err();
    assert_eq!(
        e.message,
        "In module argument foo@: invalid argument 'foo@': use _ for the empty version. Type 'bazel help mod' for syntax and help."
    );
    let e = run(&res, "deps", &["@zzz"], &o).unwrap_err();
    assert_eq!(
        e.message,
        "In module argument @zzz: No module with the apparent repo name @zzz exists in the dependency graph. Type 'bazel help mod' for syntax and help."
    );
    let bad_out = ModOptions { output: OutputFormat::Graph, ..ModOptions::default() };
    let e = run(&res, "show_extension", &[], &bad_out).unwrap_err();
    assert_eq!(
        e.message,
        "Invalid --output 'graph' for the 'show_extension' subcommand. Only 'text' is supported. Type 'bazel help mod' for syntax and help."
    );
    let bad_out = ModOptions { output: OutputFormat::StreamedProto, ..ModOptions::default() };
    let e = run(&res, "graph", &[], &bad_out).unwrap_err();
    assert_eq!(
        e.message,
        "Invalid --output 'streamed_proto' for the 'graph' subcommand. Only 'text', 'json', and 'graph' are supported. Type 'bazel help mod' for syntax and help."
    );
    let both = ModOptions { all_repos: true, all_visible_repos: true, ..ModOptions::default() };
    let e = run(&res, "show_repo", &[], &both).unwrap_err();
    assert_eq!(e.kind, ModErrorKind::TooManyArguments);
    assert_eq!(
        e.message,
        "the 'show_repo' command requires exactly one of --all_repos, --all_visible_repos, or a list of repo arguments. Type 'bazel help mod' for syntax and help."
    );
    let e = run(&res, "graph", &[], &ModOptions { all_repos: true, ..ModOptions::default() }).unwrap_err();
    assert_eq!(
        e.message,
        "the 'graph' command doesn't take the --all_repos option. Type 'bazel help mod' for syntax and help."
    );
    // Bad --from / --base_module values.
    let e = run(&res, "graph", &[], &ModOptions { from: vec!["c@1.0".into()], ..ModOptions::default() }).unwrap_err();
    assert!(e.message.starts_with("In --from [c@1.0] option: Module version c@1.0 is unused"), "{}", e.message);
    let e = run(&res, "graph", &[], &ModOptions { base_module: "c@1.0".into(), ..ModOptions::default() }).unwrap_err();
    assert!(e.message.starts_with("In --base_module c@1.0 option: Module version c@1.0 does not exist"), "{}", e.message);
    assert!(e.message.contains("(Note that unused modules cannot be used here)"));
    // tidy / dump_repo_mapping are handled by the caller.
    assert_eq!(run(&res, "tidy", &[], &o).unwrap_err().message, "tidy not handled here");
}

#[test]
fn e2e_base_module_and_apparent_names() {
    let res = mvs_resolution("base");
    // Relative to b, @d is its dependency and `deps @d` works; relative to root it does not.
    let ob = ModOptions { base_module: "b@1.0".into(), ..ModOptions::default() };
    assert_eq!(run(&res, "deps", &["@d"], &ob).unwrap(), text_out(&["<root> (root@_)", "└╌╌╌d@1.0 "]));
    assert!(run(&res, "deps", &["@d"], &ModOptions::default()).is_err());
    // @@canonical names.
    assert_eq!(
        run(&res, "deps", &["@@d+"], &ModOptions::default()).unwrap(),
        text_out(&["<root> (root@_)", "└╌╌╌d@1.0 "])
    );
}

struct TestProvider {
    specs: BTreeMap<String, RepoSpec>,
    repos: Vec<(ExtensionId, Vec<String>)>,
}

impl RepoSpecProvider for TestProvider {
    fn repo_spec(&self, canonical: &str) -> Option<RepoSpec> {
        self.specs.get(canonical).cloned()
    }
    fn external_directory(&self) -> String {
        "/ob/external".into()
    }
    fn repo_override_path(&self, canonical: &str) -> Option<String> {
        (canonical == "bazel_tools").then(|| "/install/embedded_tools".to_string())
    }
}

impl ExtensionInfoProvider for TestProvider {
    fn generated_repos(&self, id: &ExtensionId) -> Option<Vec<String>> {
        self.repos.iter().find(|(i, _)| i == id).map(|(_, r)| r.clone())
    }
}

fn ext_resolution(tag: &str) -> Resolution {
    let reg = tmpdir(&format!("modreg-ext-{tag}"));
    let url = format!("file://{}", reg.display());
    add(&reg, "b", "1.0", "bazel_dep(name='a', version='1.0')");
    add(&reg, "a", "1.0", "");
    let ws = tmpdir(&format!("modws-ext-{tag}"));
    write(
        &ws,
        "MODULE.bazel",
        "module(name='root')\n\
         bazel_dep(name='b', version='1.0')\n\
         ext = use_extension('@b//:ext.bzl', 'myext')\n\
         ext.dep(name='x', coords=['junit', 'guava'], flag=True, n=3)\n\
         use_repo(ext, 'hub', alias='repo2')\n\
         rr = use_repo_rule('@b//:r.bzl', 'myrule')\n\
         rr(name='direct', value='v')\n",
    );
    let mut o = ResolveOptions::new(&ws);
    o.registries = vec![url];
    o.check_direct_dependencies = CheckMode::Off;
    resolve(&o).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn e2e_show_extension_and_extension_info() {
    let res = ext_resolution("t0");
    let ext_id = res.extension_ids().into_iter().find(|i| i.name == "myext").unwrap();
    assert_eq!(ext_id.bzl_file, "@@b+//:ext.bzl");
    let provider = TestProvider {
        specs: BTreeMap::new(),
        repos: vec![(ext_id.clone(), vec!["hub".into(), "repo2".into(), "other".into()])],
    };
    let o = ModOptions::default();
    let args = vec!["@b//:ext.bzl%myext".to_string()];
    let out = run_mod(&res, "show_extension", &args, &o, &provider, &provider).unwrap();
    let (prefix, usage_part) = out.split_once("## Usage in").unwrap();
    assert_eq!(
        prefix,
        "## @@b+//:ext.bzl%myext:\n\nFetched repositories:\n  - hub (imported by <root>)\n  - repo2 (imported by <root>)\n  - other\n\n"
    );
    assert!(usage_part.starts_with(" <root> from "), "{usage_part}");
    assert!(usage_part.contains("MODULE.bazel"), "{usage_part}");
    assert!(
        usage_part.contains("\nmyext.dep(name=\"x\", coords=[\"junit\", \"guava\"], flag=True, n=3)\n"),
        "{usage_part}"
    );
    assert!(usage_part.ends_with("use_repo(\n  myext,\n  \"hub\",\n  alias=\"repo2\",\n)\n\n"), "{usage_part}");

    // Innate extensions (use_repo_rule) are printed as use_repo_rule calls.
    let innate = res.extension_ids().into_iter().find(|i| i.is_innate()).unwrap();
    assert_eq!(innate.name, "@b//:r.bzl myrule");
    let iarg = format!("//:MODULE.bazel%{}", innate.name);
    let _ = iarg;
    let out = run_mod(
        &res,
        "show_extension",
        &["@b//:ext.bzl%myext".to_string()],
        &ModOptions { extension_usages: vec!["<root>".into()], ..ModOptions::default() },
        &provider,
        &provider,
    )
    .unwrap();
    assert!(out.contains("## Usage in <root> from "));

    // Unknown extension.
    let e = run_mod(&res, "show_extension", &["@b//:nope.bzl%nope".to_string()], &o, &provider, &provider)
        .unwrap_err();
    assert_eq!(
        e.message,
        "No extension @@b+//:nope.bzl%nope exists in the dependency graph. Type 'bazel help mod' for syntax and help."
    );
    // Bad extension argument.
    let e = run_mod(&res, "show_extension", &["nolabel".to_string()], &o, &provider, &provider).unwrap_err();
    assert_eq!(
        e.message,
        "In extension argument nolabel: Invalid argument nolabel: missing .bzl label. Type 'bazel help mod' for syntax and help."
    );
}

#[test]
fn e2e_graph_with_extension_info() {
    let res = ext_resolution("t1");
    let ext_id = res.extension_ids().into_iter().find(|i| i.name == "myext").unwrap();
    let provider = TestProvider {
        specs: BTreeMap::new(),
        repos: vec![(ext_id, vec!["hub".into(), "repo2".into(), "other".into()])],
    };
    let og = ModOptions { extension_info: ExtensionShow::All, ..ModOptions::default() };
    let out = run_mod(&res, "graph", &[], &og, &provider, &provider).unwrap();
    assert_eq!(
        out,
        text_out(&[
            "<root> (root@_)",
            "├───$@@//:MODULE.bazel%@b//:r.bzl myrule ",
            "│   └───direct",
            "├───$@@b+//:ext.bzl%myext ",
            "│   ├───hub",
            "│   ├───repo2",
            "│   └╌╌╌other",
            "└───b@1.0 ",
            "    └───a@1.0 ",
        ])
    );
    // --extension_filter keeps only paths to modules using the extension (here: the root).
    let of = ModOptions {
        extension_info: ExtensionShow::Usages,
        extension_filter: Some(vec!["@b//:ext.bzl%myext".into()]),
        ..ModOptions::default()
    };
    let out = run_mod(&res, "graph", &[], &of, &provider, &provider).unwrap();
    assert_eq!(out, text_out(&["<root> (root@_)", "└───$@@b+//:ext.bzl%myext "]).replace("└───$", "└───$"));
    // An empty filter list means "all extensions".
    let oe = ModOptions { extension_filter: Some(vec![]), extension_info: ExtensionShow::Usages, ..ModOptions::default() };
    let out = run_mod(&res, "graph", &[], &oe, &provider, &provider).unwrap();
    assert!(out.contains("$@@b+//:ext.bzl%myext"), "{out}");
}

#[test]
fn e2e_show_repo() {
    let res = mvs_resolution("showrepo");
    let ext_id = ExtensionId { bzl_file: "@@b+//:ext.bzl".into(), name: "myext".into(), isolation_key: None };
    let mut specs = BTreeMap::new();
    specs.insert(
        "b++myext+hub".to_string(),
        RepoSpec::new(
            "@@b+//:ext.bzl",
            "data_repo",
            vec![("name".into(), AttrVal::Str("hub".into())), ("data".into(), AttrVal::Str("x".into()))],
        ),
    );
    let provider = TestProvider { specs, repos: vec![(ext_id, vec!["hub".into()])] };
    let o = ModOptions::default();
    let out = run_mod(&res, "show_repo", &["a".to_string()], &o, &provider, &provider).unwrap();
    let spec = res.dep_graph.iter().find(|(k, _)| k.name == "a").unwrap().1.repo_spec.clone().unwrap();
    let mut expected = String::from("## a@1.0:\n");
    expected.push_str(
        "load(\"@@bazel_tools//tools/build_defs/repo:http.bzl\", \"http_archive\")\nhttp_archive(\n  name = \"a+\",\n",
    );
    for (k, v) in &spec.attrs {
        expected.push_str(&format!("  {k} = {},\n", v.repr()));
    }
    expected.push_str(")\n\n");
    assert_eq!(out, expected);
    assert!(expected.contains("  urls = [\"https://example.com/x.tgz\"],\n"), "{expected}");

    let out = run_mod(&res, "show_repo", &["@@c+".to_string()], &o, &provider, &provider).unwrap();
    assert!(out.starts_with("## @@c+:\nload("), "{out}");
    let e = run_mod(&res, "show_repo", &["c@1.0".to_string()], &o, &provider, &provider).unwrap_err();
    assert_eq!(
        e.message,
        "In repo argument c@1.0: Module version c@1.0 does not exist, available versions: [c@1.2]. (Note that unused modules cannot be used here). Type 'bazel help mod' for syntax and help."
    );
    // The root has no repo definition.
    let e = run_mod(&res, "show_repo", &["<root>".to_string()], &o, &provider, &provider).unwrap_err();
    assert_eq!(e.message, "In repo argument <root>: no such repo. Type 'bazel help mod' for syntax and help.");
    // Builtin repos are printed as overrides.
    let out = run_mod(&res, "show_repo", &["@@bazel_tools".to_string()], &o, &provider, &provider).unwrap();
    assert_eq!(out, "## @@bazel_tools:\nBuiltin or overridden repo located at: /install/embedded_tools\n\n");
    // Apparent names are resolved relative to --base_module.
    let ob = ModOptions { base_module: "b@1.0".into(), ..ModOptions::default() };
    let out = run_mod(&res, "show_repo", &["@d".to_string()], &ob, &provider, &provider).unwrap();
    assert!(out.starts_with("## @d:\nload("), "{out}");
    let e = run_mod(&res, "show_repo", &["@zzz".to_string()], &o, &provider, &provider).unwrap_err();
    assert_eq!(
        e.message,
        "In repo argument @zzz: No repo visible as zzz from @main repository (Note that unused modules cannot be used here). Type 'bazel help mod' for syntax and help."
    );
    // --all_visible_repos lists everything visible from the base module, under apparent names.
    let oa = ModOptions { all_visible_repos: true, ..ModOptions::default() };
    let out = run_mod(&res, "show_repo", &[], &oa, &provider, &provider).unwrap();
    let headers: Vec<&str> = out.lines().filter(|l| l.starts_with("## ")).collect();
    for h in ["## @a:", "## @b:", "## @c:", "## @bazel_tools:"] {
        assert!(headers.contains(&h), "{headers:?}");
    }
    // --all_repos lists canonical names, excluding the main repo.
    let oall = ModOptions { all_repos: true, ..ModOptions::default() };
    let out = run_mod(&res, "show_repo", &[], &oall, &provider, &provider).unwrap();
    let headers: Vec<&str> = out.lines().filter(|l| l.starts_with("## ")).collect();
    for h in ["## @@a+:", "## @@b+:", "## @@c+:", "## @@d+:"] {
        assert!(headers.contains(&h), "{headers:?}");
    }
    assert!(!headers.contains(&"## @@:"));
    // Streamed proto and jsonproto outputs are supported.
    let op_json = ModOptions { output: OutputFormat::StreamedJsonProto, ..ModOptions::default() };
    let json_out = run_mod(&res, "show_repo", &["a".to_string()], &op_json, &provider, &provider).unwrap();
    assert!(json_out.contains("canonicalName"));

    let op_proto = ModOptions { output: OutputFormat::StreamedProto, ..ModOptions::default() };
    let proto_out = run_mod(&res, "show_repo", &["a".to_string()], &op_proto, &provider, &provider).unwrap();
    assert!(!proto_out.is_empty());
}

#[test]
fn e2e_show_repo_extension_generated() {
    let res = ext_resolution("t2");
    let ext_id = res.extension_ids().into_iter().find(|i| i.name == "myext").unwrap();
    let mut specs = BTreeMap::new();
    specs.insert(
        "b++myext+hub".to_string(),
        RepoSpec::new(
            "@@b+//:ext.bzl",
            "data_repo",
            vec![("name".into(), AttrVal::Str("hub".into())), ("data".into(), AttrVal::Str("x".into()))],
        ),
    );
    let provider = TestProvider { specs, repos: vec![(ext_id, vec!["hub".into()])] };
    let out = run_mod(&res, "show_repo", &["@hub".to_string()], &ModOptions::default(), &provider, &provider).unwrap();
    assert_eq!(
        out,
        "## @hub:\nload(\"@@b+//:ext.bzl\", \"data_repo\")\ndata_repo(\n  name = \"b++myext+hub\",\n  _original_name = \"hub\",\n  data = \"x\",\n)\n\n"
    );
}

#[test]
fn mod_options_from_parsed_flags() {
    let args: Vec<String> = [
        "--depth=3",
        "--nocycles",
        "--include_unused",
        "--verbose",
        "--extension_info=all",
        "--charset=ascii",
        "--output=json",
        "--base_module=foo@1.0",
        "--from=a,b@1.0",
        "--include_builtin",
        "--extension_filter=@x//:y.bzl%z",
        "--extension_usages=a",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let parsed = zel_options::ParsedCommandOptions::parse_for_command("mod", &args).unwrap();
    let o = ModOptions::from_parsed(&parsed).unwrap();
    assert_eq!(o.depth, 3);
    assert!(!o.cycles);
    assert!(o.include_unused && o.verbose && o.include_builtin);
    assert_eq!(o.extension_info, ExtensionShow::All);
    assert_eq!(o.charset, Charset::Ascii);
    assert_eq!(o.output, OutputFormat::Json);
    assert_eq!(o.base_module, "foo@1.0");
    assert_eq!(o.from, vec!["a".to_string(), "b@1.0".to_string()]);
    assert_eq!(o.extension_filter, Some(vec!["@x//:y.bzl%z".to_string()]));
    assert_eq!(o.extension_usages, vec!["a".to_string()]);

    let parsed = zel_options::ParsedCommandOptions::parse_for_command("mod", &[]).unwrap();
    let d = ModOptions::from_parsed(&parsed).unwrap();
    assert_eq!(d.depth, -1);
    assert!(d.cycles);
    assert_eq!(d.from, vec!["<root>".to_string()]);
    assert_eq!(d.extension_filter, None);
    assert_eq!(d.output, OutputFormat::Text);
    assert_eq!(d.charset, Charset::Utf8);
}


#[test]
fn option_parsing_errors_have_no_help_suffix_and_bazel_enum_lists() {
    // `While parsing option ...` failures come from the options parser: no `Type 'bazel help mod'` suffix.
    let o = ModOptions { extension_filter: Some(vec!["nonexist".into()]), ..ModOptions::default() };
    let res = mvs_resolution("optparse");
    let e = run(&res, "graph", &[], &o).unwrap_err();
    assert_eq!(e.message, "While parsing option --extension_filter=nonexist: Invalid argument nonexist: missing .bzl label");
    assert_eq!(e.exit_code(), 2);
}

#[test]
fn enum_lists_are_joined_like_bazel() {
    assert_eq!(super::join_english_list(&["text", "json", "graph", "streamed_proto", "streamed_jsonproto"]), "text, json, graph, streamed_proto or streamed_jsonproto");
    assert_eq!(super::join_english_list(&["utf8", "ascii"]), "utf8 or ascii");
    assert_eq!(super::join_english_list(&["a"]), "a");
}
