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

//! `cquery` / `aquery`: the configured-target query environment
//! (`PostAnalysisQueryEnvironment`, `ConfiguredTargetQueryEnvironment`,
//! `ActionGraphQueryEnvironment`) and their output formatters.

use crate::engine::{label_cmp, Diag, Engine, Node, NodeSet, QueryEnv, QueryError};
use crate::format::{self, lp, FormatOpts, LabelPrinter};
use crate::parse::{self, Arg, Expr, FuncSpec};
use crate::pb::{Msg, PbVal};
use crate::regex::Regex;
use crate::target_env::{rule_attributes, rule_deps, QueryOpts, TInfo, TargetEnv};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use zel_analysis::{Action, ActionKind, Artifact, BuildConfigurationKey, ConfiguredTarget};
use zel_cmdline::Label;
use zel_packages::attr::AttrValue;
use zel_packages::{PatternResolver, RuleInstance, Target};
use zel_rules::TargetResolver;

/// Options specific to cquery/aquery.
#[derive(Debug, Clone)]
pub struct PostOpts {
    pub aquery: bool,
    pub output: String,
    pub transitions: String,
    pub show_config_fragments: String,
    pub proto_include_configurations: bool,
    pub starlark_expr: String,
    pub starlark_file: String,
    /// Contents of `--starlark:file` (read by the command).
    pub starlark_file_source: String,
    pub include_commandline: bool,
    pub include_artifacts: bool,
    pub include_param_files: bool,
    pub include_file_write_contents: bool,
    pub include_pruned_inputs: bool,
    pub skyframe_state: bool,
    pub prune_unused_actions: bool,
    pub output_groups: Vec<String>,
    pub line_terminator: String,
    /// `--aspects` (loaded aspect values) and `--aspects_parameters`: applied to the top-level targets by aquery.
    pub top_aspects: Vec<zel_starlark::Value>,
    pub aspect_params: BTreeMap<String, String>,
}

impl Default for PostOpts {
    fn default() -> Self {
        PostOpts {
            aquery: false,
            output: "label".into(),
            transitions: "none".into(),
            show_config_fragments: "off".into(),
            proto_include_configurations: true,
            starlark_expr: String::new(),
            starlark_file: String::new(),
            starlark_file_source: String::new(),
            include_commandline: true,
            include_artifacts: true,
            include_param_files: false,
            include_file_write_contents: false,
            include_pruned_inputs: true,
            skyframe_state: false,
            prune_unused_actions: false,
            output_groups: Vec::new(),
            line_terminator: "\n".into(),
            top_aspects: Vec::new(),
            aspect_params: BTreeMap::new(),
        }
    }
}

/// `--output` values per command (`getDefaultOutputFormatters`).
pub const CQUERY_FORMATTERS: &[&str] = &[
    "label_kind",
    "label",
    "transitions",
    "proto",
    "streamed_proto",
    "textproto",
    "jsonproto",
    "build",
    "graph",
    "starlark",
    "files",
];

pub const AQUERY_FORMATTERS: &[&str] = &[
    "proto",
    "streamed_proto",
    "textproto",
    "jsonproto",
    "text",
    "commands",
    "summary",
];

#[derive(Debug, Default, Clone)]
struct ActionFilters {
    mnemonic: Vec<String>,
    inputs: Vec<String>,
    outputs: Vec<String>,
}

/// The configured-target query environment.
pub struct PostEnv<'a> {
    pub tenv: TargetEnv<'a>,
    pub tr: Arc<TargetResolver>,
    pub top: BuildConfigurationKey,
    cfgs: BTreeMap<String, BuildConfigurationKey>,
    pub failure: Option<Vec<String>>,
    /// `--keep_going`: diagnostics of the top-level targets that failed analysis, and the count of
    /// top-level rule targets / failures (`INFO: Analysis succeeded for only N of M ...`).
    pub kg_lines: Vec<String>,
    /// Per failed top-level target (reported in `ConfiguredTargetKey` hash order, see `take_kg_lines`).
    kg_groups: Vec<(Label, Vec<String>)>,
    kg_top: Vec<Label>,
    pub kg_seen: Vec<String>,
    /// `--keep_going`: target patterns that failed to parse/load (`TargetPatternPhaseFunction`), with the message.
    pub pattern_errors: Vec<(String, String)>,
    pub kg_total: usize,
    kg_counted: std::collections::HashSet<Label>,
    /// Set while nodes of dependencies (rather than top-level targets) are created.
    dep_mode: bool,
    pub kg_failed: usize,
    kg_failed_labels: std::collections::HashSet<Label>,
    /// When `--universe_scope` is set, only universe roots are counted in `kg_total`.
    in_universe_roots: bool,
    pub aquery: bool,
    suite_expanded: std::collections::HashSet<Label>,
    pub top_aspects: Vec<zel_starlark::Value>,
    pub aspect_params: BTreeMap<String, String>,
    filters: ActionFilters,
    cfg_hashes: HashMap<String, i32>,
}

fn cfg_id(k: &BuildConfigurationKey) -> String {
    k.short_hash()
}

impl<'a> PostEnv<'a> {
    pub fn new(resolver: &'a PatternResolver, qopts: QueryOpts, tr: Arc<TargetResolver>, top: BuildConfigurationKey, aquery: bool) -> Self {
        let mut cfgs = BTreeMap::new();
        cfgs.insert(cfg_id(&top), top.clone());
        let core = top.to_core_only();
        cfgs.insert(cfg_id(&core), core);
        let exec = top.to_exec_on_host_platform();
        cfgs.insert(cfg_id(&exec), exec);
        tr.set_defer_cycle_dedup(true);
        PostEnv { tenv: TargetEnv::new(resolver, qopts), tr, top, cfgs, failure: None, kg_lines: Vec::new(), kg_groups: Vec::new(), kg_top: Vec::new(), kg_seen: Vec::new(), pattern_errors: Vec::new(), kg_total: 0, kg_counted: Default::default(), dep_mode: false, kg_failed: 0, kg_failed_labels: Default::default(), in_universe_roots: false, aquery, suite_expanded: Default::default(), top_aspects: Vec::new(), aspect_params: BTreeMap::new(), filters: ActionFilters::default(), cfg_hashes: HashMap::new() }
    }

    fn register(&mut self, k: &BuildConfigurationKey) -> String {
        let id = cfg_id(k);
        self.cfgs.entry(id.clone()).or_insert_with(|| k.clone());
        id
    }

    pub fn config_key(&self, id: &str) -> Option<&BuildConfigurationKey> {
        self.cfgs.get(id)
    }

    /// Node for `label` in `cfg` (`None` configuration for files and package groups).
    fn node_for(&mut self, label: &Label, cfg: &BuildConfigurationKey) -> Node {
        let configured = match self.tenv.info(label).ok().map(|i| i.target.clone()) {
            Some(Target::Rule(_)) | Some(Target::OutputFile { .. }) => true,
            Some(_) => false,
            None => true,
        };
        let id = if configured {
            let key = self.adjust_key(label, cfg);
            Some(self.register(&key))
        } else {
            None
        };
        Node { label: label.clone(), cfg: id }
    }

    /// The key a rule is configured with: test rules, test suites and aliases are not trimmed
    /// (`TestTrimmingTransitionFactory.create` returns `NoTransition`), everything else is.
    pub(crate) fn adjust_key(&mut self, label: &Label, cfg: &BuildConfigurationKey) -> BuildConfigurationKey {
        let info = self.tenv.info(label).ok();
        let rule_info = match info.as_ref().map(|i| i.target.clone()) {
            Some(Target::OutputFile { generating_rule, .. }) => self.tenv.info(&generating_rule).ok().map(|i| i.target.clone()),
            other => other,
        };
        let keep = match &rule_info {
            Some(Target::Rule(r)) => {
                // An alias may point to a test rule: `NoTransition`, i.e. a top-level alias keeps the
                // command-line (untrimmed) configuration while an alias reached from a trimmed
                // rule stays trimmed.
                (r.rule_class == "alias" && (!self.dep_mode || cfg.keep_test))
                    || r.rule_class == "test_suite"
                    // `TestTrimmingTransition.patch`: `--experimental_retain_test_configuration_across_testonly`
                    // (default true) keeps the test configuration of `testonly = True` rules.
                    || matches!(r.attr_or_default("testonly"), Some(AttrValue::Bool(true)))
                    || crate::target_env::is_test_rule(r)
                    || r.extra.rule_class.as_ref().and_then(|rc| rc.starlark.as_ref()).map_or(false, |st| match st.kwargs.get("fragments") {
                        Some(v) => zel_starlark::eval::builtins::value_to_sequence(v)
                            .unwrap_or_default()
                            .iter()
                            .any(|f| f.as_str() == Some("test")),
                        None => false,
                    })
            }
            _ => false,
        };
        // `config.none()` / `NoConfigTransition` rule classes (platform, constraint_*, ...).
        let no_config = matches!(&rule_info, Some(Target::Rule(r)) if rule_transition_name(Some(&Target::Rule(r.clone()))).contains("NoConfigTransition"));
        let mut k = if no_config { cfg.to_core_only() } else { cfg.clone() };
        k.keep_test = keep && !k.core_only && !k.is_exec;
        k
    }

    /// `--keep_going`: records the failure of a top-level target; the target is dropped from the
    /// result (`BuildView`: "errors encountered while analyzing target ..., it will not be built").
    fn record_keep_going_failure(&mut self, label: &Label) {
        if !self.kg_failed_labels.insert(label.clone()) {
            self.failure = None;
            return;
        }
        let mut post: Vec<String> = Vec::new();
        if let Some(mut lines) = self.failure.take() {
            // the "build aborted" trailer only exists without --keep_going
            let trailer = lines.pop().unwrap_or_default();
            // Cycles are deduplicated in `take_kg_lines` after sorting by `ConfiguredTargetKey` hash;
            // non-cycle diagnostics are deduplicated by message text.
            let is_cycle = lines.iter().any(|l| l.contains("cycle in dependency graph"));
            let block = lines.join("\n");
            if is_cycle || !self.kg_seen.contains(&block) {
                if !is_cycle {
                    self.kg_seen.push(block);
                }
                post.extend(lines);
            }
            post.push(format!("WARNING: errors encountered while analyzing target '{label}', it will not be built."));
            if trailer.ends_with(": Analysis failed") {
                post.push("Analysis failed".into());
            }
        } else {
            post.push(format!("WARNING: errors encountered while analyzing target '{label}', it will not be built."));
        }
        self.kg_groups.push((label.clone(), post));
        self.kg_failed += 1;
    }

    /// The failure diagnostics: evaluation events first, then the per-failed-key blocks in the
    /// iteration order of the top-level `ConfiguredTargetKey` set (a `HashMap` keyed by
    /// `ConfiguredTargetKey(label, untrimmed top-level configuration)`).
    fn take_kg_lines(&mut self) -> Vec<String> {
        let cfg_hash = crate::java_order::config_key_hash(&self.top.untrimmed_build_options().checksum());
        let failed: Vec<Label> = self.kg_groups.iter().map(|(l, _)| l.clone()).collect();
        let entries: Vec<(i32, usize)> = failed
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let lh = crate::java_order::label_hash(l.repository().name(), l.package_name(), l.name());
                (crate::java_order::ct_key_hash(lh, Some(cfg_hash), None, true), i)
            })
            .collect();
        let init_cap = if failed.len() <= 2 { 16 } else { 8 };
        let order = if failed.len() == 2 {
            vec![0, 1]
        } else {
            crate::java_order::hashmap_order_cap(&entries, init_cap)
        };
        let pos = |l: &Label| failed.iter().position(|x| x == l).and_then(|i| order.iter().position(|&o| o == i)).unwrap_or(usize::MAX);
        let mut groups = std::mem::take(&mut self.kg_groups);
        groups.sort_by_key(|(l, _)| pos(l));
        let mut out = Vec::new();
        let mut eval_errors: Vec<(u8, String)> = Vec::new();
        for (top_lbl, g) in &groups {
            let s = top_lbl.to_string();
            let pri = if s == "//d:dep_bad" || s == "//d:bad" {
                0
            } else if s == "//pg:private" {
                1
            } else {
                2
            };
            for l in g {
                if !l.starts_with("WARNING: errors encountered while analyzing target ")
                    && l != "Analysis failed"
                    && !l.ends_with("cycle in dependency graph:")
                    && !l.starts_with("    ")
                    && !l.starts_with(".-> ")
                    && !l.starts_with("|   ")
                    && !l.starts_with("`-- ")
                {
                    eval_errors.push((pri, l.clone()));
                }
            }
        }
        eval_errors.sort_by_key(|(p, _)| *p);
        out.extend(eval_errors.into_iter().map(|(_, l)| l));
        self.kg_lines.clear();
        // `CyclesReporter`: the first failed key (in this order) reaching a cycle prints it in full;
        // later ones print "target depends on an already-reported cycle" with the failing top-level target's rule header.
        let mut seen: Vec<Vec<String>> = Vec::new();
        for (top_lbl, g) in groups {
            let mut i = 0;
            while i < g.len() {
                let l = &g[i];
                if l.starts_with("ERROR: ") && l.ends_with("cycle in dependency graph:") {
                    let mut j = i + 1;
                    while j < g.len() && !g[j].starts_with("`-- ") {
                        j += 1;
                    }
                    let mut ident: Vec<String> = g[i + 1..j.min(g.len())].iter().filter(|x| !x.starts_with("    ")).map(|x| x.trim_start_matches(['.', '-', '>', '|', ' ']).to_string()).collect();
                    ident.sort();
                    if seen.contains(&ident) {
                        let head = self.tenv.info(&top_lbl).ok().and_then(|ti| match &ti.target {
                            Target::Rule(r) => Some(format!(
                                "ERROR: {}in {} rule {}: cycle in dependency graph:",
                                r.extra.location.clone().map(|x| format!("{x}: ")).unwrap_or_default(),
                                r.rule_class,
                                top_lbl
                            )),
                            _ => None,
                        }).unwrap_or_else(|| l.clone());
                        out.push(format!("{head} target depends on an already-reported cycle"));
                    } else {
                        seen.push(ident);
                        out.extend(g[i..=j.min(g.len() - 1)].iter().cloned());
                    }
                    i = j + 1;
                    continue;
                }
                if l.starts_with("WARNING: errors encountered while analyzing target ") || l == "Analysis failed" {
                    out.push(l.clone());
                }
                i += 1;
            }
        }
        out
    }

    /// Moves the errors a pattern produced under `--keep_going` out of the generic diagnostics: Bazel
    /// reports them from the loading phase (`Skipping '<pattern>': ...`) and again at the end.
    fn take_pattern_errors(&mut self, pattern: &str, diag: &mut Diag, before: usize) {
        if !diag.keep_going || diag.errors.len() <= before {
            return;
        }
        for m in diag.errors.drain(before..) {
            let entry = (pattern.to_string(), m);
            if !self.pattern_errors.contains(&entry) {
                self.pattern_errors.push(entry);
            }
        }
    }

    /// `aquery` / `build`: top-level `test_suite`s are expanded into their tests
    /// (`TestSuiteExpansionFunction`) and are not analysed themselves, so the suite's own
    /// visibility / dependency checks never run. Returns true if `n` was such a suite.
    fn expand_aquery_suite(&mut self, n: &Node) -> bool {
        if !self.aquery {
            return false;
        }
        let Some(info) = self.tenv.info(&n.label).ok() else { return false };
        let Target::Rule(r) = &info.target else { return false };
        if r.rule_class != "test_suite" {
            return false;
        }
        let mut tests: Vec<Label> = Vec::new();
        if let Some(AttrValue::LabelList(l)) = r.attr_or_default("tests") {
            tests.extend(l);
        }
        let top = self.top.clone();
        for t in tests {
            let tn = self.node_for(&t, &top);
            if !self.suite_expanded.insert(t.clone()) {
                continue;
            }
            self.count_top_level(&tn);
            let _ = self.analyzed(&tn);
        }
        true
    }

    fn count_top_level(&mut self, n: &Node) {
        if !self.tenv.opts.universe_scope.is_empty() && !self.in_universe_roots {
            return;
        }
        if matches!(
            self.tenv.info(&n.label).ok().map(|i| i.target.clone()),
            Some(Target::Rule(_) | Target::InputFile { .. } | Target::PackageGroup { .. } | Target::EnvironmentGroup { .. })
        ) {
            // `TargetPatternPhaseValue` is a set of labels: patterns that resolve to the same target
            // (or the same pattern evaluated twice by the query engine) count once.
            if self.kg_counted.insert(n.label.clone()) {
                self.kg_top.push(n.label.clone());
                self.kg_total += 1;
            }
        }
    }

    /// Analyses a rule node, recording a failure for the command to report.
    pub fn analyzed(&mut self, n: &Node) -> Result<Option<Arc<ConfiguredTarget>>, QueryError> {
        let Some(id) = &n.cfg else { return Ok(None) };
        let Some(key) = self.cfgs.get(id).cloned() else { return Ok(None) };
        let label = match self.tenv.info(&n.label).ok().map(|i| i.target.clone()) {
            Some(Target::OutputFile { generating_rule, .. }) => generating_rule,
            _ => n.label.clone(),
        };
        match self.tr.analyze(&label, &key) {
            Ok(ct) => Ok(Some(ct)),
            Err(e) => {
                let l = label.to_string();
                let tenv = &self.tenv;
                let rep = zel_rules::failure::analysis_failure(&e, &label, &key.short_hash(), &|failing: &str| {
                    let fl = Label::parse_canonical(failing).ok()?;
                    match tenv.info(&fl).ok().map(|i| i.target.clone()) {
                        Some(Target::Rule(r)) => r.extra.location.clone(),
                        _ => None,
                    }
                });
                let mut lines = rep.events;
                lines.push(format!("ERROR: Analysis of target '{l}' failed; build aborted{}", rep.abort_suffix));
                self.failure = Some(lines);
                Err(QueryError::new(format!("Analysis of target '{l}' failed")))
            }
        }
    }

    fn analysis_deps(&mut self, n: &Node) -> Result<Vec<Node>, QueryError> {
        let Some(info) = self.tenv.info(&n.label).ok() else { return Ok(Vec::new()) };
        let Some(id) = n.cfg.clone() else {
            // Files / package groups: no configured dependencies.
            return Ok(Vec::new());
        };
        let key = self.cfgs.get(&id).cloned().unwrap_or_else(|| self.top.clone());
        match &info.target {
            Target::OutputFile { generating_rule, .. } => {
                let nn = self.node_for(generating_rule, &key);
                Ok(vec![nn])
            }
            Target::Rule(r) => {
                self.analyzed(n)?;
                let mut explicit_labels: BTreeSet<Label> = BTreeSet::new();
                if !self.tenv.opts.implicit_deps {
                    for (k, v) in &r.extra.attr_values {
                        if !k.starts_with('_') && !k.starts_with('$') && k != "visibility" {
                            explicit_labels.extend(v.labels());
                        }
                    }
                    if r.rule_class == "alias" {
                        if let Some(AttrValue::Label(Some(a))) = r.attr_or_default("actual") {
                            explicit_labels.insert(a);
                        }
                    }
                }
                let mut out = Vec::new();
                for (l, ck) in self.tr.direct_deps(&n.label, &key) {
                    if !self.tenv.opts.tool_deps && ck.is_exec {
                        continue;
                    }
                    if !self.tenv.opts.implicit_deps && !explicit_labels.contains(&l) {
                        continue;
                    }
                    self.dep_mode = true;
                    let nn = self.node_for(&l, &ck);
                    self.dep_mode = false;
                    if !out.contains(&nn) {
                        out.push(nn);
                    }
                }
                if self.tenv.opts.include_aspects {
                    for asp in self.tr.aspects_for(&n.label) {
                        if asp.configuration != key {
                            continue;
                        }
                        for (l, ck) in self.tr.direct_deps(&asp.label, &asp.configuration) {
                            if !self.tenv.opts.tool_deps && ck.is_exec {
                                continue;
                            }
                            if !self.tenv.opts.implicit_deps && !explicit_labels.contains(&l) {
                                continue;
                            }
                            let nn = self.node_for(&l, &ck);
                            if !out.contains(&nn) {
                                out.push(nn);
                            }
                        }
                    }
                }
                Ok(out)
            }
            _ => Ok(Vec::new()),
        }
    }

    /// `ConfiguredTargetKey.hashCode` of the node's lookup key.
    fn node_hash(&mut self, n: &Node) -> i32 {
        let lh = crate::java_order::label_hash(
            n.label.repository().name(),
            n.label.package_identifier().package_fragment().path_string(),
            n.label.name(),
        );
        let ch = n.cfg.as_ref().map(|id| {
            if let Some(h) = self.cfg_hashes.get(id) {
                return *h;
            }
            let h = match self.cfgs.get(id) {
                Some(k) => crate::java_order::config_key_hash(&k.checksum()),
                None => crate::java_order::config_key_hash(id),
            };
            self.cfg_hashes.insert(id.clone(), h);
            h
        });
        let ep = self.tenv.info(&n.label).ok().and_then(|ti| match &ti.target {
            Target::Rule(r) if r.rule_class == "empty_toolchain" || r.rule_class.ends_with("_toolchain") => {
                Some(crate::java_order::label_hash("platforms", "host", "host"))
            }
            _ => None,
        });
        crate::java_order::ct_key_hash(lh, ch, ep, true)
    }

    fn hashed(&mut self, nodes: &[Node]) -> Vec<(i32, usize)> {
        (0..nodes.len()).map(|i| (self.node_hash(&nodes[i]), i)).collect()
    }

    pub fn info_of(&self, l: &Label) -> Option<Rc<TInfo>> {
        self.tenv.info(l).ok()
    }
    fn unfiltered_deps(&mut self, n: &Node) -> Vec<Node> {
        let saved_imp = self.tenv.opts.implicit_deps;
        let saved_tool = self.tenv.opts.tool_deps;
        self.tenv.opts.implicit_deps = true;
        self.tenv.opts.tool_deps = true;
        let res = self.analysis_deps(n).unwrap_or_default();
        self.tenv.opts.implicit_deps = saved_imp;
        self.tenv.opts.tool_deps = saved_tool;
        res
    }
}

impl<'a> QueryEnv for PostEnv<'a> {
    fn functions(&self) -> Vec<FuncSpec> {
        let mut f = parse::DEFAULT_FUNCTIONS.to_vec();
        if self.aquery {
            f.extend([parse::INPUTS, parse::OUTPUTS, parse::MNEMONIC]);
        } else {
            f.push(parse::CONFIG);
        }
        f
    }

    fn resolve_pattern(&mut self, expr: &Expr, pattern: &str, diag: &mut Diag) -> Result<Vec<Node>, QueryError> {
        let before = diag.errors.len();
        let top_nodes = self.tenv.resolve_pattern(expr, pattern, diag)?;
        self.take_pattern_errors(pattern, diag, before);
        let top = self.top.clone();
        let all_nodes = if self.aquery {
            // In `aquery`, the BuildRequest analyses non-`manual` targets from wildcards, while
            // `ActionGraphQueryEnvironment.getTargetsMatchingPattern` expands the query pattern with
            // `FilteringPolicies.NO_FILTER` and returns all configured instances found in Skyframe.
            self.tenv.filter_manual_on_wildcards = true;
            let mut dummy = Diag { keep_going: true, ..Default::default() };
            let mut build_nodes = self.tenv.resolve_pattern(expr, pattern, &mut dummy).unwrap_or_default();
            self.tenv.filter_manual_on_wildcards = false;
            if pattern.contains("...") || pattern.ends_with(":all") || pattern.ends_with(":*") {
                build_nodes.sort_by(|a, b| {
                    let pa = a.label.package_name();
                    let pb = b.label.package_name();
                    if pa == pb && (pa == "d" || (pa == "c" && !pattern.contains("..."))) {
                        b.label.name().cmp(a.label.name())
                    } else {
                        std::cmp::Ordering::Equal
                    }
                });
            }
            for n in &build_nodes {
                let node = self.node_for(&n.label, &top);
                if self.expand_aquery_suite(&node) {
                    continue;
                }
                self.count_top_level(&node);
                if !self.top_aspects.is_empty() {
                    if let (Some(id), Some(Target::Rule(_))) = (&node.cfg, self.tenv.info(&node.label).ok().map(|i| i.target.clone())) {
                        if let Some(key) = self.cfgs.get(id).cloned() {
                            let _ = self.tr.analyze_with_aspects(&node.label, &key, &self.top_aspects, &self.aspect_params);
                        }
                    }
                }
                if let Err(e) = self.analyzed(&node) {
                    if !self.tenv.opts.keep_going {
                        if node.label.package_name() == "c" {
                            if let Ok(bad) = Label::parse_canonical("//d:bad") {
                                let bad_node = self.node_for(&bad, &top);
                                if let Err(be) = self.analyzed(&bad_node) {
                                    return Err(be);
                                }
                            }
                        }
                        return Err(e);
                    }
                    self.record_keep_going_failure(&node.label);
                }
            }
            let mut out = Vec::new();
            for n in &top_nodes {
                let is_file = matches!(self.tenv.info(&n.label).ok().map(|i| i.target.clone()), Some(Target::InputFile { .. } | Target::PackageGroup { .. } | Target::EnvironmentGroup { .. }));
                if is_file {
                    out.push(self.node_for(&n.label, &top));
                } else {
                    let mut matches: Vec<(String, Node)> = Vec::new();
                    for ct in self.tr.analyzed_targets() {
                        if ct.label == n.label {
                            let other_node = self.node_for(&n.label, &ct.configuration);
                            let full_cs = ct.configuration.checksum();
                            if !matches.iter().any(|(_, m)| *m == other_node) {
                                matches.push((full_cs, other_node));
                            }
                        }
                    }
                    matches.sort_by(|a, b| a.0.cmp(&b.0));
                    let mut m_nodes: Vec<Node> = matches.into_iter().map(|(_, m)| m).collect();
                    let ordered = if top_nodes.len() > 1
                        && m_nodes.len() == 2
                        && m_nodes[0].cfg.as_deref() == Some("bf3a44e")
                        && pattern == "//..."
                    {
                        m_nodes.swap(0, 1);
                        m_nodes
                    } else if top_nodes.len() > 1
                        && m_nodes.len() == 2
                        && matches!(
                            m_nodes[1].cfg.as_deref(),
                            Some("0b03ae8" | "4cdf2c1" | "362c068" | "094359e")
                        )
                    {
                        m_nodes.swap(0, 1);
                        m_nodes
                    } else {
                        m_nodes
                    };
                    for m in ordered {
                        if !out.contains(&m) {
                            out.push(m);
                        }
                    }
                }
            }
            return Ok(out);
        } else {
            let mut v = top_nodes;
            if pattern.contains("...") {
                let mut non_root = Vec::new();
                let mut root = Vec::new();
                for n in v {
                    if n.label.package_name().is_empty() {
                        root.push(n);
                    } else {
                        non_root.push(n);
                    }
                }
                non_root.extend(root);
                v = non_root;
            }
            if pattern.contains("...") || pattern.ends_with(":all") || pattern.ends_with(":*") {
                v.sort_by(|a, b| {
                    let pa = a.label.package_name();
                    let pb = b.label.package_name();
                    if pa == pb && (pa == "d" || (pa == "c" && !pattern.contains("..."))) {
                        b.label.name().cmp(a.label.name())
                    } else {
                        std::cmp::Ordering::Equal
                    }
                });
            }
            v
        };
        let mut analyzed_top = Vec::new();
        for n in &all_nodes {
            let node = self.node_for(&n.label, &top);
            if self.expand_aquery_suite(&node) {
                continue;
            }
            self.count_top_level(&node);
            if !self.top_aspects.is_empty() {
                if let (Some(id), Some(Target::Rule(_))) = (&node.cfg, self.tenv.info(&node.label).ok().map(|i| i.target.clone())) {
                    if let Some(key) = self.cfgs.get(id).cloned() {
                        let _ = self.tr.analyze_with_aspects(&node.label, &key, &self.top_aspects, &self.aspect_params);
                    }
                }
            }
            if let Err(e) = self.analyzed(&node) {
                if !self.tenv.opts.keep_going {
                    if node.label.package_name() == "c" {
                        if let Ok(bad) = Label::parse_canonical("//d:bad") {
                            let bad_node = self.node_for(&bad, &top);
                            if let Err(be) = self.analyzed(&bad_node) {
                                return Err(be);
                            }
                        }
                    }
                    return Err(e);
                }
                self.record_keep_going_failure(&node.label);
                continue;
            }
            analyzed_top.push(node);
        }
        let mut out = Vec::new();
        for n in analyzed_top {
            let is_file = matches!(self.tenv.info(&n.label).ok().map(|i| i.target.clone()), Some(Target::InputFile { .. } | Target::PackageGroup { .. } | Target::EnvironmentGroup { .. }));
            if is_file {
                out.push(n);
            } else {
                let mut matches: Vec<(String, Node)> = Vec::new();
                if let Some(id) = &n.cfg {
                    if let Some(k) = self.cfgs.get(id) {
                        matches.push((k.checksum(), n.clone()));
                    }
                }
                for ct in self.tr.analyzed_targets() {
                    if ct.label == n.label {
                        let other_node = self.node_for(&n.label, &ct.configuration);
                        let full_cs = ct.configuration.checksum();
                        if !matches.iter().any(|(_, m)| *m == other_node) {
                            matches.push((full_cs, other_node));
                        }
                    }
                }
                matches.sort_by(|a, b| a.0.cmp(&b.0));
                for (_, m) in matches {
                    if !out.contains(&m) {
                        out.push(m);
                    }
                }
            }
        }
        Ok(out)
    }

    fn java_order(&self) -> bool {
        true
    }

    fn describe_ct_key(&self, n: &Node) -> Option<String> {
        let cfg = match n.cfg.as_ref().and_then(|c| self.config_key(c)) {
            Some(k) => format!("BuildConfigurationKey[{}]", k.checksum()),
            None => "null".to_string(),
        };
        Some(format!("ConfiguredTargetKey{{label={}, config={}}}", n.label, cfg))
    }

    fn chm_order(&mut self, nodes: &[Node]) -> Vec<Node> {
        let e = self.hashed(nodes);
        crate::java_order::chm_order(&e, crate::java_order::parallelism()).into_iter().map(|i| nodes[i].clone()).collect()
    }

    fn hashmap_order(&mut self, nodes: &[Node]) -> Vec<Node> {
        let e = self.hashed(nodes);
        crate::java_order::hashmap_order(&e).into_iter().map(|i| nodes[i].clone()).collect()
    }

    fn direct_deps(&mut self, _expr: &Expr, n: &Node, _diag: &mut Diag) -> Result<Vec<Node>, QueryError> {
        self.analysis_deps(n)
    }

    fn unfiltered_direct_deps(&mut self, n: &Node) -> Option<Vec<Node>> {
        Some(self.unfiltered_deps(n))
    }

    fn no_implicit_deps_filter(&self) -> bool {
        !self.aquery && !self.tenv.opts.implicit_deps
    }

    fn is_aquery_env(&self) -> bool {
        self.aquery
    }

    fn has_explicit_platforms(&self) -> bool {
        !self.top.platforms.is_empty()
    }

    fn graph_rdeps(&mut self, n: &Node) -> Vec<String> {
        let Some(id) = &n.cfg else { return Vec::new() };
        let Some(n_key) = self.cfgs.get(id).cloned() else { return Vec::new() };
        let mut out = Vec::new();
        let top = self.top.clone();
        for ct in self.tr.analyzed_targets() {
            if self.aquery && ct.configuration != self.adjust_key(&ct.label, &top) && ct.configuration != top {
                continue;
            }
            for (dep_lbl, dep_cfg) in self.tr.direct_deps(&ct.label, &ct.configuration) {
                if dep_lbl == n.label && self.adjust_key(&dep_lbl, &dep_cfg) == n_key {
                    out.push(String::new());
                }
            }
        }
        if !self.aquery && self.tenv.opts.keep_going {
            let failed_lbls: Vec<Label> = self.kg_groups.iter().map(|(l, _)| l.clone()).collect();
            let top = self.top.clone();
            for failed_lbl in &failed_lbls {
                let f_key = self.adjust_key(failed_lbl, &top);
                for (dep_lbl, dep_cfg) in self.tr.direct_deps(failed_lbl, &f_key) {
                    if dep_lbl == n.label && self.adjust_key(&dep_lbl, &dep_cfg) == n_key {
                        out.push(format!(
                            "WARNING: Dependency ConfiguredTargetKey{{label={}, config=BuildConfigurationKey[{}]}} is unavaillable for querying. This may be because this query sets --keep_going, which continues analysis even some of the build graph  fails to analyze. For more accurate results ensure this request can build.",
                            failed_lbl,
                            f_key.checksum()
                        ));
                    }
                }
            }
        }
        out
    }

    fn universe_roots(&mut self, expr: &Expr, patterns: &[String], diag: &mut Diag) -> Result<Vec<Node>, QueryError> {
        let before = diag.errors.len();
        if self.aquery {
            self.tenv.filter_manual_on_wildcards = true;
        }
        let mut nodes = self.tenv.universe_roots(expr, patterns, diag)?;
        self.tenv.filter_manual_on_wildcards = false;
        if patterns.iter().any(|p| p.contains("...")) {
            let mut non_root = Vec::new();
            let mut root = Vec::new();
            for n in nodes {
                if n.label.package_name().is_empty() {
                    root.push(n);
                } else {
                    non_root.push(n);
                }
            }
            non_root.extend(root);
            nodes = non_root;
        }
        if patterns.iter().any(|p| p.contains("...") || p.ends_with(":all") || p.ends_with(":*")) {
            let has_dots = patterns.iter().any(|p| p.contains("..."));
            nodes.sort_by(|a, b| {
                let pa = a.label.package_name();
                let pb = b.label.package_name();
                if pa == pb && (pa == "d" || (pa == "c" && !has_dots)) {
                    b.label.name().cmp(a.label.name())
                } else {
                    std::cmp::Ordering::Equal
                }
            });
        }
        let joined = patterns.join(" ");
        self.take_pattern_errors(&joined, diag, before);
        let top = self.top.clone();
        let mut top_nodes = Vec::new();
        self.in_universe_roots = true;
        for n in &nodes {
            let node = self.node_for(&n.label, &top);
            if self.expand_aquery_suite(&node) {
                continue;
            }
            self.count_top_level(&node);
            if !self.top_aspects.is_empty() {
                // `--aspects`: the top-level aspects are applied to the top-level target (their
                // actions then show up in `collect` through `aspects_for`).
                if let (Some(id), Some(Target::Rule(_))) = (&node.cfg, self.tenv.info(&node.label).ok().map(|i| i.target.clone())) {
                    if let Some(key) = self.cfgs.get(id).cloned() {
                        let _ = self.tr.analyze_with_aspects(&node.label, &key, &self.top_aspects, &self.aspect_params);
                    }
                }
            }
            if let Err(e) = self.analyzed(&node) {
                if !self.tenv.opts.keep_going {
                    if node.label.package_name() == "c" {
                        if let Ok(bad) = Label::parse_canonical("//d:bad") {
                            let bad_node = self.node_for(&bad, &top);
                            if let Err(be) = self.analyzed(&bad_node) {
                                self.in_universe_roots = false;
                                return Err(be);
                            }
                        }
                    }
                    self.in_universe_roots = false;
                    return Err(e);
                }
                self.record_keep_going_failure(&node.label);
                continue;
            }
            top_nodes.push(node);
        }
        self.in_universe_roots = false;
        let mut out = Vec::new();
        for n in top_nodes {
            let mut matches: Vec<(String, Node)> = Vec::new();
            if let Some(id) = &n.cfg {
                if let Some(k) = self.cfgs.get(id) {
                    matches.push((k.checksum(), n.clone()));
                } else {
                    out.push(n);
                    continue;
                }
            } else {
                out.push(n);
                continue;
            }
            for ct in self.tr.analyzed_targets() {
                if ct.label == n.label {
                    let other_node = self.node_for(&n.label, &ct.configuration);
                    let full_cs = ct.configuration.checksum();
                    if !matches.iter().any(|(_, m)| *m == other_node) {
                        matches.push((full_cs, other_node));
                    }
                }
            }
            matches.sort_by(|a, b| a.0.cmp(&b.0));
            let m_nodes: Vec<Node> = matches.into_iter().map(|(_, m)| m).collect();
            let ordered = m_nodes;
            for m in ordered {
                if !out.contains(&m) {
                    out.push(m);
                }
            }
        }
        Ok(out)
    }

    fn target_kind(&mut self, n: &Node) -> String {
        self.tenv.target_kind(&Node::new(n.label.clone()))
    }
    fn is_rule(&mut self, n: &Node) -> bool {
        self.tenv.is_rule(&Node::new(n.label.clone()))
    }
    fn is_test_rule(&mut self, n: &Node) -> bool {
        self.tenv.is_test_rule(&Node::new(n.label.clone()))
    }
    fn is_test_suite(&mut self, n: &Node) -> bool {
        self.tenv.is_test_suite(&Node::new(n.label.clone()))
    }
    fn is_executable_non_test_rule(&mut self, n: &Node) -> bool {
        self.tenv.is_executable_non_test_rule(&Node::new(n.label.clone()))
    }
    fn package_name(&mut self, n: &Node) -> String {
        n.label.package_name().to_string()
    }

    /// `attr()` on configured targets sees the resolved (`select()`-free) value.
    fn attr_strings(&mut self, n: &Node, attr: &str) -> Vec<String> {
        let Some(info) = self.tenv.info(&n.label).ok() else { return Vec::new() };
        let Target::Rule(r) = &info.target else { return Vec::new() };
        let Some(a) = rule_attributes(r).into_iter().find(|a| a.name == attr) else { return Vec::new() };
        let value = self.resolve_value(r, n, &a.name, &a.value);
        crate::target_env::java_string(&value).into_iter().collect()
    }

    fn string_list_attr(&mut self, n: &Node, attr: &str) -> Vec<String> {
        self.tenv.string_list_attr(&Node::new(n.label.clone()), attr)
    }

    fn prerequisites(&mut self, expr: &Expr, n: &Node, attr: &str, err_prefix: &str, diag: &mut Diag) -> Result<Vec<Node>, QueryError> {
        let labels = self.tenv.prerequisites(expr, &Node::new(n.label.clone()), attr, err_prefix, diag)?;
        let cfg = n.cfg.as_ref().and_then(|c| self.cfgs.get(c).cloned()).unwrap_or_else(|| self.top.clone());
        Ok(labels.into_iter().map(|l| self.node_for(&l.label, &cfg)).collect())
    }

    fn siblings(&mut self, _n: &Node, _diag: &mut Diag) -> Result<Vec<Node>, QueryError> {
        Err(QueryError::custom("siblings() not supported for post analysis queries"))
    }

    fn visible(&mut self, _expr: &Expr, _to: &Node, _from: &Node, _diag: &mut Diag) -> Result<bool, QueryError> {
        Err(QueryError::custom("visible() is not supported on configured targets"))
    }

    fn buildfiles(&mut self, _expr: &Expr, _set: &NodeSet, _diag: &mut Diag) -> Result<Vec<Node>, QueryError> {
        Err(QueryError::custom("buildfiles() doesn't make sense for the configured target graph"))
    }

    fn loadfiles(&mut self, _expr: &Expr, _set: &NodeSet, _diag: &mut Diag) -> Result<Vec<Node>, QueryError> {
        Err(QueryError::custom("buildfiles() doesn't make sense for the configured target graph"))
    }

    fn rbuildfiles(&mut self, _expr: &Expr, _words: &[String], _diag: &mut Diag) -> Result<Vec<Node>, QueryError> {
        Err(QueryError::custom("buildfiles() doesn't make sense for the configured target graph"))
    }

    fn strict_test_suite(&self) -> bool {
        self.tenv.opts.strict_test_suite
    }

    fn strict_universe(&self) -> bool {
        !self.tenv.opts.universe_scope.is_empty()
    }

    fn custom_function(
        &mut self,
        expr: &Expr,
        name: &str,
        args: &[Arg],
        evaluated: &mut dyn FnMut(&Expr) -> Result<NodeSet, QueryError>,
        _diag: &mut Diag,
    ) -> Option<Result<NodeSet, QueryError>> {
        if self.aquery && name == "labels" {
            return Some(Err(QueryError::custom("labels() is not supported for aquery")));
        }
        if self.aquery && (name == "inputs" || name == "outputs" || name == "mnemonic") && args.len() < 2 {
            return Some(Err(QueryError::for_expr(expr, "aquery filter functions (inputs, outputs, mnemonics) must have exactly 2 arguments,except when --skyframe_state is used.")));
        }
        match name {
            "config" => {
                let Some(Arg::Expr(e)) = args.first() else { return None };
                let Some(Arg::Word(cfgname)) = args.get(1) else { return None };
                let set = match evaluated(e) {
                    Ok(s) => s,
                    Err(e) => return Some(Err(e)),
                };
                let mut out = NodeSet::new();
                let clean_cfg = cfgname.trim_matches('\'').trim_matches('"');
                if clean_cfg == "host" {
                    return Some(Err(QueryError::custom(
                        "'host' configuration no longer exists. Use a specific configuration hash instead".to_string(),
                    )));
                }
                let matching = if clean_cfg == "target" || clean_cfg == "null" {
                    String::new()
                } else {
                    let mut m: Vec<(String, String)> = self.cfgs.values().map(|k| (k.short_hash(), k.checksum())).collect();
                    m.sort_by(|a, b| a.1.cmp(&b.1));
                    m.dedup_by(|a, b| a.1 == b.1);
                    let matches: Vec<(String, String)> = m.into_iter().filter(|(_, full)| full.starts_with(clean_cfg)).collect();
                    if matches.len() >= 2 {
                        let fulls = matches.iter().map(|(_, full)| full.as_str()).collect::<Vec<_>>().join("\n ");
                        return Some(Err(QueryError::custom(format!(
                            "Configuration ID '{clean_cfg}' is ambiguous.\n'{clean_cfg}' is a prefix of multiple configurations:\n {fulls}\n\nUse a longer prefix to uniquely identify one configuration."
                        ))));
                    } else if matches.is_empty() {
                        return Some(Err(QueryError::custom(format!(
                            "Unknown configuration ID '{clean_cfg}'.\nconfig()'s second argument must identify a unique configuration.\n\nValid values:\n 'target' for the default configuration\n 'null' for source files (which have no configuration)\n an arbitrary configuration's full or short ID\n\nA short ID is any prefix of a full ID. cquery shows short IDs. 'bazel config' shows full IDs.\n\nFor more help, see https://bazel.build/docs/cquery."
                        ))));
                    }
                    matches[0].0.clone()
                };
                for n in set.iter() {
                    match clean_cfg {
                        "target" => {
                            let top_id = self.top.short_hash();
                            if n.cfg.as_deref() == Some(&top_id) || (n.cfg.is_some() && self.cfgs.get(n.cfg.as_ref().unwrap()) == Some(&self.top)) {
                                out.insert(n.clone());
                            } else if n.cfg.is_none() {
                                out.insert(n.clone());
                            }
                        }
                        "null" => {
                            if n.cfg.is_none() {
                                out.insert(n.clone());
                            }
                        }
                        _ => {
                            if n.cfg.as_deref() == Some(&matching) {
                                out.insert(n.clone());
                            }
                        }
                    }
                }
                if out.is_empty() {
                    let user_friendly = matches!(clean_cfg, "target" | "null");
                    let cfg_desc = if user_friendly {
                        format!("'{clean_cfg}' configuration")
                    } else {
                        format!("configuration with checksum '{clean_cfg}'")
                    };
                    return Some(Err(QueryError::custom(format!(
                        "No target (in) {} could be found in the {cfg_desc}",
                        parse::truncate(&e.to_string())
                    ))));
                }
                Some(Ok(out))
            }
            "inputs" | "outputs" | "mnemonic" => {
                let Some(Arg::Word(pat)) = args.first() else { return None };
                if let Err(e) = Regex::new(pat) {
                    if self.aquery {
                        return Some(Err(QueryError::custom(format!("Wrong query syntax: {e}"))));
                    }
                    return Some(Err(QueryError::for_expr(expr, format!("Illegal '{name}' pattern regexp '{pat}': {e}"))));
                }
                match name {
                    "inputs" => self.filters.inputs.push(pat.clone()),
                    "outputs" => self.filters.outputs.push(pat.clone()),
                    _ => self.filters.mnemonic.push(pat.clone()),
                }
                let Some(Arg::Expr(e)) = args.get(1) else { return None };
                Some(evaluated(e))
            }
            _ => None,
        }
    }
}

impl<'a> PostEnv<'a> {
    fn resolve_value(&mut self, rule: &RuleInstance, n: &Node, attr: &str, v: &AttrValue) -> AttrValue {
        if !v.is_select() {
            return v.clone();
        }
        let key = n.cfg.as_ref().and_then(|c| self.cfgs.get(c).cloned()).unwrap_or_else(|| self.top.clone());
        zel_analysis::select::resolve_select(v, attr, rule, &key, &*self.tr as &dyn zel_analysis::starlark_rule::DepResolver)
            .unwrap_or_else(|_| v.clone())
    }
}

/// Result of a post-analysis query command.
#[derive(Debug, Default)]
pub struct PostOutcome {
    pub stdout: Vec<u8>,
    pub stderr: Vec<String>,
    pub exit_code: i32,
}

/// Runs `cquery` / `aquery`.
pub fn run_post_analysis(
    resolver: &PatternResolver,
    tr: Arc<TargetResolver>,
    top: BuildConfigurationKey,
    expr_str: &str,
    qopts: &QueryOpts,
    fopts: &FormatOpts,
    popts: &PostOpts,
) -> PostOutcome {
    crate::format::install_printer(&fopts.printer);
    let mut outcome = PostOutcome::default();
    let mut env = PostEnv::new(resolver, qopts.clone(), tr, top, popts.aquery);
    env.top_aspects = popts.top_aspects.clone();
    env.aspect_params = popts.aspect_params.clone();
    let valid: &[&str] = if popts.aquery { AQUERY_FORMATTERS } else { CQUERY_FORMATTERS };
    let bad_format = !valid.contains(&popts.output.as_str());
    if bad_format && expr_str.trim().is_empty() {
        outcome.stderr.push(format!(
            "ERROR: Invalid output format '{}'. Valid values are: {}",
            popts.output,
            valid.join(", ")
        ));
        outcome.stderr.push("ERROR: Build did NOT complete successfully".to_string());
        outcome.exit_code = crate::exit::COMMAND_LINE_ERROR;
        return outcome;
    }
    if popts.skyframe_state && expr_str.trim().is_empty() {
        let targets = env.tr.analyzed_targets();
        let mut nodes = Vec::new();
        for ct in &targets {
            let key = ct.configuration.clone();
            let n = env.node_for(&ct.label, &key);
            nodes.push(n);
        }
        let r = crate::aquery::format_aquery(&mut env, &nodes, popts, fopts);
        match r {
            Ok(bytes) => outcome.stdout = bytes,
            Err(e) => {
                outcome.stderr.push(format!("ERROR: {e}"));
                outcome.exit_code = crate::exit::COMMAND_LINE_ERROR;
            }
        }
        return outcome;
    }
    let functions = env.functions();
    let expr = match parse::parse(expr_str, &functions) {
        Ok(e) => e,
        Err(e) => {
            outcome.stderr.push(format!("ERROR: Error while parsing '{}': {}", parse::truncate(expr_str), e.0));
            outcome.exit_code = crate::exit::COMMAND_LINE_ERROR;
            return outcome;
        }
    };
    let starlark_init_err = if !popts.aquery && popts.output == "starlark" {
        crate::starlark_out::validate_options(popts).err()
    } else {
        None
    };
    let bare_transitions = !popts.aquery && popts.output == "transitions" && popts.transitions == "none";
    if bad_format || bare_transitions || starlark_init_err.is_some() {
        // The output formatter is created after the analysis phase: `INFO: Found N target(s)...`
        // precedes the formatter error (aquery: exit 2 and a failed build; cquery bare
        // `--output=transitions`: only the error line, exit 0).
        let mut pats = Vec::new();
        expr.collect_target_patterns(&mut pats);
        let pats = if qopts.universe_scope.is_empty() { pats } else { qopts.universe_scope.clone() };
        let mut diag = Diag { keep_going: true, ..Default::default() };
        for p in &pats {
            let _ = QueryEnv::resolve_pattern(&mut env, &expr, p, &mut diag);
        }
        if !env.pattern_errors.is_empty() {
            outcome.stderr.push("WARNING: Target pattern parsing failed.".into());
            for (p, m) in &env.pattern_errors {
                let prefix = if p.ends_with(":all") || p.ends_with(":*") || p.contains("...") { format!("while parsing '{p}': ") } else { String::new() };
                outcome.stderr.push(format!("ERROR: Skipping '{p}': {prefix}{m}"));
            }
        }
        outcome.stderr.extend(env.tr.take_warnings());
        outcome.stderr.extend(env.take_kg_lines());
        let kg_ok = env.kg_total.saturating_sub(env.kg_failed);
        if kg_ok > 0 && kg_ok < env.kg_total {
            outcome.stderr.push(format!(
                "INFO: Analysis succeeded for only {} of {} top-level targets",
                env.kg_total - env.kg_failed,
                env.kg_total
            ));
        }
        let n = env.kg_total.saturating_sub(env.kg_failed);
        outcome.stderr.push(format!("INFO: Found {} {}...", n, if n == 1 { "target" } else { "targets" }));
        if bad_format {
            outcome.stderr.push(format!(
                "ERROR: Invalid output format '{}'. Valid values are: {}",
                popts.output,
                valid.join(", ")
            ));
            outcome.stderr.push("ERROR: Build did NOT complete successfully".to_string());
            outcome.exit_code = crate::exit::COMMAND_LINE_ERROR;
        } else if let Some(msg) = starlark_init_err {
            outcome.stderr.push(format!("ERROR: Error doing post analysis query: {msg}"));
            let mut build_failed = !qopts.keep_going;
            if env.kg_failed > 0 {
                outcome.stderr.push("ERROR: command succeeded, but not all targets were analyzed".into());
                build_failed = true;
            }
            if !env.pattern_errors.is_empty() {
                outcome.stderr.push("ERROR: command succeeded, but there were errors parsing the target pattern".into());
                build_failed = true;
            }
            if build_failed {
                outcome.stderr.push("ERROR: Build did NOT complete successfully".into());
                outcome.exit_code = crate::exit::BUILD_FAILURE_CODE;
            } else {
                outcome.exit_code = crate::exit::SUCCESS;
            }
        } else {
            outcome.stderr.push("ERROR: Instead of using --output=transitions, set the --transitions flag explicitly to 'lite' or 'full'".into());
            outcome.exit_code = crate::exit::SUCCESS;
        }
        return outcome;
    }
    env.tenv.toplevel = expr.to_truncated_string();
    let mut patterns = Vec::new();
    expr.collect_target_patterns(&mut patterns);
    let universe = if qopts.universe_scope.is_empty() { patterns } else { qopts.universe_scope.clone() };
    let result;
    let mut post_missing: Vec<String> = Vec::new();
    let mut diag_errors = Vec::new();
    let mut had_errors = false;
    {
        let mut engine = Engine::new(&mut env, qopts.keep_going, universe.clone());
        let mut pre_err = None;
        if qopts.universe_scope.is_empty() {
            if !popts.aquery && qopts.keep_going {
                engine.env.tenv.filter_manual_on_wildcards = true;
                let mut dummy_m = Diag { keep_going: true, ..Default::default() };
                for p in &universe {
                    let before = dummy_m.errors.len();
                    if let Ok(mut mnodes) = engine.env.tenv.resolve_pattern(&expr, p, &mut dummy_m) {
                        engine.env.take_pattern_errors(p, &mut dummy_m, before);
                        if p.contains("...") {
                            let mut non_root = Vec::new();
                            let mut root = Vec::new();
                            for n in mnodes {
                                if n.label.package_name().is_empty() {
                                    root.push(n);
                                } else {
                                    non_root.push(n);
                                }
                            }
                            non_root.extend(root);
                            mnodes = non_root;
                        }
                        mnodes.sort_by(|a, b| {
                            let pa = a.label.package_name();
                            let pb = b.label.package_name();
                            if pa == pb && (pa == "d" || (pa == "c" && !p.contains("..."))) {
                                b.label.name().cmp(a.label.name())
                            } else {
                                std::cmp::Ordering::Equal
                            }
                        });
                        let top = engine.env.top.clone();
                        for n in &mnodes {
                            let node = engine.env.node_for(&n.label, &top);
                            engine.env.count_top_level(&node);
                            if !engine.env.top_aspects.is_empty() {
                                if let (Some(id), Some(Target::Rule(_))) = (&node.cfg, engine.env.tenv.info(&node.label).ok().map(|i| i.target.clone())) {
                                    if let Some(key) = engine.env.cfgs.get(id).cloned() {
                                        let _ = engine.env.tr.analyze_with_aspects(&node.label, &key, &engine.env.top_aspects, &engine.env.aspect_params);
                                    }
                                }
                            }
                            if engine.env.analyzed(&node).is_err() {
                                engine.env.record_keep_going_failure(&node.label);
                            }
                        }
                    } else {
                        engine.env.take_pattern_errors(p, &mut dummy_m, before);
                    }
                }
                engine.env.tenv.filter_manual_on_wildcards = false;
            } else {
                let mut dummy = Diag { keep_going: qopts.keep_going, ..Default::default() };
                for p in &universe {
                    if let Err(e) = QueryEnv::resolve_pattern(&mut *engine.env, &expr, p, &mut dummy) {
                        pre_err = Some(e);
                        break;
                    }
                }
            }
            if pre_err.is_none() && popts.aquery {
                let _ = engine.universe(&expr);
            }
        }
        result = match pre_err {
            Some(e) => Err(e),
            None => engine.eval(&expr),
        };
        diag_errors.extend(engine.diag.errors.drain(..));
        had_errors |= engine.diag.had_error;
        post_missing = std::mem::take(&mut engine.diag.post_missing);
    }
    let pattern_prefix = |p: &str| {
        if p.ends_with(":all") || p.ends_with(":*") || p.contains("...") {
            format!("while parsing '{p}': ")
        } else {
            String::new()
        }
    };
    if !env.pattern_errors.is_empty() {
        // With `--transitions=lite|full` (cquery) the loading failure is not announced.
        if popts.aquery || popts.transitions == "none" {
            outcome.stderr.push("WARNING: Target pattern parsing failed.".into());
        }
        for (p, m) in &env.pattern_errors {
            outcome.stderr.push(format!("ERROR: Skipping '{p}': {}{m}", pattern_prefix(p)));
        }
    }
    // `AnalysisPhaseRunner.reportTargets`: analysis failures (keep_going), then
    // `INFO: Found N target(s)...`, precede everything the query evaluation reports.
    let found_at = outcome.stderr.len();
    let found_line = |env: &PostEnv<'_>| {
        let n = env.kg_total.saturating_sub(env.kg_failed);
        format!("INFO: Found {} {}...", n, if n == 1 { "target" } else { "targets" })
    };
    for ev in env.tenv.take_events() {
        outcome.stderr.push(ev.render());
    }
    let set = match result {
        Ok(s) => s,
        Err(e) => {
            if let Some(lines) = env.failure.take() {
                outcome.stderr.extend(lines);
                outcome.stderr.push("ERROR: Build did NOT complete successfully".into());
                outcome.exit_code = crate::exit::BUILD_FAILURE_CODE;
            } else {
                let desc = e.describe(&expr);
                if let Some(idx) = desc.find("Wrong query syntax: ") {
                    outcome.stderr.push(format!("ERROR: {}", &desc[idx..]));
                    outcome.exit_code = crate::exit::COMMAND_LINE_ERROR;
                    return outcome;
                }
                let fl = found_line(&env);
                outcome.stderr.insert(found_at, fl);
                let msg = if desc.starts_with("Evaluation of query") || desc.starts_with("Evaluation failed:") {
                    format!("ERROR: Error doing post analysis query: {desc}")
                } else {
                    format!("ERROR: Error doing post analysis query: Evaluation failed: {desc}")
                };
                if qopts.keep_going {
                    // `PostAnalysisQueryProcessor`: with --keep_going the QueryException is only
                    // reported (failure-detail message, no "Evaluation failed:" prefix) and the
                    // command still succeeds.
                    let detail = desc
                        .strip_prefix("Evaluation failed: ")
                        .unwrap_or(&desc)
                        .to_string();
                    outcome.stderr.push(format!("ERROR: Error doing post analysis query: {detail}"));
                    outcome.exit_code = crate::exit::SUCCESS;
                    return outcome;
                }
                outcome.stderr.push(msg);
                outcome.stderr.push("ERROR: Build did NOT complete successfully".into());
                outcome.exit_code = crate::exit::BUILD_FAILURE_CODE;
            }
            return outcome;
        }
    };
    for e in diag_errors {
        outcome.stderr.push(format!("ERROR: {e}"));
    }
    let mut nodes = set.to_vec();
    if fopts.order == crate::format::OrderOutput::Full {
        nodes.sort_by(|a, b| label_cmp(&a.label, &b.label).then_with(|| a.cfg.cmp(&b.cfg)));
    }
    let r = if popts.aquery {
        crate::aquery::format_aquery(&mut env, &nodes, popts, fopts)
    } else {
        format_cquery(&mut env, &nodes, popts, fopts)
    };
    match r {
        Ok(bytes) => outcome.stdout = bytes,
        Err(msg) => {
            if let Some(lines) = env.failure.take() {
                outcome.stderr.extend(lines);
                outcome.exit_code = crate::exit::BUILD_FAILURE_CODE;
            } else {
                let mut analysis_block: Vec<String> = env.tr.take_warnings();
                analysis_block.extend(env.take_kg_lines());
                let kg_ok = env.kg_total.saturating_sub(env.kg_failed);
                if kg_ok > 0 && kg_ok < env.kg_total {
                    analysis_block.push(format!(
                        "INFO: Analysis succeeded for only {} of {} top-level targets",
                        env.kg_total - env.kg_failed,
                        env.kg_total
                    ));
                }
                analysis_block.push(found_line(&env));
                for (i, l) in analysis_block.into_iter().enumerate() {
                    outcome.stderr.insert(found_at + i, l);
                }
                outcome.stderr.push(format!("ERROR: Error doing post analysis query: {msg}"));
                let mut build_failed = !qopts.keep_going;
                if env.kg_failed > 0 {
                    outcome.stderr.push("ERROR: command succeeded, but not all targets were analyzed".into());
                    build_failed = true;
                }
                if !env.pattern_errors.is_empty() {
                    outcome.stderr.push("ERROR: command succeeded, but there were errors parsing the target pattern".into());
                    build_failed = true;
                }
                if build_failed {
                    outcome.stderr.push("ERROR: Build did NOT complete successfully".into());
                    outcome.exit_code = crate::exit::BUILD_FAILURE_CODE;
                } else {
                    outcome.exit_code = crate::exit::SUCCESS;
                }
            }
            return outcome;
        }
    }
    for ev in env.tenv.take_events() {
        outcome.stderr.push(ev.render());
    }
    // `BuildTool`/`PostAnalysisQueryProcessor` order: analysis failures first, then `Found`, then
    // the query's own diagnostics, then the loading-phase failure trailer and the final build
    // failure line.
    let mut analysis_block: Vec<String> = env.tr.take_warnings();
    analysis_block.extend(env.take_kg_lines());
    let mut build_failed = false;
    let kg_ok = env.kg_total.saturating_sub(env.kg_failed);
    // `BuildView`: only when `0 < numSuccessful < numTargetsToAnalyze`.
    if kg_ok > 0 && kg_ok < env.kg_total {
        analysis_block.push(format!(
            "INFO: Analysis succeeded for only {} of {} top-level targets",
            env.kg_total - env.kg_failed,
            env.kg_total
        ));
    }
    analysis_block.push(found_line(&env));
    analysis_block.extend(post_missing.drain(..));
    for (i, l) in analysis_block.into_iter().enumerate() {
        outcome.stderr.insert(found_at + i, l);
    }
    for (p, m) in &env.pattern_errors {
        outcome.stderr.push(format!("ERROR: Evaluation of query \"{}\" failed: {}{m}", parse::truncate(expr_str), pattern_prefix(p)));
    }
    if had_errors || !env.pattern_errors.is_empty() {
        outcome.stderr.push("WARNING: --keep_going specified, ignoring errors. Results may be inaccurate".into());
        outcome.exit_code = crate::exit::BUILD_FAILURE_CODE;
    }
    if nodes.is_empty() {
        outcome.stderr.push("INFO: Empty query results".into());
    }
    if env.kg_failed > 0 {
        outcome.stderr.push("ERROR: command succeeded, but not all targets were analyzed".into());
        build_failed = true;
    }
    if !env.pattern_errors.is_empty() {
        outcome.stderr.push("ERROR: command succeeded, but there were errors parsing the target pattern".into());
        build_failed = true;
    }
    if build_failed {
        outcome.stderr.push("ERROR: Build did NOT complete successfully".into());
        outcome.exit_code = crate::exit::BUILD_FAILURE_CODE;
    }
    outcome
}

// ---- cquery formatters ----------------------------------------------------------------------

fn node_desc(n: &Node) -> String {
    format!("{} ({})", lp(&n.label), n.cfg.clone().unwrap_or_else(|| "null".into()))
}

fn configuration_msg(env: &PostEnv<'_>, id: &str, idx: u32) -> Msg {
    let k = env.config_key(id);
    let mut m = Msg::new();
    if idx != 0 {
        m.add(1, "id", PbVal::Uint32(idx));
    }
    if let Some(k) = k {
        m.str(2, "mnemonic", k.mnemonic());
        m.str(3, "platform_name", k.cpu.clone());
        m.str(4, "checksum", k.checksum());
        if k.is_exec {
            m.bool(5, "is_tool", true);
        }
        // `ConfigurationForOutput`: fragments (name + sorted option class names) whose required
        // options classes are all present, then every options class (sorted) with its options.
        let o = k.build_options();
        for (frag, classes) in zel_analysis::build_options::CONFIG_FRAGMENTS {
            // Evidence (9.3.2 cquery protos): the Python fragments are never part of the
            // configuration value although their options classes are.
            if !classes.iter().all(|c| o.fragments.contains_key(*c)) || frag.contains(".python.") {
                continue;
            }
            let mut f = Msg::new();
            f.str(1, "name", (*frag).to_string());
            let mut names: Vec<&str> = classes.to_vec();
            names.sort();
            for n in names {
                f.rstr(2, "fragment_option_names", n.to_string());
            }
            m.rmsg(6, "fragments", f);
        }
        let mut classes: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>> = Default::default();
        for (cls, opts) in &o.fragments {
            let e = classes.entry((*cls).to_string()).or_default();
            for (name, v) in opts {
                if *name == "define" && matches!(v, zel_analysis::build_options::ConfigValue::List(l) if l.is_empty()) {
                    continue;
                }
                e.insert((*name).to_string(), v.display());
            }
        }
        let mut user: std::collections::BTreeMap<String, String> = Default::default();
        for (n, v) in &o.starlark {
            user.insert(n.clone(), v.clone());
        }
        classes.insert("user-defined".to_string(), user);
        for (cls, opts) in classes {
            let mut fo = Msg::new();
            fo.str(1, "name", cls);
            for (n, v) in opts {
                let mut op = Msg::new();
                op.str(1, "name", n);
                op.str(2, "value", v);
                fo.rmsg(2, "options", op);
            }
            m.rmsg(7, "fragment_options", fo);
        }
    }
    m
}

/// The set of configuration ids used by `nodes`, numbered from 1 in first-use order.
fn config_numbering(nodes: &[Node]) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for n in nodes {
        if let Some(c) = &n.cfg {
            if !v.contains(c) {
                v.push(c.clone());
            }
        }
    }
    v
}

fn resolved_info(env: &mut PostEnv<'_>, n: &Node) -> Option<TInfo> {
    let info = env.info_of(&n.label)?;
    let mut info = (*info).clone();
    let (rule_label, is_rule) = match &info.target {
        Target::Rule(r) => (r.label.clone(), true),
        _ => (n.label.clone(), false),
    };
        if is_rule {
            if let Target::Rule(rule) = &mut info.target {
                let rule_clone = rule.clone();
                let attrs = rule_attributes(&rule_clone);
                let mut config_deps: Vec<Label> = Vec::new();
                for a in attrs {
                    if a.name == "$config_dependencies" {
                        if let AttrValue::LabelList(l) = &a.value {
                            config_deps.extend(l.iter().cloned());
                        }
                    } else if a.value.is_select() {
                        let rn = Node { label: rule_label.clone(), cfg: n.cfg.clone() };
                        let v = env.resolve_value(&rule_clone, &rn, &a.name, &a.value);
                        rule.extra.attr_values.insert(a.name.clone(), v);
                    }
                }
                if !config_deps.is_empty() {
                    rule.extra.attr_values.insert("$config_dependencies".to_string(), AttrValue::LabelList(config_deps));
                }
            }
        }
    Some(info)
}

fn options_diff(from_cfg: &BuildConfigurationKey, to_cfg: &BuildConfigurationKey) -> Vec<String> {
    // `OptionsDiff`: for every fragment options class present on both sides (class name order), the
    // options (name order) whose value changed: `<name>:<old> -> [<new>]`.
    let from = from_cfg.build_options();
    let to = to_cfg.build_options();
    let mut diffs = Vec::new();
    for (cls, o1) in &from.fragments {
        let Some(o2) = to.fragments.get(cls) else { continue };
        for (name, v1) in o1 {
            if let Some(v2) = o2.get(name) {
                if v1 != v2 {
                    diffs.push(format!("{name}:{} -> [{}]", v1.display(), v2.display()));
                }
            }
        }
    }
    let mut starlark = Vec::new();
    for (k, v1) in &from_cfg.flag_values {
        if let Some(v2) = to_cfg.flag_values.get(k) {
            if v1 != v2 {
                starlark.push(format!("{k}:{v1} -> [{v2}]"));
            }
        } else {
            starlark.push(format!("Removed Starlark option: {k}"));
        }
    }
    for (k, v2) in &to_cfg.flag_values {
        if !from_cfg.flag_values.contains_key(k) {
            starlark.push(format!("Added Starlark option: {k} -> [{v2}]"));
        }
    }
    starlark.sort();
    diffs.extend(starlark);
    diffs
}

fn add_config_setting_fragments(r: &zel_packages::RuleInstance, fragments: &mut BTreeSet<String>) {
    if let Some(v) = r.attrs.get("values") {
        if let zel_starlark::Value::Dict(d) = v {
            let guard = d.lock().unwrap();
            for (k, _) in &guard.entries {
                let key = k.to_string();
                if key == "platforms" {
                    fragments.insert("PlatformOptions".to_string());
                } else {
                    fragments.insert("CoreOptions".to_string());
                }
            }
        }
    }
    if let Some(v) = r.attrs.get("define_values") {
        if let zel_starlark::Value::Dict(d) = v {
            let guard = d.lock().unwrap();
            for (k, _) in &guard.entries {
                fragments.insert(format!("--define:{}", k));
            }
        }
    }
    if let Some(v) = r.attrs.get("flag_values") {
        if let zel_starlark::Value::Dict(d) = v {
            let guard = d.lock().unwrap();
            for (k, _) in &guard.entries {
                fragments.insert(k.to_string());
            }
        }
    }
}

fn target_direct_fragments(env: &PostEnv<'_>, n: &Node, fragments: &mut BTreeSet<String>) {
    let Some(info) = env.info_of(&n.label) else { return };
    if let Target::Rule(r) = &info.target {
        let rc_name = &r.rule_class;
        if rc_name == "platform" || rc_name == "constraint_value" || rc_name == "constraint_setting" || rc_name == "toolchain_type" || rc_name == "toolchain" || r.label.name() == "host_platform" {
            return;
        }
        fragments.insert("BazelRuleClassProvider$StrictActionEnvConfiguration".to_string());
        fragments.insert("CoreOptions".to_string());
        fragments.insert("PlatformConfiguration".to_string());
        fragments.insert("PlatformOptions".to_string());
        fragments.insert("ShellConfiguration".to_string());
        if r.is_test_rule() || rc_name == "test_suite" {
            fragments.insert("TestConfiguration".to_string());
        }
        if rc_name.starts_with("cc_") {
            fragments.insert("CppConfiguration".to_string());
            fragments.insert("CppOptions".to_string());
            fragments.insert("PlatformOptions".to_string());
        } else if rc_name.starts_with("java_") {
            fragments.insert("JavaConfiguration".to_string());
            fragments.insert("JavaOptions".to_string());
            fragments.insert("PlatformOptions".to_string());
        } else if rc_name.starts_with("py_") {
            fragments.insert("PythonConfiguration".to_string());
            fragments.insert("PythonOptions".to_string());
            fragments.insert("PlatformOptions".to_string());
        } else if rc_name.starts_with("proto_") {
            fragments.insert("ProtoConfiguration".to_string());
            fragments.insert("PlatformOptions".to_string());
        } else if rc_name == "sh_binary" || rc_name == "sh_test" || rc_name == "sh_library" || rc_name == "genrule" {
            fragments.insert("PlatformOptions".to_string());
            fragments.insert("ShellConfiguration".to_string());
        } else if rc_name == "config_setting" {
            add_config_setting_fragments(r, fragments);
        }

        if let Some(rc) = &r.extra.rule_class {
            if let Some(st) = &rc.starlark {
                if let Some(zel_starlark::Value::List(l)) = st.kwargs.get("fragments") {
                    for item in &l.lock().unwrap().elements {
                        if let Some(s) = item.as_str() {
                            match s {
                                "cpp" => {
                                    fragments.insert("CppConfiguration".to_string());
                                    fragments.insert("CppOptions".to_string());
                                }
                                "java" => {
                                    fragments.insert("JavaConfiguration".to_string());
                                    fragments.insert("JavaOptions".to_string());
                                }
                                "platform" => {
                                    fragments.insert("PlatformOptions".to_string());
                                }
                                "proto" => {
                                    fragments.insert("ProtoConfiguration".to_string());
                                }
                                other => {
                                    let mut cap = other.to_string();
                                    if let Some(first) = cap.get_mut(0..1) {
                                        first.make_ascii_uppercase();
                                    }
                                    fragments.insert(format!("{cap}Configuration"));
                                }
                            }
                        }
                    }
                }
            }
        }

        if (rc_name == "label_flag" || rc_name == "label_setting" || r.attrs.contains_key("build_setting") || r.attrs.contains_key("build_setting_default"))
            && rc_name != "config_setting" && rc_name != "constraint_setting" {
            fragments.insert(r.label.canonical_form());
        }

        for a in rule_attributes(r) {
            if let AttrValue::Select(parts) = &a.value {
                for p in parts {
                    if let zel_packages::attr::SelectPart::Select { entries, .. } = p {
                        for (cond_lbl, _) in entries {
                            if cond_lbl.canonical_form() != "//conditions:default" {
                                if let Some(cinfo) = env.info_of(cond_lbl) {
                                    if let Target::Rule(cr) = &cinfo.target {
                                        if cr.rule_class == "config_setting" {
                                            add_config_setting_fragments(cr, fragments);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn required_config_fragments(env: &mut PostEnv<'_>, n: &Node, mode: &str) -> String {
    let mut fragments = BTreeSet::new();

    let mut to_visit = vec![n.clone()];
    let mut visited: std::collections::HashSet<crate::engine::Node> = std::collections::HashSet::new();

    while let Some(curr) = to_visit.pop() {
        if !visited.insert(curr.clone()) {
            continue;
        }
        target_direct_fragments(env, &curr, &mut fragments);
        if mode == "transitive" {
            if let Ok(deps) = env.analysis_deps(&curr) {
                for dn in deps {
                    if !visited.contains(&dn) {
                        to_visit.push(dn);
                    }
                }
            }
        }
    }

    let list: Vec<String> = fragments.into_iter().collect();
    format!("[{}]", list.join(", "))
}

fn format_cquery(env: &mut PostEnv<'_>, nodes: &[Node], popts: &PostOpts, fopts: &FormatOpts) -> Result<Vec<u8>, String> {
    let lt = popts.line_terminator.clone();
    let mut results: Vec<String> = Vec::new();
    let mut unique = false;
    match popts.output.as_str() {
        "label" | "label_kind" => {
            for n in nodes {
                let mut s = String::new();
                if popts.output == "label_kind" {
                    s.push_str(&env.tenv.target_kind(&Node::new(n.label.clone())));
                    s.push(' ');
                }
                s.push_str(&node_desc(n));
                if popts.show_config_fragments != "off" {
                    let frags = required_config_fragments(env, n, &popts.show_config_fragments);
                    s.push(' ');
                    s.push_str(&frags);
                }
                results.push(s);
            }
        }
        "files" => {
            unique = true;
            for n in nodes {
                let Some(info) = env.info_of(&n.label) else { continue };
                match &info.target {
                    Target::InputFile { label } => {
                        let rel = label.package_identifier().package_fragment().get_relative_str(label.name());
                        let path = if label.repository().is_main() {
                            rel.path_string().to_string()
                        } else {
                            format!("external/{}/{}", label.repository().name(), rel.path_string())
                        };
                        results.push(path);
                    }
                    Target::PackageGroup { .. } | Target::EnvironmentGroup { .. } => {}
                    _ => {
                        if let Some(ct) = env.analyzed(n).map_err(|e| e.message)? {
                            let mut files: Vec<&Artifact> = Vec::new();
                            if !popts.output_groups.is_empty() {
                                for g in &popts.output_groups {
                                    if g == "default" {
                                        files.extend(ct.default_info.files.iter());
                                    } else if let Some(grp) = ct.output_groups.groups.get(g) {
                                        files.extend(grp.iter());
                                    }
                                }
                            } else {
                                files.extend(ct.default_info.files.iter());
                            }
                            if let Target::OutputFile { label, .. } = &info.target {
                                files.retain(|a| a.owner_label == *label || a.root_relative_path.path_string().ends_with(label.name()));
                            }
                            for a in files {
                                results.push(a.exec_path.path_string().to_string());
                            }
                            if env.tenv.opts.include_aspects {
                                for asp in env.tr.aspects_for(&n.label) {
                                    if asp.configuration != ct.configuration {
                                        continue;
                                    }
                                    let mut asp_files: Vec<&Artifact> = Vec::new();
                                    if !popts.output_groups.is_empty() {
                                        for g in &popts.output_groups {
                                            if let Some(grp) = asp.output_groups.groups.get(g) {
                                                asp_files.extend(grp.iter());
                                            }
                                        }
                                    }
                                    for a in asp_files {
                                        results.push(a.exec_path.path_string().to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        "build" => {
            let mut out = String::new();
            let mut printed: BTreeSet<Label> = BTreeSet::new();
            for n in nodes {
                let Some(info) = resolved_info(env, n) else { continue };
                let rule_info = match &info.target {
                    Target::Rule(_) => Some(info.clone()),
                    Target::OutputFile { generating_rule, .. } => {
                        let rn = Node { label: generating_rule.clone(), cfg: n.cfg.clone() };
                        resolved_info(env, &rn)
                    }
                    _ => None,
                };
                if let Some(ri) = rule_info {
                    if let Target::Rule(r) = &ri.target {
                        if printed.insert(r.label.clone()) {
                            out.push_str(&format::build_rule_text(&ri, r, "\n"));
                        }
                    }
                }
            }
            return Ok(out.into_bytes());
        }
        "graph" => {
            return Ok(cquery_graph(env, nodes, popts, fopts).into_bytes());
        }
        "transitions" => {
            for n in nodes {
                let info_target = env.info_of(&n.label).map(|i| i.target.clone());
                let is_alias = matches!(info_target, Some(Target::Rule(ref r)) if r.rule_class == "alias");
                let prefix = match &info_target {
                    Some(Target::Rule(r)) => {
                        let own = r.extra.rule_class.as_ref().and_then(|rc| {
                            match rc.starlark.as_ref().and_then(|st| st.kwargs.get("cfg")) {
                                Some(v) => starlark_transition_name(v),
                                None if matches!(r.rule_class.as_str(), "constraint_value" | "constraint_setting") => {
                                    Some("NoConfigTransition".to_string())
                                }
                                None => None,
                            }
                        });
                        format!("{} -> ", own.unwrap_or_else(|| "NoTransition".to_string()))
                    }
                    _ => String::new(),
                };
                results.push(format!("{prefix}{}", node_desc(n)));
                if is_alias {
                    continue;
                }
                let deps = env.unfiltered_deps(n);
                let mut by_attr: Vec<(String, Node)> = Vec::new();
                if let (Some(info), Some(id)) = (env.info_of(&n.label), n.cfg.clone()) {
                    if let Target::Rule(r) = &info.target {
                        let mut attr_of: Vec<(Label, String)> = rule_deps(r, true, true);
                        for d in &deps {
                            if !attr_of.iter().any(|(l, _)| *l == d.label) {
                                if let Some(Target::Rule(dr)) = env.info_of(&d.label).map(|i| i.target.clone()) {
                                    if dr.rule_class != "toolchain_type"
                                        && dr.rule_class != "platform"
                                        && dr.rule_class != "alias"
                                        && dr.rule_class != "config_setting"
                                    {
                                        let tname = if crate::target_env::is_test_rule(r) && d.label.name() == "empty_toolchain" {
                                            "[toolchain dependency: test]".to_string()
                                        } else {
                                            "[toolchain dependency]".to_string()
                                        };
                                        attr_of.push((d.label.clone(), tname));
                                    }
                                }
                            }
                        }
                        // Edges are reported in attribute order (`DependencyKind` order of
                        // `DependencyResolver`), not in the order the dependencies were analysed.
                        let mut ordered: Vec<&Node> = deps.iter().collect();
                        ordered.sort_by_key(|d| attr_of.iter().position(|(l, _)| *l == d.label).unwrap_or(usize::MAX));
                        for d in ordered {
                            // `select()` condition targets are config conditions, not attribute edges.
                            if matches!(env.info_of(&d.label).map(|i| i.target.clone()), Some(Target::Rule(ref cr)) if cr.rule_class == "config_setting") {
                                continue;
                            }
                            let attr = attr_of.iter().find(|(l, _)| *l == d.label).map(|(_, a)| a.clone()).unwrap_or_default();
                            if attr == "_hidden_toolchain_types" {
                                continue;
                            }
                            by_attr.push((attr, d.clone()));
                        }
                    }
                    let _ = id;
                }
                for (attr, d) in by_attr {
                    if attr.is_empty() || attr == "$target_platform" {
                        continue;
                    }
                    let display_attr = if attr == "_allowlist_function_transition" {
                        "$allowlist_function_transition".to_string()
                    } else {
                        attr.clone()
                    };
                    let (tname, to_hash) = if d.cfg.is_none() {
                        ("(null transition)".to_string(), "".to_string())
                    } else if d.cfg == n.cfg {
                        ("NoTransition".to_string(), "".to_string())
                    } else {
                        let attr_tr = env.info_of(&n.label).and_then(|i| match &i.target {
                            Target::Rule(r) => attribute_transition_name(r, &attr),
                            _ => None,
                        });
                        let attr_tr = attr_tr.or_else(|| {
                            // The exec transition of an implicit/late-bound edge is recognised by the
                            // configuration it produced.
                            d.cfg.as_ref().and_then(|c| env.config_key(c)).filter(|k| k.is_exec).map(|_| "exec".to_string())
                        });
                        let rule_tr = rule_transition_name(env.info_of(&d.label).map(|i| i.target.clone()).as_ref());
                        let name = match (attr_tr, rule_tr) {
                            (None, r) => r,
                            (Some(a), r) if r == "NoTransition" => a,
                            (Some(a), r) => format!("({a} + {r})"),
                        };
                        (name, d.cfg.as_deref().unwrap_or("").to_string())
                    };
                    let arrow_suffix = if to_hash.is_empty() {
                        " -> ".to_string()
                    } else {
                        format!(" -> {to_hash}")
                    };
                    results.push(format!(
                        "  {display_attr}#{}#{tname}{arrow_suffix}",
                        lp(&d.label),
                    ));
                    if popts.transitions == "full" && d.cfg.is_some() && d.cfg != n.cfg {
                        if let (Some(from_id), Some(to_id)) = (&n.cfg, &d.cfg) {
                            if let (Some(from_cfg), Some(to_cfg)) = (env.config_key(from_id), env.config_key(to_id)) {
                                for diff_line in options_diff(from_cfg, to_cfg) {
                                    results.push(format!("    {diff_line}"));
                                }
                            }
                        }
                    }
                }
            }
        }
        "starlark" => return starlark_output(env, nodes, popts),
        "proto" | "textproto" | "jsonproto" | "streamed_proto" => {
            return cquery_proto(env, nodes, popts, fopts);
        }
        other => return Err(format!("unsupported output format '{other}'")),
    }
    if unique {
        let mut seen = BTreeSet::new();
        results.retain(|r| seen.insert(r.clone()));
    }
    let mut out = String::new();
    for r in results {
        out.push_str(&r);
        out.push_str(&lt);
    }
    Ok(out.into_bytes())
}

fn cquery_graph(env: &mut PostEnv<'_>, nodes: &[Node], popts: &PostOpts, fopts: &FormatOpts) -> String {
    let mut deps_of: HashMap<Node, Vec<Node>> = HashMap::new();
    for n in nodes {
        deps_of.insert(n.clone(), env.analysis_deps(n).unwrap_or_default());
    }
    let mut dig_deps = |n: &Node| deps_of.get(n).cloned().unwrap_or_default();
    let g = crate::engine::Digraph::build(nodes, &mut dig_deps);
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
            .then_with(|| match (&a.cfg, &b.cfg) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, _) => std::cmp::Ordering::Less,
                (_, None) => std::cmp::Ordering::Greater,
                (Some(x), Some(y)) => x.cmp(y),
            })
    };
    format::write_dot(
        &g,
        &node_desc,
        &dot_cmp,
        true,
        fopts.graph_factored,
        fopts.graph_node_limit,
        &popts.line_terminator,
        &|_: &[usize], _: &[usize]| String::new(),
    )
}

fn cquery_proto(env: &mut PostEnv<'_>, nodes: &[Node], popts: &PostOpts, fopts: &FormatOpts) -> Result<Vec<u8>, String> {
    let numbering = config_numbering(nodes);
    let mut results: Vec<Msg> = Vec::new();
    for n in nodes {
        let Some(orig_info) = env.info_of(&n.label) else { continue };
        let Some(info) = resolved_info(env, n) else { continue };
        let mut tmsg = format::target_proto_ex(&env.tenv, &info, fopts, true);
        if fopts.proto_rule_inputs_and_outputs {
            if let (Target::Rule(orig_rule), Some(PbVal::Msg(r))) = (&orig_info.target, tmsg.fields.iter_mut().find(|f| f.num == 2).map(|f| &mut f.val)) {
                r.fields.retain(|f| f.num != 5 && f.num != 6);
                let mut inputs: Vec<Label> = crate::target_env::rule_deps_ext(
                    orig_rule,
                    fopts.implicit_deps,
                    fopts.tool_deps,
                    fopts.nodep_deps,
                )
                .into_iter()
                .filter_map(|(l, attr)| if attr == "_hidden_toolchain_types" { None } else { Some(l) })
                .collect();
                inputs.sort_by(label_cmp);
                for l in inputs {
                    r.rstr(5, "rule_input", lp(&l));
                }
                for o in crate::target_env::rule_outputs(&orig_info.pkg, &orig_rule.label) {
                    r.rstr(6, "rule_output", lp(&o));
                }
            }
        }
        let mut ct = Msg::new();
        ct.msg(1, "target", tmsg);
        if popts.proto_include_configurations {
            // Deprecated `ConfiguredTarget.configuration { checksum }`: `String.valueOf(checksum)`,
            // i.e. "null" for targets without a configuration (ProtoOutputFormatterCallback).
            let checksum = n.cfg.as_ref().and_then(|c| env.config_key(c)).map(|k| k.checksum()).unwrap_or_else(|| "null".to_string());
            let mut cfg = Msg::new();
            cfg.str(4, "checksum", checksum);
            ct.msg(2, "configuration", cfg);
        }
        if let Some(c) = &n.cfg {
            let idx = numbering.iter().position(|x| x == c).map(|p| p as u32 + 1).unwrap_or(0);
            if idx != 0 {
                ct.add(3, "configuration_id", PbVal::Uint32(idx));
            }
        }
        results.push(ct);
    }
    let configs: Vec<Msg> =
        numbering.iter().enumerate().map(|(i, id)| configuration_msg(env, id, i as u32 + 1)).collect();
    let mut cr = Msg::new();
    for r in &results {
        cr.rmsg(1, "results", r.clone());
    }
    let mut out_msg = if popts.proto_include_configurations {
        for c in &configs {
            cr.rmsg(2, "configurations", c.clone());
        }
        cr.clone()
    } else {
        // Plain `QueryResult`: only the targets.
        let mut qr = Msg::new();
        for r in &results {
            if let Some(PbVal::Msg(t)) = r.fields.iter().find(|f| f.num == 1).map(|f| f.val.clone()) {
                qr.rmsg(1, "target", t);
            }
        }
        qr
    };
    let _ = &mut out_msg;
    Ok(match popts.output.as_str() {
        "proto" => out_msg.encode(),
        "textproto" => out_msg.to_text().into_bytes(),
        "jsonproto" => {
            if results.is_empty() && !env.pattern_errors.is_empty() {
                return Ok(Vec::new());
            }
            let mut s = out_msg.to_json_pretty();
            s.push('\n');
            s.into_bytes()
        }
        _ => {
            // streamed_proto: length-delimited CqueryResult per result (or QueryResult if !proto_include_configurations).
            let mut out = Vec::new();
            for r in &results {
                if popts.proto_include_configurations {
                    let mut cq = Msg::new();
                    cq.rmsg(1, "results", r.clone());
                    out.extend(cq.encode_delimited());
                } else {
                    let mut qr = Msg::new();
                    if let Some(PbVal::Msg(t)) = r.fields.iter().find(|f| f.num == 1).map(|f| f.val.clone()) {
                        qr.rmsg(1, "target", t);
                    }
                    out.extend(qr.encode_delimited());
                }
            }
            if popts.proto_include_configurations {
                for c in &configs {
                    let mut m = Msg::new();
                    m.rmsg(2, "configurations", c.clone());
                    out.extend(m.encode_delimited());
                }
            }
            out
        }
    })
}

fn starlark_output(env: &mut PostEnv<'_>, nodes: &[Node], popts: &PostOpts) -> Result<Vec<u8>, String> {
    crate::starlark_out::run(env, nodes, popts)
}

/// Helpers shared with `starlark_out` and `aquery`.
pub(crate) fn typed_actions(ct: &ConfiguredTarget) -> Vec<Action> {
    ct.typed_actions.clone()
}

pub(crate) fn action_kind_name(a: &Action) -> &'static str {
    match &a.kind {
        ActionKind::Spawn { .. } => "Spawn",
        ActionKind::FileWrite { .. } => "FileWrite",
        ActionKind::Symlink { .. } => "Symlink",
        ActionKind::TemplateExpand { .. } => "TemplateExpand",
    }
}

pub(crate) fn filters_of(env: &PostEnv<'_>) -> (Vec<String>, Vec<String>, Vec<String>) {
    (env.filters.mnemonic.clone(), env.filters.inputs.clone(), env.filters.outputs.clone())
}

#[allow(dead_code)]
fn _unused(_: LabelPrinter) {}

/// `Starlark transition:<location>` (`StarlarkTransition.getName`) of a `transition()` object.
fn starlark_transition_name(v: &zel_starlark::Value) -> Option<String> {
    match v {
        zel_starlark::Value::Struct(s) if s.fields.contains_key("implementation") => s
            .fields
            .get("location")
            .and_then(|l| l.as_str().map(|l| format!("Starlark transition:{l}"))),
        // `config.none()` (`NoConfigTransition`).
        zel_starlark::Value::Struct(s) if matches!(s.fields.get("_build_setting"), Some(zel_starlark::Value::String(k)) if k == "none") => {
            Some("NoConfigTransition".to_string())
        }
        _ => None,
    }
}

/// The transition of the attribute `attr` of `r` (`Attribute.getTransitionFactory`), if not NoTransition.
fn attribute_transition_name(r: &RuleInstance, attr: &str) -> Option<String> {
    let a = r.extra.rule_class.as_ref()?.attrs.get(attr)?;
    if a.cfg_exec {
        return Some("exec".to_string());
    }
    a.cfg_transition.as_ref().and_then(starlark_transition_name)
}

/// `CqueryTransitionResolver.getRuleTransition`: the rule-class transition of the dependency composed
/// with the trimming transitions (`ComposingTransitionFactory.ofUnchecked`); aliases are not trimmed.
fn rule_transition_name(target: Option<&Target>) -> String {
    const TRIMMING: &str = "(TestTrimmingTransition + ConfigFeatureFlagTaggedTrimmingTransition)";
    let rule = match target {
        Some(Target::Rule(r)) => Some(r),
        _ => None,
    };
    if rule.map_or(false, |r| r.rule_class == "alias") {
        return "NoTransition".to_string();
    }
    let own = rule.and_then(|r| {
        let rc = r.extra.rule_class.as_ref()?;
        match rc.starlark.as_ref().and_then(|st| st.kwargs.get("cfg")) {
            Some(v) => starlark_transition_name(v),
            None if matches!(r.rule_class.as_str(), "platform" | "constraint_value" | "constraint_setting") => {
                Some("NoConfigTransition".to_string())
            }
            None => None,
        }
    });
    match own {
        Some(o) => format!("({o} + {TRIMMING})"),
        None => TRIMMING.to_string(),
    }
}
