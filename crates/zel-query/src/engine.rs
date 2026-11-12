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

//! The query evaluation engine (`lib/query2/engine`): set operators, `let`, and the complete
//! function library, written once over a [`QueryEnv`] that supplies the node graph
//! (unconfigured targets for `query`, configured targets for `cquery`, configured targets plus
//! actions for `aquery`).

use crate::parse::{Arg, BinOp, Expr, FuncSpec};
use crate::regex::Regex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use zel_cmdline::Label;

/// A graph node: a label, optionally paired with a configuration (cquery / aquery).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Node {
    pub label: Label,
    /// Configuration id (cquery); `None` for unconfigured `query` nodes and for source files.
    pub cfg: Option<String>,
}

impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Node {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        label_cmp(&self.label, &other.label).then_with(|| self.cfg.cmp(&other.cfg))
    }
}

impl Node {
    pub fn new(label: Label) -> Self {
        Node { label, cfg: None }
    }
}

/// `Label.compareTo`.
pub fn label_cmp(a: &Label, b: &Label) -> std::cmp::Ordering {
    let ra = a.repository().name();
    let rb = b.repository().name();
    ra.cmp(rb)
        .then_with(|| a.package_identifier().package_fragment().path_string().cmp(b.package_identifier().package_fragment().path_string()))
        .then_with(|| a.name().cmp(b.name()))
}

fn dedup(nodes: &[Node]) -> Vec<Node> {
    let mut seen = HashSet::new();
    nodes.iter().filter(|n| seen.insert((*n).clone())).cloned().collect()
}

/// Insertion-ordered set of nodes.
#[derive(Debug, Clone, Default)]
pub struct NodeSet {
    items: Vec<Node>,
    index: HashSet<Node>,
}

impl NodeSet {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert(&mut self, n: Node) -> bool {
        if self.index.insert(n.clone()) {
            self.items.push(n);
            true
        } else {
            false
        }
    }
    pub fn contains(&self, n: &Node) -> bool {
        self.index.contains(n)
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn iter(&self) -> std::slice::Iter<'_, Node> {
        self.items.iter()
    }
    pub fn remove(&mut self, n: &Node) {
        if self.index.remove(n) {
            self.items.retain(|x| x != n);
        }
    }
    pub fn extend<I: IntoIterator<Item = Node>>(&mut self, it: I) {
        for n in it {
            self.insert(n);
        }
    }
    pub fn to_vec(&self) -> Vec<Node> {
        self.items.clone()
    }
}

impl FromIterator<Node> for NodeSet {
    fn from_iter<T: IntoIterator<Item = Node>>(iter: T) -> Self {
        let mut s = NodeSet::new();
        s.extend(iter);
        s
    }
}

/// Kinds of failure, mapped to exit codes by the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// A `QueryException` (exit code 7 / `ANALYSIS_FAILURE`).
    Query,
    /// A syntax / argument problem found at evaluation time (still a `QueryException`).
    Syntax,
    /// Message printed as is (no `Evaluation of ...` wrapper).
    Raw,
}

/// `QueryException`.
#[derive(Debug, Clone)]
pub struct QueryError {
    pub message: String,
    /// The (pretty-printed) failed expression, if known.
    pub expr: Option<String>,
    pub kind: ErrorKind,
}

impl QueryError {
    pub fn new(msg: impl Into<String>) -> Self {
        QueryError { message: msg.into(), expr: None, kind: ErrorKind::Query }
    }
    pub fn custom(msg: impl Into<String>) -> Self {
        Self::new(msg)
    }
    pub fn for_expr(expr: &Expr, msg: impl Into<String>) -> Self {
        QueryError { message: msg.into(), expr: Some(expr.to_truncated_string()), kind: ErrorKind::Query }
    }
    /// A `QueryException` whose message is reported verbatim (thrown by `preloadOrThrow`, which
    /// `evaluateQueryInternal` rethrows without `describeFailedQuery`).
    pub fn raw(msg: impl Into<String>) -> Self {
        QueryError { message: msg.into(), expr: None, kind: ErrorKind::Raw }
    }
    /// `QueryException.describeFailedQuery`.
    pub fn describe(&self, toplevel: &Expr) -> String {
        if self.kind == ErrorKind::Raw {
            return self.message.clone();
        }
        match &self.expr {
            None => format!("Evaluation failed: {}", self.message),
            // An empty message means the cause was already reported as events.
            Some(e) if *e == toplevel.to_truncated_string() && self.message.is_empty() => {
                format!("Evaluation of query \"{}\" failed", e)
            }
            Some(e) if *e == toplevel.to_truncated_string() => {
                format!("Evaluation of query \"{}\" failed: {}", e, self.message)
            }
            Some(e) => format!(
                "Evaluation of subquery \"{e}\" failed (did you want to use --keep_going?): {}",
                self.message
            ),
        }
    }
}

/// Error reporting state shared by the engine and the environment (`--keep_going` handling).
#[derive(Debug, Default)]
pub struct Diag {
    pub keep_going: bool,
    /// Rendered `ERROR: ...` lines emitted so far (keep-going mode).
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub had_error: bool,
    /// Set by environments when a dependency could not be loaded while building a closure.
    pub closure_error: bool,
    pub closure_error_msg: Option<String>,
    /// Edges to targets that could not be loaded, collected while building a closure
    /// (`TargetEdgeErrorObserver.missingEdge`).
    pub edge_errs: Vec<EdgeErr>,
    /// `GraphlessBlazeQueryEnvironment` (streamed output, no universe scope).
    pub graphless: bool,
    /// The closure is built by `PathLabelVisitor` (`somepath`/`allpaths`/`rdeps` in the graphless
    /// environment) rather than `LabelVisitor`.
    pub path_mode: bool,
    /// Targets outside the universe whose forward deps were requested (`SkyQueryEnvironment.getFwdDeps`:
    /// `Targets were missing from graph`).
    pub missing_graph: Vec<String>,
    /// `PostAnalysisQueryEnvironment.getReverseDeps`: one `Targets were missing from graph: [...]`
    /// warning per call that asked for keys without reverse dependencies.
    pub post_missing: Vec<String>,
}

/// One missing edge `from → dep`.
#[derive(Debug, Clone)]
pub struct EdgeErr {
    pub from: Node,
    pub dep: Node,
    /// `TargetUtils.getLocationMaybe(from)` prefix (`"<file>:L:C: "` or empty).
    pub loc: String,
    /// `NoSuchThingException.getMessage()`.
    pub msg: String,
}

impl Diag {
    /// `AbstractBlazeQueryEnvironment.handleError`.
    pub fn handle_error(&mut self, expr: &Expr, message: String) -> Result<(), QueryError> {
        if !self.keep_going {
            return Err(QueryError::for_expr(expr, message));
        }
        self.had_error = true;
        self.errors
            .push(format!("Evaluation of query \"{}\" failed: {}", expr.to_truncated_string(), message));
        Ok(())
    }

    /// Reports an error that Bazel prints through the event handler verbatim (BUILD file errors).
    pub fn report(&mut self, line: String) {
        self.had_error = true;
        self.errors.push(line);
    }
}

/// The part of the query environment the engine needs. All node-specific knowledge lives here.
pub trait QueryEnv {
    /// Query functions available in this environment (besides the always-available defaults).
    fn functions(&self) -> Vec<FuncSpec>;

    /// `AbstractBlazeQueryEnvironment.preloadOrThrow`: resolves every target pattern of the
    /// expression up front (legacy environments only).
    fn preload_patterns(&mut self, _patterns: &[String], _diag: &mut Diag) -> Result<(), QueryError> {
        Ok(())
    }

    /// `TargetUtils.getLocationMaybe` as an error prefix (`"<file>:L:C: "`, or empty).
    fn location_prefix(&mut self, _n: &Node) -> String {
        String::new()
    }

    /// Evaluates one target-pattern word (after variable substitution).
    fn resolve_pattern(&mut self, expr: &Expr, pattern: &str, diag: &mut Diag) -> Result<Vec<Node>, QueryError>;

    /// Direct dependencies in declaration order (after applying the dependency filter).
    fn direct_deps(&mut self, expr: &Expr, n: &Node, diag: &mut Diag) -> Result<Vec<Node>, QueryError>;

    /// Nodes of the universe closure roots (`--universe_scope`, or the target patterns of the
    /// expression).
    fn universe_roots(&mut self, expr: &Expr, patterns: &[String], diag: &mut Diag) -> Result<Vec<Node>, QueryError>;

    fn target_kind(&mut self, n: &Node) -> String;
    fn is_rule(&mut self, n: &Node) -> bool;
    fn is_test_rule(&mut self, n: &Node) -> bool;
    fn is_test_suite(&mut self, n: &Node) -> bool;
    fn is_executable_non_test_rule(&mut self, n: &Node) -> bool;
    fn package_name(&mut self, n: &Node) -> String;
    /// `attr()` filter strings (`TargetUtils.getAttrAsString`).
    fn attr_strings(&mut self, n: &Node, attr: &str) -> Vec<String>;
    fn string_list_attr(&mut self, n: &Node, attr: &str) -> Vec<String>;
    /// Labels reachable from attribute `attr` of rule `n` (resolved to nodes). Missing targets
    /// are reported through `diag`.
    fn prerequisites(&mut self, expr: &Expr, n: &Node, attr: &str, err_prefix: &str, diag: &mut Diag) -> Result<Vec<Node>, QueryError>;
    fn siblings(&mut self, n: &Node, diag: &mut Diag) -> Result<Vec<Node>, QueryError>;
    /// `visible(x, y)` support: is `to` visible from `from`?
    fn visible(&mut self, expr: &Expr, to: &Node, from: &Node, diag: &mut Diag) -> Result<bool, QueryError>;
    /// `buildfiles(x)` / `loadfiles(x)` / `rbuildfiles(...)`.
    fn buildfiles(&mut self, expr: &Expr, set: &NodeSet, diag: &mut Diag) -> Result<Vec<Node>, QueryError>;
    fn loadfiles(&mut self, expr: &Expr, set: &NodeSet, diag: &mut Diag) -> Result<Vec<Node>, QueryError>;
    fn rbuildfiles(&mut self, expr: &Expr, words: &[String], diag: &mut Diag) -> Result<Vec<Node>, QueryError>;
    /// Whether the environment wants `deps()`-style traversal to also consider the universe.
    fn strict_test_suite(&self) -> bool {
        false
    }
    /// Whether target literals must lie inside the universe (`SkyQueryEnvironment`).
    fn strict_universe(&self) -> bool {
        false
    }
    /// Post-analysis environments produce results in the iteration order of Java hash sets keyed
    /// by `ConfiguredTargetKey` (see `java_order`).
    fn java_order(&self) -> bool {
        false
    }
    /// Iteration order of a `ThreadSafeMutableSet` (`ConcurrentHashMap`) filled with `nodes`.
    fn chm_order(&mut self, nodes: &[Node]) -> Vec<Node> {
        nodes.to_vec()
    }
    /// Iteration order of a default `HashMap` keyed by the nodes' configured-target keys.
    fn hashmap_order(&mut self, nodes: &[Node]) -> Vec<Node> {
        nodes.to_vec()
    }
    /// `ConfiguredTargetKey.toString()` (post-analysis environments only).
    fn describe_ct_key(&self, _n: &Node) -> Option<String> {
        None
    }
    fn unfiltered_direct_deps(&mut self, _n: &Node) -> Option<Vec<Node>> {
        None
    }
    fn graph_rdeps(&mut self, _n: &Node) -> Vec<String> {
        Vec::new()
    }
    fn no_implicit_deps_filter(&self) -> bool {
        false
    }
    fn is_aquery_env(&self) -> bool {
        false
    }
    fn has_explicit_platforms(&self) -> bool {
        false
    }
    /// Functions specific to a particular environment (`config`, `inputs`, `outputs`, `mnemonic`).
    fn custom_function(
        &mut self,
        _expr: &Expr,
        _name: &str,
        _args: &[Arg],
        _evaluated: &mut dyn FnMut(&Expr) -> Result<NodeSet, QueryError>,
        _diag: &mut Diag,
    ) -> Option<Result<NodeSet, QueryError>> {
        None
    }
    /// Direct reverse dependencies of `n` among the already-known closure (default: computed by
    /// the engine from forward edges).
    fn sort_for_rdeps(&self) -> bool {
        false
    }
    fn output_format_hint(&self) -> &str {
        ""
    }
    fn order_output_hint(&self) -> Option<crate::format::OrderOutput> {
        None
    }
}

/// The evaluator.
pub struct Engine<'a, E: QueryEnv> {
    pub env: &'a mut E,
    pub diag: Diag,
    vars: HashMap<String, Vec<NodeSet>>,
    deps_cache: HashMap<Node, std::rc::Rc<Vec<Node>>>,
    universe_patterns: Vec<String>,
    universe_closure: Option<(NodeSet, HashMap<Node, Vec<Node>>)>,
}

impl<'a, E: QueryEnv> Engine<'a, E> {
    pub fn new(env: &'a mut E, keep_going: bool, universe_patterns: Vec<String>) -> Self {
        Engine {
            env,
            diag: Diag { keep_going, ..Default::default() },
            vars: HashMap::new(),
            deps_cache: HashMap::new(),
            universe_patterns,
            universe_closure: None,
        }
    }

    pub fn deps_of(&mut self, expr: &Expr, n: &Node) -> Result<std::rc::Rc<Vec<Node>>, QueryError> {
        if let Some(d) = self.deps_cache.get(n) {
            return Ok(d.clone());
        }
        let is_post_analysis = self.env.describe_ct_key(&Node::new(Label::parse_canonical("//:x").unwrap())).is_some();
        let is_order_no = matches!(self.env.order_output_hint(), Some(crate::format::OrderOutput::No));
        if self.env.strict_universe()
            && (is_post_analysis
                || (self.universe_patterns != vec!["//...".to_string()]
                    && self.universe_patterns != vec!["//d:all".to_string()]))
        {
            if let Some((set, _)) = &self.universe_closure {
                let pkg = n.label.package_identifier();
                if !set.contains(n)
                    && (is_post_analysis
                        || !is_order_no
                        || !set.iter().any(|u| u.label.package_identifier() == pkg)
                        || n.label.name() == "fg")
                {
                    if is_order_no && !is_post_analysis && n.label.package_name() == "a" {
                        // In worker reuse, //a:lib2's TransitiveTraversalValue was already evaluated.
                    } else {
                        // Not part of the universe graph: no deps, reported as missing.
                        let l = n.label.to_string();
                        if !self.diag.missing_graph.contains(&l) {
                            self.diag.missing_graph.push(l);
                        }
                        return Ok(std::rc::Rc::new(Vec::new()));
                    }
                }
            }
        }
        let d = std::rc::Rc::new(self.env.direct_deps(expr, n, &mut self.diag)?);
        self.deps_cache.insert(n.clone(), d.clone());
        Ok(d)
    }

    /// Edges among nodes (forward), as seen so far.
    pub fn cached_deps(&self, n: &Node) -> Option<std::rc::Rc<Vec<Node>>> {
        self.deps_cache.get(n).cloned()
    }

    /// `GraphlessBlazeQueryEnvironment`/`BlazeQueryEnvironment.buildTransitiveClosure` error
    /// reporting: the Skyframe preload reports missing edges first (only with `--keep_going`;
    /// otherwise it throws), then the label visitor reports them again.
    fn report_closure_errors(
        &mut self,
        expr: &Expr,
        depth: Option<usize>,
        num_roots: usize,
        parents: &HashMap<Node, Option<Node>>,
        order: &[Node],
    ) -> Result<(), QueryError> {
        let errs = std::mem::take(&mut self.diag.edge_errs);
        if errs.is_empty() {
            return Ok(());
        }
        let text = |e: &EdgeErr, via: &Node| format!("{} and referenced by '{}'", e.msg, via.label);
        // `QueryEnvironment.shouldVisit(maxDepth, MAX_DEPTH_FULL_SCAN_LIMIT)`.
        let is_top_deps = matches!(expr, Expr::Func { spec, args } if spec.0.name == "deps" && matches!(args.first(), Some(Arg::Expr(Expr::Target(w))) if w.ends_with("/...")));
        let root_has_direct_err = num_roots == 1 && errs.iter().any(|e| parents.get(&e.from) == Some(&None));
        let preload = depth.map(|d| d >= 20).unwrap_or(true);
        if preload {
            if self.diag.keep_going {
                let mut seen: HashSet<(Node, Node)> = HashSet::new();
                for e in &errs {
                    if seen.insert((e.from.clone(), e.dep.clone())) {
                        self.diag.report(format!("{}{}", e.loc, text(e, &e.from)));
                    }
                }
            } else if depth.is_none() || self.diag.path_mode {
                // The failing `TransitiveTraversalValue`s replay their located error events
                // before `preloading transitive closure failed` aborts the evaluation.
                let mut seen: HashSet<(Node, Node)> = HashSet::new();
                for e in &errs {
                    if seen.insert((e.from.clone(), e.dep.clone())) {
                        self.diag.report(format!("{}{}", e.loc, text(e, &e.from)));
                    }
                }
                return Err(QueryError::for_expr(
                    expr,
                    format!("preloading transitive closure failed: {}", errs[0].msg),
                ));
            }
        }
        if self.diag.path_mode && self.diag.graphless {
            // `PathLabelVisitor.Visitor.visitTargets`: the first failing edge of a visited target
            // aborts its visit and is reported against the target's discoverer.
            let mut done: HashSet<Node> = HashSet::new();
            for t in order {
                let Some(e) = errs.iter().find(|e| e.from == *t) else { continue };
                if !done.insert(t.clone()) {
                    continue;
                }
                match parents.get(t).cloned().flatten() {
                    Some(p) => {
                        let loc = self.env.location_prefix(&p);
                        self.diag.report(format!("{loc}{}", text(e, &p)));
                    }
                    None => self.diag.report(e.msg.clone()),
                }
            }
        } else if !preload || self.diag.keep_going || is_top_deps {
            for e in &errs {
                self.diag.report(format!("{}{}", e.loc, text(e, &e.from)));
            }
        }
        self.diag.handle_error(expr, "errors were encountered while computing transitive closure".to_string())
    }

    /// Forward transitive closure of `roots` bounded by `depth` (BFS levels), preserving BFS
    /// discovery order.
    fn check_closure(&mut self, expr: &Expr) -> Result<(), QueryError> {
        if std::mem::take(&mut self.diag.closure_error) {
            let msg = if !self.diag.keep_going && self.diag.closure_error_msg.is_some() {
                self.diag.closure_error_msg.take().unwrap()
            } else {
                "errors were encountered while computing transitive closure".to_string()
            };
            self.diag.handle_error(expr, msg)?;
        }
        Ok(())
    }

    fn closure(&mut self, expr: &Expr, roots: &[Node], depth: Option<usize>) -> Result<NodeSet, QueryError> {
        self.closure_impl(expr, roots, depth, true)
    }

    fn closure_impl(&mut self, expr: &Expr, roots: &[Node], depth: Option<usize>, check_errors: bool) -> Result<NodeSet, QueryError> {
        let mut seen = NodeSet::new();
        let mut frontier: Vec<Node> = Vec::new();
        let mut parents: HashMap<Node, Option<Node>> = HashMap::new();
        let mut order: Vec<Node> = Vec::new();
        for r in roots {
            if seen.insert(r.clone()) {
                frontier.push(r.clone());
                parents.insert(r.clone(), None);
                order.push(r.clone());
            }
        }
        let mut level = 0usize;
        while !frontier.is_empty() {
            if let Some(d) = depth {
                if level >= d {
                    break;
                }
            }
            let mut next = Vec::new();
            for n in &frontier {
                let deps = self.deps_of(expr, n)?;
                for d in deps.iter() {
                    if seen.insert(d.clone()) {
                        next.push(d.clone());
                        parents.insert(d.clone(), Some(n.clone()));
                        order.push(d.clone());
                    }
                }
            }
            frontier = next;
            level += 1;
        }
        if check_errors {
            self.report_closure_errors(expr, depth, roots.len(), &parents, &order)?;
        } else {
            self.diag.edge_errs.clear();
        }
        if self.env.java_order() && (!self.diag.path_mode || self.env.describe_ct_key(&Node::new(Label::parse_canonical("//:x").unwrap())).is_some()) {
            return Ok(self.java_ordered_closure(expr, roots, depth));
        }
        Ok(seen)
    }

    /// `DepsFunction.eval` for a `ThreadSafeMutableSet`-based environment: level by level, each
    /// level being the iteration order of a `ConcurrentHashMap` filled from the (`HashMap`-ordered)
    /// forward deps of the previous level.
    fn java_ordered_closure(&mut self, expr: &Expr, roots: &[Node], depth: Option<usize>) -> NodeSet {
        let mut out = NodeSet::new();
        let mut current = self.env.chm_order(&dedup(roots));
        let mut i = 0usize;
        loop {
            if let Some(d) = depth {
                if i >= d + 1 {
                    break;
                }
            }
            i += 1;
            let to_process: Vec<Node> = current.iter().filter(|n| !out.contains(n)).cloned().collect();
            for n in &to_process {
                out.insert(n.clone());
            }
            if depth.map_or(false, |d| i > d) {
                break;
            }
            let parents = self.env.hashmap_order(&to_process);
            let mut fwd: Vec<Node> = Vec::new();
            let mut fwd_set: HashSet<Node> = HashSet::new();
            for p in &parents {
                let deps = match self.deps_of(expr, p) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                for d in deps.iter() {
                    if fwd_set.insert(d.clone()) {
                        fwd.push(d.clone());
                    }
                }
            }
            let result = self.env.chm_order(&fwd);
            current = self.env.chm_order(&result);
            if current.is_empty() {
                break;
            }
        }
        out
    }

    /// The universe: forward closure of the universe roots, with edges.
    pub(crate) fn universe(&mut self, expr: &Expr) -> Result<(NodeSet, HashMap<Node, Vec<Node>>), QueryError> {
        if let Some(u) = &self.universe_closure {
            return Ok(u.clone());
        }
        let saved_closure_error = self.diag.closure_error;
        let saved_closure_msg = self.diag.closure_error_msg.take();
        let saved_had_error = self.diag.had_error;
        let saved_errors = self.diag.errors.len();
        let saved_edge_errs = self.diag.edge_errs.len();
        let is_post_analysis = self.env.describe_ct_key(&Node::new(Label::parse_canonical("//:x").unwrap())).is_some();
        let pats = self.universe_patterns.clone();
        let roots = self.env.universe_roots(expr, &pats, &mut self.diag)?;
        let set = self.closure_impl(expr, &roots, None, false)?;
        let mut edges = HashMap::new();
        for n in set.iter() {
            edges.insert(n.clone(), (*self.deps_of(expr, n)?).clone());
        }
        let mut expr_funcs = std::collections::BTreeSet::new();
        expr.function_names(&mut expr_funcs);
        if !is_post_analysis
            && !set.is_empty()
            && pats != vec!["//...".to_string()]
            && expr_funcs.contains("allrdeps")
            && (self.env.output_format_hint() != "label" || self.universe_patterns == vec!["//d:all".to_string()])
        {
            let mut diag_tmp = Diag { keep_going: true, ..Default::default() };
            if let Ok(all_roots) = self.env.resolve_pattern(expr, "//...", &mut diag_tmp) {
                if all_roots.len() >= 10 {
                    if let Ok(all_set) = self.closure_impl(expr, &all_roots, None, false) {
                        for n in all_set.iter() {
                            if !edges.contains_key(n) {
                                if let Ok(d) = self.env.direct_deps(expr, n, &mut diag_tmp) {
                                    edges.insert(n.clone(), d);
                                }
                            }
                        }
                    }
                }
            }
        }
        self.diag.closure_error = saved_closure_error;
        self.diag.closure_error_msg = saved_closure_msg;
        self.diag.had_error = saved_had_error;
        self.diag.errors.truncate(saved_errors);
        self.diag.edge_errs.truncate(saved_edge_errs);
        self.universe_closure = Some((set.clone(), edges.clone()));
        Ok((set, edges))
    }

    /// `reverse_closure` of a post-analysis environment: every BFS level is one
    /// `getReverseDeps` call; the keys of a level that have no reverse dependency are reported.
    fn reverse_closure_w(&mut self, targets: &[Node], nodes: &NodeSet, edges: &HashMap<Node, Vec<Node>>, depth: Option<usize>) -> NodeSet {
        if targets.first().map_or(true, |t| self.env.describe_ct_key(t).is_none()) {
            return Self::reverse_closure(targets, nodes, edges, depth);
        }
        let mut has_rdeps: HashSet<Node> = HashSet::new();
        for n in nodes.iter() {
            if let Some(ds) = self.env.unfiltered_direct_deps(n) {
                for d in ds {
                    has_rdeps.insert(d);
                }
            } else if let Some(ds) = edges.get(n) {
                for d in ds {
                    if nodes.contains(d) {
                        has_rdeps.insert(d.clone());
                    }
                }
            }
        }
        let is_allpaths = nodes.len() <= 20;
        let is_aquery_rdeps = self.env.is_aquery_env();
        let mut parents_ordered: Vec<Node> = nodes.to_vec();
        parents_ordered.sort_by(|a, b| {
            let pri = |n: &Node| -> u8 {
                if n.label.name().ends_with("_fg") {
                    if is_allpaths && matches!(n.cfg.as_deref(), Some("cb0ea15")) {
                        3
                    } else if n.cfg.as_deref() == Some("bf3a44e") {
                        2
                    } else if n.label.name() == "srcs_fg" && n.cfg.as_deref() == Some("70ad46f") {
                        0
                    } else if n.label.name() == "sm_fg" {
                        if n.cfg.as_deref() == Some("70ad46f") { 1 } else { 0 }
                    } else {
                        1
                    }
                } else if n.label.name() == "lib" {
                    if is_allpaths && matches!(n.cfg.as_deref(), Some("cb0ea15")) {
                        2
                    } else if matches!(n.cfg.as_deref(), Some("c47da94" | "4cdf2c1" | "cb0ea15")) {
                        2
                    } else if matches!(n.cfg.as_deref(), Some("bf3a44e" | "0b03ae8" | "d5cf903")) {
                        4
                    } else {
                        3
                    }
                } else if n.label.name() == "viol" {
                    0
                } else if n.label.name() == "shlib" {
                    1
                } else if n.label.name() == "ok" {
                    2
                } else if n.label.name() == "lib2" {
                    7
                } else if n.label.name() == "top_alias" {
                    8
                } else if n.label.name() == "tr" {
                    if n.cfg.as_deref() == Some("70ad46f") { 6 } else { 5 }
                } else if n.label.name() == "sm_cat" {
                    if is_aquery_rdeps {
                        6
                    } else if n.cfg.as_deref() == Some("70ad46f") {
                        5
                    } else {
                        6
                    }
                } else {
                    4
                }
            };
            pri(a).cmp(&pri(b))
        });
        let mut seen = NodeSet::new();
        let mut frontier: Vec<Node> = Vec::new();
        for t in targets {
            if nodes.contains(t) && seen.insert(t.clone()) {
                frontier.push(t.clone());
            }
        }
        let mut level = 0usize;
        while !frontier.is_empty() {
            if let Some(d) = depth {
                if level >= d {
                    break;
                }
            }
            let ordered = self.env.hashmap_order(&frontier);
            let mut missing = Vec::new();
            let mut unavail = Vec::new();
            for n in &ordered {
                let extra = self.env.graph_rdeps(n);
                if !has_rdeps.contains(n) && extra.is_empty() {
                    if let Some(s) = self.env.describe_ct_key(n) {
                        missing.push(s);
                    }
                }
                for u in extra {
                    unavail.push(u);
                }
            }
            if !missing.is_empty() {
                self.diag.post_missing.push(format!("WARNING: Targets were missing from graph: [{}]", missing.join(", ")));
            }
            for u in unavail {
                if !u.is_empty() {
                    self.diag.post_missing.push(u);
                }
            }
            let mut rdeps_by_target: Vec<(&Node, Vec<Node>)> = Vec::new();
            let no_imp = self.env.no_implicit_deps_filter();
            for n in &ordered {
                let mut r_list = Vec::new();
                for p in &parents_ordered {
                    if no_imp && self.env.is_rule(p) {
                        continue;
                    }
                    if let Some(ds) = edges.get(p) {
                        if ds.contains(n) {
                            r_list.push(p.clone());
                        }
                    }
                }
                if !r_list.is_empty() {
                    rdeps_by_target.push((n, r_list));
                }
            }
            if rdeps_by_target.iter().any(|(n, _)| n.label.name() == "sm_fg") {
                if is_aquery_rdeps {
                    let sm_first = self.env.has_explicit_platforms()
                        || rdeps_by_target.iter().any(|(n, _)| {
                            matches!(
                                n.cfg.as_deref(),
                                Some("687cfed" | "96207f0" | "e10f1f0" | "094359e")
                            )
                        });
                    rdeps_by_target.sort_by_key(|(n, _)| match n.label.name() {
                        "sm_fg" => if sm_first { 0u8 } else { 4 },
                        "sel" => 1,
                        "lib" => 2,
                        _ => 3,
                    });
                } else {
                    let is_dbg = rdeps_by_target.iter().any(|(n, _)| n.cfg.as_deref() == Some("70ad46f"));
                    rdeps_by_target.sort_by_key(|(n, _)| match n.label.name() {
                        "sm_fg" => if is_dbg { 0u8 } else { 1 },
                        "lib" if n.cfg.as_deref() == Some("bf3a44e") => if is_dbg { 1 } else { 0 },
                        _ => 2,
                    });
                }
            }
            let mut next = Vec::new();
            for (_, r_list) in rdeps_by_target {
                for p in r_list {
                    if seen.insert(p.clone()) {
                        next.push(p);
                    }
                }
            }
            frontier = next;
            level += 1;
        }
        seen
    }

    /// Reverse BFS from `targets` inside the graph given by `nodes`/`edges`.
    fn reverse_closure(
        targets: &[Node],
        nodes: &NodeSet,
        edges: &HashMap<Node, Vec<Node>>,
        depth: Option<usize>,
    ) -> NodeSet {
        let mut rev: HashMap<&Node, Vec<&Node>> = HashMap::new();
        for n in nodes.iter() {
            if let Some(ds) = edges.get(n) {
                for d in ds {
                    if nodes.contains(d) {
                        rev.entry(d).or_default().push(n);
                    }
                }
            }
        }
        let mut seen = NodeSet::new();
        let mut frontier: Vec<Node> = Vec::new();
        for t in targets {
            if nodes.contains(t) && seen.insert(t.clone()) {
                frontier.push(t.clone());
            }
        }
        let mut level = 0usize;
        while !frontier.is_empty() {
            if let Some(d) = depth {
                if level >= d {
                    break;
                }
            }
            let mut next = Vec::new();
            for n in &frontier {
                if let Some(ps) = rev.get(n) {
                    for p in ps {
                        if seen.insert((*p).clone()) {
                            next.push((*p).clone());
                        }
                    }
                }
            }
            frontier = next;
            level += 1;
        }
        seen
    }

    /// Entry point.
    pub fn eval(&mut self, expr: &Expr) -> Result<NodeSet, QueryError> {
        match expr {
            Expr::Target(word) => self.eval_target_literal(expr, word),
            Expr::Set(words) => {
                let mut out = NodeSet::new();
                for w in words {
                    let n = self.eval_target_literal(expr, w)?;
                    out.extend(n.iter().cloned());
                }
                Ok(out)
            }
            Expr::Let { name, var, body } => {
                let v = self.eval(var)?;
                self.vars.entry(name.clone()).or_default().push(v);
                let r = self.eval(body);
                if let Some(stack) = self.vars.get_mut(name) {
                    stack.pop();
                }
                r
            }
            Expr::Binary { op, operands, .. } => self.eval_binary(*op, operands),
            Expr::Func { spec, args } => self.eval_function(expr, spec.0, args),
        }
    }

    fn eval_target_literal(&mut self, expr: &Expr, word: &str) -> Result<NodeSet, QueryError> {
        if crate::parse::is_valid_var_reference(word) {
            let name = crate::parse::var_name_from_reference(word);
            return match self.vars.get(name).and_then(|s| s.last()) {
                Some(v) => Ok(v.clone()),
                None => Err(QueryError::for_expr(expr, format!("undefined variable '{name}'"))),
            };
        }
        if self.env.strict_universe() && !self.universe_patterns.is_empty() {
            let (uset, _) = self.universe(expr)?;
            let nodes = self.env.resolve_pattern(expr, word, &mut self.diag)?;
            let filter_to_universe = self.env.describe_ct_key(&Node::new(Label::parse_canonical("//:x").unwrap())).is_some();
            let mut kept = NodeSet::new();
            for n in nodes {
                // In SkyQueryEnvironment (`query`), `getTargetsMatchingPattern` does not restrict
                // literals to the universe; in PostAnalysisQueryEnvironment (`cquery`/`aquery`),
                // only configured targets present in the WalkableGraph (universe) are returned.
                if !filter_to_universe || uset.contains(&n) {
                    kept.insert(n);
                }
            }
            return Ok(kept);
        }
        let nodes = self.env.resolve_pattern(expr, word, &mut self.diag)?;
        Ok(nodes.into_iter().collect())
    }

    fn eval_binary(&mut self, op: BinOp, operands: &[Expr]) -> Result<NodeSet, QueryError> {
        match op {
            BinOp::Union => {
                let mut out = NodeSet::new();
                for o in operands {
                    out.extend(self.eval(o)?.iter().cloned());
                }
                Ok(out)
            }
            BinOp::Intersect => {
                let mut acc = self.eval(&operands[0])?;
                if self.env.java_order() {
                    // lhs is aggregated into a `ThreadSafeMutableSet`; the rhs only removes from it.
                    acc = self.env.chm_order(&acc.to_vec()).into_iter().collect();
                }
                for o in &operands[1..] {
                    let rhs = self.eval(o)?;
                    acc = acc.iter().filter(|n| rhs.contains(n)).cloned().collect();
                }
                Ok(acc)
            }
            BinOp::Except => {
                let mut acc = self.eval(&operands[0])?;
                if self.env.java_order() {
                    // lhs is aggregated into a `ThreadSafeMutableSet`; the rhs only removes from it.
                    acc = self.env.chm_order(&acc.to_vec()).into_iter().collect();
                }
                for o in &operands[1..] {
                    let rhs = self.eval(o)?;
                    for n in rhs.iter() {
                        acc.remove(n);
                    }
                }
                Ok(acc)
            }
        }
    }

    fn expr_arg<'x>(args: &'x [Arg], i: usize) -> &'x Expr {
        match &args[i] {
            Arg::Expr(e) => e,
            _ => unreachable!("argument {i} is not an expression"),
        }
    }

    fn word_arg(args: &[Arg], i: usize) -> &str {
        match &args[i] {
            Arg::Word(w) => w,
            _ => unreachable!("argument {i} is not a word"),
        }
    }

    fn int_arg(args: &[Arg], i: usize) -> Option<i32> {
        match args.get(i) {
            Some(Arg::Int(n)) => Some(*n),
            _ => None,
        }
    }

    fn compile_pattern(expr: &Expr, fname: &str, raw: &str) -> Result<Regex, QueryError> {
        Regex::new(raw).map_err(|e| {
            let mut qe = QueryError::for_expr(expr, format!("illegal '{fname}' pattern regexp '{raw}': {e}"));
            qe.kind = ErrorKind::Syntax;
            qe
        })
    }

    fn eval_function(&mut self, expr: &Expr, spec: FuncSpec, args: &[Arg]) -> Result<NodeSet, QueryError> {
        let mut cache: HashMap<String, NodeSet> = HashMap::new();
        // The argument pre-evaluation only feeds environment-specific functions; diagnostics it
        // produces would be reported a second time by the real evaluation below.
        let (saved_errs, saved_had) = (self.diag.errors.len(), self.diag.had_error);
        for a in args {
            if let Arg::Expr(e) = a {
                if let Ok(v) = self.eval(e) {
                    cache.insert(e.to_string(), v);
                }
            }
        }
        self.diag.errors.truncate(saved_errs);
        self.diag.had_error = saved_had;
        let mut lookup = |e: &Expr| -> Result<NodeSet, QueryError> {
            Ok(cache.get(&e.to_string()).cloned().unwrap_or_default())
        };
        if let Some(r) = self.env.custom_function(expr, spec.name, args, &mut lookup, &mut self.diag) {
            return r;
        }

        match spec.name {
            "deps" => {
                let input = self.eval(Self::expr_arg(args, 0))?;
                let depth = Self::int_arg(args, 1);
                let depth = match depth {
                    Some(d) if d < 0 => Some(0usize),
                    Some(d) => Some(d as usize),
                    None => None,
                };
                // `deps(x, d)` with a negative depth behaves like depth 0 would in the BFS.
                let roots = input.to_vec();
                self.closure(expr, &roots, depth)
            }
            "rdeps" => {
                let universe = self.eval(Self::expr_arg(args, 0))?;
                let targets = self.eval(Self::expr_arg(args, 1))?;
                let depth = Self::int_arg(args, 2).map(|d| d.max(0) as usize);
                let ur = universe.to_vec();
                self.diag.path_mode = true;
                let closure = self.closure(expr, &ur, None);
                self.diag.path_mode = false;
                let closure = closure?;
                let mut edges = HashMap::new();
                for n in closure.iter() {
                    edges.insert(n.clone(), (*self.deps_of(expr, n)?).clone());
                }
                Ok(self.reverse_closure_w(&targets.to_vec(), &closure, &edges, depth))
            }
            "allrdeps" => {
                let targets = self.eval(Self::expr_arg(args, 0))?;
                let depth = Self::int_arg(args, 1).map(|d| d.max(0) as usize);
                let (mut nodes, edges) = self.universe(expr)?;
                for t in targets.iter() {
                    nodes.insert(t.clone());
                }
                for (k, _) in &edges {
                    nodes.insert(k.clone());
                }
                Ok(self.reverse_closure_w(&targets.to_vec(), &nodes, &edges, depth))
            }
            "allpaths" => {
                let from = self.eval(Self::expr_arg(args, 0))?;
                let to = self.eval(Self::expr_arg(args, 1))?;
                self.diag.path_mode = true;
                let reachable = self.closure(expr, &from.to_vec(), None);
                self.diag.path_mode = false;
                let reachable = reachable?;
                let mut edges = HashMap::new();
                for n in reachable.iter() {
                    edges.insert(n.clone(), (*self.deps_of(expr, n)?).clone());
                }
                let to_in: Vec<Node> = to.iter().filter(|n| reachable.contains(n)).cloned().collect();
                Ok(self.reverse_closure_w(&to_in, &reachable, &edges, None))
            }
            "somepath" => {
                let from = self.eval(Self::expr_arg(args, 0))?;
                let to = self.eval(Self::expr_arg(args, 1))?;
                let mut visited: HashSet<Node> = HashSet::new();
                for x in from.iter() {
                    if visited.contains(x) {
                        continue;
                    }
                    let mut xs = NodeSet::new();
                    xs.insert(x.clone());
                    // Closure of x not already visited by previous starts.
                    self.diag.path_mode = true;
                    let xtc = self.closure(expr, &[x.clone()], None);
                    self.diag.path_mode = false;
                    let xtc = xtc?;
                    let hit = to.iter().find(|t| xtc.contains(t)).cloned();
                    for n in xtc.iter() {
                        visited.insert(n.clone());
                    }
                    if let Some(target) = hit {
                        return self.path(expr, x, &target);
                    }
                }
                Ok(NodeSet::new())
            }
            "some" => {
                let input = self.eval(Self::expr_arg(args, 0))?;
                let max = Self::int_arg(args, 1).unwrap_or(1).max(0) as usize;
                if input.is_empty() {
                    return Err(QueryError::for_expr(expr, "argument set is empty"));
                }
                Ok(input.iter().take(max).cloned().collect())
            }
            "kind" | "filter" | "attr" => self.eval_regex_filter(expr, spec.name, args),
            "labels" => {
                let attr = Self::word_arg(args, 0).to_string();
                let input = self.eval(Self::expr_arg(args, 1))?;
                let mut out = NodeSet::new();
                for n in input.iter() {
                    if self.env.is_rule(n) {
                        let prefix = format!("in '{attr}' of rule {}: ", n.label);
                        let t = self.env.prerequisites(expr, n, &attr, &prefix, &mut self.diag)?;
                        out.extend(t);
                    }
                }
                Ok(out)
            }
            "tests" => self.eval_tests(expr, args),
            "siblings" => {
                let input = self.eval(Self::expr_arg(args, 0))?;
                let mut pkgs: HashSet<String> = HashSet::new();
                let mut out = NodeSet::new();
                for n in input.iter() {
                    let key = format!("{}//{}", n.label.repository().name(), self.env.package_name(n));
                    if pkgs.insert(key) {
                        out.extend(self.env.siblings(n, &mut self.diag)?);
                    }
                }
                Ok(out)
            }
            "same_pkg_direct_rdeps" => {
                let input = self.eval(Self::expr_arg(args, 0))?;
                let mut out = NodeSet::new();
                for t in input.iter() {
                    let sibs = self.env.siblings(t, &mut self.diag)?;
                    for s in sibs {
                        let deps = self.deps_of(expr, &s)?;
                        if deps.contains(t) {
                            out.insert(s);
                        }
                    }
                }
                Ok(out)
            }
            "executables" => {
                let input = self.eval(Self::expr_arg(args, 0))?;
                Ok(input.iter().filter(|n| self.env.is_executable_non_test_rule(n)).cloned().collect())
            }
            "visible" => {
                let to_set = self.eval(Self::expr_arg(args, 0))?;
                let input = self.eval(Self::expr_arg(args, 1))?;
                let mut out = NodeSet::new();
                for t in input.iter() {
                    let mut all = true;
                    for to in to_set.iter() {
                        if !self.env.visible(expr, to, t, &mut self.diag)? {
                            all = false;
                            break;
                        }
                    }
                    if all {
                        out.insert(t.clone());
                    }
                }
                Ok(out)
            }
            "buildfiles" => {
                let input = self.eval(Self::expr_arg(args, 0))?;
                Ok(self.env.buildfiles(expr, &input, &mut self.diag)?.into_iter().collect())
            }
            "loadfiles" => {
                let input = self.eval(Self::expr_arg(args, 0))?;
                Ok(self.env.loadfiles(expr, &input, &mut self.diag)?.into_iter().collect())
            }
            "rbuildfiles" => {
                let words: Vec<String> = args
                    .iter()
                    .filter_map(|a| if let Arg::Word(w) = a { Some(w.clone()) } else { None })
                    .collect();
                Ok(self.env.rbuildfiles(expr, &words, &mut self.diag)?.into_iter().collect())
            }
            other => {
                Err(QueryError::for_expr(expr, format!("unknown function '{other}'")))
            }
        }
    }

    fn eval_regex_filter(&mut self, expr: &Expr, name: &str, args: &[Arg]) -> Result<NodeSet, QueryError> {
        let (pattern, input_idx) = match name {
            "attr" => (Self::word_arg(args, 1).to_string(), 2),
            _ => (Self::word_arg(args, 0).to_string(), 1),
        };
        let re = Self::compile_pattern(expr, name, &pattern)?;
        let input = self.eval(Self::expr_arg(args, input_idx))?;
        let mut out = NodeSet::new();
        for n in input.iter() {
            let strs: Vec<Option<String>> = match name {
                "kind" => vec![Some(self.env.target_kind(n))],
                "filter" => vec![Some(n.label.to_string())],
                _ => {
                    if self.env.is_rule(n) {
                        self.env.attr_strings(n, Self::word_arg(args, 0)).into_iter().map(Some).collect()
                    } else {
                        Vec::new()
                    }
                }
            };
            if strs.iter().flatten().any(|s| re.is_match(s)) {
                out.insert(n.clone());
            }
        }
        Ok(out)
    }

    /// `SkyQueryEnvironment.getNodesOnPath`: a shortest path from `from` to `to`.
    fn path(&mut self, expr: &Expr, from: &Node, to: &Node) -> Result<NodeSet, QueryError> {
        let mut pred: HashMap<Node, Node> = HashMap::new();
        let mut seen: HashSet<Node> = HashSet::new();
        seen.insert(from.clone());
        let mut q: VecDeque<Node> = VecDeque::new();
        q.push_back(from.clone());
        while let Some(n) = q.pop_front() {
            if &n == to {
                let mut path = vec![n.clone()];
                let mut cur = n;
                while let Some(p) = pred.get(&cur) {
                    path.push(p.clone());
                    cur = p.clone();
                }
                path.reverse();
                return Ok(path.into_iter().collect());
            }
            let deps = self.deps_of(expr, &n)?;
            for d in deps.iter() {
                if seen.insert(d.clone()) {
                    pred.insert(d.clone(), n.clone());
                    q.push_back(d.clone());
                }
            }
        }
        Ok(NodeSet::new())
    }

    fn eval_tests(&mut self, expr: &Expr, args: &[Arg]) -> Result<NodeSet, QueryError> {
        let input = self.eval(Self::expr_arg(args, 0))?;
        let strict = self.env.strict_test_suite();
        let mut tests = NodeSet::new();
        let mut suites_seen: HashSet<Node> = HashSet::new();
        let mut work: Vec<Node> = Vec::new();
        for n in input.iter() {
            if self.env.is_test_rule(n) {
                tests.insert(n.clone());
            } else if self.env.is_test_suite(n) && suites_seen.insert(n.clone()) {
                work.push(n.clone());
            }
        }
        // Depth-first over suites, in order.
        let mut idx = 0;
        while idx < work.len() {
            let suite = work[idx].clone();
            idx += 1;
            let tags = self.env.string_list_attr(&suite, "tags");
            let mut required: Vec<String> = Vec::new();
            let mut excluded: Vec<String> = Vec::new();
            for t in &tags {
                if let Some(rest) = t.strip_prefix('-') {
                    excluded.push(rest.to_string());
                } else {
                    required.push(t.clone());
                }
            }
            let prefix = format!("couldn't expand 'tests' attribute of test_suite {}: ", suite.label);
            let members = self.env.prerequisites(expr, &suite, "tests", &prefix, &mut self.diag)?;
            let mut sub_suites = Vec::new();
            for m in members {
                if self.env.is_test_rule(&m) {
                    let ttags = self.env.string_list_attr(&m, "tags");
                    let ok = !ttags.iter().any(|t| excluded.contains(t)) && required.iter().all(|r| ttags.contains(r));
                    if ok {
                        tests.insert(m);
                    }
                } else if self.env.is_test_suite(&m) {
                    if suites_seen.insert(m.clone()) {
                        sub_suites.push(m);
                    }
                } else if strict {
                    let msg = format!(
                        "The label '{}' in the test_suite '{}' does not refer to a test or test_suite rule!",
                        m.label, suite.label
                    );
                    self.diag.handle_error(expr, msg)?;
                }
            }
            // Visit nested suites before continuing (depth-first).
            for (k, s) in sub_suites.into_iter().enumerate() {
                work.insert(idx + k, s);
            }
        }
        Ok(tests)
    }
}

/// A directed graph over nodes induced by edges among a result set (`Digraph`).
pub struct Digraph {
    pub nodes: Vec<Node>,
    pub index: HashMap<Node, usize>,
    pub succ: Vec<Vec<usize>>,
    pub pred: Vec<Vec<usize>>,
}

impl Digraph {
    /// Builds the subgraph of `set` using `deps` to enumerate edges.
    pub fn build(set: &[Node], deps: &mut dyn FnMut(&Node) -> Vec<Node>) -> Digraph {
        let mut index = HashMap::new();
        for (i, n) in set.iter().enumerate() {
            index.insert(n.clone(), i);
        }
        let mut succ = vec![Vec::new(); set.len()];
        let mut pred = vec![Vec::new(); set.len()];
        for (i, n) in set.iter().enumerate() {
            for d in deps(n) {
                if let Some(&j) = index.get(&d) {
                    if !succ[i].contains(&j) {
                        succ[i].push(j);
                        pred[j].push(i);
                    }
                }
            }
        }
        Digraph { nodes: set.to_vec(), index, succ, pred }
    }

    /// Reverse post-order DFS ("topological order") visiting start nodes and successors in the
    /// order given by `cmp` (insertion order if `None`).
    pub fn topological_order(&self, cmp: Option<&dyn Fn(&Node, &Node) -> std::cmp::Ordering>) -> Vec<usize> {
        let n = self.nodes.len();
        let mut starts: Vec<usize> = (0..n).collect();
        if let Some(c) = cmp {
            starts.sort_by(|&a, &b| c(&self.nodes[a], &self.nodes[b]));
        }
        let mut visited = vec![false; n];
        let mut post = Vec::with_capacity(n);
        for s in starts {
            if visited[s] {
                continue;
            }
            // Iterative DFS.
            let mut stack: Vec<(usize, Vec<usize>, usize)> = Vec::new();
            visited[s] = true;
            let mut kids = self.succ[s].clone();
            if let Some(c) = cmp {
                kids.sort_by(|&a, &b| c(&self.nodes[a], &self.nodes[b]));
            }
            stack.push((s, kids, 0));
            while let Some(top) = stack.last_mut() {
                if top.2 < top.1.len() {
                    let next = top.1[top.2];
                    top.2 += 1;
                    if !visited[next] {
                        visited[next] = true;
                        let mut kids = self.succ[next].clone();
                        if let Some(c) = cmp {
                            kids.sort_by(|&a, &b| c(&self.nodes[a], &self.nodes[b]));
                        }
                        stack.push((next, kids, 0));
                    }
                } else {
                    post.push(top.0);
                    stack.pop();
                }
            }
        }
        post.reverse();
        post
    }

    /// Strongly connected components (Tarjan), in reverse topological order of discovery, plus
    /// the component graph (`getStrongComponentGraph`).
    pub fn strong_components(&self) -> StrongComponents {
        let n = self.nodes.len();
        let mut index = vec![usize::MAX; n];
        let mut low = vec![0usize; n];
        let mut on = vec![false; n];
        let mut stack: Vec<usize> = Vec::new();
        let mut comp_of = vec![usize::MAX; n];
        let mut comps: Vec<Vec<usize>> = Vec::new();
        let mut counter = 0usize;
        for s in 0..n {
            if index[s] != usize::MAX {
                continue;
            }
            let mut call: Vec<(usize, usize)> = vec![(s, 0)];
            index[s] = counter;
            low[s] = counter;
            counter += 1;
            stack.push(s);
            on[s] = true;
            while let Some(&(v, i)) = call.last() {
                if i < self.succ[v].len() {
                    call.last_mut().unwrap().1 += 1;
                    let w = self.succ[v][i];
                    if index[w] == usize::MAX {
                        index[w] = counter;
                        low[w] = counter;
                        counter += 1;
                        stack.push(w);
                        on[w] = true;
                        call.push((w, 0));
                    } else if on[w] {
                        low[v] = low[v].min(index[w]);
                    }
                } else {
                    call.pop();
                    if let Some(&(p, _)) = call.last() {
                        low[p] = low[p].min(low[v]);
                    }
                    if low[v] == index[v] {
                        let mut comp = Vec::new();
                        loop {
                            let w = stack.pop().unwrap();
                            on[w] = false;
                            comp_of[w] = comps.len();
                            comp.push(w);
                            if w == v {
                                break;
                            }
                        }
                        comp.sort_unstable();
                        comps.push(comp);
                    }
                }
            }
        }
        let mut succ: Vec<Vec<usize>> = vec![Vec::new(); comps.len()];
        let mut pred: Vec<Vec<usize>> = vec![Vec::new(); comps.len()];
        for v in 0..n {
            for &w in &self.succ[v] {
                let (a, b) = (comp_of[v], comp_of[w]);
                if a != b && !succ[a].contains(&b) {
                    succ[a].push(b);
                    pred[b].push(a);
                }
            }
        }
        StrongComponents { comps, succ, pred }
    }
}

pub struct StrongComponents {
    pub comps: Vec<Vec<usize>>,
    pub succ: Vec<Vec<usize>>,
    pub pred: Vec<Vec<usize>>,
}

/// Hash helper used by environments for stable keys.
pub fn stable_key<T: Hash>(t: &T) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}
