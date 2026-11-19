#[cfg(unix)]
use std::collections::BTreeMap;
use zel_analysis::{ActionAnalysisMetadata, Artifact};
use zel_cmdline::Label;
use zel_events::Reporter;
use zel_exec::action_cache::CompactPersistentActionCache;
#[cfg(unix)]
use zel_exec::ToolRunfilesSpec;
use zel_exec::{execute_action_with, ExecOptions};
#[cfg(unix)]
use zel_vfs::LocalFileSystem;
use zel_vfs::{FileSystem, InMemoryFileSystem, PathFragment};

#[test]
fn unresolved_symlink_keeps_raw_relative_target() {
    let fs = InMemoryFileSystem::default();
    let execroot = PathFragment::create("/er");
    fs.create_directory_and_parents(&execroot.get_relative_str("bazel-out/k8-fastbuild/bin/pkg"))
        .unwrap();
    let mut out = Artifact::derived(
        "k8-fastbuild",
        PathFragment::create("pkg/link"),
        Label::parse_canonical("//pkg:t").unwrap(),
    );
    out.is_unresolved_symlink = true;
    let action = ActionAnalysisMetadata::SymlinkAction {
        owner: Label::parse_canonical("//pkg:t").unwrap(),
        input_path: PathFragment::create("../some/relative/path"),
        output: out,
        is_executable: false,
    };
    let cache = CompactPersistentActionCache::new();
    let reporter = Reporter::new();
    let res = execute_action_with(
        &action,
        &execroot,
        &fs,
        &cache,
        &reporter,
        &ExecOptions::default(),
    )
    .unwrap();
    assert!(res.failure.is_none());
    let got = fs
        .read_symbolic_link(&execroot.get_relative_str("bazel-out/k8-fastbuild/bin/pkg/link"))
        .unwrap();
    assert_eq!(got.path_string(), "../some/relative/path");
}

#[cfg(unix)]
#[test]
fn tool_runfiles_tree_materialized_and_invalidated_on_mapping_change() {
    let root = std::env::temp_dir().join(format!("zel-exec-runfiles-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let execroot_dir = root.join("execroot/_main");
    std::fs::create_dir_all(execroot_dir.join("bazel-out/k8-fastbuild/bin/pkg")).unwrap();
    std::fs::create_dir_all(execroot_dir.join("pkg")).unwrap();
    let tool_rel = "bazel-out/k8-fastbuild/bin/pkg/tool";
    std::fs::write(execroot_dir.join(tool_rel), "#!/bin/sh\necho ok\n").unwrap();
    std::fs::write(execroot_dir.join("pkg/data.txt"), "hello").unwrap();

    let execroot = PathFragment::create(execroot_dir.to_str().unwrap());
    let fs = LocalFileSystem::default();
    let cache = CompactPersistentActionCache::new();
    let reporter = Reporter::new();
    let owner = Label::parse_canonical("//pkg:g").unwrap();
    let tool_art = Artifact::derived(
        "k8-fastbuild",
        PathFragment::create("pkg/tool"),
        owner.clone(),
    );
    let out_art = Artifact::derived(
        "k8-fastbuild",
        PathFragment::create("pkg/out.txt"),
        owner.clone(),
    );
    let action = ActionAnalysisMetadata::SpawnAction {
        owner,
        mnemonic: "Genrule".into(),
        progress_message: "Executing genrule //pkg:g".into(),
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "ls -la {tool_rel}.runfiles/_main > {}",
                out_art.exec_path.path_string()
            ),
        ],
        env: BTreeMap::new(),
        inherited_env: Default::default(),
        unset_env: Default::default(),
        inputs: vec![tool_art.clone()],
        outputs: vec![out_art.clone()],
        execution_info: BTreeMap::new(),
        tools: vec![tool_art],
        java_compile: None,
    };

    let mut entries = BTreeMap::new();
    entries.insert("_main/link_one".to_string(), "pkg/data.txt".to_string());
    let mut tool_runfiles = BTreeMap::new();
    tool_runfiles.insert(
        tool_rel.to_string(),
        ToolRunfilesSpec {
            workspace_name: "_main".into(),
            entries,
            repo_mapping: Vec::new(),
            enable_runfiles: true,
            legacy_external_runfiles: false,
        },
    );
    let mut opts = ExecOptions {
        tool_runfiles,
        ..Default::default()
    };

    let r1 = execute_action_with(&action, &execroot, &fs, &cache, &reporter, &opts).unwrap();
    assert!(r1.failure.is_none(), "{:?}", r1.failure);
    assert!(!r1.cache_hit);
    assert!(execroot_dir
        .join(format!("{tool_rel}.runfiles/_main/link_one"))
        .exists());

    let mut entries2 = BTreeMap::new();
    entries2.insert("_main/link_two".to_string(), "pkg/data.txt".to_string());
    opts.tool_runfiles.get_mut(tool_rel).unwrap().entries = entries2;
    let r2 = execute_action_with(&action, &execroot, &fs, &cache, &reporter, &opts).unwrap();
    assert!(r2.failure.is_none(), "{:?}", r2.failure);
    assert!(!r2.cache_hit);
    assert!(execroot_dir
        .join(format!("{tool_rel}.runfiles/_main/link_two"))
        .exists());

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn executable_symlink_validation_tree_dangling_check_self_cycle_and_minimal_runfiles() {
    let root = std::env::temp_dir().join(format!("zel-exec-parity2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let execroot_dir = root.join("execroot/_main");
    std::fs::create_dir_all(execroot_dir.join("pkg/adir")).unwrap();
    let execroot = PathFragment::create(execroot_dir.to_str().unwrap());
    let fs = LocalFileSystem::default();
    let cache = CompactPersistentActionCache::new();
    let reporter = Reporter::new();
    let owner = Label::parse_canonical("//pkg:t").unwrap();

    // 1. ExecutableSymlink targeting a directory fails with `'...' is not a file`.
    let sym_out = Artifact::derived(
        "k8-fastbuild",
        PathFragment::create("pkg/sym_bin"),
        owner.clone(),
    );
    let bad_sym = ActionAnalysisMetadata::SymlinkAction {
        owner: owner.clone(),
        input_path: PathFragment::create("pkg/adir"),
        output: sym_out,
        is_executable: true,
    };
    let r_sym = execute_action_with(
        &bad_sym,
        &execroot,
        &fs,
        &cache,
        &reporter,
        &ExecOptions::default(),
    )
    .unwrap();
    let msg = r_sym.failure.unwrap().messages.join("\n");
    assert!(msg.contains("'pkg/adir' is not a file"), "{msg}");

    // 2. Tree artifact containing a dangling symlink fails validation.
    let mut tree_out = Artifact::derived(
        "k8-fastbuild",
        PathFragment::create("pkg/tree_out"),
        owner.clone(),
    );
    tree_out.is_tree_artifact = true;
    let dangling_tree = ActionAnalysisMetadata::SpawnAction {
        owner: owner.clone(),
        mnemonic: "MakeTree".into(),
        progress_message: String::new(),
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "ln -s /nonexistent/nowhere {}/bad_link",
                tree_out.exec_path.path_string()
            ),
        ],
        env: BTreeMap::new(),
        inherited_env: Default::default(),
        unset_env: Default::default(),
        inputs: vec![],
        outputs: vec![tree_out],
        execution_info: BTreeMap::new(),
        tools: vec![],
        java_compile: None,
    };
    let r_tree = execute_action_with(
        &dangling_tree,
        &execroot,
        &fs,
        &cache,
        &reporter,
        &ExecOptions::default(),
    )
    .unwrap();
    let tree_msg = r_tree.failure.unwrap().messages.join("\n");
    assert!(tree_msg.contains("dangling symbolic link"), "{tree_msg}");

    // 3. Self-cyclic action (`self_art` is both input and output) is detected by scheduler.
    let self_art = Artifact::derived(
        "k8-fastbuild",
        PathFragment::create("pkg/self_io"),
        owner.clone(),
    );
    let self_cycle = ActionAnalysisMetadata::SpawnAction {
        owner,
        mnemonic: "Cycle".into(),
        progress_message: String::new(),
        argv: vec!["/bin/true".into()],
        env: BTreeMap::new(),
        inherited_env: Default::default(),
        unset_env: Default::default(),
        inputs: vec![self_art.clone()],
        outputs: vec![self_art],
        execution_info: BTreeMap::new(),
        tools: vec![],
        java_compile: None,
    };
    let acts = [&self_cycle];
    let sched = zel_exec::execute_actions(
        &acts,
        1,
        false,
        &execroot,
        &fs,
        &cache,
        &reporter,
        &ExecOptions::default(),
    );
    assert!(sched.cycle.is_some(), "self-dependency must report a cycle");

    // 4. `enable_runfiles: false` creates a minimal empty `.runfiles` directory alongside MANIFEST.
    let rf_exe = execroot_dir.join("pkg/bin_norf");
    std::fs::write(&rf_exe, "#!/bin/sh\n").unwrap();
    let mut rf_entries = BTreeMap::new();
    rf_entries.insert("_main/f".to_string(), "pkg/adir".to_string());
    let _manifest_hash = zel_exec::runfiles_tree::build_runfiles_tree(
        &execroot_dir,
        &rf_exe,
        "_main",
        &rf_entries,
        &zel_exec::runfiles_tree::RunfilesTreeOptions {
            enable_runfiles: false,
            ..Default::default()
        },
    )
    .unwrap();
    let rf_dir = execroot_dir.join("pkg/bin_norf.runfiles");
    assert!(rf_dir.is_dir(), "minimal .runfiles directory should exist");
    assert!(!rf_dir.join("_main/f").exists());

    let _ = std::fs::remove_dir_all(&root);
}
