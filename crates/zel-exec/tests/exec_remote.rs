//! The remote strategy, the remote cache wrapped around local strategies, and dynamic execution
//! (local vs remote race) through `ExecRuntime`, against the in-process fake REAPI server.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zel_events::Reporter;
use zel_exec::resources::ResourceManager;
use zel_exec::strategy::{SpawnStrategyRegistry, StrategyAvailability, StrategyOptions};
use zel_exec::{DynamicOptions, ExecRuntime};
use zel_proto::reapi::{ActionResult, OutputFile};
use zel_remote::fake_server::FakeReapiServer;
use zel_remote::options::{options_from_args, RemoteOptions};
use zel_remote::spawn_runner::{LocalSpawnRunner, RemoteSpawnRunner};
use zel_sandbox::sandbox::{SandboxKind, SandboxOptions, SandboxRunner};
use zel_sandbox::{Spawn, SpawnStatus};
use zel_vfs::{LocalFileSystem, PathFragment};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zel-xr-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("ob/execroot/_main")).unwrap();
    std::fs::canonicalize(&d).unwrap()
}

fn runtime(
    root: &Path,
    server: &FakeReapiServer,
    exec: bool,
    flags: &[&str],
    opts: StrategyOptions,
    dynamic: DynamicOptions,
) -> Arc<ExecRuntime> {
    let er = root.join("ob/execroot/_main");
    let av = StrategyAvailability {
        process_wrapper_sandbox: true,
        remote: true,
        remote_exec: exec,
        dynamic: true,
        ..Default::default()
    };
    let registry = SpawnStrategyRegistry::build(&opts, &av).unwrap();
    let sandbox = SandboxRunner::new(
        SandboxKind::ProcessWrapper,
        &er,
        &root.join("ob/sandbox"),
        SandboxOptions::default(),
        BTreeMap::new(),
        None,
    );
    let ropts = RemoteOptions::from_source(&options_from_args(flags)).unwrap();
    let remote = RemoteSpawnRunner::new(ropts, Some(Arc::new(server.client(exec))), Arc::new(LocalSpawnRunner)).unwrap();
    Arc::new(ExecRuntime {
        registry,
        sandbox_runners: vec![sandbox],
        worker: None,
        resources: ResourceManager::new(8.0, 1e9, BTreeMap::new(), 0),
        sandbox_debug: false,
        kill_delay: None,
        remote: Some(Arc::new(remote)),
        dynamic,
        delay_local: Default::default(),
        allowed_local_actions_regex: None,
        remote_setup_error: None,
    })
}

fn spawn(script: &str, inputs: &[&str], outputs: &[&str], info: &[(&str, &str)]) -> Spawn {
    Spawn {
        mnemonic: "Gen".into(),
        owner_label: "//t:t".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        inputs: inputs.iter().map(|s| PathFragment::create(s)).collect(),
        outputs: outputs.iter().map(|s| PathFragment::create(s)).collect(),
        execution_info: info.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        ..Default::default()
    }
}

fn execroot(root: &Path) -> PathFragment {
    PathFragment::create(root.join("ob/execroot/_main").to_str().unwrap())
}

fn run(rt: &Arc<ExecRuntime>, root: &Path, s: &Spawn) -> Result<zel_sandbox::SpawnResult, String> {
    rt.execute(s, "Doing Gen //t:t", &execroot(root), &LocalFileSystem::default(), &Reporter::new())
        .map(|r| r.unwrap())
}

/// An executor that produces `out/a.txt` containing `content` after `delay_ms`.
fn install_executor(server: &FakeReapiServer, content: &'static str, delay_ms: u64) {
    server.set_executor(move |ctx| {
        std::thread::sleep(Duration::from_millis(delay_ms));
        let out = ctx.server.put_blob(content.as_bytes());
        let path = ctx.command.output_paths.first().or(ctx.command.output_files.first()).expect("declared output").clone();
        Ok(ActionResult {
            output_files: vec![OutputFile { path, digest: Some(out), ..Default::default() }],
            ..Default::default()
        })
    });
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join("ob/execroot/_main").join(rel)).unwrap()
}

#[test]
fn remote_strategy_executes_remotely_then_hits_the_remote_cache() {
    let root = tmp("remote");
    let server = FakeReapiServer::new();
    install_executor(&server, "remote-made", 0);
    let so = StrategyOptions { spawn_strategy: vec!["remote".into()], ..Default::default() };
    let rt = runtime(&root, &server, true, &["--remote_download_outputs=all"], so, DynamicOptions::default());
    let s = spawn("echo local > out/a.txt", &[], &["out/a.txt"], &[]);
    let r = run(&rt, &root, &s).unwrap();
    assert_eq!(r.status, SpawnStatus::Success, "{}", String::from_utf8_lossy(&r.stderr));
    assert_eq!(r.runner_name, "remote");
    assert_eq!(read(&root, "out/a.txt"), "remote-made");
    assert_eq!(server.executions(), 1);
    std::fs::remove_file(root.join("ob/execroot/_main/out/a.txt")).unwrap();
    let r = run(&rt, &root, &s).unwrap();
    assert_eq!(r.runner_name, "remote cache hit");
    assert_eq!(server.executions(), 1);
}

#[test]
fn remote_is_not_usable_without_an_executor_or_for_local_spawns() {
    let root = tmp("noexec");
    let server = FakeReapiServer::new();
    let so = StrategyOptions { spawn_strategy: vec!["remote".into()], ..Default::default() };
    let rt = runtime(&root, &server, false, &[], so, DynamicOptions::default());
    let e = run(&rt, &root, &spawn("true", &[], &[], &[])).unwrap_err();
    assert!(e.contains("Gen spawn cannot be executed with any of the available strategies: [remote]"), "{e}");

    let so = StrategyOptions { spawn_strategy: vec!["remote".into(), "local".into()], ..Default::default() };
    let rt = runtime(&root, &server, true, &[], so, DynamicOptions::default());
    install_executor(&server, "remote-made", 0);
    // `local` execution requirement: skips remote, runs locally.
    let r = run(&rt, &root, &spawn("echo here > out/l.txt", &[], &["out/l.txt"], &[("local", "")])).unwrap();
    assert_eq!(r.runner_name, "local");
    assert_eq!(read(&root, "out/l.txt"), "here\n");
    assert_eq!(server.executions(), 0);
}

#[test]
fn remote_cache_wraps_local_strategies() {
    let root = tmp("cache");
    let server = FakeReapiServer::new();
    let so = StrategyOptions { spawn_strategy: vec!["local".into()], ..Default::default() };
    let rt = runtime(&root, &server, false, &["--remote_download_outputs=all"], so, DynamicOptions::default());
    let s = spawn("echo built >> out/a.txt; echo hi", &[], &["out/a.txt"], &[]);
    let r = run(&rt, &root, &s).unwrap();
    assert_eq!(r.runner_name, "local");
    assert_eq!(read(&root, "out/a.txt"), "built\n");
    assert_eq!(server.ac_count(), 1, "result uploaded");
    std::fs::remove_file(root.join("ob/execroot/_main/out/a.txt")).unwrap();
    let r = run(&rt, &root, &s).unwrap();
    assert_eq!(r.runner_name, "remote cache hit");
    assert_eq!(read(&root, "out/a.txt"), "built\n", "restored from cache, script not rerun");
    assert_eq!(String::from_utf8_lossy(&r.stdout), "hi\n");
}

fn dynamic_opts() -> StrategyOptions {
    StrategyOptions { spawn_strategy: vec!["dynamic".into()], ..Default::default() }
        .with_dynamic(&["sandboxed".to_string()], &[])
        .unwrap()
}

#[test]
fn dynamic_remote_wins_and_kills_the_slow_local_branch() {
    let root = tmp("dynremote");
    let server = FakeReapiServer::new();
    install_executor(&server, "from-remote", 20);
    let rt = runtime(&root, &server, true, &["--remote_download_outputs=all"], dynamic_opts(), DynamicOptions::default());
    // The local branch would take 20s and write a different result.
    let s = spawn("sleep 20; echo from-local > out/a.txt", &[], &["out/a.txt"], &[]);
    let t = Instant::now();
    let r = run(&rt, &root, &s).unwrap();
    assert!(t.elapsed() < Duration::from_secs(10), "local branch was not cancelled: {:?}", t.elapsed());
    assert_eq!(r.status, SpawnStatus::Success);
    assert_eq!(r.runner_name, "remote");
    assert_eq!(read(&root, "out/a.txt"), "from-remote");
}

#[test]
fn dynamic_local_wins_and_remote_never_writes_outputs() {
    let root = tmp("dynlocal");
    let server = FakeReapiServer::new();
    install_executor(&server, "from-remote", 700);
    let rt = runtime(&root, &server, true, &["--remote_download_outputs=all"], dynamic_opts(), DynamicOptions::default());
    let s = spawn("echo from-local > out/a.txt", &[], &["out/a.txt"], &[]);
    let r = run(&rt, &root, &s).unwrap();
    assert_eq!(r.runner_name, "processwrapper-sandbox");
    assert_eq!(read(&root, "out/a.txt"), "from-local\n");
    // Let the abandoned remote branch finish: it must not touch the exec root.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(read(&root, "out/a.txt"), "from-local\n");
}

#[test]
fn dynamic_with_only_one_branch_runs_it_directly() {
    let root = tmp("dynone");
    let server = FakeReapiServer::new();
    install_executor(&server, "from-remote", 0);
    let rt = runtime(&root, &server, true, &["--remote_download_outputs=all"], dynamic_opts(), DynamicOptions::default());
    let r = run(&rt, &root, &spawn("echo x > out/a.txt", &[], &["out/a.txt"], &[("no-remote", "")])).unwrap();
    assert_eq!(r.runner_name, "processwrapper-sandbox");
    let r = run(&rt, &root, &spawn("echo x > out/b.txt", &[], &["out/b.txt"], &[("no-local", "")])).unwrap();
    assert_eq!(r.runner_name, "remote");
    let e = run(&rt, &root, &spawn("true", &[], &[], &[("no-local", ""), ("no-remote", "")])).unwrap_err();
    // `DynamicSpawnStrategy.canExec` is false, so the resolver reports it.
    assert!(e.starts_with("Gen spawn cannot be executed with any of the available strategies: [DynamicSpawnStrategy]"), "{e}");
}

#[test]
fn dynamic_failing_local_branch_wins_the_race_with_its_failure() {
    let root = tmp("dynfail");
    let server = FakeReapiServer::new();
    install_executor(&server, "from-remote", 1500);
    let rt = runtime(&root, &server, true, &["--remote_download_outputs=all"], dynamic_opts(), DynamicOptions::default());
    let r = run(&rt, &root, &spawn("exit 3", &[], &["out/a.txt"], &[])).unwrap();
    assert_eq!(r.exit_code, 3);
    assert_eq!(r.status, SpawnStatus::NonZeroExit);
}

#[test]
fn dynamic_ignores_local_signal_failures_in_favor_of_remote() {
    let root = tmp("dynsig");
    let server = FakeReapiServer::new();
    install_executor(&server, "from-remote", 600);
    let dynamic = DynamicOptions { ignore_local_signals: vec![9], ..Default::default() };
    let rt = runtime(&root, &server, true, &["--remote_download_outputs=all"], dynamic_opts(), dynamic);
    let r = run(&rt, &root, &spawn("kill -9 $$", &[], &["out/a.txt"], &[])).unwrap();
    assert_eq!(r.runner_name, "remote");
    assert_eq!(read(&root, "out/a.txt"), "from-remote");
}

#[test]
fn dynamic_strategy_flags_are_validated() {
    let av = StrategyAvailability { process_wrapper_sandbox: true, dynamic: true, remote: true, ..Default::default() };
    let o = StrategyOptions::default().with_dynamic(&["dynamic".to_string()], &[]).unwrap();
    let e = SpawnStrategyRegistry::build(&o, &av).unwrap_err();
    assert_eq!(e, "Cannot use strategy [dynamic, dynamic_worker] in flag --dynamic_local_strategy as it would create a cycle during execution");
    let o = StrategyOptions::default().with_dynamic(&["Javac=worker,bogus".to_string()], &[]).unwrap();
    let e = SpawnStrategyRegistry::build(&o, &av).unwrap_err();
    assert!(e.starts_with("'bogus' was requested for local mnemonic Javac but no strategy with that identifier was registered. Valid values are: ["), "{e}");
    let o = StrategyOptions { remote_local_fallback_strategy: "nope".into(), ..Default::default() };
    let e = SpawnStrategyRegistry::build(&o, &av).unwrap_err();
    assert!(e.starts_with("'nope' was requested for remote fallback strategy"), "{e}");
}
