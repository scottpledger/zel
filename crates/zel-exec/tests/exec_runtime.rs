//! Integration tests of the execution runtime: strategy selection → runners, sandboxing, action
//! cache semantics (action key, discovered inputs, digest functions), `--check_up_to_date` and
//! resource limits.

use std::collections::BTreeMap;
use std::sync::Arc;
use zel_analysis::{ActionAnalysisMetadata, Artifact};
use zel_cmdline::Label;
use zel_events::Reporter;
use zel_exec::resources::ResourceManager;
use zel_exec::strategy::{SpawnStrategyRegistry, StrategyAvailability, StrategyOptions};
use zel_exec::*;
use zel_sandbox::sandbox::{SandboxKind, SandboxOptions, SandboxRunner};
use zel_vfs::{LocalFileSystem, PathFragment};

struct Ws {
    execroot: PathFragment,
    root: std::path::PathBuf,
}

fn ws(name: &str) -> Ws {
    let d = std::env::temp_dir().join(format!("zel-rt-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let er = d.join("ob/execroot/_main");
    std::fs::create_dir_all(&er).unwrap();
    let root = std::fs::canonicalize(&d).unwrap();
    let er = root.join("ob/execroot/_main");
    Ws { execroot: PathFragment::create(er.to_str().unwrap()), root }
}

fn out(rel: &str) -> Artifact {
    Artifact::derived(
        "k8-fastbuild",
        PathFragment::create(rel.trim_start_matches("bazel-out/k8-fastbuild/bin/")),
        Label::parse_canonical("//pkg:t").unwrap(),
    )
}

fn src(rel: &str) -> Artifact {
    Artifact::source(PathFragment::create(rel), Label::parse_canonical("//pkg:t").unwrap())
}

fn spawn(mnemonic: &str, script: &str, inputs: Vec<Artifact>, outputs: Vec<Artifact>, info: &[(&str, &str)]) -> ActionAnalysisMetadata {
    ActionAnalysisMetadata::SpawnAction {
        owner: Label::parse_canonical("//pkg:t").unwrap(),
        mnemonic: mnemonic.into(),
        progress_message: format!("Doing {mnemonic} //pkg:t"),
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        env: BTreeMap::new(),
        inherited_env: Default::default(),
        unset_env: Default::default(),
        inputs,
        outputs,
        execution_info: info.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        java_compile: None,
    }
}

fn runtime(w: &Ws, opts: StrategyOptions, cpu: f64) -> Arc<ExecRuntime> {
    let av = StrategyAvailability { process_wrapper_sandbox: true, ..Default::default() };
    let registry = SpawnStrategyRegistry::build(&opts, &av).unwrap();
    let er = std::path::Path::new(w.execroot.path_string());
    let sandbox_base = w.root.join("ob/sandbox");
    let runner = SandboxRunner::new(
        SandboxKind::ProcessWrapper,
        er,
        &sandbox_base,
        SandboxOptions::default(),
        BTreeMap::new(),
        None,
    );
    Arc::new(ExecRuntime {
        registry,
        sandbox_runners: vec![runner],
        worker: None,
        resources: ResourceManager::new(cpu, 1e9, BTreeMap::new(), 0),
        sandbox_debug: false,
        kill_delay: None,
        remote: None,
        dynamic: Default::default(),
        delay_local: Default::default(),
        allowed_local_actions_regex: None,
        remote_setup_error: None,
    })
}

fn run(
    a: &ActionAnalysisMetadata,
    w: &Ws,
    cache: &CompactPersistentActionCache,
    opts: &ExecOptions,
) -> ActionExecutionOutcome {
    execute_action_with(a, &w.execroot, &LocalFileSystem::default(), cache, &Reporter::new(), opts).unwrap()
}

#[test]
fn strategies_select_the_runner() {
    let w = ws("strat");
    let o = out("bazel-out/k8-fastbuild/bin/pkg/a");
    let a = spawn("Genrule", "echo a > bazel-out/k8-fastbuild/bin/pkg/a", vec![], vec![o], &[]);
    let cache = CompactPersistentActionCache::new();
    // Default: worker (unsupported by the spawn) → sandboxed (processwrapper-sandbox).
    let opts = ExecOptions { runtime: Some(runtime(&w, StrategyOptions::default(), 4.0)), ..Default::default() };
    let r = run(&a, &w, &cache, &opts);
    assert!(r.succeeded(), "{:?}", r.failure);
    assert_eq!(r.runner.as_deref(), Some("processwrapper-sandbox"));

    // --strategy=Genrule=local routes to the standalone runner.
    let so = StrategyOptions::from_raw(&[], &[], &["Genrule=local".into()], &[]).unwrap();
    let opts = ExecOptions { runtime: Some(runtime(&w, so, 4.0)), ..Default::default() };
    let o2 = out("bazel-out/k8-fastbuild/bin/pkg/b");
    let b = spawn("Genrule", "echo b > bazel-out/k8-fastbuild/bin/pkg/b", vec![], vec![o2], &[]);
    let r = run(&b, &w, &cache, &opts);
    assert_eq!(r.runner.as_deref(), Some("local"));

    // Execution requirement `local` skips sandboxing even under the default strategy list.
    let opts = ExecOptions { runtime: Some(runtime(&w, StrategyOptions::default(), 4.0)), ..Default::default() };
    let o3 = out("bazel-out/k8-fastbuild/bin/pkg/c");
    let c = spawn("Genrule", "echo c > bazel-out/k8-fastbuild/bin/pkg/c", vec![], vec![o3], &[("local", "1")]);
    assert_eq!(run(&c, &w, &cache, &opts).runner.as_deref(), Some("local"));
}

#[test]
fn no_usable_strategy_is_a_failure_with_resolver_message() {
    let w = ws("nousable");
    let so = StrategyOptions { spawn_strategy: vec!["worker".into()], ..Default::default() };
    let opts = ExecOptions { runtime: Some(runtime(&w, so, 4.0)), ..Default::default() };
    let a = spawn("Genrule", "true", vec![], vec![out("bazel-out/k8-fastbuild/bin/pkg/x")], &[]);
    let r = run(&a, &w, &CompactPersistentActionCache::new(), &opts);
    let f = r.failure.unwrap();
    assert!(f.messages[0].contains("Doing Genrule //pkg:t failed: Genrule spawn cannot be executed with any of the available strategies: [worker]."), "{:?}", f.messages);
}

#[test]
fn sandbox_hides_undeclared_inputs_and_local_does_not() {
    let w = ws("hide");
    std::fs::create_dir_all(w.root.join("ob/execroot/_main/pkg")).unwrap();
    std::fs::write(w.root.join("ob/execroot/_main/pkg/secret.txt"), "s").unwrap();
    std::fs::write(w.root.join("ob/execroot/_main/pkg/in.txt"), "i").unwrap();
    let o = out("bazel-out/k8-fastbuild/bin/pkg/h");
    let script = "cat pkg/in.txt > bazel-out/k8-fastbuild/bin/pkg/h; if [ -e pkg/secret.txt ]; then echo SEES >> bazel-out/k8-fastbuild/bin/pkg/h; fi";
    let a = spawn("Genrule", script, vec![src("pkg/in.txt")], vec![o], &[]);
    let cache = CompactPersistentActionCache::new();
    let opts = ExecOptions { runtime: Some(runtime(&w, StrategyOptions::default(), 4.0)), ..Default::default() };
    assert!(run(&a, &w, &cache, &opts).succeeded());
    assert_eq!(std::fs::read_to_string(w.root.join("ob/execroot/_main/bazel-out/k8-fastbuild/bin/pkg/h")).unwrap(), "i");
    let so = StrategyOptions::from_raw(&["local".into()], &[], &[], &[]).unwrap();
    let opts = ExecOptions { runtime: Some(runtime(&w, so, 4.0)), ..Default::default() };
    cache.clear();
    assert!(run(&a, &w, &cache, &opts).succeeded());
    assert_eq!(std::fs::read_to_string(w.root.join("ob/execroot/_main/bazel-out/k8-fastbuild/bin/pkg/h")).unwrap(), "iSEES\n");
}

#[test]
fn action_key_covers_command_env_and_inputs() {
    let w = ws("key");
    std::fs::create_dir_all(w.root.join("ob/execroot/_main/pkg")).unwrap();
    let input = w.root.join("ob/execroot/_main/pkg/in.txt");
    std::fs::write(&input, "one").unwrap();
    let o = out("bazel-out/k8-fastbuild/bin/pkg/k");
    let cache = CompactPersistentActionCache::new();
    let opts = ExecOptions::default();
    let mk = |script: &str| spawn("Genrule", script, vec![src("pkg/in.txt")], vec![o.clone()], &[]);
    let a = mk("cat pkg/in.txt > bazel-out/k8-fastbuild/bin/pkg/k");
    assert!(!run(&a, &w, &cache, &opts).cache_hit);
    assert!(run(&a, &w, &cache, &opts).cache_hit, "unchanged action is an action cache hit");
    // Input content change.
    std::fs::write(&input, "two").unwrap();
    assert!(!run(&a, &w, &cache, &opts).cache_hit);
    assert_eq!(std::fs::read_to_string(w.root.join("ob/execroot/_main/bazel-out/k8-fastbuild/bin/pkg/k")).unwrap(), "two");
    assert!(run(&a, &w, &cache, &opts).cache_hit);
    // Command line change.
    let b = mk("cat pkg/in.txt > bazel-out/k8-fastbuild/bin/pkg/k # changed");
    assert!(!run(&b, &w, &cache, &opts).cache_hit);
    // Execution requirement change.
    let c = spawn("Genrule", "cat pkg/in.txt > bazel-out/k8-fastbuild/bin/pkg/k # changed", vec![src("pkg/in.txt")], vec![o.clone()], &[("no-cache", "1")]);
    assert!(!run(&c, &w, &cache, &opts).cache_hit);
    // Env change.
    let mut d = c.clone();
    if let ActionAnalysisMetadata::SpawnAction { env, .. } = &mut d {
        env.insert("X".into(), "1".into());
    }
    assert!(!run(&d, &w, &cache, &opts).cache_hit);
}

#[test]
fn discovered_inputs_from_depfile_invalidate_the_action() {
    let w = ws("dotd");
    let er = w.root.join("ob/execroot/_main");
    std::fs::create_dir_all(er.join("pkg")).unwrap();
    std::fs::write(er.join("pkg/a.c"), "int main(){}").unwrap();
    std::fs::write(er.join("pkg/hidden.h"), "v1").unwrap();
    let obj = out("bazel-out/k8-fastbuild/bin/pkg/a.o");
    let dfile = out("bazel-out/k8-fastbuild/bin/pkg/a.d");
    let script = "cp pkg/a.c bazel-out/k8-fastbuild/bin/pkg/a.o; printf 'bazel-out/k8-fastbuild/bin/pkg/a.o: pkg/a.c \\\\\\n pkg/hidden.h\\n' > bazel-out/k8-fastbuild/bin/pkg/a.d";
    let a = spawn("CppCompile", script, vec![src("pkg/a.c")], vec![obj, dfile], &[]);
    let cache = CompactPersistentActionCache::new();
    let opts = ExecOptions::default();
    assert!(!run(&a, &w, &cache, &opts).cache_hit);
    let e = cache.get_entry("bazel-out/k8-fastbuild/bin/pkg/a.o").unwrap();
    assert_eq!(e.discovered_inputs, vec!["pkg/hidden.h"]);
    assert!(run(&a, &w, &cache, &opts).cache_hit);
    // Touching the discovered (undeclared) header re-executes.
    std::fs::write(er.join("pkg/hidden.h"), "v2").unwrap();
    assert!(!run(&a, &w, &cache, &opts).cache_hit);
    assert!(run(&a, &w, &cache, &opts).cache_hit);
}

#[test]
fn depfile_parser() {
    assert_eq!(
        parse_depfile("out.o: a.c \\\n  b.h c\\ d.h\nother.o: x.h\n"),
        vec!["a.c", "b.h", "c d.h", "x.h"]
    );
    assert!(parse_depfile("").is_empty());
}

#[test]
fn check_up_to_date_never_executes() {
    let w = ws("cuptd");
    let o = out("bazel-out/k8-fastbuild/bin/pkg/u");
    let a = spawn("Genrule", "echo u > bazel-out/k8-fastbuild/bin/pkg/u", vec![], vec![o], &[]);
    let cache = CompactPersistentActionCache::new();
    let check = ExecOptions { check_up_to_date: true, ..Default::default() };
    let r = run(&a, &w, &cache, &check);
    assert_eq!(r.failure.unwrap().messages, vec!["ERROR: action 'Doing Genrule //pkg:t' is not up-to-date"]);
    assert!(!w.root.join("ob/execroot/_main/bazel-out/k8-fastbuild/bin/pkg/u").exists());
    // After a real build, the check passes (action cache hit).
    assert!(run(&a, &w, &cache, &ExecOptions::default()).succeeded());
    let r = run(&a, &w, &cache, &check);
    assert!(r.cache_hit && r.succeeded());
}

#[test]
fn resource_manager_limits_parallel_spawns() {
    let w = ws("res");
    // cpu=1 => spawns (1 cpu each) run one at a time even with 4 scheduler threads.
    let rt = runtime(&w, StrategyOptions::from_raw(&["local".into()], &[], &[], &[]).unwrap(), 1.0);
    let mk = |n: &str| {
        spawn(
            "Genrule",
            &format!("sleep 0.3; echo x > bazel-out/k8-fastbuild/bin/pkg/{n}"),
            vec![],
            vec![out(&format!("bazel-out/k8-fastbuild/bin/pkg/{n}"))],
            &[],
        )
    };
    let actions = vec![mk("r1"), mk("r2"), mk("r3")];
    let refs: Vec<&ActionAnalysisMetadata> = actions.iter().collect();
    let opts = ExecOptions { runtime: Some(rt), ..Default::default() };
    let t = std::time::Instant::now();
    let res = execute_actions(
        &refs,
        4,
        false,
        &w.execroot,
        &LocalFileSystem::default(),
        &CompactPersistentActionCache::new(),
        &Reporter::new(),
        &opts,
    );
    assert_eq!(res.failed(), 0);
    assert!(t.elapsed().as_millis() >= 880, "serialized by the resource manager: {:?}", t.elapsed());

    let w2 = ws("res2");
    let rt = runtime(&w2, StrategyOptions::from_raw(&["local".into()], &[], &[], &[]).unwrap(), 8.0);
    let opts = ExecOptions { runtime: Some(rt), ..Default::default() };
    let t = std::time::Instant::now();
    let res = execute_actions(&refs, 4, false, &w2.execroot, &LocalFileSystem::default(), &CompactPersistentActionCache::new(), &Reporter::new(), &opts);
    assert_eq!(res.failed(), 0);
    assert!(t.elapsed().as_millis() < 800, "parallel with enough cpu: {:?}", t.elapsed());
}

#[test]
fn digest_function_changes_cache_keys_and_is_honored() {
    let w = ws("dig");
    let o = out("bazel-out/k8-fastbuild/bin/pkg/d");
    let a = spawn("Genrule", "echo d > bazel-out/k8-fastbuild/bin/pkg/d", vec![], vec![o], &[]);
    for f in DigestFunction::ALL {
        let cache = CompactPersistentActionCache::with_digest(f);
        let opts = ExecOptions { digest: f, ..Default::default() };
        assert!(!run(&a, &w, &cache, &opts).cache_hit);
        let e = cache.get_entry("bazel-out/k8-fastbuild/bin/pkg/d").unwrap();
        assert_eq!(e.files["bazel-out/k8-fastbuild/bin/pkg/d"].len(), f.digest_length() * 2, "{f:?}");
        assert_eq!(e.action_key.len(), f.digest_length() * 2);
        assert!(run(&a, &w, &cache, &opts).cache_hit);
    }
}

#[test]
fn sandbox_failure_message_and_debug_command() {
    let w = ws("dbg");
    let a = spawn("Genrule", "echo bad >&2; exit 4", vec![], vec![out("bazel-out/k8-fastbuild/bin/pkg/f")], &[]);
    let mut rt = runtime(&w, StrategyOptions::default(), 4.0);
    let r = run(&a, &w, &CompactPersistentActionCache::new(), &ExecOptions { runtime: Some(rt.clone()), ..Default::default() });
    let f = r.failure.unwrap();
    assert!(f.messages[0].contains("(Exit 4): sh failed: error executing Genrule command (from genrule rule target //pkg:t) /bin/sh -c"), "{:?}", f.messages);
    assert_eq!(r.runner.as_deref(), Some("processwrapper-sandbox"));
    // With --sandbox_debug the message shows the sandbox command line (cd into the sandbox).
    {
        let er = std::path::Path::new(w.execroot.path_string());
        let runner = SandboxRunner::new(
            SandboxKind::ProcessWrapper,
            er,
            &w.root.join("ob/sandbox"),
            SandboxOptions { debug: true, ..Default::default() },
            BTreeMap::new(),
            None,
        );
        let r0 = Arc::get_mut(&mut rt);
        assert!(r0.is_none() || true);
        let av = StrategyAvailability { process_wrapper_sandbox: true, ..Default::default() };
        let rt2 = Arc::new(ExecRuntime {
            registry: SpawnStrategyRegistry::build(&StrategyOptions::default(), &av).unwrap(),
            sandbox_runners: vec![runner],
            worker: None,
            resources: ResourceManager::new(4.0, 1e9, BTreeMap::new(), 0),
            sandbox_debug: true,
            kill_delay: None,
            remote: None,
            dynamic: Default::default(),
            delay_local: Default::default(),
            allowed_local_actions_regex: None,
        remote_setup_error: None,
        });
        let r = run(&a, &w, &CompactPersistentActionCache::new(), &ExecOptions { runtime: Some(rt2), ..Default::default() });
        let m = &r.failure.unwrap().messages[0];
        // The sandbox id is a server-lifetime counter (`SpawnExecutionContext.getId`), so the
        // directory number depends on the spawns run before in this process.
        assert!(m.contains("processwrapper-sandbox/") && m.contains("/execroot/_main"), "{m}");
        let kept = std::fs::read_dir(w.root.join("ob/sandbox/processwrapper-sandbox"))
            .unwrap()
            .any(|e| e.unwrap().path().join("execroot/_main").exists());
        assert!(kept, "sandbox kept with --sandbox_debug");
    }
}

#[test]
fn symlink_action_links_to_absolute_execroot_path() {
    let w = ws("symlink");
    let er = w.root.join("ob/execroot/_main");
    std::fs::create_dir_all(er.join("pkg")).unwrap();
    std::fs::write(er.join("pkg/run.sh"), "#!/bin/sh\n").unwrap();
    let a = ActionAnalysisMetadata::SymlinkAction {
        owner: Label::parse_canonical("//pkg:sb").unwrap(),
        input_path: PathFragment::create("pkg/run.sh"),
        output: out("bazel-out/k8-fastbuild/bin/pkg/sb"),
        is_executable: false,
    };
    let r = run(&a, &w, &CompactPersistentActionCache::new(), &ExecOptions::default());
    assert!(r.succeeded(), "{:?}", r.failure);
    let link = er.join("bazel-out/k8-fastbuild/bin/pkg/sb");
    assert_eq!(std::fs::read_link(&link).unwrap(), er.join("pkg/run.sh"));
    assert!(link.exists());
}

#[test]
fn tree_artifact_outputs_are_precreated_in_every_runner() {
    for (name, strategies) in [("tree-sbx", vec![]), ("tree-local", vec!["local".to_string()])] {
        let w = ws(name);
        let mut d = out("bazel-out/k8-fastbuild/bin/pkg/d");
        d.is_tree_artifact = true;
        // Writes into the directory without mkdir (Bazel creates tree outputs beforehand).
        let a = spawn("Genrule", "echo A > bazel-out/k8-fastbuild/bin/pkg/d/a.txt", vec![], vec![d.clone()], &[]);
        let so = StrategyOptions { spawn_strategy: strategies, ..Default::default() };
        let opts = ExecOptions { runtime: Some(runtime(&w, so, 4.0)), ..Default::default() };
        let r = run(&a, &w, &CompactPersistentActionCache::new(), &opts);
        assert!(r.succeeded(), "{name}: {:?}", r.failure);
        let p = w.root.join("ob/execroot/_main/bazel-out/k8-fastbuild/bin/pkg/d/a.txt");
        assert_eq!(std::fs::read_to_string(p).unwrap(), "A\n", "{name}");
        // An empty tree is valid too.
        let b = spawn("Genrule", "true", vec![], vec![{ let mut e = out("bazel-out/k8-fastbuild/bin/pkg/e"); e.is_tree_artifact = true; e }], &[]);
        assert!(run(&b, &w, &CompactPersistentActionCache::new(), &opts).succeeded(), "{name}");
    }
}

#[test]
fn inherited_env_comes_from_the_client_env_and_is_part_of_the_cache_key() {
    let w = ws("inherit");
    let o = out("bazel-out/k8-fastbuild/bin/pkg/env.txt");
    let mut a = spawn(
        "Genrule",
        "for v in FIXED INH UNS ABSENT; do echo \"$v=${!v-<unset>}\"; done > bazel-out/k8-fastbuild/bin/pkg/env.txt",
        vec![],
        vec![o],
        &[("local", "1")],
    );
    if let ActionAnalysisMetadata::SpawnAction { argv, env, inherited_env, unset_env, .. } = &mut a {
        argv[0] = "/bin/bash".into();
        env.insert("FIXED".into(), "f".into());
        env.insert("UNS".into(), "fixed-but-unset".into());
        inherited_env.insert("INH".into());
        inherited_env.insert("ABSENT".into());
        unset_env.insert("UNS".into());
    }
    let client = |inh: Option<&str>| -> BTreeMap<String, String> {
        let mut m: BTreeMap<String, String> = [("UNS".to_string(), "client".to_string()), ("IGNORED".to_string(), "x".to_string())].into();
        if let Some(v) = inh {
            m.insert("INH".into(), v.into());
        }
        m
    };
    let cache = CompactPersistentActionCache::new();
    let read = || std::fs::read_to_string(std::path::Path::new(w.execroot.path_string()).join("bazel-out/k8-fastbuild/bin/pkg/env.txt")).unwrap();
    let run_with = |c: BTreeMap<String, String>| run(&a, &w, &cache, &ExecOptions { client_env: c, ..Default::default() });

    let r = run_with(client(Some("v1")));
    assert!(r.succeeded() && !r.cache_hit, "{:?}", r.failure);
    // Fixed value, inherited value, unset (even though both fixed and in the client env), absent.
    assert_eq!(read(), "FIXED=f\nINH=v1\nUNS=<unset>\nABSENT=<unset>\n");
    // Same inherited value, unrelated variable changed: cache hit.
    let mut c = client(Some("v1"));
    c.insert("IGNORED".into(), "y".into());
    assert!(run_with(c).cache_hit);
    // Different value of an inherited variable: re-executed with the new value.
    let r = run_with(client(Some("v2")));
    assert!(!r.cache_hit);
    assert_eq!(read(), "FIXED=f\nINH=v2\nUNS=<unset>\nABSENT=<unset>\n");
    // Inherited variable disappears from the client environment: re-executed.
    assert!(!run_with(client(None)).cache_hit);
    assert_eq!(read(), "FIXED=f\nINH=<unset>\nUNS=<unset>\nABSENT=<unset>\n");
    // Changing the set of inherited / unset names changes the action key.
    let mut b = a.clone();
    if let ActionAnalysisMetadata::SpawnAction { inherited_env, .. } = &mut b {
        inherited_env.insert("OTHER".into());
    }
    assert_ne!(a.action_key(), b.action_key());
    assert!(!run(&b, &w, &cache, &ExecOptions { client_env: client(None), ..Default::default() }).cache_hit);
}

/// `JavaCompileAction` with `--experimental_java_classpath=bazel`: the first attempt gets the
/// reduced classpath (direct jars + jars named by the direct deps' .jdeps) and
/// `BAZEL_REDUCED`; when the tool reports `requires_reduced_classpath_fallback` in the .jdeps the
/// outputs are deleted and the action re-runs with the full classpath and `BAZEL_FALLBACK`.
#[test]
fn java_compile_reduced_classpath_falls_back_to_the_full_classpath() {
    let w = ws("javared");
    let er = std::path::PathBuf::from(w.execroot.path_string());
    let mk = |rel: &str, body: &[u8]| {
        let p = er.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    mk("pkg/A.java", b"class A {}");
    mk("pkg/a.jar", b"a");
    mk("pkg/b.jar", b"b");
    mk("bazel-out/k8-fastbuild/bin/pkg/a.jdeps", b"");
    let jar = "bazel-out/k8-fastbuild/bin/pkg/x.jar";
    let jdeps = "bazel-out/k8-fastbuild/bin/pkg/x.jdeps";
    let params = format!("{jar}-1.params");
    // Reduced attempt: report the fallback (no jar). Fallback attempt: the "jar" is the
    // classpath param file the tool saw.
    let script = format!(
        "P=${{1#@}}; if grep -q BAZEL_REDUCED \"$P\"; then printf \"\\050\\001\" > {jdeps}; else cp \"$P\" {jar}; : > {jdeps}; fi"
    );
    let spec = |mode: &str| zel_analysis::JavaCompileSpec {
        classpath_mode: mode.into(),
        classpath_arg_index: 1,
        classpath_params_path: params.clone(),
        transitive_inputs: vec!["pkg/a.jar".into(), "pkg/b.jar".into()],
        direct_jars: vec!["pkg/a.jar".into()],
        dependency_artifacts: vec!["bazel-out/k8-fastbuild/bin/pkg/a.jdeps".into()],
        mandatory_inputs: vec!["pkg/A.java".into()],
        deps_proto: jdeps.into(),
    };
    let action = |mode: &str| {
        let mut a = spawn(
            "Javac",
            "",
            vec![src("pkg/A.java"), src("pkg/a.jar"), src("pkg/b.jar"), out("bazel-out/k8-fastbuild/bin/pkg/a.jdeps")],
            vec![out(jar), out(jdeps)],
            &[],
        );
        if let ActionAnalysisMetadata::SpawnAction { argv, java_compile, .. } = &mut a {
            *argv = vec!["/bin/sh".into(), "-c".into(), script.clone(), "sh".into(), format!("@{params}")];
            *java_compile = Some(Box::new(spec(mode)));
        }
        a
    };
    let opts = ExecOptions { runtime: Some(runtime(&w, StrategyOptions::default(), 4.0)), ..Default::default() };
    let r = run(&action("BAZEL"), &w, &CompactPersistentActionCache::new(), &opts);
    assert!(r.succeeded(), "{:?}", r.failure);
    assert!(er.join(jar).exists(), "the fallback run produced the jar");
    let seen = std::fs::read_to_string(er.join(jar)).unwrap();
    assert_eq!(seen, "--classpath\npkg/a.jar\npkg/b.jar\n--reduce_classpath_mode\nBAZEL_FALLBACK\n");

    // BAZEL_NO_FALLBACK: a single attempt with the reduced classpath and `--reduce_classpath_mode NONE`.
    let w2 = ws("javared2");
    let er2 = std::path::PathBuf::from(w2.execroot.path_string());
    for (rel, body) in [("pkg/A.java", "class A {}"), ("pkg/a.jar", "a"), ("pkg/b.jar", "b"), ("bazel-out/k8-fastbuild/bin/pkg/a.jdeps", "")] {
        let p = er2.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    let opts2 = ExecOptions { runtime: Some(runtime(&w2, StrategyOptions::default(), 4.0)), ..Default::default() };
    let r = run(&action("BAZEL_NO_FALLBACK"), &w2, &CompactPersistentActionCache::new(), &opts2);
    let seen_nf = std::fs::read_to_string(er2.join(&params)).unwrap();
    assert_eq!(seen_nf, "--classpath\npkg/a.jar\n--reduce_classpath_mode\nNONE\n");
    assert!(r.succeeded(), "{:?}", r.failure);
    assert_eq!(std::fs::read_to_string(er2.join(jar)).unwrap(), seen_nf, "a single attempt with the reduced classpath");
}
