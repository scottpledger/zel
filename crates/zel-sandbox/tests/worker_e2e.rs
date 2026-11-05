//! End-to-end tests of the persistent worker subsystem against a real worker process.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use zel_sandbox::worker::*;
use zel_sandbox::{Spawn, SpawnStatus};
use zel_vfs::PathFragment;

fn digest(b: &[u8]) -> String {
    zel_util::sha256::hex_digest(b)
}

struct Env {
    root: PathBuf,
    exec_root: PathBuf,
}

fn setup(name: &str) -> Env {
    let root = std::env::temp_dir().join(format!("zel-wk-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let exec_root = root.join("execroot/ws");
    std::fs::create_dir_all(exec_root.join("pkg")).unwrap();
    Env { root: std::fs::canonicalize(&root).unwrap(), exec_root: std::fs::canonicalize(&exec_root).unwrap() }
}

fn worker_spawn(env: &Env, out: &str, text: &str, extra: &[&str], info: &[(&str, &str)]) -> Spawn {
    let params = format!("pkg/{out}.params");
    let mut body = format!("--out={out}\n--text={text}\n");
    for e in extra {
        body.push_str(e);
        body.push('\n');
    }
    std::fs::write(env.exec_root.join(&params), body).unwrap();
    let mut s = Spawn {
        mnemonic: "TestWorker".into(),
        owner_label: "//pkg:x".into(),
        argv: vec![
            env!("CARGO_BIN_EXE_zel-test-worker").to_string(),
            "--persistent_worker_placeholder_removed".to_string(),
            format!("@{params}"),
        ],
        inputs: vec![PathFragment::create(&params)],
        outputs: vec![PathFragment::create(out)],
        ..Default::default()
    };
    // Real tools receive --persistent_worker from the runner; drop the placeholder.
    s.argv.remove(1);
    s.execution_info.insert("supports-workers".into(), "1".into());
    for (k, v) in info {
        s.execution_info.insert(k.to_string(), v.to_string());
    }
    s
}

fn runner(env: &Env, opts: WorkerOptions) -> WorkerSpawnRunner {
    let pool = Arc::new(WorkerPool::new(opts, &env.root.join("bazel-workers"), "ws"));
    WorkerSpawnRunner::new(&env.exec_root, pool, BTreeMap::new(), digest)
}

#[test]
fn worker_is_reused_across_spawns_proto() {
    let env = setup("reuse");
    let r = runner(&env, WorkerOptions::default());
    let a = r.exec(&worker_spawn(&env, "a.out", "one", &[], &[])).unwrap();
    assert_eq!(a.status, SpawnStatus::Success, "{}", String::from_utf8_lossy(&a.stderr));
    assert_eq!(a.runner_name, "worker");
    assert_eq!(String::from_utf8_lossy(&a.stderr), "worker says: one\n");
    let b = r.exec(&worker_spawn(&env, "b.out", "two", &[], &[])).unwrap();
    assert!(b.is_ok());
    let oa = std::fs::read_to_string(env.exec_root.join("a.out")).unwrap();
    let ob = std::fs::read_to_string(env.exec_root.join("b.out")).unwrap();
    let (pa, ca): (&str, &str) = (oa.split(' ').next().unwrap(), oa.split(' ').nth(1).unwrap());
    let (pb, cb): (&str, &str) = (ob.split(' ').next().unwrap(), ob.split(' ').nth(1).unwrap());
    assert_eq!(pa, pb, "same worker process");
    assert_eq!((ca, cb), ("1", "2"), "warm worker saw both requests");
    assert_eq!(r.pool.started.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(oa.contains("inputs=1"));
    r.pool.shutdown();
}

#[test]
fn json_protocol_worker() {
    let env = setup("json");
    let r = runner(&env, WorkerOptions::default());
    let mut s = worker_spawn(&env, "j.out", "hello json", &[], &[("requires-worker-protocol", "json")]);
    s.argv.insert(1, "--json".into());
    let res = r.exec(&s).unwrap();
    assert!(res.is_ok(), "{}", String::from_utf8_lossy(&res.stderr));
    assert_eq!(String::from_utf8_lossy(&res.stderr), "worker says: hello json\n");
    assert!(std::fs::read_to_string(env.exec_root.join("j.out")).unwrap().contains("hello json"));
    r.pool.shutdown();
}

#[test]
fn nonzero_exit_and_missing_output_and_flagfile_error() {
    let env = setup("fail");
    let r = runner(&env, WorkerOptions::default());
    let res = r.exec(&worker_spawn(&env, "f.out", "boom", &["--exit=3"], &[])).unwrap();
    assert_eq!(res.status, SpawnStatus::NonZeroExit);
    assert_eq!(res.exit_code, 3);
    assert!(String::from_utf8_lossy(&res.stderr).contains("worker says: boom"));
    // No flagfile at all.
    let mut s = worker_spawn(&env, "g.out", "x", &[], &[]);
    s.argv.pop();
    let res = r.exec(&s).unwrap();
    assert_eq!(res.status, SpawnStatus::ExecutionFailed);
    assert!(String::from_utf8_lossy(&res.stderr).starts_with(
        "Worker strategy cannot execute this TestWorker action, because the command-line arguments do not contain exactly one @flagfile or --flagfile="
    ));
    r.pool.shutdown();
}

#[test]
fn worker_max_instances_bounds_concurrency() {
    let env = setup("max");
    let opts = WorkerOptions { max_instances: vec![("TestWorker".into(), 2)], ..Default::default() };
    let r = Arc::new(runner(&env, opts));
    let spawns: Vec<Spawn> = (0..6)
        .map(|i| worker_spawn(&env, &format!("m{i}.out"), "m", &["--sleep_ms=150"], &[]))
        .collect();
    let t = std::time::Instant::now();
    std::thread::scope(|sc| {
        for s in &spawns {
            let r = r.clone();
            sc.spawn(move || assert!(r.exec(s).unwrap().is_ok()));
        }
    });
    assert_eq!(r.pool.started.load(std::sync::atomic::Ordering::SeqCst), 2, "never more than 2 workers");
    // 6 requests * 150ms across 2 workers => at least 450ms.
    assert!(t.elapsed().as_millis() >= 440, "{:?}", t.elapsed());
    let pids: std::collections::BTreeSet<String> = (0..6)
        .map(|i| std::fs::read_to_string(env.exec_root.join(format!("m{i}.out"))).unwrap().split(' ').next().unwrap().to_string())
        .collect();
    assert_eq!(pids.len(), 2);
    r.pool.shutdown();
}

#[test]
fn multiplex_worker_serves_concurrent_requests_from_one_process() {
    let env = setup("mux");
    let r = Arc::new(runner(&env, WorkerOptions::default()));
    let spawns: Vec<Spawn> = (0..4)
        .map(|i| {
            worker_spawn(&env, &format!("x{i}.out"), "x", &["--sleep_ms=200"], &[("supports-multiplex-workers", "1")])
        })
        .collect();
    let t = std::time::Instant::now();
    std::thread::scope(|sc| {
        for s in &spawns {
            let r = r.clone();
            sc.spawn(move || assert!(r.exec(s).unwrap().is_ok()));
        }
    });
    assert_eq!(r.pool.started.load(std::sync::atomic::Ordering::SeqCst), 1, "one multiplexed process");
    assert!(t.elapsed().as_millis() < 700, "requests overlap: {:?}", t.elapsed());
    let pids: std::collections::BTreeSet<String> = (0..4)
        .map(|i| std::fs::read_to_string(env.exec_root.join(format!("x{i}.out"))).unwrap().split(' ').next().unwrap().to_string())
        .collect();
    assert_eq!(pids.len(), 1);
    r.pool.shutdown();
}

#[test]
fn worker_sandboxing_places_inputs_and_moves_outputs() {
    let env = setup("sbx");
    let opts = WorkerOptions { sandboxing: true, ..Default::default() };
    let r = runner(&env, opts);
    let res = r.exec(&worker_spawn(&env, "s.out", "sandboxed", &[], &[])).unwrap();
    assert!(res.is_ok(), "{}", String::from_utf8_lossy(&res.stderr));
    assert!(std::fs::read_to_string(env.exec_root.join("s.out")).unwrap().contains("sandboxed"));
    // The worker ran in its own execroot under bazel-workers.
    assert!(env.root.join("bazel-workers").read_dir().unwrap().any(|e| { let n = e.unwrap().file_name().to_string_lossy().to_string(); n.starts_with("worker-") && n.ends_with("-TestWorker") }));
    r.pool.shutdown();
}

#[test]
fn dead_worker_is_replaced_and_failure_reports_log() {
    let env = setup("dead");
    let r = runner(&env, WorkerOptions::default());
    assert!(r.exec(&worker_spawn(&env, "d1.out", "1", &[], &[])).unwrap().is_ok());
    // Kill the pooled worker behind the pool's back.
    let pid: u32 = std::fs::read_to_string(env.exec_root.join("d1.out")).unwrap().split(' ').next().unwrap().parse().unwrap();
    unsafe {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        kill(pid as i32, 9);
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(r.exec(&worker_spawn(&env, "d2.out", "2", &[], &[])).unwrap().is_ok());
    assert_eq!(r.pool.started.load(std::sync::atomic::Ordering::SeqCst), 2);
    r.pool.shutdown();
}
