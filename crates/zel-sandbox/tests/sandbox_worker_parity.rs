use std::collections::BTreeMap;
use std::sync::Arc;
#[cfg(unix)]
use zel_sandbox::sandbox::{SandboxKind, SandboxOptions, SandboxRunner};
use zel_sandbox::worker::{WorkerOptions, WorkerPool, WorkerSpawnRunner};
use zel_sandbox::Spawn;
#[cfg(unix)]
use zel_sandbox::SpawnStatus;
use zel_vfs::PathFragment;

fn digest(b: &[u8]) -> String {
    zel_util::sha256::hex_digest(b)
}

#[test]
fn worker_invalidates_when_tool_file_hash_changes() {
    let root = std::env::temp_dir().join(format!("zel-wk-hash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let exec_root = root.join("execroot/ws");
    std::fs::create_dir_all(exec_root.join("pkg")).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let exec_root = std::fs::canonicalize(&exec_root).unwrap();

    let tool_file = exec_root.join("pkg/worker_tool.bin");
    std::fs::write(&tool_file, b"v1").unwrap();
    std::fs::write(exec_root.join("pkg/a.params"), "--out=a.out\n--text=one\n").unwrap();
    std::fs::write(exec_root.join("pkg/b.params"), "--out=b.out\n--text=two\n").unwrap();

    let pool = Arc::new(WorkerPool::new(
        WorkerOptions::default(),
        &root.join("bazel-workers"),
        "ws",
    ));
    let runner = WorkerSpawnRunner::new(&exec_root, Arc::clone(&pool), BTreeMap::new(), digest);

    let mk_spawn = |out: &str, params: &str| {
        let mut s = Spawn {
            mnemonic: "TestWorker".into(),
            owner_label: "//pkg:x".into(),
            argv: vec![
                env!("CARGO_BIN_EXE_zel-test-worker").to_string(),
                format!("@{params}"),
            ],
            inputs: vec![PathFragment::create(params)],
            outputs: vec![PathFragment::create(out)],
            tool_inputs: vec![PathFragment::create("pkg/worker_tool.bin")],
            ..Default::default()
        };
        s.execution_info
            .insert("supports-workers".into(), "1".into());
        s
    };

    let r1 = runner.exec(&mk_spawn("a.out", "pkg/a.params")).unwrap();
    assert!(r1.is_ok(), "{}", String::from_utf8_lossy(&r1.stderr));
    assert_eq!(pool.started.load(std::sync::atomic::Ordering::SeqCst), 1);

    // Mutate the tool input file on disk; next worker request must reject old worker and start a new one.
    std::fs::write(&tool_file, b"v2").unwrap();
    let r2 = runner.exec(&mk_spawn("b.out", "pkg/b.params")).unwrap();
    assert!(r2.is_ok(), "{}", String::from_utf8_lossy(&r2.stderr));
    assert_eq!(pool.started.load(std::sync::atomic::Ordering::SeqCst), 2);
    let out_b = std::fs::read_to_string(exec_root.join("b.out")).unwrap();
    assert!(
        out_b.contains(" 1 "),
        "new worker counter must restart at 1: {out_b}"
    );
    pool.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn sandbox_moves_output_dirs_and_stages_tool_runfiles() {
    let root = std::env::temp_dir().join(format!("zel-sbx-outdirs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let exec_root = root.join("execroot/ws");
    let sbx_base = root.join("sandbox");
    std::fs::create_dir_all(exec_root.join("pkg/tool.runfiles/_main")).unwrap();
    std::fs::create_dir_all(&sbx_base).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let exec_root = std::fs::canonicalize(&exec_root).unwrap();
    let sbx_base = std::fs::canonicalize(&sbx_base).unwrap();

    std::fs::write(exec_root.join("pkg/tool"), "#!/bin/sh\necho ok\n").unwrap();
    std::fs::write(exec_root.join("pkg/tool.runfiles/_main/data.txt"), "rfdata").unwrap();

    let runner = SandboxRunner::new(
        SandboxKind::ProcessWrapper,
        &exec_root,
        &sbx_base,
        SandboxOptions::default(),
        BTreeMap::new(),
        None,
    );
    let spawn = Spawn {
        mnemonic: "TestSbx".into(),
        owner_label: "//pkg:s".into(),
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            "mkdir -p very/output.dir && cat pkg/tool.runfiles/_main/data.txt > very/output.dir/test.txt".into(),
        ],
        inputs: vec![PathFragment::create("pkg/tool")],
        tool_inputs: vec![PathFragment::create("pkg/tool")],
        outputs: vec![],
        output_dirs: vec![PathFragment::create("very/output.dir")],
        ..Default::default()
    };
    let res = runner.exec(&spawn).unwrap();
    assert_eq!(
        res.status,
        SpawnStatus::Success,
        "{}",
        String::from_utf8_lossy(&res.stderr)
    );
    let moved = std::fs::read_to_string(exec_root.join("very/output.dir/test.txt")).unwrap();
    assert_eq!(moved, "rfdata");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn worker_pool_quota_counts_borrowed_only_and_parse_instances_keywords() {
    use zel_sandbox::worker::{create_worker_key, parse_instances_flag_opt};

    assert_eq!(parse_instances_flag_opt("null", 4).unwrap(), None);
    assert_eq!(parse_instances_flag_opt("auto", 4).unwrap(), None);
    assert_eq!(
        parse_instances_flag_opt("Javac=auto", 4).unwrap(),
        Some(("Javac".to_string(), 4))
    );
    assert_eq!(
        parse_instances_flag_opt("Javac=auto", 8).unwrap(),
        Some(("Javac".to_string(), 8))
    );
    assert!(parse_instances_flag_opt("HOST_CPUS", 4).unwrap().is_some());

    let root = std::env::temp_dir().join(format!("zel-wk-quota-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let exec_root = root.join("execroot/ws");
    std::fs::create_dir_all(exec_root.join("pkg")).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let exec_root = std::fs::canonicalize(&exec_root).unwrap();
    std::fs::write(exec_root.join("pkg/a.params"), "--out=a.out\n--text=one\n").unwrap();
    std::fs::write(exec_root.join("pkg/tool.bin"), b"tool-v1").unwrap();

    let mut wopts = WorkerOptions {
        multiplex: false,
        cancellation: true,
        ..Default::default()
    };
    wopts.max_instances.push((String::new(), 1));

    let mut sp = Spawn {
        mnemonic: "QuotaWorker".into(),
        owner_label: "//pkg:q".into(),
        argv: vec![
            env!("CARGO_BIN_EXE_zel-test-worker").to_string(),
            "@pkg/a.params".to_string(),
        ],
        inputs: vec![PathFragment::create("pkg/a.params")],
        tool_inputs: vec![PathFragment::create("pkg/tool.bin")],
        outputs: vec![PathFragment::create("a.out")],
        ..Default::default()
    };
    sp.execution_info
        .insert("supports-workers".into(), "1".into());
    // Without supports-worker-cancellation on spawn, WorkerKey.cancellable stays false;
    // dynamic=true forces sandboxed=true.
    let key_dyn = create_worker_key(
        &sp,
        vec![env!("CARGO_BIN_EXE_zel-test-worker").to_string()],
        BTreeMap::new(),
        &exec_root.to_string_lossy(),
        "00".repeat(32),
        &wopts,
        true,
        zel_sandbox::worker::ProtocolFormat::Proto,
    );
    assert!(!key_dyn.cancellable);
    assert!(key_dyn.sandboxed);

    sp.execution_info
        .insert("supports-worker-cancellation".into(), "1".into());
    let key_can = create_worker_key(
        &sp,
        vec![env!("CARGO_BIN_EXE_zel-test-worker").to_string()],
        BTreeMap::new(),
        &exec_root.to_string_lossy(),
        "00".repeat(32),
        &wopts,
        false,
        zel_sandbox::worker::ProtocolFormat::Proto,
    );
    assert!(key_can.cancellable);
    assert!(!key_can.sandboxed);

    let pool = Arc::new(WorkerPool::new(wopts, &root.join("bazel-workers"), "ws"));
    let runner = WorkerSpawnRunner::new(&exec_root, Arc::clone(&pool), BTreeMap::new(), digest);
    // Three sequential borrows with max_instances=1 must reuse the single idle worker without deadlocking!
    for _ in 0..3 {
        let r = runner.exec(&sp).unwrap();
        assert!(r.is_ok(), "{}", String::from_utf8_lossy(&r.stderr));
    }
    assert_eq!(pool.started.load(std::sync::atomic::Ordering::SeqCst), 1);
    pool.shutdown();
    assert!(pool.live_worker_metrics().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn sandbox_base_init_mount_validation_hermetic_input_and_xcode_env() {
    use zel_sandbox::linux::validate_bind_mounts;
    use zel_sandbox::sandbox::{
        darwin_sandbox_config, initialize_sandbox_base, rewrite_xcode_local_env_with,
    };

    let root = std::env::temp_dir().join(format!("zel-sbx-init-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let sbx_base = root.join("sandbox");
    std::fs::create_dir_all(sbx_base.join("stale_dir")).unwrap();
    std::fs::write(sbx_base.join("stale_dir/leftover.txt"), "old").unwrap();

    initialize_sandbox_base(&sbx_base).unwrap();
    assert!(!sbx_base.join("stale_dir").exists());
    assert!(sbx_base.join("inaccessibleHelperFile").exists());
    assert!(sbx_base.join("inaccessibleHelperDir").is_dir());

    // Missing source and file-vs-directory mismatch must fail validateBindMounts.
    let err_missing = validate_bind_mounts(&[(
        "/tmp".to_string(),
        "/definitely-missing-zel-src".to_string(),
    )])
    .unwrap_err();
    assert!(
        err_missing.contains("Mount source '/definitely-missing-zel-src' does not exist."),
        "{err_missing}"
    );
    let sample_file = root.join("mount_file.txt");
    std::fs::write(&sample_file, "x").unwrap();
    let err_type = validate_bind_mounts(&[(
        sample_file.to_string_lossy().into_owned(),
        "/tmp".to_string(),
    )])
    .unwrap_err();
    assert!(
        err_type.contains("Cannot mount directory '/tmp' onto file"),
        "{err_type}"
    );

    // Darwin sandbox profile without network must omit network-bind and local unix-socket allowances,
    // while keeping remote unix-socket.
    let writable = std::collections::BTreeSet::from(["/tmp".to_string()]);
    let inaccessible = std::collections::BTreeSet::new();
    let profile = darwin_sandbox_config(&writable, &inaccessible, false, None);
    assert!(!profile.contains("network-bind"), "{profile}");
    assert!(!profile.contains("local unix-socket"), "{profile}");
    assert!(profile.contains("remote unix-socket"), "{profile}");

    // Xcode env rewrite: non-Xcode leaves env untouched except TMPDIR;
    // malformed dotted version fails; duplicate SDKROOT fails.
    let plain = rewrite_xcode_local_env_with(
        &BTreeMap::new(),
        &BTreeMap::new(),
        |_, _| unreachable!("must not locate developer dir without override"),
        |_, _| unreachable!("must not invoke xcrun without APPLE_SDK_PLATFORM"),
    )
    .unwrap();
    assert_eq!(plain.get("TMPDIR").map(String::as_str), Some("/tmp"));
    assert!(!plain.contains_key("DEVELOPER_DIR"));

    let mut bad_ver = BTreeMap::new();
    bad_ver.insert("XCODE_VERSION_OVERRIDE".into(), "15.abc".into());
    assert!(rewrite_xcode_local_env_with(
        &bad_ver,
        &BTreeMap::new(),
        |_, _| Ok("/Applications/Xcode.app/Contents/Developer".into()),
        |_, _| Ok("/sdk\n".into()),
    )
    .is_err());

    let mut sdk_env = BTreeMap::new();
    sdk_env.insert("APPLE_SDK_PLATFORM".into(), "MacOSX".into());
    sdk_env.insert("SDKROOT".into(), "/preexisting".into());
    assert!(rewrite_xcode_local_env_with(
        &sdk_env,
        &BTreeMap::new(),
        |_, _| Ok("/Applications/Xcode.app/Contents/Developer".into()),
        |_, _| Ok("/sdk\r\n".into()),
    )
    .is_err());

    // Hermetic hardlinked input staging + mutation detection.
    let exec_root = root.join("execroot/ws");
    std::fs::create_dir_all(exec_root.join("pkg")).unwrap();
    let exec_root = std::fs::canonicalize(&exec_root).unwrap();
    let sbx_base = std::fs::canonicalize(&sbx_base).unwrap();
    std::fs::write(exec_root.join("pkg/in.txt"), "immutable").unwrap();
    let runner = SandboxRunner::new(
        SandboxKind::Linux,
        &exec_root,
        &sbx_base,
        SandboxOptions {
            use_hermetic_linux_sandbox: true,
            ..Default::default()
        },
        BTreeMap::new(),
        None,
    );
    let sp = Spawn {
        mnemonic: "MutateInput".into(),
        owner_label: "//pkg:m".into(),
        argv: vec!["/bin/true".into()],
        inputs: vec![PathFragment::create("pkg/in.txt")],
        outputs: vec![PathFragment::create("pkg/out.txt")],
        ..Default::default()
    };
    let (layout, _env) = runner.prepare(&sp, 1).unwrap();
    zel_sandbox::sandbox::verify_hermetic_inputs_unchanged(&layout.hermetic_file_snapshots)
        .unwrap();
    // Mutating through the hardlinked sandbox input must be caught!
    std::fs::write(layout.sandbox_exec_root.join("pkg/in.txt"), "mutated-bytes").unwrap();
    let err =
        zel_sandbox::sandbox::verify_hermetic_inputs_unchanged(&layout.hermetic_file_snapshots)
            .unwrap_err();
    assert!(
        err.to_string()
            .contains("was modified during hermetic sandbox execution"),
        "{err}"
    );
    let _ = runner.cleanup_sandbox_base();
    let _ = zel_sandbox::delete_tree(&root);
}
