//! Engine-level tests: versioned nodes, dirty/rebuild, change pruning, restarts, errors,
//! cycles, parallelism.

use crate::cycles::CyclesReporter;
use crate::engine::*;
use crate::{SkyFunction, SkyFunctionError, SkyFunctionName, SkyKey, SkyValue};
use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const T: SkyFunctionName = SkyFunctionName("TEST");

fn key(s: &str) -> SkyKey {
    SkyKey::Custom { function_name: T, argument: s.to_string() }
}

fn name_of(k: &SkyKey) -> String {
    match k {
        SkyKey::Custom { argument, .. } => argument.clone(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Num(i64);

impl SkyValue for Num {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn eq_value(&self, other: &dyn SkyValue) -> bool {
        other.as_any().downcast_ref::<Self>() == Some(self)
    }
}

#[derive(Clone, Debug)]
enum Def {
    Leaf(i64),
    /// Sum of the named deps, requested in one batch.
    Sum(Vec<&'static str>),
    /// Requests deps one by one (forces restarts).
    SeqSum(Vec<&'static str>),
    /// value % 2 of the dep.
    Parity(&'static str),
    Fail(&'static str),
    TransientFail(&'static str),
    /// Sum of deps with failures replaced by -1.
    Tolerant(Vec<&'static str>),
    Panic,
    /// Chooses deps depending on the first dep's value: if > 0 uses `then`, else `other`.
    Branch(&'static str, &'static str, &'static str),
}

struct World {
    defs: Mutex<HashMap<&'static str, Def>>,
    calls: Mutex<Vec<String>>,
    transient_fail_count: AtomicUsize,
}

impl World {
    fn new(defs: Vec<(&'static str, Def)>) -> Arc<World> {
        Arc::new(World {
            defs: Mutex::new(defs.into_iter().collect()),
            calls: Mutex::new(vec![]),
            transient_fail_count: AtomicUsize::new(0),
        })
    }
    fn set(&self, k: &'static str, d: Def) {
        self.defs.lock().unwrap().insert(k, d);
    }
    fn calls_for(&self, k: &str) -> usize {
        self.calls.lock().unwrap().iter().filter(|c| *c == k).count()
    }
    fn clear_calls(&self) {
        self.calls.lock().unwrap().clear();
    }
}

struct TestFn(Arc<World>);

fn num(v: Option<Arc<dyn SkyValue>>) -> i64 {
    v.unwrap().as_any().downcast_ref::<Num>().unwrap().0
}

impl SkyFunction for TestFn {
    fn compute(&self, k: &SkyKey, env: &mut Environment<'_>) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
        let name = name_of(k);
        self.0.calls.lock().unwrap().push(name.clone());
        let def = self.0.defs.lock().unwrap().get(name.as_str()).cloned().unwrap_or_else(|| panic!("no def {name}"));
        let val = match def {
            Def::Leaf(v) => v,
            Def::Sum(deps) => {
                let keys: Vec<SkyKey> = deps.iter().map(|d| key(d)).collect();
                let vals = env.get_values(&keys);
                if env.values_missing() {
                    return Ok(None);
                }
                vals.into_iter().map(num_opt).sum()
            }
            Def::SeqSum(deps) => {
                let mut sum = 0;
                for d in deps {
                    match env.get_value(&key(d)) {
                        Some(v) => sum += num(Some(v)),
                        None => return Ok(None),
                    }
                }
                sum
            }
            Def::Parity(d) => {
                let Some(v) = env.get_value(&key(d)) else { return Ok(None) };
                num(Some(v)) % 2
            }
            Def::Fail(m) => return Err(SkyFunctionError::persistent(m)),
            Def::TransientFail(m) => {
                self.0.transient_fail_count.fetch_add(1, Ordering::SeqCst);
                return Err(SkyFunctionError::transient(m));
            }
            Def::Tolerant(deps) => {
                let mut sum = 0;
                let mut missing = false;
                for d in deps {
                    match env.get_value_or_error(&key(d)) {
                        Ok(Some(v)) => sum += num(Some(v)),
                        Ok(None) => missing = true,
                        Err(_) => sum -= 1,
                    }
                }
                if missing {
                    return Ok(None);
                }
                sum
            }
            Def::Panic => panic!("kaboom"),
            Def::Branch(cond, then, other) => {
                let Some(c) = env.get_value(&key(cond)) else { return Ok(None) };
                let pick = if num(Some(c)) > 0 { then } else { other };
                let Some(v) = env.get_value(&key(pick)) else { return Ok(None) };
                num(Some(v))
            }
        };
        Ok(Some(Arc::new(Num(val))))
    }
}

fn num_opt(v: Option<Arc<dyn SkyValue>>) -> i64 {
    num(v)
}

fn evaluator(w: &Arc<World>) -> InMemoryMemoizingEvaluator {
    let mut e = InMemoryMemoizingEvaluator::new();
    e.register_function(T, Arc::new(TestFn(Arc::clone(w))));
    e
}

fn val(r: &EvaluationResult, k: &str) -> i64 {
    r.get::<Num>(&key(k)).unwrap_or_else(|| panic!("no value for {k}: {:?}", r.errors)).0
}

fn names(keys: &[SkyKey]) -> Vec<String> {
    let mut v: Vec<String> = keys.iter().map(name_of).collect();
    v.sort();
    v
}

#[test]
fn evaluates_a_dag_with_batches_and_sequential_restarts() {
    let w = World::new(vec![
        ("a", Def::Leaf(1)),
        ("b", Def::Leaf(2)),
        ("c", Def::Leaf(3)),
        ("batch", Def::Sum(vec!["a", "b", "c"])),
        ("seq", Def::SeqSum(vec!["a", "b", "c"])),
        ("top", Def::Sum(vec!["batch", "seq"])),
    ]);
    let e = evaluator(&w);
    // One worker makes the schedule deterministic: `seq` runs before its leaves exist.
    let r = e.evaluate_with(&[key("top")], &EvaluationOptions { keep_going: true, threads: 1, cancel: None });
    assert!(!r.has_error());
    assert_eq!(val(&r, "top"), 12);
    // Every leaf computed once; restarts only re-run the dependent functions.
    for l in ["a", "b", "c"] {
        assert_eq!(w.calls_for(l), 1, "{l}");
    }
    assert!(w.calls_for("seq") >= 2, "sequential requests need restarts");
    assert_eq!(e.version(), 1);
    assert_eq!(e.node_count(), 6);
    // Reverse deps are recorded.
    assert_eq!(names(&e.reverse_deps(&key("a"))), ["batch", "seq"]);
    assert_eq!(names(&e.direct_deps(&key("top"))), ["batch", "seq"]);
}

#[test]
fn unchanged_graph_reevaluates_nothing() {
    let w = World::new(vec![("a", Def::Leaf(1)), ("top", Def::Sum(vec!["a"]))]);
    let e = evaluator(&w);
    e.evaluate(&[key("top")]);
    w.clear_calls();
    let r = e.evaluate(&[key("top")]);
    assert_eq!(val(&r, "top"), 1);
    assert!(w.calls.lock().unwrap().is_empty());
    assert_eq!(r.stats.function_calls, 0);
    assert_eq!(r.version, 2);
}

#[test]
fn invalidation_rebuilds_only_the_affected_path() {
    let w = World::new(vec![
        ("a", Def::Leaf(1)),
        ("b", Def::Leaf(10)),
        ("left", Def::Sum(vec!["a"])),
        ("right", Def::Sum(vec!["b"])),
        ("top", Def::Sum(vec!["left", "right"])),
    ]);
    let e = evaluator(&w);
    assert_eq!(val(&e.evaluate(&[key("top")]), "top"), 11);
    w.clear_calls();
    w.set("a", Def::Leaf(5));
    let n = e.invalidate(&[key("a")]);
    assert_eq!(n, 3, "a + left + top");
    assert!(!e.is_clean(&key("top")));
    assert!(e.is_clean(&key("right")));
    let r = e.evaluate(&[key("top")]);
    assert_eq!(val(&r, "top"), 15);
    assert_eq!(names(&r.stats.evaluated), ["a", "left", "top"]);
    assert_eq!(w.calls_for("b"), 0);
    assert_eq!(w.calls_for("right"), 0);
}

#[test]
fn change_pruning_stops_propagation() {
    let w = World::new(vec![
        ("leaf", Def::Leaf(2)),
        ("parity", Def::Parity("leaf")),
        ("top", Def::Sum(vec!["parity"])),
        ("top2", Def::Sum(vec!["top"])),
    ]);
    let e = evaluator(&w);
    assert_eq!(val(&e.evaluate(&[key("top2")]), "top2"), 0);
    w.clear_calls();
    w.set("leaf", Def::Leaf(4)); // still even
    e.invalidate(&[key("leaf")]);
    let r = e.evaluate(&[key("top2")]);
    assert_eq!(val(&r, "top2"), 0);
    assert_eq!(w.calls_for("leaf"), 1);
    assert_eq!(w.calls_for("parity"), 1);
    assert_eq!(w.calls_for("top"), 0, "parity unchanged => top verified clean, not rebuilt");
    assert_eq!(w.calls_for("top2"), 0);
    assert_eq!(names(&r.stats.cleaned), ["top", "top2"]);
    assert!(names(&r.stats.pruned).contains(&"parity".to_string()));
    // Now an odd value does propagate.
    w.clear_calls();
    w.set("leaf", Def::Leaf(5));
    e.invalidate(&[key("leaf")]);
    let r = e.evaluate(&[key("top2")]);
    assert_eq!(val(&r, "top2"), 1);
    assert_eq!(w.calls_for("top"), 1);
    assert_eq!(w.calls_for("top2"), 1);
}

#[test]
fn dirty_node_with_identical_result_is_verified_not_recomputed_downstream() {
    let w = World::new(vec![("a", Def::Leaf(1)), ("mid", Def::Sum(vec!["a"])), ("top", Def::Sum(vec!["mid"]))]);
    let e = evaluator(&w);
    e.evaluate(&[key("top")]);
    w.clear_calls();
    e.invalidate(&[key("a")]); // external "change" that produces the same value
    let r = e.evaluate(&[key("top")]);
    assert_eq!(val(&r, "top"), 1);
    assert_eq!(w.calls_for("a"), 1);
    assert_eq!(w.calls_for("mid"), 0);
    assert_eq!(w.calls_for("top"), 0);
}

#[test]
fn dependency_set_changes_update_reverse_deps() {
    let w = World::new(vec![
        ("sel", Def::Leaf(1)),
        ("x", Def::Leaf(100)),
        ("y", Def::Leaf(200)),
        ("top", Def::Branch("sel", "x", "y")),
    ]);
    let e = evaluator(&w);
    assert_eq!(val(&e.evaluate(&[key("top")]), "top"), 100);
    assert!(e.reverse_deps(&key("y")).is_empty(), "y was never requested");
    w.set("sel", Def::Leaf(0));
    e.invalidate(&[key("sel")]);
    assert_eq!(val(&e.evaluate(&[key("top")]), "top"), 200);
    assert!(e.reverse_deps(&key("x")).is_empty(), "stale edge removed");
    assert_eq!(names(&e.reverse_deps(&key("y"))), ["top"]);
    // Changing x no longer affects top.
    w.clear_calls();
    w.set("x", Def::Leaf(7));
    e.invalidate(&[key("x")]);
    e.evaluate(&[key("top")]);
    assert_eq!(w.calls_for("top"), 0);
}

#[test]
fn inject_precomputed_dirties_dependents_only_on_change() {
    let mut e = InMemoryMemoizingEvaluator::new();
    struct UsesInjected(Arc<AtomicUsize>);
    impl SkyFunction for UsesInjected {
        fn compute(&self, _k: &SkyKey, env: &mut Environment<'_>) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let Some(v) = env.get_value(&SkyKey::Precomputed("opt".into())) else { return Ok(None) };
            Ok(Some(Arc::new(Num(num(Some(v)) * 2))))
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    e.register_function(T, Arc::new(UsesInjected(Arc::clone(&calls))));
    e.inject_precomputed(SkyKey::Precomputed("opt".into()), Arc::new(Num(21)));
    assert_eq!(val(&e.evaluate(&[key("top")]), "top"), 42);
    let c0 = calls.load(Ordering::SeqCst);
    e.inject_precomputed(SkyKey::Precomputed("opt".into()), Arc::new(Num(21)));
    e.evaluate(&[key("top")]);
    assert_eq!(calls.load(Ordering::SeqCst), c0, "same value: nothing dirtied");
    e.inject_precomputed(SkyKey::Precomputed("opt".into()), Arc::new(Num(5)));
    assert_eq!(val(&e.evaluate(&[key("top")]), "top"), 10);
    assert!(calls.load(Ordering::SeqCst) > c0);
}

#[test]
fn keep_going_collects_all_errors_and_independent_values() {
    let w = World::new(vec![
        ("ok", Def::Leaf(1)),
        ("bad", Def::Fail("boom")),
        ("bad_parent", Def::Sum(vec!["bad", "ok"])),
        ("good_root", Def::Sum(vec!["ok"])),
    ]);
    let e = evaluator(&w);
    let r = e.evaluate_with(
        &[key("bad_parent"), key("good_root")],
        &EvaluationOptions { keep_going: true, ..Default::default() },
    );
    assert!(r.has_error());
    assert_eq!(val(&r, "good_root"), 1);
    assert!(r.get::<Num>(&key("bad_parent")).is_none());
    assert_eq!(r.errors[&key("bad_parent")].message, "boom");
    assert_eq!(r.errors[&key("bad")].message, "boom");
    let info = &r.error_infos[&key("bad_parent")];
    assert_eq!(info.root_causes, vec![key("bad")], "root cause identified");
    assert!(!r.aborted);
}

#[test]
fn without_keep_going_the_first_error_aborts() {
    // A long chain of independent slow-ish work after the failure must not all run.
    let mut defs: Vec<(&'static str, Def)> = vec![("bad", Def::Fail("early failure")), ("root_bad", Def::Sum(vec!["bad"]))];
    let leaked: Vec<&'static str> = (0..50).map(|i| &*Box::leak(format!("n{i}").into_boxed_str())).collect();
    for n in &leaked {
        defs.push((n, Def::Leaf(1)));
    }
    defs.push(("root_ok", Def::Sum(leaked.clone())));
    let w = World::new(defs);
    let e = evaluator(&w);
    let r = e.evaluate_with(
        &[key("root_bad"), key("root_ok")],
        &EvaluationOptions { keep_going: false, threads: 1, cancel: None },
    );
    assert!(r.aborted);
    assert_eq!(r.errors[&key("root_bad")].message, "early failure");
    assert!(r.get::<Num>(&key("root_ok")).is_none(), "second root abandoned");
}

#[test]
fn errors_bubble_unless_the_parent_handles_them() {
    let w = World::new(vec![
        ("bad", Def::Fail("nope")),
        ("ok", Def::Leaf(5)),
        ("tolerant", Def::Tolerant(vec!["bad", "ok"])),
        ("strict", Def::Sum(vec!["bad", "ok"])),
        ("above_tolerant", Def::Sum(vec!["tolerant"])),
        ("above_strict", Def::Sum(vec!["strict"])),
    ]);
    let e = evaluator(&w);
    let r = e.evaluate(&[key("above_tolerant"), key("above_strict")]);
    assert_eq!(val(&r, "above_tolerant"), 4, "5 - 1");
    assert!(r.errors.contains_key(&key("above_strict")));
    assert_eq!(r.error_infos[&key("above_strict")].root_causes, vec![key("bad")]);
}

#[test]
fn persistent_errors_are_cached_transient_errors_retried() {
    let w = World::new(vec![
        ("p", Def::Fail("persistent")),
        ("t", Def::TransientFail("flaky")),
        ("top_p", Def::Sum(vec!["p"])),
        ("top_t", Def::Sum(vec!["t"])),
    ]);
    let e = evaluator(&w);
    e.evaluate(&[key("top_p"), key("top_t")]);
    assert_eq!(w.calls_for("p"), 1);
    assert_eq!(w.transient_fail_count.load(Ordering::SeqCst), 1);
    let r = e.evaluate(&[key("top_p"), key("top_t")]);
    assert_eq!(w.calls_for("p"), 1, "persistent error memoized");
    assert_eq!(w.transient_fail_count.load(Ordering::SeqCst), 2, "transient error re-evaluated");
    assert!(r.errors.contains_key(&key("top_t")));
    // Once the transient failure heals, the whole chain recovers.
    w.set("t", Def::Leaf(9));
    let r = e.evaluate(&[key("top_t")]);
    assert_eq!(val(&r, "top_t"), 9);
}

#[test]
fn fixing_a_failed_dependency_rebuilds_failed_parents() {
    let w = World::new(vec![("bad", Def::Fail("broken")), ("top", Def::Sum(vec!["bad"]))]);
    let e = evaluator(&w);
    assert!(e.evaluate(&[key("top")]).has_error());
    w.set("bad", Def::Leaf(3));
    e.invalidate(&[key("bad")]);
    assert_eq!(val(&e.evaluate(&[key("top")]), "top"), 3);
}

#[test]
fn panics_become_errors() {
    let w = World::new(vec![("p", Def::Panic), ("top", Def::Sum(vec!["p"]))]);
    let e = evaluator(&w);
    let r = e.evaluate(&[key("top")]);
    assert!(r.errors[&key("p")].message.contains("kaboom"), "{:?}", r.errors);
    assert!(r.errors.contains_key(&key("top")));
}

#[test]
fn unregistered_function_is_an_error() {
    let e = InMemoryMemoizingEvaluator::new();
    let r = e.evaluate(&[key("x")]);
    assert!(r.errors[&key("x")].message.contains("No SkyFunction registered"));
}

fn pretty(k: &SkyKey) -> String {
    format!("//t:{}", name_of(k))
}

#[test]
fn cycle_detection_reports_path_and_cycle() {
    let w = World::new(vec![
        ("top", Def::Sum(vec!["a"])),
        ("a", Def::Sum(vec!["b"])),
        ("b", Def::Sum(vec!["c"])),
        ("c", Def::Sum(vec!["b"])),
    ]);
    let e = evaluator(&w);
    let r = e.evaluate(&[key("top")]);
    assert!(r.has_error());
    assert_eq!(r.root_cycles.len(), 1);
    let (root, info) = &r.root_cycles[0];
    assert_eq!(root, &key("top"));
    assert_eq!(names(&info.path_to_cycle), ["a", "top"]);
    assert_eq!(info.path_to_cycle[0], key("top"), "path starts at the top-level key");
    assert_eq!(info.cycle, vec![key("b"), key("c")]);
    assert_eq!(r.cycles[0].first(), Some(&key("top")));
    assert_eq!(r.cycles[0].last(), Some(&key("b")), "closed cycle repeats its entry");
    let mut rep = CyclesReporter::new();
    let msg = rep.report(&key("top"), info, &pretty, &|k| (format!("/ws/BUILD:{}", name_of(k)), format!("rule {}", pretty(k))));
    assert_eq!(
        msg,
        "ERROR: /ws/BUILD:b: in rule //t:b: cycle in dependency graph:\n    //t:top\n    //t:a\n.-> //t:b\n|   //t:c\n`-- //t:b"
    );
    // Cycle members and their ancestors all carry the error.
    for k in ["a", "b", "c", "top"] {
        assert!(e.is_clean(&key(k)) || r.error_infos.contains_key(&key(k)));
    }
}

#[test]
fn self_edge_and_shared_cycle_between_roots() {
    let w = World::new(vec![
        ("self", Def::Sum(vec!["self"])),
        ("r1", Def::Sum(vec!["x"])),
        ("r2", Def::Sum(vec!["y"])),
        ("x", Def::Sum(vec!["y"])),
        ("y", Def::Sum(vec!["x"])),
    ]);
    let e = evaluator(&w);
    let r = e.evaluate(&[key("self"), key("r1"), key("r2")]);
    let selfc = r.root_cycles.iter().find(|(k, _)| *k == key("self")).unwrap();
    assert_eq!(selfc.1.cycle, vec![key("self")]);
    let mut rep = CyclesReporter::new();
    let d = |k: &SkyKey| ("loc".to_string(), pretty(k));
    let m = rep.report(&selfc.0, &selfc.1, &pretty, &d);
    assert!(m.ends_with(".-> //t:self [self-edge]\n`--'"), "{m}");
    let mut texts = vec![];
    for (root, info) in r.root_cycles.iter().filter(|(k, _)| *k != key("self")) {
        texts.push(rep.report(root, info, &pretty, &d));
    }
    assert_eq!(texts.len(), 2);
    assert!(texts.iter().any(|t| t.contains("already-reported")), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("cycle in dependency graph:\n")));
}

#[test]
fn cycle_is_cleared_by_breaking_it() {
    let w = World::new(vec![("a", Def::Sum(vec!["b"])), ("b", Def::Sum(vec!["a"]))]);
    let e = evaluator(&w);
    assert!(e.evaluate(&[key("a")]).has_error());
    w.set("b", Def::Leaf(4));
    e.invalidate(&[key("b")]);
    let r = e.evaluate(&[key("a")]);
    assert_eq!(val(&r, "a"), 4);
}

#[test]
fn large_layered_graph_evaluates_in_parallel_and_updates_incrementally() {
    // 15 layers x 25 nodes; each node sums 3 nodes of the previous layer.
    let layers = 15usize;
    let width = 25usize;
    let mut names_v: Vec<Vec<&'static str>> = vec![];
    for l in 0..layers {
        let mut row = vec![];
        for i in 0..width {
            row.push(&*Box::leak(format!("L{l}_{i}").into_boxed_str()));
        }
        names_v.push(row);
    }
    let mut defs = vec![];
    for i in 0..width {
        defs.push((names_v[0][i], Def::Leaf(1)));
    }
    for l in 1..layers {
        for i in 0..width {
            let deps = vec![names_v[l - 1][i], names_v[l - 1][(i + 1) % width], names_v[l - 1][(i + 2) % width]];
            defs.push((names_v[l][i], Def::Sum(deps)));
        }
    }
    let root: &'static str = "ROOT";
    defs.push((root, Def::Sum(names_v[layers - 1].clone())));
    let w = World::new(defs);
    let e = evaluator(&w);
    let r = e.evaluate_with(&[key(root)], &EvaluationOptions { keep_going: true, threads: 8, cancel: None });
    assert!(!r.has_error(), "{:?}", r.errors);
    assert_eq!(val(&r, root), 25 * 3i64.pow(14));
    let total = (layers * width) + 1;
    assert_eq!(e.node_count(), total);
    for l in 0..layers {
        for i in 0..width {
            assert_eq!(w.calls_for(names_v[l][i]) >= 1, true);
        }
    }
    // Incremental: tweak one leaf; only its cone is recomputed.
    w.clear_calls();
    w.set(names_v[0][0], Def::Leaf(2));
    e.invalidate(&[key(names_v[0][0])]);
    let r2 = e.evaluate_with(&[key(root)], &EvaluationOptions { keep_going: true, threads: 8, cancel: None });
    assert!(r2.stats.evaluated.len() < total);
    assert!(!r2.stats.evaluated.is_empty());
    assert_eq!(w.calls_for(names_v[0][5]), 0, "untouched leaf not recomputed");
}

#[test]
fn smaller_parallel_graph_has_exact_values() {
    let mut defs: Vec<(&'static str, Def)> = vec![];
    let leaves: Vec<&'static str> = (0..200).map(|i| &*Box::leak(format!("leaf{i}").into_boxed_str())).collect();
    for (i, l) in leaves.iter().enumerate() {
        defs.push((l, Def::Leaf(i as i64)));
    }
    let mids: Vec<&'static str> = (0..20).map(|i| &*Box::leak(format!("mid{i}").into_boxed_str())).collect();
    for (i, m) in mids.iter().enumerate() {
        defs.push((m, Def::Sum(leaves[i * 10..(i + 1) * 10].to_vec())));
    }
    defs.push(("total", Def::Sum(mids.clone())));
    let w = World::new(defs);
    let e = evaluator(&w);
    let r = e.evaluate_with(&[key("total")], &EvaluationOptions { keep_going: true, threads: 16, cancel: None });
    assert_eq!(val(&r, "total"), (0..200).sum::<i64>());
    assert_eq!(r.stats.evaluated.len(), 221);
}

#[test]
fn delete_functions_rebuilds_dependents() {
    const OTHER: SkyFunctionName = SkyFunctionName("OTHER");
    let mut e = InMemoryMemoizingEvaluator::new();
    struct F(&'static str, Arc<AtomicUsize>);
    impl SkyFunction for F {
        fn compute(&self, k: &SkyKey, env: &mut Environment<'_>) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
            self.1.fetch_add(1, Ordering::SeqCst);
            if self.0 == "leaf" {
                return Ok(Some(Arc::new(Num(1))));
            }
            let dep = SkyKey::Custom { function_name: SkyFunctionName("OTHER"), argument: format!("dep-of-{}", name_of(k)) };
            let Some(v) = env.get_value(&dep) else { return Ok(None) };
            Ok(Some(Arc::new(Num(num(Some(v)) + 1))))
        }
    }
    let leaf_calls = Arc::new(AtomicUsize::new(0));
    let top_calls = Arc::new(AtomicUsize::new(0));
    e.register_function(OTHER, Arc::new(F("leaf", Arc::clone(&leaf_calls))));
    e.register_function(T, Arc::new(F("top", Arc::clone(&top_calls))));
    assert_eq!(val(&e.evaluate(&[key("x")]), "x"), 2);
    assert_eq!(e.delete_functions(&[OTHER]), 1);
    assert!(!e.is_clean(&key("x")));
    assert_eq!(val(&e.evaluate(&[key("x")]), "x"), 2);
    assert_eq!(leaf_calls.load(Ordering::SeqCst), 2, "deleted node recomputed");
    assert_eq!(top_calls.load(Ordering::SeqCst), 4, "2 calls (restart) per evaluation");
}

#[test]
fn cancellation_stops_evaluation() {
    struct Slow;
    impl SkyFunction for Slow {
        fn compute(&self, _k: &SkyKey, _env: &mut Environment<'_>) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
            std::thread::sleep(std::time::Duration::from_millis(30));
            Ok(Some(Arc::new(Num(1))))
        }
    }
    let mut e = InMemoryMemoizingEvaluator::new();
    e.register_function(T, Arc::new(Slow));
    let cancel = Arc::new(AtomicBool::new(false));
    let roots: Vec<SkyKey> = (0..200).map(|i| key(&format!("s{i}"))).collect();
    let c2 = Arc::clone(&cancel);
    let h = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(60));
        c2.store(true, Ordering::SeqCst);
    });
    let r = e.evaluate_with(&roots, &EvaluationOptions { keep_going: true, threads: 2, cancel: Some(cancel) });
    h.join().unwrap();
    assert!(r.aborted);
    assert!(r.values.len() < 200);
}
