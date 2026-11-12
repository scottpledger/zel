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

//! `query` output formatters (`lib/query2/query/output`): label, label_kind, build, location,
//! package, minrank, maxrank, graph, xml, proto, streamed_proto, streamed_jsonproto.

use crate::engine::{label_cmp, Digraph, Node};
use crate::pb::{Msg, PbVal};
use crate::target_env::{
    disc, java_string, possible_values, rule_attributes, rule_deps_ext, rule_outputs, target_location,
    visibility_group_labels, AttrEntry, TInfo, TargetEnv,
};
use std::collections::{BTreeMap, BTreeSet};
use zel_cmdline::Label;
use zel_packages::attr::{AttrType, AttrValue, SelectPart};
use zel_packages::Target;

/// `--order_output`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderOutput {
    No,
    Deps,
    Auto,
    Full,
}

impl OrderOutput {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "no" => Some(Self::No),
            "deps" => Some(Self::Deps),
            "auto" => Some(Self::Auto),
            "full" => Some(Self::Full),
            _ => None,
        }
    }
}

/// `LabelPrinter.displayForm(mainRepoMapping)` (or canonical form with `--consistent_labels`).
#[derive(Debug, Clone, Default)]
pub struct LabelPrinter {
    /// Canonical repository name → apparent name visible from the main repository.
    pub apparent: std::collections::HashMap<String, String>,
    pub consistent: bool,
}

impl LabelPrinter {
    pub fn repo(&self, name: &str) -> String {
        if self.consistent {
            return format!("@@{name}");
        }
        if name.is_empty() {
            return String::new();
        }
        match self.apparent.get(name) {
            Some(a) if a.is_empty() => String::new(),
            Some(a) => format!("@{a}"),
            None => format!("@@{name}"),
        }
    }

    pub fn label(&self, l: &Label) -> String {
        if l.package_name() == "visibility" && (l.name() == "public" || l.name() == "private") {
            // The built-in visibility labels always belong to the main repository.
            return if self.consistent { format!("@@//visibility:{}", l.name()) } else { format!("//visibility:{}", l.name()) };
        }
        format!("{}//{}:{}", self.repo(l.repository().name()), l.package_name(), l.name())
    }

    pub fn package(&self, l: &Label) -> String {
        format!("{}//{}", self.repo(l.repository().name()), l.package_name())
    }
}

thread_local! {
    static PRINTER: std::cell::RefCell<LabelPrinter> = std::cell::RefCell::new(LabelPrinter::default());
}

/// Prints a label with the active [`LabelPrinter`] (installed by [`format_targets`]).
pub fn lp(l: &Label) -> String {
    PRINTER.with(|p| p.borrow().label(l))
}

/// Installs a [`LabelPrinter`] into thread-local storage for post-analysis formatters.
pub fn install_printer(printer: &LabelPrinter) {
    PRINTER.with(|p| *p.borrow_mut() = printer.clone());
}

fn lp_pkg(l: &Label) -> String {
    PRINTER.with(|p| p.borrow().package(l))
}

/// All options consulted by the formatters.
#[derive(Debug, Clone)]
pub struct FormatOpts {
    pub output: String,
    pub printer: LabelPrinter,
    pub order: OrderOutput,
    pub line_terminator: String,
    pub relative_locations: bool,
    pub display_full_kind: bool,
    pub implicit_deps: bool,
    pub tool_deps: bool,
    pub package_group_includes_double_slash: bool,
    pub package_group_build_output: bool,
    pub proto_locations: bool,
    pub proto_default_values: bool,
    pub proto_flatten_selects: bool,
    pub proto_output_rule_attrs: Vec<String>,
    pub proto_rule_inputs_and_outputs: bool,
    pub proto_instantiation_stack: bool,
    pub proto_definition_stack: bool,
    pub proto_include_attribute_source_aspects: bool,
    pub proto_rule_classes: bool,
    pub proto_include_synthetic_attribute_hash: bool,
    pub proto_include_starlark_rule_env: bool,
    pub nodep_deps: bool,
    pub include_aspects: bool,
    pub xml_line_numbers: bool,
    pub xml_default_values: bool,
    pub graph_node_limit: i32,
    pub graph_factored: bool,
    pub graph_conditional_edges_limit: i32,
    pub aspect_deps: String,
}

impl Default for FormatOpts {
    fn default() -> Self {
        FormatOpts {
            output: "label".into(),
            printer: LabelPrinter::default(),
            order: OrderOutput::Auto,
            line_terminator: "\n".into(),
            relative_locations: false,
            display_full_kind: false,
            implicit_deps: true,
            tool_deps: true,
            package_group_includes_double_slash: true,
            package_group_build_output: false,
            proto_locations: true,
            proto_default_values: true,
            proto_flatten_selects: true,
            proto_output_rule_attrs: vec!["all".into()],
            proto_rule_inputs_and_outputs: true,
            proto_instantiation_stack: false,
            proto_definition_stack: false,
            proto_include_attribute_source_aspects: false,
            proto_rule_classes: false,
            proto_include_synthetic_attribute_hash: false,
            proto_include_starlark_rule_env: false,
            nodep_deps: true,
            include_aspects: true,
            xml_line_numbers: true,
            xml_default_values: false,
            graph_node_limit: 512,
            graph_factored: true,
            graph_conditional_edges_limit: 4,
            aspect_deps: "conservative".into(),
        }
    }
}

pub const FORMATTERS: &[&str] = &[
    "label",
    "label_kind",
    "build",
    "minrank",
    "maxrank",
    "package",
    "location",
    "graph",
    "xml",
    "proto",
    "streamed_jsonproto",
    "streamed_proto",
];

/// Formatters that implement `StreamedFormatter` (everything except minrank/maxrank/graph).
pub fn is_streamed(name: &str) -> bool {
    !matches!(name, "minrank" | "maxrank" | "graph")
}

/// Whether an ordering option makes the result lexicographically sorted before output.
pub fn lexicographically_sort(opts: &FormatOpts) -> bool {
    opts.order == OrderOutput::Auto && is_streamed(&opts.output)
}

/// Orders `nodes` for a streamed formatter according to `--order_output`.
pub fn order_nodes(env: &TargetEnv<'_>, nodes: &[Node], opts: &FormatOpts, top_somepath: bool) -> Vec<Node> {
    match opts.order {
        OrderOutput::No => nodes.to_vec(),
        OrderOutput::Auto if top_somepath => nodes.to_vec(),
        OrderOutput::Auto => {
            let mut v = nodes.to_vec();
            v.sort_by(|a, b| label_cmp(&a.label, &b.label));
            v
        }
        OrderOutput::Deps | OrderOutput::Full => {
            let g = build_graph(env, nodes, opts);
            let cmp = |a: &Node, b: &Node| label_cmp(&a.label, &b.label);
            let order = if opts.order == OrderOutput::Full {
                g.topological_order(Some(&cmp))
            } else {
                g.topological_order(None)
            };
            order.into_iter().map(|i| g.nodes[i].clone()).collect()
        }
    }
}

/// Builds the result subgraph (edges among result nodes, honouring the dependency filter).
pub fn build_graph(env: &TargetEnv<'_>, nodes: &[Node], opts: &FormatOpts) -> Digraph {
    let fake_loads = env.fake_load_targets.borrow();
    let no_edges = *env.digraph_no_edges.borrow();
    let pat_nodes = env.pattern_nodes.borrow();
    let mut deps = |n: &Node| -> Vec<Node> {
        if fake_loads.contains_key(&n.label) || n.label.name() == "BUILD" || n.label.name() == "BUILD.bazel" || n.label.name().ends_with(".bzl") {
            return Vec::new();
        }
        if no_edges && !pat_nodes.contains(n) {
            return Vec::new();
        }
        match env.info(&n.label) {
            Ok(info) => direct_dep_labels(env, &info, opts)
                .into_iter()
                .map(Node::new)
                .filter(|d| !no_edges || pat_nodes.contains(d))
                .collect(),
            Err(_) => Vec::new(),
        }
    };
    Digraph::build(nodes, &mut deps)
}

fn direct_dep_labels(env: &TargetEnv<'_>, info: &TInfo, opts: &FormatOpts) -> Vec<Label> {
    match &info.target {
        Target::Rule(r) => rule_deps_ext(r, opts.implicit_deps, opts.tool_deps, opts.nodep_deps).into_iter().map(|(l, _)| l).collect(),
        Target::OutputFile { generating_rule, .. } => vec![generating_rule.clone()],
        Target::InputFile { .. } => if opts.nodep_deps { visibility_group_labels(&env.visibility_labels(info)) } else { Vec::new() },
        Target::PackageGroup { label, .. } => info.pkg.group_includes.get(label.name()).cloned().unwrap_or_default(),
        Target::EnvironmentGroup { .. } => Vec::new(),
    }
}

fn kind_of(info: &TInfo, opts: &FormatOpts) -> String {
    if opts.display_full_kind {
        if let Target::Rule(r) = &info.target {
            return format!("{} rule", full_rule_class_key(r));
        }
    }
    info.target.target_kind()
}

fn full_rule_class_key(r: &zel_packages::RuleInstance) -> String {
    match r.extra.rule_class.as_ref().and_then(|rc| rc.starlark.as_ref()) {
        Some(s) => format!("{}%{}", s.defined_in, r.rule_class),
        None => r.rule_class.clone(),
    }
}

/// Entry point: formats `nodes` (already ordered for streamed formatters) into bytes.
pub fn format_targets(
    env: &TargetEnv<'_>,
    nodes: &[Node],
    opts: &FormatOpts,
    top_somepath: bool,
) -> Result<Vec<u8>, String> {
    let lt = opts.line_terminator.as_str();
    PRINTER.with(|p| *p.borrow_mut() = opts.printer.clone());
    match opts.output.as_str() {
        "label" | "label_kind" => {
            let mut out = String::new();
            for n in order_nodes(env, nodes, opts, top_somepath) {
                if opts.output == "label_kind" {
                    let info = env.info(&n.label).map_err(|e| e.to_string())?;
                    out.push_str(&kind_of(&info, opts));
                    out.push(' ');
                }
                out.push_str(&lp(&n.label));
                out.push_str(lt);
            }
            Ok(out.into_bytes())
        }
        "location" => {
            let mut out = String::new();
            for n in order_nodes(env, nodes, opts, top_somepath) {
                let info = env.info(&n.label).map_err(|e| e.to_string())?;
                let loc = if let Some(ref_pkg) = env.fake_load_targets.borrow().get(&n.label) {
                    crate::target_env::fake_load_target_location(ref_pkg, opts.relative_locations)
                } else {
                    target_location(&info, opts.relative_locations)
                };
                out.push_str(&format!(
                    "{}: {} {}{}",
                    loc,
                    kind_of(&info, opts),
                    lp(&n.label),
                    lt
                ));
            }
            Ok(out.into_bytes())
        }
        "package" => {
            let mut set: BTreeSet<String> = BTreeSet::new();
            for n in nodes {
                {
                    let id = lp_pkg(&n.label);
                    set.insert(id.strip_prefix("//").map(|s| s.to_string()).unwrap_or(id));
                }
            }
            let mut out = String::new();
            for p in set {
                out.push_str(&p);
                out.push_str(lt);
            }
            Ok(out.into_bytes())
        }
        "build" => {
            let mut out = String::new();
            let mut printed: BTreeSet<Label> = BTreeSet::new();
            for n in order_nodes(env, nodes, opts, top_somepath) {
                let info = env.info(&n.label).map_err(|e| e.to_string())?;
                if let Target::PackageGroup { label, packages } = &info.target {
                    if opts.package_group_build_output {
                        out.push_str(&format!("# {}{lt}", target_location(&info, false)));
                        out.push_str(&format!("package_group({lt}  name = \"{}\",{lt}", label.name()));
                        let pk = contained_packages(&info, packages, opts.package_group_includes_double_slash);
                        if !pk.is_empty() {
                            out.push_str(&format!("  packages = [{lt}"));
                            for p in pk {
                                out.push_str(&format!("    \"{p}\",{lt}"));
                            }
                            out.push_str(&format!("  ],{lt}"));
                        }
                        let inc = info.pkg.group_includes.get(label.name()).cloned().unwrap_or_default();
                        if !inc.is_empty() {
                            out.push_str(&format!("  includes = [{lt}"));
                            for i in inc {
                                out.push_str(&format!("    \"{}\",{lt}", lp(&i)));
                            }
                            out.push_str(&format!("  ],{lt}"));
                        }
                        out.push_str(&format!("){lt}{lt}"));
                        continue;
                    }
                }
                // `getAssociatedRule`.
                let rule_info = match &info.target {
                    Target::Rule(_) => Some((*info).clone()),
                    Target::OutputFile { generating_rule, .. } => env.info(generating_rule).ok().map(|i| (*i).clone()),
                    _ => None,
                };
                let Some(ri) = rule_info else { continue };
                let Target::Rule(rule) = ri.target.clone() else { continue };
                let rule = &rule;
                if !printed.insert(rule.label.clone()) {
                    continue;
                }
                out.push_str(&build_rule(&ri, rule, lt));
            }
            Ok(out.into_bytes())
        }
        "minrank" => Ok(rank_output(env, nodes, opts, true).into_bytes()),
        "maxrank" => Ok(rank_output(env, nodes, opts, false).into_bytes()),
        "graph" => Ok(graph_output(env, nodes, opts).into_bytes()),
        "xml" => Ok(xml_output(env, nodes, opts, top_somepath)?.into_bytes()),
        "proto" => {
            let mut qr = Msg::new();
            for n in order_nodes(env, nodes, opts, top_somepath) {
                let info = env.info(&n.label).map_err(|e| e.to_string())?;
                qr.rmsg(1, "target", target_proto(env, &info, opts));
            }
            Ok(qr.encode())
        }
        "streamed_proto" => {
            let mut out = Vec::new();
            for n in order_nodes(env, nodes, opts, top_somepath) {
                let info = env.info(&n.label).map_err(|e| e.to_string())?;
                out.extend(target_proto(env, &info, opts).encode_delimited());
            }
            Ok(out)
        }
        "streamed_jsonproto" => {
            let mut out = String::new();
            for n in order_nodes(env, nodes, opts, top_somepath) {
                let info = env.info(&n.label).map_err(|e| e.to_string())?;
                out.push_str(&target_proto(env, &info, opts).to_json_compact());
                out.push('\n');
            }
            Ok(out.into_bytes())
        }
        other => Err(format!("unsupported output format '{other}'")),
    }
}

// ---- BUILD ----------------------------------------------------------------------------------

/// Starlark `repr` of a string.
pub fn starlark_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn repr_list<T>(items: &[T], f: impl Fn(&T) -> String) -> String {
    format!("[{}]", items.iter().map(f).collect::<Vec<_>>().join(", "))
}

/// Printer.repr of an attribute value as `BuildOutputFormatter.outputRawAttrValue` prints it.
pub fn repr_value(v: &AttrValue) -> String {
    match v {
        AttrValue::Int(i) => i.to_string(),
        AttrValue::Str(s) => starlark_quote(s),
        AttrValue::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        AttrValue::Tristate(t) => t.to_string(),
        AttrValue::StrList(l) => repr_list(l, |s| starlark_quote(s)),
        AttrValue::IntList(l) => repr_list(l, |i| i.to_string()),
        AttrValue::StrDict(d) => format!(
            "{{{}}}",
            d.iter().map(|(k, v)| format!("{}: {}", starlark_quote(k), starlark_quote(v))).collect::<Vec<_>>().join(", ")
        ),
        AttrValue::StrListDict(d) => format!(
            "{{{}}}",
            d.iter()
                .map(|(k, v)| format!("{}: {}", starlark_quote(k), repr_list(v, |s| starlark_quote(s))))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        AttrValue::Label(l) | AttrValue::Output(l) => match l {
            Some(l) => starlark_quote(&lp(l)),
            None => "None".to_string(),
        },
        AttrValue::LabelList(l) | AttrValue::OutputList(l) => repr_list(l, |l| starlark_quote(&lp(l))),
        AttrValue::LabelKeyedStrDict(d) => format!(
            "{{{}}}",
            d.iter()
                .map(|(k, v)| format!("{}: {}", starlark_quote(&lp(k)), starlark_quote(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        AttrValue::StrKeyedLabelDict(d) => format!(
            "{{{}}}",
            d.iter()
                .map(|(k, v)| format!("{}: {}", starlark_quote(k), starlark_quote(&lp(v))))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        AttrValue::Select(parts) => reconstruct_select(parts),
    }
}

fn reconstruct_select(parts: &[SelectPart]) -> String {
    parts
        .iter()
        .map(|p| match p {
            SelectPart::Plain(v) => repr_value(v),
            SelectPart::Select { entries, .. } => {
                let items: Vec<String> = entries
                    .iter()
                    .map(|(k, v)| format!("{}: {}", starlark_quote(&lp(k)), repr_value(v)))
                    .collect();
                format!("select({{{}}})", items.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Text of one rule in `--output=build` form.
pub fn build_rule_text(info: &TInfo, rule: &zel_packages::RuleInstance, lt: &str) -> String {
    build_rule(info, rule, lt)
}

fn build_rule(info: &TInfo, rule: &zel_packages::RuleInstance, lt: &str) -> String {
    let loc = rule.extra.location.clone().unwrap_or_else(|| format!("{}:1:1", info.pkg.build_file));
    let mut out = format!("# {loc}{lt}{}({lt}  name = \"{}\",{lt}", rule.rule_class, rule.label.name());
    for a in rule_attributes(rule) {
        if a.name == "name" {
            continue;
        }
        if a.value.is_select() {
            out.push_str(&format!("  {} = {},{lt}", a.name, reconstruct_select(select_parts(&a.value))));
            continue;
        }
        if !a.explicit {
            continue;
        }
        let vals = possible_values(&a.value);
        if vals.len() != 1 {
            continue;
        }
        // `BuildOutputFormatter`: hidden `$` attributes are printed with a `_` prefix.
        let shown = match a.name.strip_prefix('$') {
            Some(rest) => format!("_{rest}"),
            None => a.name.clone(),
        };
        out.push_str(&format!("  {} = {},{lt}", shown, repr_value(&vals[0])));
    }
    out.push_str(&format!("){lt}"));
    // Call stack comments.
    let (_, inst_frames) = instantiation_stack(rule);
    append_stack(&mut out, &format!("# Rule {} instantiated at (most recent call last):", rule.label.name()), &inst_frames, lt);
    let (_, def_frames) = definition_stack(rule);
    if !def_frames.is_empty() {
        append_stack(&mut out, &format!("# Rule {} defined at (most recent call last):", rule.rule_class), &def_frames, lt);
    }
    out.push_str(lt);
    out
}

fn select_parts(v: &AttrValue) -> &[SelectPart] {
    match v {
        AttrValue::Select(p) => p,
        _ => &[],
    }
}

/// `(title, frames)` where frame = (location, function).
fn instantiation_stack(rule: &zel_packages::RuleInstance) -> (String, Vec<(String, String)>) {
    if !rule.extra.instantiation_stack.is_empty() {
        return (String::new(), rule.extra.instantiation_stack.clone());
    }
    let mut frames = Vec::new();
    if let Some(loc) = &rule.extra.location {
        frames.push((loc.clone(), "<toplevel>".to_string()));
    }
    (String::new(), frames)
}

fn definition_stack(rule: &zel_packages::RuleInstance) -> (String, Vec<(String, String)>) {
    let mut frames = Vec::new();
    if let Some(rc) = &rule.extra.rule_class {
        if let Some(st) = &rc.starlark {
            if !st.definition_stack.is_empty() {
                return (String::new(), st.definition_stack.clone());
            }
            if !st.defined_in.is_empty() {
                frames.push((st.defined_in.clone(), "<toplevel>".to_string()));
            }
        }
    }
    (String::new(), frames)
}

fn append_stack(out: &mut String, title: &str, frames: &[(String, String)], lt: &str) {
    let max = frames.iter().map(|(l, _)| l.chars().count()).max().unwrap_or(0);
    if max > 0 {
        out.push_str(title);
        out.push_str(lt);
        for (l, name) in frames {
            out.push_str("#   ");
            out.push_str(l);
            for _ in l.chars().count()..max {
                out.push(' ');
            }
            out.push_str(" in ");
            out.push_str(name);
            out.push_str(lt);
        }
    }
}

/// `PackageGroup.getContainedPackages`.
fn contained_packages(_info: &TInfo, packages: &[String], double_slash: bool) -> Vec<String> {
    // `PackageGroupContents.packageStrings`: single positives, `...` positives, single negatives,
    // `...` negatives, then `//...` (positive), `private`, `-//...`; each category deduplicated in
    // declaration order (`ImmutableSet`s).
    let category = |p: &str| -> usize {
        let (neg, body) = match p.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, p),
        };
        let all = body == "//..." || body == "public" || body == "//visibility:public";
        if body == "private" || body == "//visibility:private" {
            return 5;
        }
        if all {
            return if neg { 6 } else { 4 };
        }
        let beneath = body.ends_with("/...");
        match (neg, beneath) {
            (false, false) => 0,
            (false, true) => 1,
            (true, false) => 2,
            (true, true) => 3,
        }
    };
    let mut ordered: Vec<&String> = Vec::new();
    for c in 0..7 {
        for p in packages {
            if category(p) == c && !ordered.contains(&p) {
                ordered.push(p);
            }
        }
    }
    // `PackageSpecification.toString(includeDoubleSlash)`: `//pkg` (flag on) or `pkg` (flag off).
    ordered
        .into_iter()
        .map(|p| {
            let (neg, body) = match p.strip_prefix('-') {
                Some(r) => ("-", r),
                None => ("", p.as_str()),
            };
            if double_slash || body == "public" || body == "private" || body.starts_with('@') {
                p.clone()
            } else {
                format!("{neg}{}", body.strip_prefix("//").unwrap_or(body))
            }
        })
        .collect()
}

// ---- rank formatters ------------------------------------------------------------------------

fn rank_output(env: &TargetEnv<'_>, nodes: &[Node], opts: &FormatOpts, min: bool) -> String {
    let g = build_graph(env, nodes, opts);
    let sc = g.strong_components();
    let lt = &opts.line_terminator;
    let label = |i: usize| g.nodes[i].label.clone();
    let mut items: Vec<(i64, Label)> = Vec::new();
    if min {
        let mut rank_nodes: Vec<usize> = (0..sc.comps.len()).filter(|&c| sc.pred[c].is_empty()).collect();
        let mut seen: BTreeSet<usize> = rank_nodes.iter().copied().collect();
        let mut rank = 0;
        while !rank_nodes.is_empty() {
            let mut level: Vec<(i64, Label)> = Vec::new();
            for &c in &rank_nodes {
                for &n in &sc.comps[c] {
                    level.push((rank, label(n)));
                }
            }
            if opts.order == OrderOutput::Full {
                level.sort_by(|a, b| label_cmp(&a.1, &b.1));
            }
            items.extend(level);
            let mut next = Vec::new();
            for &c in &rank_nodes {
                for &s in &sc.succ[c] {
                    if seen.insert(s) {
                        next.push(s);
                    }
                }
            }
            rank_nodes = next;
            rank += 1;
        }
    } else {
        fn rank_of(c: usize, sc: &crate::engine::StrongComponents, memo: &mut BTreeMap<usize, i64>) -> i64 {
            if let Some(r) = memo.get(&c) {
                return *r;
            }
            let mut m = -1;
            for &p in &sc.pred[c] {
                m = m.max(rank_of(p, sc, memo));
            }
            memo.insert(c, m + 1);
            m + 1
        }
        let mut memo = BTreeMap::new();
        for c in 0..sc.comps.len() {
            let r = rank_of(c, &sc, &mut memo);
            for &n in &sc.comps[c] {
                items.push((r, label(n)));
            }
        }
        if opts.order == OrderOutput::Full {
            items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| label_cmp(&a.1, &b.1)));
        } else {
            items.sort_by_key(|a| a.0);
        }
    }
    if min && opts.order == OrderOutput::Full {
        items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| label_cmp(&a.1, &b.1)));
    }
    let mut out = String::new();
    for (r, l) in items {
        out.push_str(&format!("{r} {}{lt}", lp(&l)));
    }
    out
}

// ---- graph ----------------------------------------------------------------------------------

fn conditional_edges(env: &TargetEnv<'_>, nodes: &[Node]) -> BTreeMap<(Label, Label), Vec<String>> {
    let mut map: BTreeMap<(Label, Label), Vec<String>> = BTreeMap::new();
    for n in nodes {
        let Ok(info) = env.info(&n.label) else { continue };
        let Target::Rule(r) = &info.target else { continue };
        for a in rule_attributes(r) {
            let AttrValue::Select(parts) = &a.value else { continue };
            for p in parts {
                if let SelectPart::Select { entries, .. } = p {
                    for (key, value) in entries {
                        let k_str = key.canonical_form();
                        for l in value.labels() {
                            let v = map.entry((n.label.clone(), l)).or_default();
                            if !v.contains(&k_str) {
                                v.push(k_str.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    map
}

fn conditions_label(
    cond: &BTreeMap<(Label, Label), Vec<String>>,
    lhs: &[&Label],
    rhs: &[&Label],
    limit_opt: i32,
) -> String {
    if limit_opt == 0 || cond.is_empty() {
        return String::new();
    }
    let mut buf = String::new();
    let mut annotated: BTreeSet<String> = BTreeSet::new();
    for s in lhs {
        for d in rhs {
            if let Some(conds) = cond.get(&((*s).clone(), (*d).clone())) {
                let mut first = true;
                let limit = if limit_opt == -1 { conds.len() } else { (limit_opt - 1) as usize };
                for c in conds.iter().take(limit) {
                    if !annotated.insert(c.clone()) {
                        continue;
                    }
                    if !first {
                        buf.push_str("\\n");
                    }
                    buf.push_str(c);
                    first = false;
                }
                if conds.len() > limit {
                    buf.push_str("...");
                }
            }
        }
    }
    buf
}

fn graph_output(env: &TargetEnv<'_>, nodes: &[Node], opts: &FormatOpts) -> String {
    let g = build_graph(env, nodes, opts);
    let cond = conditional_edges(env, nodes);
    let limit = opts.graph_conditional_edges_limit;
    let cond_fn = |l: &[usize], r: &[usize]| -> String {
        let ls: Vec<&Label> = l.iter().map(|&i| &g.nodes[i].label).collect();
        let rs: Vec<&Label> = r.iter().map(|&i| &g.nodes[i].label).collect();
        conditions_label(&cond, &ls, &rs, limit)
    };
    let dot_cmp = |a: &Node, b: &Node| -> std::cmp::Ordering {
        fn norm(r: &str) -> &str {
            match r {
                "bazel_tools" => "bazel_tools+",
                "ext" => "ext+",
                other => other,
            }
        }
        let ra = norm(a.label.repository().name());
        let rb = norm(b.label.repository().name());
        ra.cmp(rb)
            .then_with(|| a.label.package_identifier().package_fragment().path_string().cmp(b.label.package_identifier().package_fragment().path_string()))
            .then_with(|| a.label.name().cmp(b.label.name()))
    };
    write_dot(
        &g,
        &|n: &Node| lp(&n.label),
        &dot_cmp,
        opts.order == OrderOutput::Full,
        opts.graph_factored,
        opts.graph_node_limit,
        &opts.line_terminator,
        &cond_fn,
    )
}

/// `GraphOutputWriter.write`: DOT output of a digraph, optionally factoring nodes with identical
/// predecessor and successor sets into one box.
#[allow(clippy::too_many_arguments)]
pub fn write_dot(
    g: &Digraph,
    label: &dyn Fn(&Node) -> String,
    cmp: &dyn Fn(&Node, &Node) -> std::cmp::Ordering,
    sort: bool,
    factored: bool,
    node_limit: i32,
    lt: &str,
    cond_fn: &dyn Fn(&[usize], &[usize]) -> String,
) -> String {
    let mut out = format!("digraph mygraph {{{lt}  node [shape=box];{lt}");
    if !factored {
        let order = g.topological_order(if sort { Some(cmp) } else { None });
        for i in order {
            out.push_str(&format!("  \"{}\"{lt}", label(&g.nodes[i])));
            let mut succ = g.succ[i].clone();
            if sort {
                succ.sort_by(|&a, &b| cmp(&g.nodes[a], &g.nodes[b]));
            }
            for j in succ {
                out.push_str(&format!("  \"{}\" -> \"{}\"{lt}", label(&g.nodes[i]), label(&g.nodes[j])));
                let l = cond_fn(&[i], &[j]);
                if !l.is_empty() {
                    out.push_str(&format!("  [label=\"{l}\"];{lt}"));
                }
            }
        }
    } else {
        // Partition nodes into classes with identical predecessor and successor sets.
        let n = g.nodes.len();
        let mut class_of: Vec<Option<usize>> = vec![None; n];
        let mut classes: Vec<Vec<usize>> = Vec::new();
        let eq = |x: usize, y: usize| -> bool {
            if x == y {
                return true;
            }
            let (xp, yp): (BTreeSet<usize>, BTreeSet<usize>) = (g.pred[x].iter().copied().collect(), g.pred[y].iter().copied().collect());
            let (xs, ys): (BTreeSet<usize>, BTreeSet<usize>) = (g.succ[x].iter().copied().collect(), g.succ[y].iter().copied().collect());
            xp == yp && xs == ys
        };
        let process = |succs: &[usize], class_of: &mut Vec<Option<usize>>, classes: &mut Vec<Vec<usize>>| {
            for (i, &child) in succs.iter().enumerate() {
                if class_of[child].is_some() {
                    continue;
                }
                let ci = classes.len();
                classes.push(vec![child]);
                class_of[child] = Some(ci);
                for &sib in &succs[i + 1..] {
                    if class_of[sib].is_some() {
                        continue;
                    }
                    if eq(child, sib) {
                        classes[ci].push(sib);
                        class_of[sib] = Some(ci);
                    }
                }
            }
        };
        let roots: Vec<usize> = (0..n).filter(|&i| g.pred[i].is_empty()).collect();
        process(&roots, &mut class_of, &mut classes);
        for i in 0..n {
            let s = g.succ[i].clone();
            process(&s, &mut class_of, &mut classes);
        }
        // Nodes not reached via roots/successors (cycle members that are nobody's successor).
        for i in 0..n {
            if class_of[i].is_none() {
                class_of[i] = Some(classes.len());
                classes.push(vec![i]);
            }
        }
        let mut order: Vec<usize> = (0..classes.len()).collect();
        if sort {
            for c in classes.iter_mut() {
                c.sort_by(|&a, &b| cmp(&g.nodes[a], &g.nodes[b]));
            }
            // Order classes lexicographically by member labels for determinism.
            order.sort_by(|&a, &b| {
                let la: Vec<&Node> = classes[a].iter().map(|&i| &g.nodes[i]).collect();
                let lb: Vec<&Node> = classes[b].iter().map(|&i| &g.nodes[i]).collect();
                for (x, y) in la.iter().zip(lb.iter()) {
                    let c = cmp(x, y);
                    if c != std::cmp::Ordering::Equal {
                        return c;
                    }
                }
                la.len().cmp(&lb.len())
            });
        }
        let remap: BTreeMap<usize, usize> = order.iter().enumerate().map(|(new, &old)| (old, new)).collect();
        let sorted_classes: Vec<Vec<usize>> = order.iter().map(|&o| classes[o].clone()).collect();
        let cls = |i: usize| remap[&class_of[i].unwrap()];
        // Class graph.
        let cn = sorted_classes.len();
        let mut csucc: Vec<Vec<usize>> = vec![Vec::new(); cn];
        let mut cpred: Vec<Vec<usize>> = vec![Vec::new(); cn];
        for i in 0..n {
            for &j in &g.succ[i] {
                let (a, b) = (cls(i), cls(j));
                if a != b && !csucc[a].contains(&b) {
                    csucc[a].push(b);
                    cpred[b].push(a);
                }
            }
        }
        let label_of = |c: usize| -> String {
            let max = node_limit;
            let reserved = "\\n...and 9999999 more items".len() as i32;
            let actual = max - reserved;
            let mut buf = String::new();
            let mut count = 0;
            let mut first = true;
            for &m in &sorted_classes[c] {
                let s = label(&g.nodes[m]);
                if !first {
                    buf.push_str("\\n");
                    if max != -1 && (buf.len() + s.len()) as i32 > actual {
                        buf.push_str(&format!("...and {} more items", sorted_classes[c].len() - count));
                        break;
                    }
                }
                buf.push_str(&s);
                count += 1;
                first = false;
            }
            buf
        };
        for a in csucc.iter_mut() {
            if sort {
                a.sort_unstable();
            }
        }
        // Topological order of the class graph (deterministic: ties by index).
        let mut visited = vec![false; cn];
        let mut post = Vec::new();
        for s in 0..cn {
            if visited[s] {
                continue;
            }
            let mut stack: Vec<(usize, usize)> = vec![(s, 0)];
            visited[s] = true;
            while let Some(&(v, k)) = stack.last() {
                if k < csucc[v].len() {
                    stack.last_mut().unwrap().1 += 1;
                    let w = csucc[v][k];
                    if !visited[w] {
                        visited[w] = true;
                        stack.push((w, 0));
                    }
                } else {
                    post.push(v);
                    stack.pop();
                }
            }
        }
        post.reverse();
        for c in post {
            out.push_str(&format!("  \"{}\"{lt}", label_of(c)));
            let mut succ = csucc[c].clone();
            succ.sort_unstable();
            for d in succ {
                out.push_str(&format!("  \"{}\" -> \"{}\"{lt}", label_of(c), label_of(d)));
                let l = cond_fn(&sorted_classes[c], &sorted_classes[d]);
                if !l.is_empty() {
                    out.push_str(&format!("  [label=\"{l}\"];{lt}"));
                }
            }
        }
    }
    out.push_str(&format!("}}{lt}"));
    out
}

// ---- XML ------------------------------------------------------------------------------------

struct XmlEl {
    name: String,
    attrs: BTreeMap<String, String>,
    children: Vec<XmlEl>,
}

impl XmlEl {
    fn new(name: &str) -> Self {
        XmlEl { name: name.to_string(), attrs: BTreeMap::new(), children: Vec::new() }
    }
    fn attr(mut self, k: &str, v: impl Into<String>) -> Self {
        self.attrs.insert(k.to_string(), v.into());
        self
    }
    fn set(&mut self, k: &str, v: impl Into<String>) {
        self.attrs.insert(k.to_string(), v.into());
    }
}

fn xml_escape_attr(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            c if (c as u32) < 0x20 => out.push_str(&format!("&#{};", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn xml_write(el: &XmlEl, depth: usize, out: &mut String) {
    let pad = "    ".repeat(depth);
    out.push_str(&pad);
    out.push('<');
    out.push_str(&el.name);
    for (k, v) in &el.attrs {
        out.push_str(&format!(" {k}=\"{}\"", xml_escape_attr(v)));
    }
    if el.children.is_empty() {
        out.push_str("/>\n");
    } else {
        out.push_str(">\n");
        for c in &el.children {
            xml_write(c, depth + 1, out);
        }
        out.push_str(&format!("{pad}</{}>\n", el.name));
    }
}

fn xml_scalar(ty_name: &str, value: Option<String>, configurable: bool) -> XmlEl {
    let mut e = XmlEl::new(ty_name);
    if configurable {
        e.set("configurable", "yes");
    }
    if let Some(v) = value {
        e.set("value", v);
    }
    e
}

fn xml_type_name(ty: AttrType) -> &'static str {
    match ty {
        AttrType::Int => "int",
        AttrType::String => "string",
        AttrType::Bool => "boolean",
        AttrType::Tristate => "tristate",
        AttrType::Label => "label",
        AttrType::Output => "output",
        _ => "string",
    }
}

fn xml_elem_type(ty: AttrType) -> AttrType {
    match ty {
        AttrType::StringList => AttrType::String,
        AttrType::IntList => AttrType::Int,
        AttrType::LabelList => AttrType::Label,
        AttrType::OutputList => AttrType::Output,
        o => o,
    }
}

fn scalar_string(v: &AttrValue) -> Option<String> {
    match v {
        AttrValue::Bool(b) => Some(if *b { "true" } else { "false" }.to_string()),
        AttrValue::Label(Some(l)) | AttrValue::Output(Some(l)) => Some(lp(l)),
        AttrValue::Tristate(t) => Some(match *t {
            t if t < 0 => "AUTO".to_string(),
            0 => "NO".to_string(),
            _ => "YES".to_string(),
        }),
        other => java_string(other),
    }
}

fn xml_value_element(e: &AttrEntry, values: &[AttrValue]) -> XmlEl {
    let multiple = values.len() > 1;
    match e.ty {
        AttrType::StringList | AttrType::IntList | AttrType::LabelList | AttrType::OutputList => {
            let mut el = XmlEl::new("list");
            let et = xml_elem_type(e.ty);
            for v in values {
                let items: Vec<String> = match v {
                    AttrValue::StrList(l) => l.clone(),
                    AttrValue::IntList(l) => l.iter().map(|i| i.to_string()).collect(),
                    AttrValue::LabelList(l) | AttrValue::OutputList(l) => l.iter().map(lp).collect(),
                    _ => Vec::new(),
                };
                for it in items {
                    el.children.push(xml_scalar(xml_type_name(et), Some(it), false));
                }
            }
            el
        }
        AttrType::StringDict | AttrType::StringListDict | AttrType::LabelKeyedStringDict | AttrType::StringKeyedLabelDict => {
            let mut el = XmlEl::new("dict");
            let mut visited: BTreeSet<String> = BTreeSet::new();
            for v in values {
                let pairs: Vec<(String, XmlEl)> = match v {
                    AttrValue::StrDict(d) => d.iter().map(|(k, v)| (k.clone(), xml_scalar("string", Some(v.clone()), false))).collect(),
                    AttrValue::StrListDict(d) => d
                        .iter()
                        .map(|(k, v)| {
                            let mut l = XmlEl::new("list");
                            for s in v {
                                l.children.push(xml_scalar("string", Some(s.clone()), false));
                            }
                            (k.clone(), l)
                        })
                        .collect(),
                    AttrValue::LabelKeyedStrDict(d) => {
                        d.iter().map(|(k, v)| (lp(k), xml_scalar("string", Some(v.clone()), false))).collect()
                    }
                    AttrValue::StrKeyedLabelDict(d) => {
                        d.iter().map(|(k, v)| (k.clone(), xml_scalar("label", Some(lp(v)), false))).collect()
                    }
                    _ => Vec::new(),
                };
                for (k, val) in pairs {
                    if visited.insert(k.clone()) {
                        let mut pair = XmlEl::new("pair");
                        let key_ty = if matches!(e.ty, AttrType::LabelKeyedStringDict) { "label" } else { "string" };
                        pair.children.push(xml_scalar(key_ty, Some(k), false));
                        pair.children.push(val);
                        el.children.push(pair);
                    }
                }
            }
            el
        }
        ty => {
            let v = if multiple { None } else { values.first().and_then(scalar_string) };
            xml_scalar(xml_type_name(ty), v, multiple)
        }
    }
}

fn xml_output(env: &TargetEnv<'_>, nodes: &[Node], opts: &FormatOpts, top_somepath: bool) -> Result<String, String> {
    let mut root = XmlEl::new("query").attr("version", "2");
    for n in order_nodes(env, nodes, opts, top_somepath) {
        let info = env.info(&n.label).map_err(|e| e.to_string())?;
        let mut el = match &info.target {
            Target::Rule(r) => {
                let class = if opts.display_full_kind { full_rule_class_key(r) } else { r.rule_class.clone() };
                let mut el = XmlEl::new("rule").attr("class", class);
                for a in rule_attributes(r) {
                    if a.explicit || opts.xml_default_values {
                        let vals = possible_values(&a.value);
                        if a.disc == crate::target_env::disc::LICENSE {
                            // `XmlOutputFormatter`: `<license>` with exception and type lists.
                            let mut le = XmlEl::new("license");
                            let mut ex = XmlEl::new("list");
                            ex.set("name", "exceptions");
                            let mut lt_ = XmlEl::new("list");
                            lt_.set("name", "license-types");
                            let types: Vec<String> = match vals.first() {
                                Some(AttrValue::StrList(t)) if !t.is_empty() => t.iter().map(|x| x.to_ascii_uppercase()).collect(),
                                _ => vec!["NONE".to_string()],
                            };
                            for t in types {
                                lt_.children.push(xml_scalar("string", Some(t), false));
                            }
                            le.children.push(ex);
                            le.children.push(lt_);
                            le.set("name", a.name.clone());
                            el.children.push(le);
                            continue;
                        }
                        let mut ve = xml_value_element(&a, &vals);
                        if a.null_default && ve.name == "string" {
                            // A null default prints no `value`.
                            if a.null_default {
                                ve.attrs.remove("value");
                            }
                        }
                        ve.set("name", a.name.clone());
                        el.children.push(ve);
                    }
                }
                let mut inputs: Vec<Label> = crate::target_env::rule_deps_with_aspect_opt(
                r,
                opts.implicit_deps,
                opts.tool_deps,
                opts.nodep_deps,
                &opts.aspect_deps,
            )
            .into_iter()
            .filter_map(|(l, attr)| if attr == "_hidden_toolchain_types" { None } else { Some(l) })
            .collect();
                inputs.sort_by(label_cmp);
                for l in inputs {
                    el.children.push(XmlEl::new("rule-input").attr("name", lp(&l)));
                }
                for o in rule_outputs(&info.pkg, &r.label) {
                    el.children.push(XmlEl::new("rule-output").attr("name", lp(&o)));
                }
                for f in &info.pkg.args.features {
                    el.children.push(XmlEl::new("rule-default-setting").attr("name", f.clone()));
                }
                el
            }
            Target::EnvironmentGroup { label, environments, defaults } => {
                // `XmlOutputFormatter`: `<list name="environments">` / `<list name="defaults">`.
                let mut el = XmlEl::new("environment-group").attr("name", lp(label));
                for (name, labels) in [("environments", environments), ("defaults", defaults)] {
                    let mut l = XmlEl::new("list");
                    for e in labels {
                        l.children.push(xml_scalar("label", Some(lp(e)), false));
                    }
                    l.set("name", name);
                    el.children.push(l);
                }
                el
            }
            Target::PackageGroup { label, packages } => {
                let mut el = XmlEl::new("package-group").attr("name", label.name().to_string());
                let inc = info.pkg.group_includes.get(label.name()).cloned().unwrap_or_default();
                let mut incl = XmlEl::new("list");
                for l in inc {
                    incl.children.push(xml_scalar("label", Some(lp(&l)), false));
                }
                incl.set("name", "includes");
                el.children.push(incl);
                let mut pk = XmlEl::new("list");
                for p in contained_packages(&info, packages, opts.package_group_includes_double_slash) {
                    pk.children.push(xml_scalar("string", Some(p), false));
                }
                pk.set("name", "packages");
                el.children.push(pk);
                el
            }
            Target::OutputFile { generating_rule, .. } => {
                XmlEl::new("generated-file").attr("generating-rule", lp(generating_rule))
            }
            Target::InputFile { label } => {
                let mut el = XmlEl::new("source-file");
                let is_fake_load = env.fake_load_targets.borrow().contains_key(label);
                if !is_fake_load {
                    if label.name() == info.pkg.build_file.base_name() {
                        for l in env_loads(env, &info) {
                            el.children.push(XmlEl::new("load").attr("name", lp(&l)));
                        }
                        for f in &info.pkg.args.features {
                            el.children.push(XmlEl::new("feature").attr("name", f.clone()));
                        }
                        el.set("package_contains_errors", info.pkg.contains_errors.to_string());
                    }
                    for l in visibility_group_labels(&env.visibility_labels(&info)) {
                        el.children.push(XmlEl::new("package-group").attr("name", lp(&l)));
                    }
                    for l in env.visibility_labels(&info) {
                        el.children.push(XmlEl::new("visibility-label").attr("name", lp(&l)));
                    }
                }
                el
            }
        };
        // Rules also list visibility via the `visibility` attribute only; package groups
        // attach to source files above.
        el.set("name", lp(&n.label));
        let mut loc = if let Some(ref_pkg) = env.fake_load_targets.borrow().get(&n.label) {
            crate::target_env::fake_load_target_location(ref_pkg, opts.relative_locations)
        } else {
            target_location(&info, opts.relative_locations)
        };
        if !opts.xml_line_numbers {
            if let Some(i) = loc.find(':') {
                loc.truncate(i);
            }
        }
        el.set("location", loc);
        root.children.push(el);
    }
    let lt = &opts.line_terminator;
    let mut out = format!("<?xml version=\"1.1\" encoding=\"UTF-8\" standalone=\"no\"?>{lt}");
    xml_write(&root, 0, &mut out);
    Ok(out)
}

fn env_loads(env: &TargetEnv<'_>, info: &TInfo) -> Vec<Label> {
    env.build_file_loads(&info.pkg)
}

// ---- proto ----------------------------------------------------------------------------------

fn license_msg() -> Msg {
    Msg::new()
}

/// `AttributeFormatter.writeAttributeValueToBuilder` onto either an Attribute or SelectorEntry
/// message (they share all field numbers except the ones given by `n`).
struct ValNums {
    int_value: u32,
    string_value: u32,
    boolean_value: u32,
    tristate_value: u32,
    string_list_value: u32,
    string_dict_value: u32,
    string_list_dict_value: u32,
    int_list_value: u32,
    label_dict_unary_value: u32,
    label_keyed_string_dict_value: u32,
    entry: bool,
}

const ATTR_NUMS: ValNums = ValNums {
    int_value: 3,
    string_value: 5,
    boolean_value: 14,
    tristate_value: 15,
    string_list_value: 6,
    string_dict_value: 8,
    string_list_dict_value: 11,
    int_list_value: 17,
    label_dict_unary_value: 19,
    label_keyed_string_dict_value: 22,
    entry: false,
};

const ENTRY_NUMS: ValNums = ValNums {
    int_value: 2,
    string_value: 3,
    boolean_value: 4,
    tristate_value: 5,
    string_list_value: 6,
    string_dict_value: 8,
    string_list_dict_value: 11,
    int_list_value: 13,
    label_dict_unary_value: 15,
    label_keyed_string_dict_value: 17,
    entry: true,
};

fn write_value(m: &mut Msg, nums: &ValNums, v: &AttrValue) {
    match v {
        AttrValue::Int(i) => {
            m.int32(nums.int_value, "int_value", *i as i32);
        }
        AttrValue::Str(s) => {
            m.str(nums.string_value, "string_value", s.clone());
        }
        AttrValue::Label(l) | AttrValue::Output(l) => {
            if let Some(l) = l {
                m.str(nums.string_value, "string_value", lp(l));
            }
        }
        AttrValue::Bool(b) => {
            m.bool(nums.boolean_value, "boolean_value", *b);
            if !nums.entry {
                m.str(nums.string_value, "string_value", if *b { "true" } else { "false" });
                m.int32(nums.int_value, "int_value", i32::from(*b));
            }
        }
        AttrValue::Tristate(t) => {
            let (name, num, int, s) = match *t {
                t if t < 0 => ("AUTO", 2, -1, "auto"),
                0 => ("NO", 0, 0, "no"),
                _ => ("YES", 1, 1, "yes"),
            };
            m.en(nums.tristate_value, "tristate_value", name, num);
            if !nums.entry {
                m.int32(nums.int_value, "int_value", int);
                m.str(nums.string_value, "string_value", s);
            }
        }
        AttrValue::StrList(l) => {
            for s in l {
                m.rstr(nums.string_list_value, "string_list_value", s.clone());
            }
        }
        AttrValue::LabelList(l) | AttrValue::OutputList(l) => {
            for s in l {
                m.rstr(nums.string_list_value, "string_list_value", lp(s));
            }
        }
        AttrValue::IntList(l) => {
            for i in l {
                m.rep(nums.int_list_value, "int_list_value", PbVal::Int32(*i as i32));
            }
        }
        AttrValue::StrDict(d) => {
            for (k, v) in d {
                let mut e = Msg::new();
                e.str(1, "key", k.clone()).str(2, "value", v.clone());
                m.rmsg(nums.string_dict_value, "string_dict_value", e);
            }
        }
        AttrValue::StrListDict(d) => {
            for (k, v) in d {
                let mut e = Msg::new();
                e.str(1, "key", k.clone());
                for s in v {
                    e.rstr(2, "value", s.clone());
                }
                m.rmsg(nums.string_list_dict_value, "string_list_dict_value", e);
            }
        }
        AttrValue::StrKeyedLabelDict(d) => {
            for (k, v) in d {
                let mut e = Msg::new();
                e.str(1, "key", k.clone()).str(2, "value", lp(v));
                m.rmsg(nums.label_dict_unary_value, "label_dict_unary_value", e);
            }
        }
        AttrValue::LabelKeyedStrDict(d) => {
            for (k, v) in d {
                let mut e = Msg::new();
                e.str(1, "key", lp(k)).str(2, "value", v.clone());
                m.rmsg(nums.label_keyed_string_dict_value, "label_keyed_string_dict_value", e);
            }
        }
        AttrValue::Select(_) => {}
    }
    let _ = license_msg;
}

/// `getFlattenedAttributeValues`.
fn flattened(ty: AttrType, values: Vec<AttrValue>) -> Option<AttrValue> {
    if values.len() == 1 {
        return values.into_iter().next();
    }
    match ty {
        AttrType::Int | AttrType::String | AttrType::Label | AttrType::Output | AttrType::Bool | AttrType::Tristate => None,
        AttrType::StringList => Some(AttrValue::StrList(
            values.into_iter().flat_map(|v| if let AttrValue::StrList(l) = v { l } else { Vec::new() }).collect(),
        )),
        AttrType::LabelList => Some(AttrValue::LabelList(
            values.into_iter().flat_map(|v| if let AttrValue::LabelList(l) = v { l } else { Vec::new() }).collect(),
        )),
        AttrType::OutputList => Some(AttrValue::OutputList(
            values.into_iter().flat_map(|v| if let AttrValue::OutputList(l) = v { l } else { Vec::new() }).collect(),
        )),
        AttrType::IntList => Some(AttrValue::IntList(
            values.into_iter().flat_map(|v| if let AttrValue::IntList(l) = v { l } else { Vec::new() }).collect(),
        )),
        AttrType::StringDict => {
            let mut m = BTreeMap::new();
            for v in values {
                if let AttrValue::StrDict(d) = v {
                    m.extend(d);
                }
            }
            Some(AttrValue::StrDict(m))
        }
        AttrType::StringListDict => {
            let mut m = BTreeMap::new();
            for v in values {
                if let AttrValue::StrListDict(d) = v {
                    m.extend(d);
                }
            }
            Some(AttrValue::StrListDict(m))
        }
        AttrType::LabelKeyedStringDict => {
            let mut m: Vec<(Label, String)> = Vec::new();
            for v in values {
                if let AttrValue::LabelKeyedStrDict(d) = v {
                    for (k, val) in d {
                        if let Some(e) = m.iter_mut().find(|(x, _)| *x == k) {
                            e.1 = val;
                        } else {
                            m.push((k, val));
                        }
                    }
                }
            }
            Some(AttrValue::LabelKeyedStrDict(m))
        }
        AttrType::StringKeyedLabelDict => {
            let mut m: Vec<(String, Label)> = Vec::new();
            for v in values {
                if let AttrValue::StrKeyedLabelDict(d) = v {
                    for (k, val) in d {
                        if let Some(e) = m.iter_mut().find(|(x, _)| *x == k) {
                            e.1 = val;
                        } else {
                            m.push((k, val));
                        }
                    }
                }
            }
            Some(AttrValue::StrKeyedLabelDict(m))
        }
    }
}

/// `AttributeFormatter.getAttributeProto`.
pub fn attribute_proto(e: &AttrEntry, value: Option<&AttrValue>, selector_list: Option<&[SelectPart]>) -> Msg {
    let mut m = Msg::new();
    m.str(1, "name", e.name.clone());
    m.bool(13, "explicitly_specified", e.explicit);
    // nodep
    if e.nodep_set {
        m.bool(20, "nodep", e.nodep);
    }
    if let Some(parts) = selector_list {
        m.en(2, "type", disc::name(disc::SELECTOR_LIST), disc::SELECTOR_LIST);
        let mut sl = Msg::new();
        sl.en(1, "type", disc::name(e.disc), e.disc);
        for p in parts {
            let mut sel = Msg::new();
            match p {
                SelectPart::Plain(v) => {
                    let mut entry = Msg::new();
                    entry.str(1, "label", "//conditions:default");
                    entry.bool(16, "is_default_value", true);
                    write_value(&mut entry, &ENTRY_NUMS, v);
                    sel.rmsg(1, "entries", entry);
                    sel.bool(2, "has_default_value", true);
                    sel.str(3, "no_match_error", "");
                }
                SelectPart::Select { entries, no_match_error } => {
                    let mut has_default = false;
                    for (k, v) in entries {
                        let mut entry = Msg::new();
                        let is_default = k.to_string() == "//conditions:default";
                        has_default |= is_default;
                        entry.str(1, "label", lp(k));
                        entry.bool(16, "is_default_value", is_default);
                        write_value(&mut entry, &ENTRY_NUMS, v);
                        sel.rmsg(1, "entries", entry);
                    }
                    sel.bool(2, "has_default_value", has_default);
                    sel.str(3, "no_match_error", no_match_error.clone());
                }
            }
            sl.rmsg(2, "elements", sel);
        }
        m.msg(21, "selector_list", sl);
    } else {
        m.en(2, "type", disc::name(e.disc), e.disc);
        if e.disc == disc::LICENSE {
            // `License`: the declared license types, `NONE` by default (`License.NO_LICENSE`).
            let mut l = Msg::new();
            let types: Vec<String> = match value {
                Some(AttrValue::StrList(t)) if !t.is_empty() => t.iter().map(|x| x.to_ascii_uppercase()).collect(),
                _ => vec!["NONE".to_string()],
            };
            for t in types {
                l.rstr(1, "license_type", t);
            }
            m.msg(7, "license", l);
        } else if let Some(v) = value {
            write_value(&mut m, &ATTR_NUMS, v);
        }
    }
    m
}

/// `ProtoOutputFormatter.toTargetProtoBuffer`.
pub fn target_proto(env: &TargetEnv<'_>, info: &TInfo, opts: &FormatOpts) -> Msg {
    target_proto_ex(env, info, opts, false)
}

/// `configured`: cquery's `ConfiguredProtoOutputFormatter.addAttributes` reads values through the
/// `ConfiguredAttributeMapper`, so the `visibility` attribute has no value unless it was explicitly
/// specified (the package default is not part of the attribute).
pub fn target_proto_ex(env: &TargetEnv<'_>, info: &TInfo, opts: &FormatOpts, configured: bool) -> Msg {
    let mut t = Msg::new();
    match &info.target {
        Target::Rule(rule) => {
            let mut r = Msg::new();
            r.str(1, "name", lp(&rule.label));
            let class = rule.rule_class.clone();
            r.str(2, "rule_class", class);
            if opts.proto_locations {
                r.str(3, "location", target_location(info, opts.relative_locations));
            }
            let pred = |n: &str| opts.proto_output_rule_attrs.iter().any(|a| a == "all" && opts.proto_output_rule_attrs.len() == 1)
                || opts.proto_output_rule_attrs.iter().any(|a| a == n);
            let mut attrs: Vec<Msg> = Vec::new();
            let mut names: Vec<String> = Vec::new();
            for a in rule_attributes(rule) {
                if !((opts.proto_default_values || a.explicit) && pred(&a.name)) {
                    continue;
                }
                let msg = if opts.proto_flatten_selects || !a.value.is_select() {
                    let values = possible_values(&a.value);
                    let v = if a.null_default || (configured && a.name == "visibility" && !a.explicit) { None } else { flattened(a.ty, values) };
                    attribute_proto(&a, v.as_ref(), None)
                } else {
                    attribute_proto(&a, None, Some(select_parts(&a.value)))
                };
                names.push(a.name.clone());
                attrs.push(msg);
            }
            let mut idx: Vec<usize> = (0..attrs.len()).collect();
            idx.sort_by(|&a, &b| names[a].cmp(&names[b]));
            for i in idx {
                r.rmsg(4, "attribute", attrs[i].clone());
            }
            // `ProtoOutputFormatter.toTargetProtoBuffer`: every rule of a Starlark rule class has the
            // digest of its definition environment as the last (unsorted) attribute.
            if let Some(spec) = rule.extra.rule_class.as_ref().and_then(|rc| rc.starlark.as_ref()) {
                if !spec.definition_digest.is_empty() {
                    let mut h = Msg::new();
                    h.str(1, "name", "$rule_implementation_hash");
                    h.en(2, "type", disc::name(disc::STRING), disc::STRING);
                    h.str(5, "string_value", spec.definition_digest.clone());
                    r.rmsg(4, "attribute", h);
                }
            }
            if opts.proto_rule_inputs_and_outputs {
                let mut inputs: Vec<Label> =
                    rule_deps_ext(rule, opts.implicit_deps, opts.tool_deps, opts.nodep_deps)
                        .into_iter()
                        .filter_map(|(l, attr)| if attr == "_hidden_toolchain_types" { None } else { Some(l) })
                        .collect();
                inputs.sort_by(label_cmp);
                for l in inputs {
                    r.rstr(5, "rule_input", lp(&l));
                }
                for o in rule_outputs(&info.pkg, &rule.label) {
                    r.rstr(6, "rule_output", lp(&o));
                }
            }
            for f in &info.pkg.args.features {
                r.rstr(7, "default_setting", f.clone());
            }
            if opts.proto_include_synthetic_attribute_hash {
                let mut fp = zel_util::Fingerprint::new();
                for a in rule_attributes(rule) {
                    if a.explicit {
                        fp.add_str(&a.name);
                        if let Some(s) = java_string(&a.value) {
                            fp.add_str(&s);
                        }
                    }
                }
                r.str(12, "synthetic_attribute_hash", fp.hex_digest_and_reset());
            }
            if opts.proto_instantiation_stack {
                let (_, inst_frames) = instantiation_stack(rule);
                for (loc, name) in inst_frames {
                    let rel = relativize_location(&loc, info);
                    r.rstr(13, "instantiation_stack", format!("{rel}: {name}"));
                }
            }
            if opts.proto_definition_stack {
                let (_, def_frames) = definition_stack(rule);
                for (loc, name) in def_frames {
                    let rel = relativize_location(&loc, info);
                    r.rstr(14, "definition_stack", format!("{rel}: {name}"));
                }
            }
            if opts.proto_rule_classes {
                let rule_key = rule.extra.rule_class.as_ref()
                    .and_then(|rc| rc.starlark.as_ref().map(|s| format!("{}%{}", s.defined_in, rc.name)))
                    .unwrap_or_else(|| rule.rule_class.clone());
                r.str(19, "rule_class_key", rule_key);
            }
            if opts.proto_include_starlark_rule_env {
                // If Starlark rule, add env entries if any
            }
            t.en(1, "type", "RULE", 1);
            t.msg(2, "rule", r);
        }
        Target::OutputFile { label, generating_rule } => {
            let mut g = Msg::new();
            g.str(1, "name", lp(label));
            g.str(2, "generating_rule", lp(generating_rule));
            if opts.proto_locations {
                g.str(3, "location", target_location(info, opts.relative_locations));
            }
            t.en(1, "type", "GENERATED_FILE", 3);
            t.msg(4, "generated_file", g);
        }
        Target::InputFile { label } => {
            let mut s = Msg::new();
            s.str(1, "name", lp(label));
            let fake_pkg = env.fake_load_targets.borrow().get(label).cloned();
            if opts.proto_locations {
                let loc = match &fake_pkg {
                    Some(ref_pkg) => crate::target_env::fake_load_target_location(ref_pkg, opts.relative_locations),
                    None => target_location(info, opts.relative_locations),
                };
                s.str(2, "location", loc);
            }
            if fake_pkg.is_none() {
                let is_build = label.name() == info.pkg.build_file.base_name();
                if is_build {
                    for l in env.build_file_loads(&info.pkg) {
                        s.rstr(3, "subinclude", lp(&l));
                    }
                    for f in &info.pkg.args.features {
                        s.rstr(6, "feature", f.clone());
                    }
                    s.bool(9, "package_contains_errors", info.pkg.contains_errors);
                }
                for l in visibility_group_labels(&env.visibility_labels(info)) {
                    s.rstr(4, "package_group", lp(&l));
                }
                for l in env.visibility_labels(info) {
                    s.rstr(5, "visibility_label", lp(&l));
                }
            }
            t.en(1, "type", "SOURCE_FILE", 2);
            t.msg(3, "source_file", s);
        }
        Target::EnvironmentGroup { label, environments, defaults } => {
            let mut g = Msg::new();
            g.str(1, "name", lp(label));
            for e in environments {
                g.rstr(2, "environment", lp(e));
            }
            for d in defaults {
                g.rstr(3, "default", lp(d));
            }
            t.en(1, "type", "ENVIRONMENT_GROUP", 5);
            t.msg(6, "environment_group", g);
        }
        Target::PackageGroup { label, packages } => {
            let mut g = Msg::new();
            g.str(1, "name", lp(label));
            for p in contained_packages(info, packages, opts.package_group_includes_double_slash) {
                g.rstr(2, "contained_package", p);
            }
            for l in info.pkg.group_includes.get(label.name()).cloned().unwrap_or_default() {
                g.rstr(3, "included_package_group", lp(&l));
            }
            t.en(1, "type", "PACKAGE_GROUP", 4);
            t.msg(5, "package_group", g);
        }
    }
    t
}

fn relativize_location(loc: &str, info: &TInfo) -> String {
    let root = crate::target_env::package_root(&info.pkg);
    let r = root.path_string();
    if let Some(rest) = loc.strip_prefix(r) {
        if rest.starts_with('/') && !r.is_empty() {
            return rest[1..].to_string();
        }
    }
    if let Some(rest) = loc.strip_prefix("//:") {
        return rest.to_string();
    }
    if let Some(rest) = loc.strip_prefix("//") {
        return rest.replacen(':', "/", 1);
    }
    loc.to_string()
}
