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
