//! The remote strategy, the remote cache wrapped around local strategies, and dynamic execution
//! (local vs remote race) through `ExecRuntime`, against the in-process fake REAPI server.
#![cfg(unix)]

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
    let remote = RemoteSpawnRunner::new(
        ropts,
        Some(Arc::new(server.client(exec))),
        Arc::new(LocalSpawnRunner),
    )
    .unwrap();
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
        execution_info: info
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        ..Default::default()
    }
}

fn execroot(root: &Path) -> PathFragment {
    PathFragment::create(root.join("ob/execroot/_main").to_str().unwrap())
}

fn run(rt: &Arc<ExecRuntime>, root: &Path, s: &Spawn) -> Result<zel_sandbox::SpawnResult, String> {
    rt.execute(
        s,
        "Doing Gen //t:t",
        &execroot(root),
        &LocalFileSystem::default(),
        &Reporter::new(),
    )
    .map(|r| r.unwrap())
}

/// An executor that produces `out/a.txt` containing `content` after `delay_ms`.
fn install_executor(server: &FakeReapiServer, content: &'static str, delay_ms: u64) {
    server.set_executor(move |ctx| {
        std::thread::sleep(Duration::from_millis(delay_ms));
        let out = ctx.server.put_blob(content.as_bytes());
        let path = ctx
            .command
            .output_paths
            .first()
            .or(ctx.command.output_files.first())
            .expect("declared output")
            .clone();
        Ok(ActionResult {
            output_files: vec![OutputFile {
                path,
                digest: Some(out),
                ..Default::default()
            }],
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
    let so = StrategyOptions {
        spawn_strategy: vec!["remote".into()],
        ..Default::default()
    };
    let rt = runtime(
        &root,
        &server,
        true,
        &["--remote_download_outputs=all"],
        so,
        DynamicOptions::default(),
    );
    let s = spawn("echo local > out/a.txt", &[], &["out/a.txt"], &[]);
    let r = run(&rt, &root, &s).unwrap();
    assert_eq!(
        r.status,
        SpawnStatus::Success,
        "{}",
        String::from_utf8_lossy(&r.stderr)
    );
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
    let so = StrategyOptions {
        spawn_strategy: vec!["remote".into()],
        ..Default::default()
    };
    let rt = runtime(&root, &server, false, &[], so, DynamicOptions::default());
    let e = run(&rt, &root, &spawn("true", &[], &[], &[])).unwrap_err();
    assert!(
        e.contains("Gen spawn cannot be executed with any of the available strategies: [remote]"),
        "{e}"
    );

    let so = StrategyOptions {
        spawn_strategy: vec!["remote".into(), "local".into()],
        ..Default::default()
    };
    let rt = runtime(&root, &server, true, &[], so, DynamicOptions::default());
    install_executor(&server, "remote-made", 0);
    // `local` execution requirement: skips remote, runs locally.
    let r = run(
        &rt,
        &root,
        &spawn(
            "echo here > out/l.txt",
            &[],
            &["out/l.txt"],
            &[("local", "")],
        ),
    )
    .unwrap();
    assert_eq!(r.runner_name, "local");
    assert_eq!(read(&root, "out/l.txt"), "here\n");
    assert_eq!(server.executions(), 0);
}

#[test]
fn remote_cache_wraps_local_strategies() {
    let root = tmp("cache");
    let server = FakeReapiServer::new();
    let so = StrategyOptions {
        spawn_strategy: vec!["local".into()],
        ..Default::default()
    };
    let rt = runtime(
        &root,
        &server,
        false,
        &["--remote_download_outputs=all"],
        so,
        DynamicOptions::default(),
    );
    let s = spawn("echo built >> out/a.txt; echo hi", &[], &["out/a.txt"], &[]);
    let r = run(&rt, &root, &s).unwrap();
    assert_eq!(r.runner_name, "local");
    assert_eq!(read(&root, "out/a.txt"), "built\n");
    assert_eq!(server.ac_count(), 1, "result uploaded");
    std::fs::remove_file(root.join("ob/execroot/_main/out/a.txt")).unwrap();
    let r = run(&rt, &root, &s).unwrap();
    assert_eq!(r.runner_name, "remote cache hit");
    assert_eq!(
        read(&root, "out/a.txt"),
        "built\n",
        "restored from cache, script not rerun"
    );
    assert_eq!(String::from_utf8_lossy(&r.stdout), "hi\n");
}

fn dynamic_opts() -> StrategyOptions {
    StrategyOptions {
        spawn_strategy: vec!["dynamic".into()],
        ..Default::default()
    }
    .with_dynamic(&["sandboxed".to_string()], &[])
    .unwrap()
}

#[test]
fn dynamic_remote_wins_and_kills_the_slow_local_branch() {
    let root = tmp("dynremote");
    let server = FakeReapiServer::new();
    install_executor(&server, "from-remote", 20);
    let rt = runtime(
        &root,
        &server,
        true,
        &["--remote_download_outputs=all"],
        dynamic_opts(),
        DynamicOptions::default(),
    );
    // The local branch would take 20s and write a different result.
    let s = spawn(
        "sleep 20; echo from-local > out/a.txt",
        &[],
        &["out/a.txt"],
        &[],
    );
    let t = Instant::now();
    let r = run(&rt, &root, &s).unwrap();
    assert!(
        t.elapsed() < Duration::from_secs(10),
        "local branch was not cancelled: {:?}",
        t.elapsed()
    );
    assert_eq!(r.status, SpawnStatus::Success);
    assert_eq!(r.runner_name, "remote");
    assert_eq!(read(&root, "out/a.txt"), "from-remote");
}

#[test]
fn dynamic_local_wins_and_remote_never_writes_outputs() {
    let root = tmp("dynlocal");
    let server = FakeReapiServer::new();
    install_executor(&server, "from-remote", 700);
    let rt = runtime(
        &root,
        &server,
        true,
        &["--remote_download_outputs=all"],
        dynamic_opts(),
        DynamicOptions::default(),
    );
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
    let rt = runtime(
        &root,
        &server,
        true,
        &["--remote_download_outputs=all"],
        dynamic_opts(),
        DynamicOptions::default(),
    );
    let r = run(
        &rt,
        &root,
        &spawn(
            "echo x > out/a.txt",
            &[],
            &["out/a.txt"],
            &[("no-remote", "")],
        ),
    )
    .unwrap();
    assert_eq!(r.runner_name, "processwrapper-sandbox");
    let r = run(
        &rt,
        &root,
        &spawn(
            "echo x > out/b.txt",
            &[],
            &["out/b.txt"],
            &[("no-local", "")],
        ),
    )
    .unwrap();
    assert_eq!(r.runner_name, "remote");
    let e = run(
        &rt,
        &root,
        &spawn("true", &[], &[], &[("no-local", ""), ("no-remote", "")]),
    )
    .unwrap_err();
    // `DynamicSpawnStrategy.canExec` is false, so the resolver reports it.
    assert!(e.starts_with("Gen spawn cannot be executed with any of the available strategies: [DynamicSpawnStrategy]"), "{e}");
}

#[test]
fn dynamic_failing_local_branch_wins_the_race_with_its_failure() {
    let root = tmp("dynfail");
    let server = FakeReapiServer::new();
    install_executor(&server, "from-remote", 1500);
    let rt = runtime(
        &root,
        &server,
        true,
        &["--remote_download_outputs=all"],
        dynamic_opts(),
        DynamicOptions::default(),
    );
    let r = run(&rt, &root, &spawn("exit 3", &[], &["out/a.txt"], &[])).unwrap();
    assert_eq!(r.exit_code, 3);
    assert_eq!(r.status, SpawnStatus::NonZeroExit);
}

#[test]
fn dynamic_ignores_local_signal_failures_in_favor_of_remote() {
    let root = tmp("dynsig");
    let server = FakeReapiServer::new();
    install_executor(&server, "from-remote", 600);
    let dynamic = DynamicOptions {
        ignore_local_signals: vec![9],
        ..Default::default()
    };
    let rt = runtime(
        &root,
        &server,
        true,
        &["--remote_download_outputs=all"],
        dynamic_opts(),
        dynamic,
    );
    let r = run(&rt, &root, &spawn("kill -9 $$", &[], &["out/a.txt"], &[])).unwrap();
    assert_eq!(r.runner_name, "remote");
    assert_eq!(read(&root, "out/a.txt"), "from-remote");
}

#[test]
fn dynamic_strategy_flags_are_validated() {
    let av = StrategyAvailability {
        process_wrapper_sandbox: true,
        dynamic: true,
        remote: true,
        ..Default::default()
    };
    let o = StrategyOptions::default()
        .with_dynamic(&["dynamic".to_string()], &[])
        .unwrap();
    let e = SpawnStrategyRegistry::build(&o, &av).unwrap_err();
    assert_eq!(e, "Cannot use strategy [dynamic, dynamic_worker] in flag --dynamic_local_strategy as it would create a cycle during execution");
    let o = StrategyOptions::default()
        .with_dynamic(&["Javac=worker,bogus".to_string()], &[])
        .unwrap();
    let e = SpawnStrategyRegistry::build(&o, &av).unwrap_err();
    assert!(e.starts_with("'bogus' was requested for local mnemonic Javac but no strategy with that identifier was registered. Valid values are: ["), "{e}");
    let o = StrategyOptions {
        remote_local_fallback_strategy: "nope".into(),
        ..Default::default()
    };
    let e = SpawnStrategyRegistry::build(&o, &av).unwrap_err();
    assert!(
        e.starts_with("'nope' was requested for remote fallback strategy"),
        "{e}"
    );
}

#[test]
fn execute_with_hooks_forwards_stream_cancel_and_progress_through_dynamic() {
    let root = tmp("dyn-hooks");
    let server = FakeReapiServer::new();
    install_executor(&server, "from-remote", 5000);
    let so = StrategyOptions {
        spawn_strategy: vec!["dynamic".into()],
        local_lockfree_output: true,
        ..Default::default()
    }
    .with_dynamic(&["standalone".to_string()], &[])
    .unwrap();
    let rt = runtime(
        &root,
        &server,
        true,
        &["--remote_download_outputs=all"],
        so,
        DynamicOptions {
            local_lockfree_output: true,
            ..Default::default()
        },
    );

    // 1. Single-branch dynamic (`no-remote`) on a `TestRunner` spawn forwards `stream_out`
    //    live and emits the `Progress` fallback event ("Testing //t:t").
    let mut test_sp = spawn(
        "echo STREAMED_THROUGH_DYNAMIC && echo ok > out/t.txt",
        &[],
        &["out/t.txt"],
        &[("no-remote", "")],
    );
    test_sp.mnemonic = "TestRunner".into();
    let streamed = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let streamed_cb = streamed.clone();
    let hooks = zel_sandbox::ExecHooks {
        stream_out: Some(Arc::new(move |chunk: &[u8]| {
            streamed_cb.lock().unwrap().extend_from_slice(chunk);
        })),
        ..Default::default()
    };
    let rep = Reporter::new();
    let r = rt
        .execute_with_hooks(
            &test_sp,
            "Testing //t:t",
            &execroot(&root),
            &LocalFileSystem::default(),
            &rep,
            &hooks,
        )
        .unwrap()
        .unwrap();
    assert_eq!(r.status, SpawnStatus::Success);
    assert_eq!(
        String::from_utf8_lossy(&streamed.lock().unwrap()),
        "STREAMED_THROUGH_DYNAMIC\n"
    );
    assert!(
        rep.events().iter().any(|ev| {
            ev.kind == zel_events::EventKind::Progress && ev.message == "Testing //t:t"
        }),
        "expected Progress event from execute_with_hooks: {:?}",
        rep.events()
    );

    // 2. Two-branch dynamic race forwards outer `hooks.cancel` and interrupts the running local branch.
    let started = root.join("ob/execroot/_main/local_started");
    let slow_sp = spawn(
        &format!(
            "touch '{}' && sleep 20 && echo done > out/c.txt",
            started.display()
        ),
        &[],
        &["out/c.txt"],
        &[],
    );
    let cancel = zel_sandbox::process_wrapper::CancelFlag::default();
    let cancel_trigger = cancel.clone();
    let t = Instant::now();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !started.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            cancel_trigger.cancel();
        });
        let cancel_hooks = zel_sandbox::ExecHooks {
            cancel: Some(cancel),
            ..Default::default()
        };
        let err = rt
            .execute_with_hooks(
                &slow_sp,
                "Doing Gen //t:t",
                &execroot(&root),
                &LocalFileSystem::default(),
                &Reporter::new(),
                &cancel_hooks,
            )
            .unwrap()
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
    });
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "outer cancel must promptly stop dynamic spawn: {:?}",
        t.elapsed()
    );
}

#[test]
fn dynamic_lockfree_local_loser_cleans_partial_outputs_before_remote_publishes_and_non_lockfree_preserves_outputs(
) {
    let root = tmp("dyn-lockfree");
    let server = FakeReapiServer::new();
    let started = root.join("ob/execroot/_main/lockfree_wrote_garbage");
    let started_remote = started.clone();
    server.set_executor(move |ctx| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !started_remote.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let out = ctx.server.put_blob(b"clean-from-remote");
        Ok(ActionResult {
            output_files: vec![OutputFile {
                path: "out/a.txt".to_string(),
                digest: Some(out),
                ..Default::default()
            }],
            ..Default::default()
        })
    });
    let so = StrategyOptions {
        spawn_strategy: vec!["dynamic".into()],
        local_lockfree_output: true,
        ..Default::default()
    }
    .with_dynamic(&["standalone".to_string()], &[])
    .unwrap();
    let rt = runtime(
        &root,
        &server,
        true,
        &["--remote_download_outputs=all"],
        so,
        DynamicOptions {
            local_lockfree_output: true,
            ..Default::default()
        },
    );
    // Local lockfree branch writes garbage to both `out/a.txt` and optional `out/extra.txt`, then sleeps;
    // remote wins, cancels local, waits on `local_done` while local cleans partial outputs, and then
    // publishes `clean-from-remote` to `out/a.txt`.
    let mut s = spawn(
        &format!(
            "printf partial-local > out/a.txt && printf partial-extra > out/extra.txt && touch '{}' && sleep 20",
            started.display()
        ),
        &[],
        &["out/a.txt", "out/extra.txt"],
        &[],
    );
    s.optional_outputs = vec![PathFragment::create("out/extra.txt")];
    let r = run(&rt, &root, &s).unwrap();
    assert_eq!(r.runner_name, "remote");
    assert_eq!(read(&root, "out/a.txt"), "clean-from-remote");
    assert!(
        !root.join("ob/execroot/_main/out/extra.txt").exists(),
        "local loser's partial optional output must be cleaned before remote completes"
    );

    // Non-lockfree local execution (`local_lockfree_output = false`) and pre-start negative timeout
    // must NOT delete pre-existing files at `spawn.outputs` (`LocalSpawnRunner.java` parity).
    let er_path = root.join("ob/execroot/_main");
    let keep_file = er_path.join("out/keep.txt");
    std::fs::write(&keep_file, "preexisting").unwrap();
    let mut neg_sp = spawn("true", &[], &["out/keep.txt"], &[("timeout", "-1")]);
    neg_sp.outputs = vec![PathFragment::create("out/keep.txt")];
    let reject_hooks = zel_sandbox::ExecHooks {
        local_lockfree_output: true,
        lock_outputs: Some(Arc::new(|_| false)),
        ..Default::default()
    };
    let err = zel_sandbox::execute_spawn_local_with(
        &neg_sp,
        &execroot(&root),
        &LocalFileSystem::default(),
        &reject_hooks,
    )
    .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(
        std::fs::read_to_string(&keep_file).unwrap(),
        "preexisting",
        "pre-start negative timeout must not delete pre-existing output"
    );

    let cancel_started = er_path.join("non_lockfree_started");
    let cancel_sp = spawn(
        &format!("touch '{}' && sleep 20", cancel_started.display()),
        &[],
        &["out/keep.txt"],
        &[],
    );
    let cancel = zel_sandbox::process_wrapper::CancelFlag::default();
    let cancel_bg = cancel.clone();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !cancel_started.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            cancel_bg.cancel();
        });
        let non_lockfree_hooks = zel_sandbox::ExecHooks {
            cancel: Some(cancel),
            local_lockfree_output: false,
            ..Default::default()
        };
        let err = zel_sandbox::execute_spawn_local_with(
            &cancel_sp,
            &execroot(&root),
            &LocalFileSystem::default(),
            &non_lockfree_hooks,
        )
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
    });
    assert_eq!(
        std::fs::read_to_string(&keep_file).unwrap(),
        "preexisting",
        "non-lockfree cancellation must not delete pre-existing output"
    );
}

#[test]
fn outer_cancel_while_remote_waits_for_local_done_prevents_remote_publish() {
    let root = tmp("dyn-outer-cancel-remote-wait");
    let server = FakeReapiServer::new();
    let er_path = root.join("ob/execroot/_main");
    let local_started = er_path.join("local_started");
    let got_term = er_path.join("local_got_term");
    let release_local = er_path.join("release_local");
    let local_exited = er_path.join("local_exited");

    let local_started_remote = local_started.clone();
    server.set_executor(move |ctx| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !local_started_remote.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let out = ctx.server.put_blob(b"remote-should-not-publish");
        Ok(ActionResult {
            output_files: vec![OutputFile {
                path: "out/a.txt".to_string(),
                digest: Some(out),
                ..Default::default()
            }],
            ..Default::default()
        })
    });

    let so = StrategyOptions {
        spawn_strategy: vec!["dynamic".into()],
        local_lockfree_output: true,
        ..Default::default()
    }
    .with_dynamic(&["standalone".to_string()], &[])
    .unwrap();
    let rt = runtime(
        &root,
        &server,
        true,
        &["--remote_download_outputs=all"],
        so,
        DynamicOptions {
            local_lockfree_output: true,
            ..Default::default()
        },
    );

    // Local traps SIGTERM (delivered only when the remote branch wins `Race` and cancels local,
    // blocking on `local_done`). Outer cancellation then fires while remote is waiting on
    // `local_done`, before local finishes cleanup.
    let mut sp = spawn(
        &format!(
            "trap 'touch \"{got}\"; while [ ! -f \"{rel}\" ]; do sleep 0.02; done; touch \"{exited}\"; exit 0' TERM; \
             touch \"{started}\"; \
             while :; do sleep 0.05; done",
            got = got_term.display(),
            rel = release_local.display(),
            exited = local_exited.display(),
            started = local_started.display(),
        ),
        &[],
        &["out/a.txt"],
        &[("supports-graceful-termination", "")],
    );
    sp.kill_delay = Some(Duration::from_secs(5));

    let outer_cancel = zel_sandbox::process_wrapper::CancelFlag::default();
    let outer_cancel_trigger = outer_cancel.clone();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !got_term.exists() {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for remote branch to win lock and send SIGTERM to local"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            outer_cancel_trigger.cancel();
            std::thread::sleep(Duration::from_millis(50));
            std::fs::write(&release_local, b"go").unwrap();
        });
        let hooks = zel_sandbox::ExecHooks {
            cancel: Some(outer_cancel),
            ..Default::default()
        };
        let err = rt
            .execute_with_hooks(
                &sp,
                "Doing Gen //t:t",
                &execroot(&root),
                &LocalFileSystem::default(),
                &Reporter::new(),
                &hooks,
            )
            .unwrap()
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
    });

    assert!(
        local_exited.exists(),
        "dynamic execution must wait for local branch cleanup before returning Interrupted"
    );
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !er_path.join("out/a.txt").exists(),
        "cancelled dynamic spawn must not publish remote outputs after waiting on local_done"
    );
}

#[test]
fn bwob_zero_remote_cache_ttl_succeeds_post_execution_and_reexecutes_next_invocation() {
    use zel_analysis::{ActionAnalysisMetadata, Artifact};
    use zel_cmdline::Label;
    use zel_exec::action_cache::CompactPersistentActionCache;
    use zel_exec::{execute_action_with, ExecOptions};

    let root = tmp("bwob_zero_ttl");
    let server = FakeReapiServer::new();
    install_executor(&server, "from-remote", 0);
    let opts = StrategyOptions {
        spawn_strategy: vec!["remote".into()],
        ..Default::default()
    };
    let rt_zero = runtime(
        &root,
        &server,
        true,
        &[
            "--remote_download_outputs=minimal",
            "--experimental_remote_cache_ttl=0s",
        ],
        opts.clone(),
        DynamicOptions::default(),
    );

    let owner = Label::parse_canonical("//t:t").unwrap();
    let out_art = Artifact::derived(
        "k8-fastbuild",
        PathFragment::create("out/a.txt"),
        owner.clone(),
    );
    let action = ActionAnalysisMetadata::SpawnAction {
        owner,
        mnemonic: "Gen".into(),
        progress_message: "Doing Gen //t:t".into(),
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            format!("echo from-local > {}", out_art.exec_path),
        ],
        env: BTreeMap::new(),
        inherited_env: Default::default(),
        unset_env: Default::default(),
        inputs: vec![],
        outputs: vec![out_art.clone()],
        execution_info: BTreeMap::new(),
        tools: Vec::new(),
        java_compile: None,
    };

    let fs = LocalFileSystem::default();
    let er = execroot(&root);
    let ac = CompactPersistentActionCache::new();
    let exec_opts_zero = ExecOptions {
        runtime: Some(rt_zero),
        ..Default::default()
    };

    // First build with `--experimental_remote_cache_ttl=0s`: post-execution output check must
    // succeed even though `expires_at <= now` and the output is unmaterialized on disk.
    let first = execute_action_with(&action, &er, &fs, &ac, &Reporter::new(), &exec_opts_zero)
        .expect("execute_action_with");
    assert!(
        first.failure.is_none(),
        "unexpected failure: {:?}",
        first.failure
    );
    assert!(!first.cache_hit);
    assert!(
        !root
            .join("ob/execroot/_main")
            .join(out_art.exec_path.path_string())
            .exists(),
        "minimal mode must keep output unmaterialized"
    );
    let recorded = ac
        .get_entry(out_art.exec_path.path_string())
        .expect("action cache entry recorded");
    assert!(
        !recorded.files[out_art.exec_path.path_string()].is_empty(),
        "recorded remote digest must be non-empty even when ttl=0s"
    );

    // Subsequent check with `ttl=0s`: expired remote metadata is not trusted by
    // `ActionCacheChecker.mustExecute`, so the action is not skipped as `UpToDate`.
    let second = execute_action_with(&action, &er, &fs, &ac, &Reporter::new(), &exec_opts_zero)
        .expect("second execute_action_with");
    assert!(second.failure.is_none());
    assert!(
        !second.cache_hit,
        "expired remote metadata must not satisfy pre-execution local action cache check"
    );

    // Contrast with a positive TTL (`3h`): first run populates lazy metadata with unexpired TTL,
    // and the second run is skipped as `UpToDate` via `ActionCacheChecker`.
    let rt_alive = runtime(
        &root,
        &server,
        true,
        &[
            "--remote_download_outputs=minimal",
            "--experimental_remote_cache_ttl=3h",
        ],
        opts,
        DynamicOptions::default(),
    );
    let ac_alive = CompactPersistentActionCache::new();
    let exec_opts_alive = ExecOptions {
        runtime: Some(rt_alive),
        ..Default::default()
    };
    let alive1 = execute_action_with(
        &action,
        &er,
        &fs,
        &ac_alive,
        &Reporter::new(),
        &exec_opts_alive,
    )
    .unwrap();
    assert!(alive1.failure.is_none());
    assert!(!alive1.cache_hit);
    let alive2 = execute_action_with(
        &action,
        &er,
        &fs,
        &ac_alive,
        &Reporter::new(),
        &exec_opts_alive,
    )
    .unwrap();
    assert!(alive2.failure.is_none());
    assert!(
        alive2.cache_hit,
        "unexpired remote metadata must hit local action cache"
    );
}
