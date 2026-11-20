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
        unused_inputs_list: None,
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
            ..Default::default()
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
        unused_inputs_list: None,
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
        unused_inputs_list: None,
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

#[cfg(unix)]
#[test]
fn tool_runfiles_unresolved_relative_symlink_executes_in_sandbox() {
    use std::collections::BTreeSet;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use zel_exec::resources::ResourceManager;
    use zel_exec::strategy::{SpawnStrategyRegistry, StrategyAvailability, StrategyOptions};
    use zel_exec::ExecRuntime;
    use zel_sandbox::sandbox::{SandboxKind, SandboxOptions, SandboxRunner};

    let root = std::env::temp_dir().join(format!("zel-exec-venv-rf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let execroot_dir = root.join("execroot/_main");
    let sandbox_base = root.join("sandbox");
    std::fs::create_dir_all(execroot_dir.join("external/ext_py/bin")).unwrap();
    std::fs::create_dir_all(execroot_dir.join("bazel-out/k8-opt-exec/bin/pylyglot/private"))
        .unwrap();
    std::fs::create_dir_all(&sandbox_base).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let execroot_dir = std::fs::canonicalize(&execroot_dir).unwrap();
    let sandbox_base = std::fs::canonicalize(&sandbox_base).unwrap();

    // Target python executable in external repo.
    let ext_py = execroot_dir.join("external/ext_py/bin/python3");
    std::fs::write(&ext_py, "#!/bin/sh\necho BUNDLER_VENV_OK\n").unwrap();
    std::fs::set_permissions(&ext_py, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Tool script that invokes its own venv python3 via its `.runfiles` tree.
    let tool_rel = "bazel-out/k8-opt-exec/bin/pylyglot/private/bundler";
    let tool_abs = execroot_dir.join(tool_rel);
    std::fs::write(
        &tool_abs,
        "#!/bin/sh\nexec \"$0.runfiles/_main/pylyglot/private/_bundler.venv/bin/python3\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&tool_abs, std::fs::Permissions::from_mode(0o755)).unwrap();

    let execroot = PathFragment::create(execroot_dir.to_str().unwrap());
    let fs = LocalFileSystem::default();
    let cache = CompactPersistentActionCache::new();
    let reporter = Reporter::new();
    let owner = Label::parse_canonical("//:bundle").unwrap();

    // Action 1: Unresolved relative symlink action (dangling in bazel-out).
    let mut venv_py_art = Artifact::derived(
        "k8-opt-exec",
        PathFragment::create("pylyglot/private/_bundler.venv/bin/python3"),
        owner.clone(),
    );
    venv_py_art.is_unresolved_symlink = true;
    let symlink_act = ActionAnalysisMetadata::SymlinkAction {
        owner: owner.clone(),
        input_path: PathFragment::create("../../../../../ext_py/bin/python3"),
        output: venv_py_art.clone(),
        is_executable: false,
    };

    // Action 2: Sandboxed spawn using `bundler` as a tool.
    let tool_art = Artifact::derived(
        "k8-opt-exec",
        PathFragment::create("pylyglot/private/bundler"),
        owner.clone(),
    );
    let out_art = Artifact::derived(
        "k8-fastbuild",
        PathFragment::create("bundle.out"),
        owner.clone(),
    );
    let spawn_act = ActionAnalysisMetadata::SpawnAction {
        owner,
        mnemonic: "PylyglotBundle".into(),
        progress_message: "Bundling".into(),
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            format!("{tool_rel} > {}", out_art.exec_path.path_string()),
        ],
        env: BTreeMap::new(),
        inherited_env: Default::default(),
        unset_env: Default::default(),
        inputs: vec![tool_art.clone(), venv_py_art.clone()],
        outputs: vec![out_art.clone()],
        execution_info: BTreeMap::new(),
        tools: vec![tool_art],
        unused_inputs_list: None,
        java_compile: None,
    };

    let mut entries = BTreeMap::new();
    entries.insert(
        "_main/pylyglot/private/bundler".to_string(),
        tool_rel.to_string(),
    );
    entries.insert(
        "_main/pylyglot/private/_bundler.venv/bin/python3".to_string(),
        venv_py_art.exec_path.path_string().to_string(),
    );
    entries.insert(
        "ext_py/bin/python3".to_string(),
        "external/ext_py/bin/python3".to_string(),
    );
    let mut unresolved_symlink_paths = BTreeSet::new();
    unresolved_symlink_paths.insert(venv_py_art.exec_path.path_string().to_string());

    let mut tool_runfiles = BTreeMap::new();
    tool_runfiles.insert(
        tool_rel.to_string(),
        ToolRunfilesSpec {
            workspace_name: "_main".into(),
            entries,
            repo_mapping: Vec::new(),
            enable_runfiles: true,
            legacy_external_runfiles: false,
            unresolved_symlink_paths,
        },
    );

    let av = StrategyAvailability {
        process_wrapper_sandbox: true,
        ..Default::default()
    };
    let registry = SpawnStrategyRegistry::build(&StrategyOptions::default(), &av).unwrap();
    let runner = SandboxRunner::new(
        SandboxKind::ProcessWrapper,
        &execroot_dir,
        &sandbox_base,
        SandboxOptions::default(),
        BTreeMap::new(),
        None,
    );
    let runtime = Arc::new(ExecRuntime {
        registry,
        sandbox_runners: vec![runner],
        worker: None,
        resources: ResourceManager::new(2.0, 1e9, BTreeMap::new(), 0),
        sandbox_debug: false,
        kill_delay: None,
        remote: None,
        dynamic: Default::default(),
        delay_local: Default::default(),
        allowed_local_actions_regex: None,
        remote_setup_error: None,
    });
    let opts = ExecOptions {
        runtime: Some(runtime),
        tool_runfiles,
        ..Default::default()
    };

    let acts = [&symlink_act, &spawn_act];
    let sched =
        zel_exec::execute_actions(&acts, 2, false, &execroot, &fs, &cache, &reporter, &opts);
    assert!(sched.cycle.is_none(), "{:?}", sched.cycle);
    assert_eq!(sched.failed(), 0, "{:?}", sched.actions);
    assert_eq!(
        std::fs::read_to_string(execroot_dir.join("bazel-out/k8-fastbuild/bin/bundle.out"))
            .unwrap()
            .trim(),
        "BUNDLER_VENV_OK"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn source_symlink_manifest_action_records_unresolved_targets_and_caches() {
    use std::collections::BTreeSet;
    use zel_analysis::SourceSymlinkManifestSpec;

    let root = std::env::temp_dir().join(format!("zel-exec-src-mf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let execroot_dir = root.join("execroot/_main");
    std::fs::create_dir_all(execroot_dir.join("pkg")).unwrap();
    std::fs::create_dir_all(execroot_dir.join("bazel-out/k8-fastbuild/bin/pkg")).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let execroot_dir = std::fs::canonicalize(&execroot_dir).unwrap();

    // Regular source file and a non-dangling source symlink.
    std::fs::write(execroot_dir.join("pkg/real.txt"), "hello\n").unwrap();
    std::os::unix::fs::symlink("real.txt", execroot_dir.join("pkg/src_sym.txt")).unwrap();
    let exe_rel = "bazel-out/k8-fastbuild/bin/pkg/tool";
    std::fs::write(execroot_dir.join(exe_rel), "#!/bin/sh\n").unwrap();

    let execroot = PathFragment::create(execroot_dir.to_str().unwrap());
    let fs = LocalFileSystem::default();
    let cache = CompactPersistentActionCache::new();
    let reporter = Reporter::new();
    let owner = Label::parse_canonical("//pkg:tool").unwrap();

    let real_art = Artifact::source(PathFragment::create("pkg/real.txt"), owner.clone());
    let src_sym_art = Artifact::source(PathFragment::create("pkg/src_sym.txt"), owner.clone());
    let mut unres_rel_art = Artifact::derived(
        "k8-fastbuild",
        PathFragment::create("pkg/sub/unres_rel"),
        owner.clone(),
    );
    unres_rel_art.is_unresolved_symlink = true;
    let mut unres_abs_art = Artifact::derived(
        "k8-fastbuild",
        PathFragment::create("pkg/sub/unres_abs"),
        owner.clone(),
    );
    unres_abs_art.is_unresolved_symlink = true;

    let mf_art = Artifact::derived(
        "k8-fastbuild",
        PathFragment::create("pkg/tool.runfiles_manifest"),
        owner.clone(),
    );

    let sym_rel_act = ActionAnalysisMetadata::SymlinkAction {
        owner: owner.clone(),
        input_path: PathFragment::create("../real.txt"),
        output: unres_rel_art.clone(),
        is_executable: false,
    };
    let sym_abs_act = ActionAnalysisMetadata::SymlinkAction {
        owner: owner.clone(),
        input_path: PathFragment::create("/etc/hosts"),
        output: unres_abs_art.clone(),
        is_executable: false,
    };

    let mut entries = BTreeMap::new();
    entries.insert("_main/pkg/real.txt".into(), "pkg/real.txt".into());
    entries.insert("_main/pkg/src_sym.txt".into(), "pkg/src_sym.txt".into());
    entries.insert(
        "_main/pkg/sub/unres_rel".into(),
        unres_rel_art.exec_path.path_string().to_string(),
    );
    entries.insert(
        "_main/pkg/sub/unres_abs".into(),
        unres_abs_art.exec_path.path_string().to_string(),
    );
    entries.insert("_main/pkg/tool".into(), exe_rel.into());

    let mf_act = ActionAnalysisMetadata::SourceSymlinkManifestAction {
        owner: owner.clone(),
        output: mf_art.clone(),
        spec: SourceSymlinkManifestSpec {
            entries: entries.clone(),
            workspace_name: "_main".into(),
            legacy_external_runfiles: false,
        },
        inputs: vec![
            real_art.clone(),
            src_sym_art.clone(),
            unres_rel_art.clone(),
            unres_abs_art.clone(),
        ],
    };

    // Run symlink + SourceSymlinkManifest actions.
    let acts = [&sym_rel_act, &sym_abs_act, &mf_act];
    let sched = zel_exec::execute_actions(
        &acts,
        2,
        false,
        &execroot,
        &fs,
        &cache,
        &reporter,
        &ExecOptions::default(),
    );
    assert!(sched.cycle.is_none(), "{:?}", sched.cycle);
    assert_eq!(sched.failed(), 0, "{:?}", sched.actions);

    let mf_abs = execroot_dir.join(mf_art.exec_path.path_string());
    let mf_text = std::fs::read_to_string(&mf_abs).unwrap();
    // Unresolved symlinks record raw read_link targets; regular/non-dangling files record abs paths.
    assert!(
        mf_text.contains("_main/pkg/sub/unres_rel ../real.txt\n"),
        "expected raw relative target:\n{mf_text}"
    );
    assert!(
        mf_text.contains("_main/pkg/sub/unres_abs /etc/hosts\n"),
        "expected raw absolute target:\n{mf_text}"
    );
    assert!(
        mf_text.contains(&format!(
            "_main/pkg/real.txt {}\n",
            execroot_dir.join("pkg/real.txt").display()
        )),
        "expected abs path for regular file:\n{mf_text}"
    );
    assert!(
        mf_text.contains(&format!(
            "_main/pkg/src_sym.txt {}\n",
            execroot_dir.join("pkg/src_sym.txt").display()
        )),
        "expected abs path for non-dangling source symlink:\n{mf_text}"
    );
    assert!(
        !mf_text.contains("bazel-out/k8-fastbuild/bin/pkg/sub/unres_rel"),
        "dangling bazel-out path must not appear in manifest:\n{mf_text}"
    );

    // Building the runfiles tree (`enable_runfiles: false` and `true`) preserves the exact
    // manifest bytes so a subsequent SourceSymlinkManifestAction hits the action cache and
    // never overwrites the manifest with stale paths.
    let mut unresolved_paths = BTreeSet::new();
    unresolved_paths.insert(unres_rel_art.exec_path.path_string().to_string());
    unresolved_paths.insert(unres_abs_art.exec_path.path_string().to_string());
    for enable_runfiles in [false, true] {
        zel_exec::runfiles_tree::build_runfiles_tree(
            &execroot_dir,
            &execroot_dir.join(exe_rel),
            "_main",
            &entries,
            &zel_exec::runfiles_tree::RunfilesTreeOptions {
                enable_runfiles,
                unresolved_symlink_paths: unresolved_paths.clone(),
                ..Default::default()
            },
        )
        .unwrap();
        let in_tree_mf = std::fs::read_to_string(
            execroot_dir.join("bazel-out/k8-fastbuild/bin/pkg/tool.runfiles/MANIFEST"),
        )
        .unwrap();
        assert_eq!(in_tree_mf, mf_text);
    }

    // Re-running must hit the action cache (0 executed, 3 cache hits).
    let sched2 = zel_exec::execute_actions(
        &acts,
        2,
        false,
        &execroot,
        &fs,
        &cache,
        &reporter,
        &ExecOptions::default(),
    );
    assert_eq!(sched2.failed(), 0, "{:?}", sched2.actions);
    assert_eq!(sched2.cache_hits(), 3, "{:?}", sched2.actions);

    // Changing the unresolved symlink target invalidates SourceSymlinkManifestAction and updates
    // the on-disk manifest to the new raw target.
    let sym_rel_act_v2 = ActionAnalysisMetadata::SymlinkAction {
        owner,
        input_path: PathFragment::create("../other.txt"),
        output: unres_rel_art,
        is_executable: false,
    };
    let acts_v2 = [&sym_rel_act_v2, &sym_abs_act, &mf_act];
    let sched3 = zel_exec::execute_actions(
        &acts_v2,
        2,
        false,
        &execroot,
        &fs,
        &cache,
        &reporter,
        &ExecOptions::default(),
    );
    assert_eq!(sched3.failed(), 0, "{:?}", sched3.actions);
    let mf_text_v2 = std::fs::read_to_string(&mf_abs).unwrap();
    assert!(
        mf_text_v2.contains("_main/pkg/sub/unres_rel ../other.txt\n"),
        "expected updated raw relative target:\n{mf_text_v2}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
