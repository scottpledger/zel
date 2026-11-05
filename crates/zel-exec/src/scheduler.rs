//! Parallel, dependency-aware action scheduler (`--jobs`, `--keep_going`), mirroring the
//! behaviour of Bazel's `SkyframeActionExecutor` for a fixed, already-analyzed action graph.

use crate::{execute_action_with, ActionExecutionOutcome, CompactPersistentActionCache, ExecOptions};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Condvar, Mutex};
use zel_analysis::ActionAnalysisMetadata;
use zel_events::Reporter;
use zel_vfs::{FileSystem, PathFragment};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionState {
    /// Never started (build stopped early or a dependency failed).
    NotRun,
    Succeeded,
    Failed,
}

#[derive(Debug)]
pub struct ScheduleResult {
    /// One entry per *unique* action (deduplicated by first output), in input order.
    pub actions: Vec<ActionResult>,
    /// Indices into `actions` in completion order.
    pub completion_order: Vec<usize>,
    pub critical_path_ms: u64,
    /// Set when the action graph contains a dependency cycle.
    pub cycle: Option<Vec<String>>,
}

#[derive(Debug)]
pub struct ActionResult {
    pub state: ActionState,
    pub outcome: Option<ActionExecutionOutcome>,
    pub io_error: Option<String>,
    pub is_spawn: bool,
    /// Exec paths of all outputs of this action.
    pub outputs: Vec<String>,
}

impl ScheduleResult {
    pub fn executed(&self) -> usize {
        self.actions
            .iter()
            .filter(|a| matches!(&a.outcome, Some(o) if !o.cache_hit))
            .count()
    }
    pub fn cache_hits(&self) -> usize {
        self.actions
            .iter()
            .filter(|a| matches!(&a.outcome, Some(o) if o.cache_hit))
            .count()
    }
    pub fn failed(&self) -> usize {
        self.actions.iter().filter(|a| a.state == ActionState::Failed).count()
    }
    /// True if the action producing `output_exec_path` ran (or was cached) successfully.
    pub fn output_ok(&self, output_exec_path: &str) -> Option<bool> {
        self.actions
            .iter()
            .find(|a| a.outputs.iter().any(|o| o == output_exec_path))
            .map(|a| a.state == ActionState::Succeeded)
    }
}

struct State {
    ready: VecDeque<usize>,
    pending_deps: Vec<usize>,
    states: Vec<ActionState>,
    results: Vec<Option<ActionExecutionOutcome>>,
    io_errors: Vec<Option<String>>,
    finished_at: Vec<u64>,
    running: usize,
    stop: bool,
    completion: Vec<usize>,
    done_or_dead: usize,
}

/// Runs all `actions` with at most `jobs` concurrent workers. Actions are started only after all
/// actions producing their inputs succeeded. Without `keep_going`, the first failure stops new
/// actions from starting (running ones finish); with it, independent actions continue.
pub fn execute_actions(
    actions: &[&ActionAnalysisMetadata],
    jobs: usize,
    keep_going: bool,
    execroot: &PathFragment,
    fs: &dyn FileSystem,
    cache: &CompactPersistentActionCache,
    reporter: &Reporter,
    opts: &ExecOptions,
) -> ScheduleResult {
    if let Some(ref p) = opts.explain {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if !p.exists() {
            let _ = std::fs::write(p, "Build options: \n");
        }
    }
    let jobs = if opts.experimental_async_execution && opts.async_execution_max_concurrent_actions > 0 {
        jobs.max(opts.async_execution_max_concurrent_actions)
    } else {
        jobs
    };
    // Deduplicate shared actions (same first output).
    let mut seen = HashSet::new();
    let mut uniq: Vec<&ActionAnalysisMetadata> = Vec::new();
    for a in actions {
        let key = a
            .outputs()
            .first()
            .map(|o| o.exec_path.path_string().to_string())
            .unwrap_or_else(|| a.action_key());
        if seen.insert(key) {
            uniq.push(a);
        }
    }
    let n = uniq.len();
    let mut producer: HashMap<String, usize> = HashMap::new();
    for (i, a) in uniq.iter().enumerate() {
        for o in a.outputs() {
            producer.insert(o.exec_path.path_string().to_string(), i);
        }
    }
    let mut deps: Vec<HashSet<usize>> = vec![HashSet::new(); n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, a) in uniq.iter().enumerate() {
        for inp in a.inputs() {
            if let Some(&p) = producer.get(inp.exec_path.path_string()) {
                if p != i && deps[i].insert(p) {
                    dependents[p].push(i);
                }
            }
        }
        // Symlink actions implicitly depend on the producer of their target.
        if let ActionAnalysisMetadata::SymlinkAction { input_path, .. } = a {
            if let Some(&p) = producer.get(input_path.path_string()) {
                if p != i && deps[i].insert(p) {
                    dependents[p].push(i);
                }
            }
        }
    }
    let pending: Vec<usize> = deps.iter().map(|d| d.len()).collect();
    let ready: VecDeque<usize> = (0..n).filter(|&i| pending[i] == 0).collect();
    let state = Mutex::new(State {
        ready,
        pending_deps: pending,
        states: vec![ActionState::NotRun; n],
        results: (0..n).map(|_| None).collect(),
        io_errors: vec![None; n],
        finished_at: vec![0; n],
        running: 0,
        stop: false,
        completion: Vec::new(),
        done_or_dead: 0,
    });
    let cv = Condvar::new();
    let workers = jobs.max(1).min(n.max(1));

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let idx = {
                    let mut st = state.lock().unwrap();
                    loop {
                        if st.stop && !keep_going {
                            // No new actions; wake others.
                            cv.notify_all();
                            if st.running == 0 || st.ready.is_empty() {
                                return;
                            }
                            return;
                        }
                        if let Some(i) = st.ready.pop_front() {
                            st.running += 1;
                            break i;
                        }
                        if st.running == 0 {
                            cv.notify_all();
                            return;
                        }
                        st = cv.wait(st).unwrap();
                    }
                };
                let listening = reporter.has_action_listeners();
                let info = listening.then(|| action_info(uniq[idx]));
                let start_millis = zel_events::now_millis();
                if let Some(i) = &info {
                    reporter.action_started(i);
                }
                let res = execute_action_with(uniq[idx], execroot, fs, cache, reporter, opts);
                if let Some(i) = &info {
                    reporter.action_finished(i, &action_result_info(&res, start_millis));
                }
                let mut st = state.lock().unwrap();
                st.running -= 1;
                let (ok, outcome, err) = match res {
                    Ok(o) => (o.succeeded(), Some(o), None),
                    Err(e) => (false, None, Some(e.to_string())),
                };
                st.results[idx] = outcome;
                st.io_errors[idx] = err;
                st.completion.push(idx);
                st.done_or_dead += 1;
                let dep_finish = deps[idx].iter().map(|&d| st.finished_at[d]).max().unwrap_or(0);
                let dur = st.results[idx].as_ref().map(|o| o.wall_time_ms).unwrap_or(0);
                st.finished_at[idx] = dep_finish + dur;
                if ok {
                    st.states[idx] = ActionState::Succeeded;
                    for &d in &dependents[idx] {
                        st.pending_deps[d] -= 1;
                        if st.pending_deps[d] == 0 {
                            st.ready.push_back(d);
                        }
                    }
                } else {
                    st.states[idx] = ActionState::Failed;
                    if !keep_going {
                        st.stop = true;
                    }
                }
                cv.notify_all();
            });
        }
    });

    let st = state.into_inner().unwrap();
    let cycle = if st.states.iter().all(|s| *s != ActionState::NotRun) || st.stop {
        None
    } else {
        // Remaining NotRun actions whose deps never failed => cycle among them.
        let stuck: Vec<usize> = (0..n)
            .filter(|&i| {
                st.states[i] == ActionState::NotRun
                    && deps[i].iter().all(|&d| st.states[d] != ActionState::Failed)
                    && st.pending_deps[i] > 0
                    && deps[i].iter().any(|&d| st.states[d] == ActionState::NotRun)
            })
            .collect();
        if stuck.is_empty() {
            None
        } else {
            Some(
                stuck
                    .iter()
                    .map(|&i| {
                        uniq[i]
                            .outputs()
                            .first()
                            .map(|o| o.exec_path.path_string().to_string())
                            .unwrap_or_default()
                    })
                    .collect(),
            )
        }
    };
    let critical = st.finished_at.iter().copied().max().unwrap_or(0);
    let actions_out = (0..n)
        .map(|i| ActionResult {
            state: st.states[i],
            outputs: uniq[i]
                .outputs()
                .iter()
                .map(|o| o.exec_path.path_string().to_string())
                .collect(),
            outcome: None,
            io_error: st.io_errors[i].clone(),
            is_spawn: matches!(uniq[i], ActionAnalysisMetadata::SpawnAction { .. }),
        })
        .collect::<Vec<_>>();
    let mut results = st.results;
    let mut actions_out = actions_out;
    for (i, a) in actions_out.iter_mut().enumerate() {
        a.outcome = results[i].take();
    }
    ScheduleResult {
        actions: actions_out,
        completion_order: st.completion,
        critical_path_ms: critical,
        cycle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use zel_analysis::Artifact;
    use zel_cmdline::Label;
    use zel_vfs::LocalFileSystem;

    fn root(name: &str) -> PathFragment {
        let d = std::env::temp_dir().join(format!("zel-sched-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        PathFragment::create(d.to_str().unwrap())
    }

    fn out(rel: &str) -> Artifact {
        Artifact::derived("k8-fastbuild", PathFragment::create(rel.trim_start_matches("bazel-out/k8-fastbuild/bin/")), Label::parse_canonical("//pkg:t").unwrap())
    }

    fn sh(script: &str, inputs: Vec<Artifact>, outputs: Vec<Artifact>) -> ActionAnalysisMetadata {
        ActionAnalysisMetadata::SpawnAction {
            owner: Label::parse_canonical("//pkg:t").unwrap(),
            mnemonic: "Genrule".into(),
            progress_message: "Executing genrule //pkg:t".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            env: BTreeMap::new(),
            inherited_env: Default::default(),
            unset_env: Default::default(),
            inputs,
            outputs,
            execution_info: BTreeMap::new(),
            java_compile: None,
        }
    }

    fn run(actions: &[ActionAnalysisMetadata], jobs: usize, kg: bool, r: &PathFragment) -> ScheduleResult {
        let refs: Vec<&ActionAnalysisMetadata> = actions.iter().collect();
        execute_actions(&refs, jobs, kg, r, &LocalFileSystem::default(), &CompactPersistentActionCache::new(), &Reporter::new(), &ExecOptions::default())
    }

    #[test]
    fn failure_message_and_missing_outputs() {
        let r = root("fail");
        let a = sh("echo oops >&2; exit 1", vec![], vec![out("bazel-out/k8-fastbuild/bin/pkg/a.txt")]);
        let res = run(&[a], 2, false, &r);
        let o = res.actions[0].outcome.as_ref().unwrap();
        let f = o.failure.as_ref().unwrap();
        assert_eq!(f.exit_code, 1);
        assert!(f.messages[0].contains("Executing genrule //pkg:t failed: (Exit 1): sh failed: error executing Genrule command (from genrule rule target //pkg:t)"), "{:?}", f.messages);
        assert_eq!(o.stderr, b"oops\n");

        let a = sh("true", vec![], vec![out("bazel-out/k8-fastbuild/bin/pkg/b.txt")]);
        let res = run(&[a], 1, false, &r);
        let f = res.actions[0].outcome.as_ref().unwrap().failure.as_ref().unwrap();
        assert!(f.messages[0].contains("output 'pkg/b.txt' was not created"), "{:?}", f.messages);
        assert!(f.messages[1].contains("not all outputs were created or valid"));
    }

    #[test]
    fn dependencies_order_and_dependents_skipped_on_failure() {
        let r = root("deps");
        let a_out = out("bazel-out/k8-fastbuild/bin/pkg/a");
        let b_out = out("bazel-out/k8-fastbuild/bin/pkg/b");
        let c_out = out("bazel-out/k8-fastbuild/bin/pkg/c");
        // b depends on a; listed first to prove ordering is by dependency, not input order.
        let b = sh("cat bazel-out/k8-fastbuild/bin/pkg/a > bazel-out/k8-fastbuild/bin/pkg/b", vec![a_out.clone()], vec![b_out.clone()]);
        let a = sh("echo A > bazel-out/k8-fastbuild/bin/pkg/a", vec![], vec![a_out.clone()]);
        let res = run(&[b, a], 4, false, &r);
        assert_eq!(res.failed(), 0);
        assert_eq!(std::fs::read_to_string(format!("{}/bazel-out/k8-fastbuild/bin/pkg/b", r)).unwrap(), "A\n");

        // a fails => b (dependent) never runs; independent c runs with keep_going.
        let a = sh("exit 2", vec![], vec![a_out.clone()]);
        let b = sh("echo B > bazel-out/k8-fastbuild/bin/pkg/b", vec![a_out.clone()], vec![b_out.clone()]);
        let c = sh("echo C > bazel-out/k8-fastbuild/bin/pkg/c", vec![], vec![c_out.clone()]);
        let r2 = root("deps2");
        let res = run(&[a, b, c], 1, true, &r2);
        assert_eq!(res.actions[0].state, ActionState::Failed);
        assert_eq!(res.actions[1].state, ActionState::NotRun);
        assert_eq!(res.actions[2].state, ActionState::Succeeded);
        assert_eq!(res.output_ok("bazel-out/k8-fastbuild/bin/pkg/b"), Some(false));
    }

    #[test]
    fn stop_on_first_failure_without_keep_going_and_parallelism() {
        let r = root("nokg");
        let mk = |n: &str, s: &str| sh(s, vec![], vec![out(&format!("bazel-out/k8-fastbuild/bin/pkg/{n}"))]);
        let actions = vec![mk("f", "exit 1"), mk("g", "echo g > bazel-out/k8-fastbuild/bin/pkg/g")];
        let res = run(&actions, 1, false, &r);
        assert_eq!(res.actions[0].state, ActionState::Failed);
        assert_eq!(res.actions[1].state, ActionState::NotRun);

        // 4 actions each sleeping 0.4s finish in well under 1.6s with jobs=4.
        let r = root("par");
        let actions: Vec<_> = (0..4)
            .map(|i| mk(&format!("p{i}"), &format!("sleep 0.4; echo x > bazel-out/k8-fastbuild/bin/pkg/p{i}")))
            .collect();
        let t = std::time::Instant::now();
        let res = run(&actions, 4, false, &r);
        assert_eq!(res.failed(), 0);
        assert!(t.elapsed().as_millis() < 1300, "{:?}", t.elapsed());
        let t = std::time::Instant::now();
        let r = root("ser");
        let res = run(&actions, 1, false, &r);
        assert_eq!(res.executed(), 4);
        assert!(t.elapsed().as_millis() >= 1600);
    }

    #[test]
    fn action_cache_skips_second_run_and_detects_tampered_output() {
        let r = root("cache");
        let o = out("bazel-out/k8-fastbuild/bin/pkg/z");
        let act = sh("echo hi > bazel-out/k8-fastbuild/bin/pkg/z", vec![], vec![o]);
        let cache = CompactPersistentActionCache::new();
        let fs = LocalFileSystem::default();
        let rep = Reporter::new();
        let opts = ExecOptions::default();
        let a = [&act];
        let r1 = execute_actions(&a, 1, false, &r, &fs, &cache, &rep, &opts);
        assert_eq!(r1.executed(), 1);
        let r2 = execute_actions(&a, 1, false, &r, &fs, &cache, &rep, &opts);
        assert_eq!(r2.cache_hits(), 1);
        std::fs::write(format!("{}/bazel-out/k8-fastbuild/bin/pkg/z", r), "tampered").unwrap();
        let r3 = execute_actions(&a, 1, false, &r, &fs, &cache, &rep, &opts);
        assert_eq!(r3.executed(), 1);
        assert_eq!(std::fs::read_to_string(format!("{}/bazel-out/k8-fastbuild/bin/pkg/z", r)).unwrap(), "hi\n");
    }
}

/// Plain-data view of an action for [`zel_events::ActionListener`]s.
fn action_info(a: &ActionAnalysisMetadata) -> zel_events::ActionInfo {
    let outputs: Vec<String> = a.outputs().iter().map(|o| o.exec_path.path_string().to_string()).collect();
    let mut info = zel_events::ActionInfo {
        id: outputs.first().cloned().unwrap_or_default(),
        owner: a.owner().canonical_form(),
        mnemonic: a.mnemonic().to_string(),
        inputs: a.inputs().iter().map(|i| (i.exec_path.path_string().to_string(), i.is_source)).collect(),
        outputs,
        ..Default::default()
    };
    if let ActionAnalysisMetadata::SpawnAction { argv, env, progress_message, .. } = a {
        info.is_spawn = true;
        info.argv = argv.clone();
        info.env = env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        info.progress_message = progress_message.clone();
    }
    if info.progress_message.is_empty() {
        info.progress_message = format!("{} {}", info.mnemonic, info.owner);
    }
    info
}

fn action_result_info(res: &std::io::Result<ActionExecutionOutcome>, start_millis: i64) -> zel_events::ActionResultInfo {
    match res {
        Ok(o) => zel_events::ActionResultInfo {
            cache_hit: o.cache_hit,
            success: o.succeeded(),
            exit_code: o.exit_code,
            runner: o.runner.clone(),
            wall_time_ms: o.wall_time_ms,
            start_millis,
            stdout: o.stdout.clone(),
            stderr: o.stderr.clone(),
            error: None,
        },
        Err(e) => zel_events::ActionResultInfo {
            success: false,
            exit_code: 1,
            start_millis,
            error: Some(e.to_string()),
            ..Default::default()
        },
    }
}
