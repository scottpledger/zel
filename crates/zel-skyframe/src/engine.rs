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

//! The incremental evaluation engine (`InMemoryMemoizingEvaluator` + `ParallelEvaluator`).
//!
//! Faithful to Bazel's model:
//!
//! * **Versioned nodes.** Each node remembers the version at which its value last *changed*
//!   (`last_changed`) and the version at which it was last *evaluated/verified*
//!   (`last_evaluated`). Every `evaluate` call runs at a fresh, increasing graph version.
//! * **Dirty/rebuild lifecycle.** Invalidating a key marks it `NeedsRebuild` and all of its
//!   transitive reverse dependencies `CheckDependencies`. During evaluation a
//!   `CheckDependencies` node re-evaluates its previous dependency groups *in order*; if none
//!   of them changed after the node's `last_evaluated`, the node is verified clean without
//!   running its function; otherwise it is rebuilt.
//! * **Change pruning.** A rebuilt node whose new value equals the old one keeps its
//!   `last_changed`, so its reverse dependencies are verified clean instead of rebuilt.
//! * **Restarts.** A function that needs a missing dependency returns `Ok(None)`; the engine
//!   evaluates the missing deps (in parallel) and re-runs the function from scratch.
//! * **Parallel evaluation** on a pool of worker threads; function bodies run outside the
//!   graph lock.
//! * **Errors.** `keep_going` mode finishes every independent subgraph; without it the first
//!   error aborts the evaluation. Errors bubble to reverse dependencies unless they request the
//!   dependency with `get_value_or_error`.
//! * **Cycles.** When evaluation stalls with unfinished nodes the engine runs a DFS over the
//!   waiting graph, reports `CycleInfo`s (path to cycle + cycle) and fails the involved nodes.

use crate::cycles::CycleInfo;
use crate::{SkyFunction, SkyFunctionError, SkyFunctionName, SkyKey, SkyValue};
use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

pub type Version = u64;

/// Error state of a node (`ErrorInfo`).
#[derive(Debug, Clone, PartialEq)]
pub struct ErrorInfo {
    /// The originating function error (`None` for a pure cycle).
    pub error: Option<SkyFunctionError>,
    /// Keys whose functions produced the underlying error.
    pub root_causes: Vec<SkyKey>,
    pub cycles: Vec<CycleInfo>,
}

impl ErrorInfo {
    fn from_error(key: &SkyKey, e: SkyFunctionError) -> Self {
        Self { error: Some(e), root_causes: vec![key.clone()], cycles: vec![] }
    }

    pub fn message(&self) -> String {
        match (&self.error, self.cycles.first()) {
            (Some(e), _) => e.message.clone(),
            (None, Some(_)) => "cycle in dependency graph".to_string(),
            _ => "unknown error".to_string(),
        }
    }

    pub fn is_transient(&self) -> bool {
        self.error.as_ref().map(|e| e.transient).unwrap_or(false)
    }

    /// Error of `parent` caused by the failure of its dependency `child`.
    fn bubbled(parent: &SkyKey, child: &ErrorInfo) -> Self {
        let cycles = child
            .cycles
            .iter()
            .map(|c| {
                let mut path = vec![parent.clone()];
                path.extend(c.path_to_cycle.iter().cloned());
                CycleInfo::new(path, c.cycle.clone())
            })
            .collect();
        Self { error: child.error.clone(), root_causes: child.root_causes.clone(), cycles }
    }
}

#[derive(Clone)]
enum NodeValue {
    Value(Arc<dyn SkyValue>),
    Error(ErrorInfo),
}

impl NodeValue {
    fn same_as(&self, other: &NodeValue) -> bool {
        match (self, other) {
            (NodeValue::Value(a), NodeValue::Value(b)) => a.eq_value(b.as_ref()),
            (NodeValue::Error(a), NodeValue::Error(b)) => a == b,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dirty {
    Clean,
    /// Some dependency may have changed.
    CheckDeps,
    /// The node itself was invalidated (or never evaluated).
    NeedsRebuild,
}

struct Node {
    value: Option<NodeValue>,
    last_changed: Version,
    last_evaluated: Version,
    /// Direct deps of the last evaluation, in request groups (a group is one `get_values` call).
    deps: Vec<Vec<SkyKey>>,
    rdeps: HashSet<SkyKey>,
    dirty: Dirty,
}

impl Node {
    fn new() -> Self {
        Self { value: None, last_changed: 0, last_evaluated: 0, deps: vec![], rdeps: HashSet::new(), dirty: Dirty::NeedsRebuild }
    }
    fn is_done(&self) -> bool {
        self.value.is_some() && self.dirty == Dirty::Clean
    }
}

#[derive(Default)]
struct Graph {
    nodes: HashMap<SkyKey, Node>,
}

impl Graph {
    fn mark_dirty(&mut self, keys: &[SkyKey], rebuild: bool) -> usize {
        let mut n = 0;
        let mut stack: Vec<(SkyKey, bool)> = keys.iter().map(|k| (k.clone(), rebuild)).collect();
        while let Some((k, rb)) = stack.pop() {
            let Some(node) = self.nodes.get_mut(&k) else { continue };
            let target = if rb { Dirty::NeedsRebuild } else { Dirty::CheckDeps };
            // Already (at least as) dirty nodes have already propagated to their rdeps.
            let already = match (node.dirty, target) {
                (Dirty::NeedsRebuild, _) => true,
                (Dirty::CheckDeps, Dirty::CheckDeps) => true,
                _ => false,
            };
            if already && node.value.is_some() {
                continue;
            }
            if node.dirty == Dirty::Clean || target == Dirty::NeedsRebuild {
                node.dirty = target;
            }
            n += 1;
            for r in node.rdeps.iter() {
                stack.push((r.clone(), false));
            }
        }
        n
    }
}

/// Per-evaluation statistics (used by tests and `--noshow_progress`-style summaries).
#[derive(Debug, Default, Clone)]
pub struct EvaluationStats {
    /// Keys whose function ran to completion (value or error) in this evaluation.
    pub evaluated: Vec<SkyKey>,
    /// Keys verified clean without running their function (dirty, but no dep changed).
    pub cleaned: Vec<SkyKey>,
    /// Function invocations including restarts.
    pub function_calls: usize,
    /// Rebuilt nodes whose new value equalled the old one (change pruning).
    pub pruned: Vec<SkyKey>,
}

#[derive(Debug)]
pub struct EvaluationResult {
    /// Values of the requested root keys that evaluated successfully.
    pub values: HashMap<SkyKey, Arc<dyn SkyValue>>,
    /// Errors of root keys (and of the originating failing keys).
    pub errors: HashMap<SkyKey, SkyFunctionError>,
    /// Full error information for every key that failed in this evaluation.
    pub error_infos: HashMap<SkyKey, ErrorInfo>,
    /// Cycles (closed: first key repeated at the end) for backward compatibility.
    pub cycles: Vec<Vec<SkyKey>>,
    /// Cycle information per root key.
    pub root_cycles: Vec<(SkyKey, CycleInfo)>,
    pub stats: EvaluationStats,
    pub version: Version,
    pub aborted: bool,
}

impl EvaluationResult {
    pub fn has_error(&self) -> bool {
        !self.errors.is_empty() || !self.cycles.is_empty() || !self.error_infos.is_empty()
    }

    pub fn get<T: SkyValue>(&self, key: &SkyKey) -> Option<&T> {
        self.values.get(key)?.as_any().downcast_ref::<T>()
    }
}

/// Options for one evaluation (`EvaluationContext`).
#[derive(Clone)]
pub struct EvaluationOptions {
    pub keep_going: bool,
    pub threads: usize,
    /// Set to abort a running evaluation (Ctrl-C).
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for EvaluationOptions {
    fn default() -> Self {
        Self {
            keep_going: true,
            threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(32),
            cancel: None,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Environment

pub(crate) enum Lookup {
    Value(Arc<dyn SkyValue>),
    Error(ErrorInfo),
    Missing,
}

pub(crate) trait DepLookup {
    fn lookup(&self, key: &SkyKey) -> Lookup;
}

/// `SkyFunction.Environment`: request dependencies; missing ones make the function restart.
pub struct Environment<'a> {
    lookup: &'a dyn DepLookup,
    groups: Vec<Vec<SkyKey>>,
    seen: HashSet<SkyKey>,
    handled: HashSet<SkyKey>,
    missing: Vec<SkyKey>,
    error_deps: Vec<(SkyKey, ErrorInfo)>,
    values_missing: bool,
}

impl<'a> Environment<'a> {
    pub(crate) fn new(lookup: &'a dyn DepLookup) -> Self {
        Self {
            lookup,
            groups: Vec::new(),
            seen: HashSet::new(),
            handled: HashSet::new(),
            missing: Vec::new(),
            error_deps: Vec::new(),
            values_missing: false,
        }
    }

    fn request(&mut self, keys: &[SkyKey], or_error: bool) -> Vec<Result<Option<Arc<dyn SkyValue>>, SkyFunctionError>> {
        let mut group = Vec::new();
        for k in keys {
            if self.seen.insert(k.clone()) {
                group.push(k.clone());
            }
        }
        if !group.is_empty() {
            self.groups.push(group);
        }
        if or_error {
            for k in keys {
                self.handled.insert(k.clone());
            }
        }
        keys.iter()
            .map(|k| match self.lookup.lookup(k) {
                Lookup::Value(v) => Ok(Some(v)),
                Lookup::Error(info) => {
                    if or_error {
                        Err(info.error.clone().unwrap_or_else(|| SkyFunctionError::persistent(info.message())))
                    } else {
                        self.values_missing = true;
                        self.error_deps.push((k.clone(), info));
                        Ok(None)
                    }
                }
                Lookup::Missing => {
                    self.values_missing = true;
                    if !self.missing.contains(k) {
                        self.missing.push(k.clone());
                    }
                    Ok(None)
                }
            })
            .collect()
    }

    /// Requests one dependency; `None` if it is not (yet) available.
    pub fn get_value(&mut self, key: &SkyKey) -> Option<Arc<dyn SkyValue>> {
        self.request(std::slice::from_ref(key), false).pop().and_then(|r| r.ok().flatten())
    }

    /// Requests several dependencies at once (they are evaluated in parallel).
    pub fn get_values(&mut self, keys: &[SkyKey]) -> Vec<Option<Arc<dyn SkyValue>>> {
        self.request(keys, false).into_iter().map(|r| r.ok().flatten()).collect()
    }

    /// `getValueOrThrow`: a failed dependency is returned as `Err` instead of failing this node.
    pub fn get_value_or_error(&mut self, key: &SkyKey) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
        self.request(std::slice::from_ref(key), true).pop().unwrap()
    }

    pub fn values_missing(&self) -> bool {
        self.values_missing
    }

    /// Flat list of requested keys (compat helper).
    pub fn into_requested_deps(self) -> Vec<SkyKey> {
        self.groups.into_iter().flatten().collect()
    }
}

// ---------------------------------------------------------------------------------------
// Evaluator

#[derive(Default)]
struct Inflight {
    waiting_on: Vec<SkyKey>,
    handled: HashSet<SkyKey>,
    running: bool,
    /// Dependency groups requested by the attempt that is now waiting (recorded as the node's
    /// deps if the node ends up failing, so a later fix of the cause re-dirties it).
    groups: Vec<Vec<SkyKey>>,
}

struct RunState {
    inflight: HashMap<SkyKey, Inflight>,
    /// dep key -> keys waiting for it.
    waiters: HashMap<SkyKey, Vec<SkyKey>>,
    ready: VecDeque<SkyKey>,
    active: usize,
    roots: Vec<SkyKey>,
    done: bool,
    aborted: bool,
    keep_going: bool,
    version: Version,
    stats: EvaluationStats,
    failed: HashMap<SkyKey, ErrorInfo>,
    cycle_checked: bool,
}

struct Core {
    graph: Graph,
    run: RunState,
    transient_errors: HashSet<SkyKey>,
}

struct LookupView<'a> {
    core: &'a Mutex<Core>,
}

impl DepLookup for LookupView<'_> {
    fn lookup(&self, key: &SkyKey) -> Lookup {
        let core = self.core.lock().unwrap();
        match core.graph.nodes.get(key) {
            Some(n) if n.is_done() => match n.value.as_ref().unwrap() {
                NodeValue::Value(v) => Lookup::Value(Arc::clone(v)),
                NodeValue::Error(e) => Lookup::Error(e.clone()),
            },
            _ => Lookup::Missing,
        }
    }
}

/// Incremental memoizing evaluator (`InMemoryMemoizingEvaluator`) with a parallel evaluator.
pub struct InMemoryMemoizingEvaluator {
    functions: HashMap<SkyFunctionName, Arc<dyn SkyFunction>>,
    core: Mutex<Core>,
    cv: Condvar,
    eval_lock: Mutex<()>,
    version: Mutex<Version>,
    pub options: Mutex<EvaluationOptions>,
}

impl Default for InMemoryMemoizingEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryMemoizingEvaluator {
    pub fn new() -> Self {
        Self {
            functions: HashMap::new(),
            core: Mutex::new(Core {
                graph: Graph::default(),
                run: RunState::empty(),
                transient_errors: HashSet::new(),
            }),
            cv: Condvar::new(),
            eval_lock: Mutex::new(()),
            version: Mutex::new(0),
            options: Mutex::new(EvaluationOptions::default()),
        }
    }

    pub fn register_function(&mut self, name: SkyFunctionName, func: Arc<dyn SkyFunction>) -> &mut Self {
        self.functions.insert(name, func);
        self
    }

    /// Graph version of the last completed evaluation.
    pub fn version(&self) -> Version {
        *self.version.lock().unwrap()
    }

    pub fn node_count(&self) -> usize {
        self.core.lock().unwrap().graph.nodes.len()
    }

    /// Total number of dependency edges (node -> direct dep) in the graph.
    pub fn edge_count(&self) -> usize {
        self.core.lock().unwrap().graph.nodes.values().map(|n| n.deps.iter().map(|g| g.len()).sum::<usize>()).sum()
    }

    pub fn contains(&self, key: &SkyKey) -> bool {
        self.core.lock().unwrap().graph.nodes.contains_key(key)
    }

    /// Whether `key` currently holds a clean value (not dirty).
    pub fn is_clean(&self, key: &SkyKey) -> bool {
        self.core.lock().unwrap().graph.nodes.get(key).map(|n| n.is_done()).unwrap_or(false)
    }

    pub fn keys_with_function(&self, name: SkyFunctionName) -> Vec<SkyKey> {
        self.core.lock().unwrap().graph.nodes.keys().filter(|k| k.function_name() == name).cloned().collect()
    }

    /// Direct dependencies recorded for `key` (flattened, request order).
    pub fn direct_deps(&self, key: &SkyKey) -> Vec<SkyKey> {
        self.core.lock().unwrap().graph.nodes.get(key).map(|n| n.deps.iter().flatten().cloned().collect()).unwrap_or_default()
    }

    pub fn reverse_deps(&self, key: &SkyKey) -> Vec<SkyKey> {
        self.core.lock().unwrap().graph.nodes.get(key).map(|n| n.rdeps.iter().cloned().collect()).unwrap_or_default()
    }

    /// Injects (or updates) a value for a node that has no function (`Injectable.inject`).
    /// A changed value dirties all reverse dependencies.
    pub fn inject_precomputed(&self, key: SkyKey, value: Arc<dyn SkyValue>) {
        let next = self.version() + 1;
        let mut core = self.core.lock().unwrap();
        let node = core.graph.nodes.entry(key.clone()).or_insert_with(Node::new);
        let changed = match &node.value {
            Some(NodeValue::Value(old)) => !old.eq_value(value.as_ref()),
            _ => true,
        };
        if changed {
            node.value = Some(NodeValue::Value(value));
            node.last_changed = next;
            node.last_evaluated = next;
            node.dirty = Dirty::Clean;
            let rdeps: Vec<SkyKey> = node.rdeps.iter().cloned().collect();
            core.graph.mark_dirty(&rdeps, false);
        } else {
            node.dirty = Dirty::Clean;
        }
    }

    /// Marks `keys` as changed externally (`NeedsRebuild`) and their transitive reverse
    /// dependencies as needing verification. Returns the number of nodes dirtied.
    pub fn invalidate(&self, keys: &[SkyKey]) -> usize {
        let mut core = self.core.lock().unwrap();
        core.graph.mark_dirty(keys, true)
    }

    /// Deletes all nodes computed by the given functions (`--discard_analysis_cache`-style).
    /// Their remaining reverse dependencies are dirtied so they get rebuilt.
    pub fn delete_functions(&self, names: &[SkyFunctionName]) -> usize {
        let mut core = self.core.lock().unwrap();
        let victims: Vec<SkyKey> =
            core.graph.nodes.keys().filter(|k| names.contains(&k.function_name())).cloned().collect();
        let vset: HashSet<SkyKey> = victims.iter().cloned().collect();
        let mut survivors_dirty: Vec<SkyKey> = Vec::new();
        for k in &victims {
            if let Some(n) = core.graph.nodes.get(k) {
                survivors_dirty.extend(n.rdeps.iter().filter(|r| !vset.contains(*r)).cloned());
            }
        }
        // Remove edges from surviving deps to the victims.
        for k in &victims {
            let deps: Vec<SkyKey> =
                core.graph.nodes.get(k).map(|n| n.deps.iter().flatten().cloned().collect()).unwrap_or_default();
            for d in deps {
                if let Some(dn) = core.graph.nodes.get_mut(&d) {
                    dn.rdeps.remove(k);
                }
            }
        }
        for k in &victims {
            core.graph.nodes.remove(k);
            core.transient_errors.remove(k);
        }
        // Rdeps lost a dependency: they must re-run to recompute it.
        for s in &survivors_dirty {
            if let Some(n) = core.graph.nodes.get_mut(s) {
                n.dirty = Dirty::NeedsRebuild;
            }
        }
        core.graph.mark_dirty(&survivors_dirty, true);
        victims.len()
    }

    /// Drops the entire graph (`clean` / `--discard_graph_edges`-style full reset).
    pub fn clear(&self) {
        let mut core = self.core.lock().unwrap();
        core.graph = Graph::default();
        core.transient_errors.clear();
    }

    /// Keys of all nodes whose value was computed by one of the given functions.
    pub fn keys_by_function(&self, names: &[SkyFunctionName]) -> Vec<SkyKey> {
        self.core.lock().unwrap().graph.nodes.keys().filter(|k| names.contains(&k.function_name())).cloned().collect()
    }

    /// Clean values of all nodes computed by the given functions.
    pub fn snapshot_values(&self, names: &[SkyFunctionName]) -> Vec<(SkyKey, Arc<dyn SkyValue>)> {
        let core = self.core.lock().unwrap();
        core.graph
            .nodes
            .iter()
            .filter(|(k, n)| names.contains(&k.function_name()) && n.is_done())
            .filter_map(|(k, n)| match n.value.as_ref()? {
                NodeValue::Value(v) => Some((k.clone(), Arc::clone(v))),
                _ => None,
            })
            .collect()
    }

    /// Value of a clean node, if any.
    pub fn peek(&self, key: &SkyKey) -> Option<Arc<dyn SkyValue>> {
        match self.core.lock().unwrap().graph.nodes.get(key) {
            Some(n) if n.is_done() => match n.value.as_ref()? {
                NodeValue::Value(v) => Some(Arc::clone(v)),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn evaluate(&self, roots: &[SkyKey]) -> EvaluationResult {
        let opts = self.options.lock().unwrap().clone();
        self.evaluate_with(roots, &opts)
    }

    pub fn evaluate_with(&self, roots: &[SkyKey], opts: &EvaluationOptions) -> EvaluationResult {
        let _serial = self.eval_lock.lock().unwrap();
        let version = {
            let mut v = self.version.lock().unwrap();
            *v += 1;
            *v
        };
        {
            let mut core = self.core.lock().unwrap();
            // Transient errors are re-evaluated on every build (`ErrorTransienceValue`).
            let transient: Vec<SkyKey> = core.transient_errors.drain().collect();
            core.graph.mark_dirty(&transient, true);
            let mut run = RunState::empty();
            run.keep_going = opts.keep_going;
            run.version = version;
            run.roots = roots.to_vec();
            core.run = run;
            for r in roots {
                Self::schedule(&mut core, r);
            }
        }
        // Nothing to do if every root is already clean (the common warm case).
        let work = !self.core.lock().unwrap().run.ready.is_empty();
        let threads = if work { opts.threads.max(1) } else { 0 };
        let cancel = opts.cancel.clone();
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| self.worker(cancel.as_deref()));
            }
        });
        let mut core = self.core.lock().unwrap();
        let run = std::mem::replace(&mut core.run, RunState::empty());
        let mut result = EvaluationResult {
            values: HashMap::new(),
            errors: HashMap::new(),
            error_infos: run.failed.clone(),
            cycles: vec![],
            root_cycles: vec![],
            stats: run.stats,
            version,
            aborted: run.aborted,
        };
        for r in roots {
            match core.graph.nodes.get(r) {
                Some(n) if n.is_done() => match n.value.as_ref().unwrap() {
                    NodeValue::Value(v) => {
                        result.values.insert(r.clone(), Arc::clone(v));
                    }
                    NodeValue::Error(e) => {
                        result.error_infos.entry(r.clone()).or_insert_with(|| e.clone());
                    }
                },
                _ => {}
            }
        }
        let infos: Vec<(SkyKey, ErrorInfo)> = result.error_infos.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (k, info) in infos {
            if let Some(e) = &info.error {
                result.errors.insert(k.clone(), e.clone());
            } else if !info.cycles.is_empty() {
                result.errors.insert(k.clone(), SkyFunctionError::persistent(info.message()));
            }
        }
        for r in roots {
            if let Some(info) = result.error_infos.get(r) {
                for c in &info.cycles {
                    let mut closed = c.path_to_cycle.clone();
                    closed.extend(c.cycle.iter().cloned());
                    closed.push(c.cycle[0].clone());
                    result.cycles.push(closed);
                    result.root_cycles.push((r.clone(), c.clone()));
                }
            }
        }
        result
    }

    // ------------------------------------------------------------------ scheduling

    fn schedule(core: &mut Core, key: &SkyKey) {
        if core.run.inflight.contains_key(key) {
            return;
        }
        if core.graph.nodes.get(key).map(|n| n.is_done()).unwrap_or(false) {
            return;
        }
        core.run.inflight.insert(key.clone(), Inflight::default());
        core.run.ready.push_back(key.clone());
    }

    fn worker(&self, cancel: Option<&AtomicBool>) {
        loop {
            let key = {
                let mut core = self.core.lock().unwrap();
                loop {
                    if core.run.done || core.run.aborted {
                        self.cv.notify_all();
                        return;
                    }
                    if cancel.map(|c| c.load(Ordering::SeqCst)).unwrap_or(false) {
                        core.run.aborted = true;
                        self.cv.notify_all();
                        return;
                    }
                    if let Some(k) = core.run.ready.pop_front() {
                        core.run.active += 1;
                        if let Some(i) = core.run.inflight.get_mut(&k) {
                            i.running = true;
                        }
                        break k;
                    }
                    if core.run.active == 0 {
                        // Stalled or finished.
                        if core.run.inflight.is_empty() {
                            core.run.done = true;
                            self.cv.notify_all();
                            return;
                        }
                        if !core.run.cycle_checked {
                            core.run.cycle_checked = true;
                            Self::detect_cycles(&mut core);
                            self.cv.notify_all();
                            continue;
                        }
                        // No progress possible: fail whatever is left.
                        Self::fail_stalled(&mut core);
                        core.run.done = true;
                        self.cv.notify_all();
                        return;
                    }
                    core = match cancel {
                        Some(_) => self.cv.wait_timeout(core, std::time::Duration::from_millis(20)).unwrap().0,
                        None => self.cv.wait(core).unwrap(),
                    };
                }
            };
            self.process(key);
            let mut core = self.core.lock().unwrap();
            core.run.active -= 1;
            if !core.run.ready.is_empty() || core.run.active == 0 || core.run.done {
                self.cv.notify_all();
            }
        }
    }

    fn process(&self, key: SkyKey) {
        enum Step {
            Nothing,
            Compute(Arc<dyn SkyFunction>),
        }
        let step = {
            let mut core = self.core.lock().unwrap();
            self.prepare(&mut core, &key)
                .map(|compute| if compute { Step::Compute(self.function_for(&key)) } else { Step::Nothing })
                .unwrap_or(Step::Nothing)
        };
        let Step::Compute(func_opt) = step else {
            return;
        };
        let version = self.core.lock().unwrap().run.version;
        let _ = version;
        let view = LookupView { core: &self.core };
        let mut env = Environment::new(&view);
        let outcome = catch_unwind(AssertUnwindSafe(|| func_opt.compute(&key, &mut env)));
        let mut core = self.core.lock().unwrap();
        core.run.stats.function_calls += 1;
        if core.run.aborted {
            return;
        }
        let Environment { groups, handled, missing, error_deps, values_missing, .. } = env;
        match outcome {
            Err(p) => {
                let msg = p
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| p.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".into());
                let info = ErrorInfo::from_error(&key, SkyFunctionError::persistent(format!("SkyFunction for {key} panicked: {msg}")));
                Self::finish_error(&mut core, &key, info, true, groups);
            }
            Ok(Err(e)) => {
                let info = ErrorInfo::from_error(&key, e);
                Self::finish_error(&mut core, &key, info, true, groups);
            }
            Ok(Ok(Some(v))) => {
                if values_missing {
                    // A function must not produce a value while deps are missing; treat as a bug.
                    let info = ErrorInfo::from_error(
                        &key,
                        SkyFunctionError::persistent(format!("SkyFunction for {key} returned a value with missing dependencies")),
                    );
                    Self::finish_error(&mut core, &key, info, true, groups);
                } else {
                    Self::finish_value(&mut core, &key, NodeValue::Value(v), groups);
                }
            }
            Ok(Ok(None)) => {
                if let Some((_, child)) = error_deps.first() {
                    // Unhandled failed dependency: this node fails with the child's error.
                    let info = ErrorInfo::bubbled(&key, child);
                    // Merge cycle info and root causes of all failed deps.
                    let mut info = info;
                    for (_, c) in error_deps.iter().skip(1) {
                        for rc in &c.root_causes {
                            if !info.root_causes.contains(rc) {
                                info.root_causes.push(rc.clone());
                            }
                        }
                        let b = ErrorInfo::bubbled(&key, c);
                        info.cycles.extend(b.cycles);
                    }
                    Self::finish_error(&mut core, &key, info, false, groups);
                    return;
                }
                if missing.is_empty() {
                    let info = ErrorInfo::from_error(
                        &key,
                        SkyFunctionError::persistent(format!("SkyFunction for {key} returned null without requesting missing dependencies")),
                    );
                    Self::finish_error(&mut core, &key, info, true, groups);
                    return;
                }
                // Wait for the missing deps (which may have completed in the meantime).
                let mut waiting = Vec::new();
                for d in missing {
                    if core.graph.nodes.get(&d).map(|n| n.is_done()).unwrap_or(false) {
                        continue;
                    }
                    waiting.push(d);
                }
                let handled_set = handled;
                Self::wait_on(&mut core, &key, waiting, handled_set, groups);
            }
        }
    }

    fn function_for(&self, key: &SkyKey) -> Arc<dyn SkyFunction> {
        match self.functions.get(&key.function_name()) {
            Some(f) => Arc::clone(f),
            None => Arc::new(MissingFunction(key.function_name())),
        }
    }

    fn wait_on(core: &mut Core, key: &SkyKey, waiting: Vec<SkyKey>, handled: HashSet<SkyKey>, groups: Vec<Vec<SkyKey>>) {
        if waiting.is_empty() {
            // Everything already available: restart right away.
            if let Some(i) = core.run.inflight.get_mut(key) {
                i.running = false;
            }
            core.run.ready.push_back(key.clone());
            return;
        }
        for d in &waiting {
            Self::schedule(core, d);
            core.run.waiters.entry(d.clone()).or_default().push(key.clone());
        }
        let i = core.run.inflight.entry(key.clone()).or_default();
        i.running = false;
        i.waiting_on = waiting;
        i.handled = handled;
        i.groups = groups;
    }

    /// Decides what to do with `key`. Returns `Some(true)` to compute, `Some(false)`/`None`
    /// when nothing more to do now (done, or waiting on deps).
    fn prepare(&self, core: &mut Core, key: &SkyKey) -> Option<bool> {
        let version = core.run.version;
        let node = core.graph.nodes.entry(key.clone()).or_insert_with(Node::new);
        if node.is_done() {
            Self::complete(core, key);
            return Some(false);
        }
        if node.dirty == Dirty::CheckDeps && node.value.is_some() {
            let groups = node.deps.clone();
            let last_eval = node.last_evaluated;
            let mut changed = false;
            let mut waiting: Vec<SkyKey> = Vec::new();
            'outer: for g in &groups {
                for d in g {
                    match core.graph.nodes.get(d) {
                        Some(dn) if dn.is_done() => {
                            if dn.last_changed > last_eval {
                                changed = true;
                                break 'outer;
                            }
                        }
                        _ => waiting.push(d.clone()),
                    }
                }
                if !waiting.is_empty() {
                    break;
                }
            }
            if changed {
                let n = core.graph.nodes.get_mut(key).unwrap();
                n.dirty = Dirty::NeedsRebuild;
                return Some(true);
            }
            if !waiting.is_empty() {
                // A dependency vanished from the graph (deleted) counts as a change.
                let missing_from_graph: bool = waiting.iter().any(|d| !core.graph.nodes.contains_key(d) && !self.functions.contains_key(&d.function_name()));
                if missing_from_graph {
                    let n = core.graph.nodes.get_mut(key).unwrap();
                    n.dirty = Dirty::NeedsRebuild;
                    return Some(true);
                }
                Self::wait_on(core, key, waiting, HashSet::new(), groups.clone());
                return Some(false);
            }
            // Verified clean: nothing changed, keep the value (no function call).
            let n = core.graph.nodes.get_mut(key).unwrap();
            n.dirty = Dirty::Clean;
            n.last_evaluated = version;
            core.run.stats.cleaned.push(key.clone());
            Self::complete(core, key);
            return Some(false);
        }
        Some(true)
    }

    fn finish_value(core: &mut Core, key: &SkyKey, value: NodeValue, groups: Vec<Vec<SkyKey>>) {
        Self::store_value(core, key, value, groups);
        Self::complete(core, key);
    }

    /// Records `value` and the dependency edges for `key` without signalling waiters.
    fn store_value(core: &mut Core, key: &SkyKey, value: NodeValue, groups: Vec<Vec<SkyKey>>) {
        let version = core.run.version;
        // Update the dependency edges.
        let old_deps: HashSet<SkyKey> =
            core.graph.nodes.get(key).map(|n| n.deps.iter().flatten().cloned().collect()).unwrap_or_default();
        let new_deps: HashSet<SkyKey> = groups.iter().flatten().cloned().collect();
        for d in old_deps.difference(&new_deps) {
            if let Some(dn) = core.graph.nodes.get_mut(d) {
                dn.rdeps.remove(key);
            }
        }
        for d in &new_deps {
            core.graph.nodes.entry(d.clone()).or_insert_with(Node::new).rdeps.insert(key.clone());
        }
        let node = core.graph.nodes.get_mut(key).unwrap();
        let unchanged = node.value.as_ref().map(|old| old.same_as(&value)).unwrap_or(false);
        if !unchanged {
            node.last_changed = version;
        } else {
            core.run.stats.pruned.push(key.clone());
        }
        let is_transient = matches!(&value, NodeValue::Error(e) if e.is_transient());
        let node = core.graph.nodes.get_mut(key).unwrap();
        // Always store the fresh value (equal values may carry different metadata such as the
        // observed mtime); only `last_changed` is held back for change pruning.
        node.value = Some(value);
        node.last_evaluated = version;
        node.dirty = Dirty::Clean;
        node.deps = groups;
        if is_transient {
            core.transient_errors.insert(key.clone());
        }
        core.run.stats.evaluated.push(key.clone());
    }

    /// Finishes `key` with an error. `origin` is true for errors produced by the function itself
    /// (as opposed to bubbled from a dependency).
    fn finish_error(core: &mut Core, key: &SkyKey, info: ErrorInfo, origin: bool, groups: Vec<Vec<SkyKey>>) {
        if origin && !core.run.keep_going {
            core.run.aborted = true;
        }
        core.run.failed.insert(key.clone(), info.clone());
        Self::finish_value(core, key, NodeValue::Error(info), groups);
    }

    /// Signals waiters that `key` is done; error-bubbling is done iteratively.
    fn complete(core: &mut Core, key: &SkyKey) {
        let mut stack = vec![key.clone()];
        while let Some(k) = stack.pop() {
            core.run.inflight.remove(&k);
            let waiters = core.run.waiters.remove(&k).unwrap_or_default();
            let child_err = match core.graph.nodes.get(&k).and_then(|n| n.value.as_ref()) {
                Some(NodeValue::Error(e)) => Some(e.clone()),
                _ => None,
            };
            for w in waiters {
                let Some(inf) = core.run.inflight.get_mut(&w) else { continue };
                inf.waiting_on.retain(|x| x != &k);
                if let Some(ce) = &child_err {
                    if !inf.handled.contains(&k) {
                        // Bubble: `w` fails with the child's error (unless it is mid-compute).
                        let mut info = ErrorInfo::bubbled(&w, ce);
                        info.root_causes = ce.root_causes.clone();
                        core.run.failed.insert(w.clone(), info.clone());
                        // Remove w from remaining waiters so it is not woken again.
                        let remaining: Vec<SkyKey> = core.run.inflight.get(&w).map(|i| i.waiting_on.clone()).unwrap_or_default();
                        for r in remaining {
                            if let Some(ws) = core.run.waiters.get_mut(&r) {
                                ws.retain(|x| x != &w);
                            }
                        }
                        let groups = {
                            let g = core.run.inflight.get(&w).map(|i| i.groups.clone()).unwrap_or_default();
                            if g.is_empty() { core.graph.nodes.get(&w).map(|n| n.deps.clone()).unwrap_or_default() } else { g }
                        };
                        Self::store_value(core, &w, NodeValue::Error(info), groups);
                        stack.push(w);
                        continue;
                    }
                }
                if inf.waiting_on.is_empty() && !inf.running {
                    core.run.ready.push_back(w);
                }
            }
        }
    }

    // ------------------------------------------------------------------ cycles

    fn detect_cycles(core: &mut Core) {
        // Waiting graph among unfinished nodes.
        let mut order: Vec<SkyKey> = core.run.roots.iter().filter(|r| core.run.inflight.contains_key(*r)).cloned().collect();
        let mut rest: Vec<SkyKey> = core.run.inflight.keys().filter(|k| !order.contains(k)).cloned().collect();
        rest.sort();
        order.extend(rest);
        let mut visited: HashSet<SkyKey> = HashSet::new();
        let mut cycles: Vec<Vec<SkyKey>> = Vec::new();
        let mut seen_ids: HashSet<Vec<SkyKey>> = HashSet::new();
        for start in order {
            if visited.contains(&start) {
                continue;
            }
            // Iterative DFS with explicit path and child iterators.
            let mut path: Vec<SkyKey> = vec![start.clone()];
            let mut idx: Vec<usize> = vec![0];
            visited.insert(start.clone());
            while let Some(top) = path.last().cloned() {
                let edges: Vec<SkyKey> = core.run.inflight.get(&top).map(|i| i.waiting_on.clone()).unwrap_or_default();
                let i = *idx.last().unwrap();
                if i >= edges.len() {
                    path.pop();
                    idx.pop();
                    continue;
                }
                *idx.last_mut().unwrap() += 1;
                let next = edges[i].clone();
                if !core.run.inflight.contains_key(&next) {
                    continue;
                }
                if let Some(pos) = path.iter().position(|p| *p == next) {
                    let cycle: Vec<SkyKey> = path[pos..].to_vec();
                    let mut id = cycle.clone();
                    id.sort();
                    if seen_ids.insert(id) {
                        cycles.push(cycle);
                    }
                    continue;
                }
                if visited.insert(next.clone()) {
                    path.push(next);
                    idx.push(0);
                }
            }
        }
        if cycles.is_empty() {
            return;
        }
        // Phase 1: give every cycle member its own error (no signalling yet).
        let mut members: Vec<SkyKey> = Vec::new();
        for c in &cycles {
            for (j, m) in c.iter().enumerate() {
                if members.contains(m) {
                    continue;
                }
                let info = ErrorInfo {
                    error: None,
                    root_causes: vec![m.clone()],
                    cycles: vec![CycleInfo::new(vec![], CycleInfo::rotated(c, j))],
                };
                core.run.failed.insert(m.clone(), info.clone());
                let groups = {
                    let g = core.run.inflight.get(m).map(|i| i.groups.clone()).unwrap_or_default();
                    if g.is_empty() { core.graph.nodes.get(m).map(|n| n.deps.clone()).unwrap_or_default() } else { g }
                };
                core.graph.nodes.entry(m.clone()).or_insert_with(Node::new);
                Self::store_value(core, m, NodeValue::Error(info), groups);
                members.push(m.clone());
            }
        }
        // Remove members from the waiting structures.
        for m in &members {
            let wo = core.run.inflight.remove(m).map(|i| i.waiting_on).unwrap_or_default();
            for r in wo {
                if let Some(ws) = core.run.waiters.get_mut(&r) {
                    ws.retain(|x| x != m);
                }
            }
        }
        // Phase 2: signal (bubbles errors to everything waiting on a cycle member).
        for m in &members {
            Self::complete(core, m);
        }
        core.run.cycle_checked = false;
    }

    fn fail_stalled(core: &mut Core) {
        let keys: Vec<SkyKey> = core.run.inflight.keys().cloned().collect();
        for k in keys {
            let info = ErrorInfo::from_error(&k, SkyFunctionError::persistent(format!("evaluation of {k} stalled: unresolved dependencies")));
            core.run.failed.insert(k.clone(), info.clone());
            let groups = core.run.inflight.get(&k).map(|i| i.groups.clone()).unwrap_or_default();
            core.graph.nodes.entry(k.clone()).or_insert_with(Node::new);
            Self::store_value(core, &k, NodeValue::Error(info), groups);
        }
        core.run.inflight.clear();
        core.run.waiters.clear();
    }
}

impl RunState {
    fn empty() -> Self {
        Self {
            inflight: HashMap::new(),
            waiters: HashMap::new(),
            ready: VecDeque::new(),
            active: 0,
            roots: Vec::new(),
            done: false,
            aborted: false,
            keep_going: true,
            version: 0,
            stats: EvaluationStats::default(),
            failed: HashMap::new(),
            cycle_checked: false,
        }
    }
}

struct MissingFunction(SkyFunctionName);

impl SkyFunction for MissingFunction {
    fn compute(&self, key: &SkyKey, _env: &mut Environment<'_>) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
        Err(SkyFunctionError::persistent(format!("No SkyFunction registered for {} (key {key})", self.0)))
    }
}
