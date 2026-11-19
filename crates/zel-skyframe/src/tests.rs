//! Tests of the file/package/action node functions, diff awareness and the executor.

use super::*;
use crate::diff::{DiffAwareness, ExplicitDiffAwareness, ModifiedFileSet, StatDiffAwareness};
use crate::executor::*;
use std::collections::HashMap;
use std::sync::Mutex;
use zel_packages::{LoaderConfig, PackageLoader, SimpleRepoResolver};
use zel_vfs::{InMemoryFileSystem, LocalFileSystem};

fn mem_world(files: &[(&str, &str)]) -> (Arc<InMemoryFileSystem>, PathFragment) {
    let fs = Arc::new(InMemoryFileSystem::default());
    let ws = PathFragment::create("/workspace");
    fs.create_directory_and_parents(&ws).unwrap();
    for (p, c) in files {
        let path = ws.get_relative_str(p);
        fs.create_directory_and_parents(&path.parent_directory().unwrap())
            .unwrap();
        fs.write_file(&path, c.as_bytes()).unwrap();
    }
    (fs, ws)
}

fn pkg(name: &str) -> PackageIdentifier {
    PackageIdentifier::create_in_main_repo(name).unwrap()
}

fn rooted(ws: &PathFragment, rel: &str) -> RootedPath {
    RootedPath::to_rooted_path(ws.clone(), PathFragment::create(rel)).unwrap()
}

fn tmpdir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("zel-sky-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn old_style_evaluator(
    fs: &Arc<InMemoryFileSystem>,
    ws: &PathFragment,
) -> InMemoryMemoizingEvaluator {
    let mut evaluator = InMemoryMemoizingEvaluator::new();
    let dynfs = Arc::clone(fs) as Arc<dyn FileSystem>;
    evaluator
        .register_function(
            SkyFunctionName::FILE_STATE,
            Arc::new(FileStateFunction::new(Arc::clone(&dynfs))),
        )
        .register_function(
            SkyFunctionName::DIRECTORY_LISTING,
            Arc::new(DirectoryListingStateFunction {
                fs: Arc::clone(&dynfs),
            }),
        )
        .register_function(
            SkyFunctionName::PACKAGE_LOOKUP,
            Arc::new(PackageLookupFunction {
                workspace_root: ws.clone(),
                repo_resolver: Default::default(),
            }),
        )
        .register_function(
            SkyFunctionName::PACKAGE,
            Arc::new(PackageFunction::new(dynfs)),
        );
    evaluator
}

const BUILD_V1: &str = r#"
filegroup(
    name = "lib",
    srcs = ["lib.sh"],
)
filegroup(
    name = "bin",
    srcs = ["main.sh", ":lib"],
)
"#;

#[test]
fn package_loading_and_invalidation() {
    let (fs, ws) = mem_world(&[("foo/BUILD", BUILD_V1)]);
    let evaluator = old_style_evaluator(&fs, &ws);
    let pkg_key = SkyKey::Package(pkg("foo"));
    let res = evaluator.evaluate(std::slice::from_ref(&pkg_key));
    assert!(!res.has_error(), "{:?}", res.errors);
    let pkg_val = res.get::<PackageValue>(&pkg_key).unwrap();
    assert_eq!(pkg_val.targets.len(), 2);
    assert_eq!(pkg_val.targets["bin"].rule_class, "filegroup");
    assert_eq!(
        pkg_val.targets["bin"].srcs,
        vec!["//foo:main.sh", "//foo:lib"]
    );

    // Modify the BUILD file; the recorded dependency (found through the recording file system)
    // is invalidated. No manual knowledge of which key to dirty is needed beyond the file path.
    fs.write_file(
        &ws.get_relative_str("foo/BUILD"),
        br#"filegroup(name = "new_bin", srcs = ["main.sh"])"#,
    )
    .unwrap();
    evaluator.invalidate(&[SkyKey::FileState(rooted(&ws, "foo/BUILD"))]);
    let res2 = evaluator.evaluate(std::slice::from_ref(&pkg_key));
    let pkg_val2 = res2.get::<PackageValue>(&pkg_key).unwrap();
    assert_eq!(pkg_val2.targets.len(), 1);
    assert!(pkg_val2.targets.contains_key("new_bin"));
}

#[test]
fn package_depends_on_every_file_it_reads_not_just_the_build_file() {
    let (fs, ws) = mem_world(&[
        (
            "app/BUILD",
            r#"
load(":defs.bzl", "mk")
mk("generated")
filegroup(name = "all_txt", srcs = glob(["*.txt"]))
"#,
        ),
        (
            "app/defs.bzl",
            "def mk(name):\n    native.filegroup(name = name, srcs = [])\n",
        ),
        ("app/a.txt", "a"),
    ]);
    let evaluator = old_style_evaluator(&fs, &ws);
    let k = SkyKey::Package(pkg("app"));
    let r = evaluator.evaluate(std::slice::from_ref(&k));
    assert!(!r.has_error(), "{:?}", r.errors);
    let deps = evaluator.direct_deps(&k);
    let dep_strs: Vec<String> = deps.iter().map(|d| d.to_string()).collect();
    assert!(
        dep_strs.iter().any(|d| d.contains("defs.bzl")),
        "bzl file is a dependency: {dep_strs:?}"
    );
    assert!(
        dep_strs
            .iter()
            .any(|d| d.starts_with("DIRECTORY_LISTING_STATE") && d.contains("/workspace/app")),
        "glob'd directory is a dependency: {dep_strs:?}"
    );
    let v = r.get::<PackageValue>(&k).unwrap();
    assert!(v.targets.contains_key("generated"));
    assert_eq!(v.targets["all_txt"].srcs.len(), 1);

    // 1) A new file matching the glob: only the directory listing changes.
    fs.write_file(&ws.get_relative_str("app/b.txt"), b"b")
        .unwrap();
    let diff = StatDiffAwareness { threads: 2 }.get_diff(&evaluator, fs.as_ref());
    let ModifiedFileSet::Keys(keys) = &diff else {
        panic!("expected a diff, got {diff:?}")
    };
    assert!(
        keys.iter()
            .any(|k| matches!(k, SkyKey::DirectoryListingState(_))),
        "{keys:?}"
    );
    evaluator.invalidate(keys);
    let r2 = evaluator.evaluate(std::slice::from_ref(&k));
    assert_eq!(
        r2.get::<PackageValue>(&k).unwrap().targets["all_txt"]
            .srcs
            .len(),
        2
    );

    // 2) Editing the loaded .bzl file changes the package.
    fs.write_file(
        &ws.get_relative_str("app/defs.bzl"),
        b"def mk(name):\n    native.filegroup(name = name + \"_v2\", srcs = [])\n",
    )
    .unwrap();
    evaluator.invalidate(&[SkyKey::FileState(
        recording_fs::rooted(&ws.get_relative_str("app/defs.bzl")).unwrap(),
    )]);
    let r3 = evaluator.evaluate(std::slice::from_ref(&k));
    let v3 = r3.get::<PackageValue>(&k).unwrap();
    assert!(v3.targets.contains_key("generated_v2") && !v3.targets.contains_key("generated"));
}

#[test]
fn touching_a_file_without_changing_it_is_pruned() {
    let (fs, ws) = mem_world(&[("p/BUILD", BUILD_V1)]);
    let evaluator = old_style_evaluator(&fs, &ws);
    let k = SkyKey::Package(pkg("p"));
    evaluator.evaluate(std::slice::from_ref(&k));
    // Same bytes rewritten: FILE_STATE is rebuilt but equal => PACKAGE only verified.
    fs.write_file(&ws.get_relative_str("p/BUILD"), BUILD_V1.as_bytes())
        .unwrap();
    evaluator.invalidate(&[SkyKey::FileState(
        recording_fs::rooted(&ws.get_relative_str("p/BUILD")).unwrap(),
    )]);
    let r = evaluator.evaluate(std::slice::from_ref(&k));
    assert!(
        r.stats
            .evaluated
            .iter()
            .all(|k| matches!(k, SkyKey::FileState(_))),
        "{:?}",
        r.stats.evaluated
    );
    assert!(r.stats.cleaned.contains(&k));
}

#[test]
fn missing_package_is_an_error_and_recovers_when_created() {
    let (fs, ws) = mem_world(&[("other/BUILD", "")]);
    let evaluator = old_style_evaluator(&fs, &ws);
    let k = SkyKey::Package(pkg("nope"));
    let r = evaluator.evaluate(std::slice::from_ref(&k));
    assert!(
        r.errors[&k].message.contains("no such package 'nope'"),
        "{:?}",
        r.errors
    );
    fs.create_directory_and_parents(&ws.get_relative_str("nope"))
        .unwrap();
    fs.write_file(&ws.get_relative_str("nope/BUILD"), BUILD_V1.as_bytes())
        .unwrap();
    // The lookup recorded the *absence* of BUILD / BUILD.bazel as FILE_STATE deps.
    evaluator.invalidate(&[SkyKey::FileState(rooted(&ws, "nope/BUILD"))]);
    let r = evaluator.evaluate(std::slice::from_ref(&k));
    assert!(r.get::<PackageValue>(&k).is_some(), "{:?}", r.errors);
}

// ---------------------------------------------------------------------------- diff awareness

#[test]
fn stat_diff_detects_modification_deletion_creation_on_a_real_file_system() {
    let dir = tmpdir("statdiff");
    let ws = PathFragment::create(dir.to_str().unwrap());
    std::fs::write(dir.join("a.txt"), "one").unwrap();
    std::fs::write(dir.join("gone.txt"), "x").unwrap();
    std::fs::create_dir_all(dir.join("d")).unwrap();
    let fs: Arc<dyn FileSystem> = Arc::new(LocalFileSystem::default());
    let mut e = InMemoryMemoizingEvaluator::new();
    e.register_function(
        SkyFunctionName::FILE_STATE,
        Arc::new(FileStateFunction::new(Arc::clone(&fs))),
    );
    e.register_function(
        SkyFunctionName::DIRECTORY_LISTING,
        Arc::new(DirectoryListingStateFunction {
            fs: Arc::clone(&fs),
        }),
    );
    let keys: Vec<SkyKey> = ["a.txt", "gone.txt", "absent.txt"]
        .iter()
        .map(|f| SkyKey::FileState(rooted(&ws, f)))
        .collect();
    let dirk = SkyKey::DirectoryListingState(rooted(&ws, "d"));
    let mut roots = keys.clone();
    roots.push(dirk.clone());
    assert!(!e.evaluate(&roots).has_error());

    let sd = StatDiffAwareness { threads: 3 };
    // Back-date mtimes so that none of the files is "racy".
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for f in ["a.txt", "gone.txt"] {
        std::fs::File::options()
            .write(true)
            .open(dir.join(f))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    e.invalidate(&keys[..2]);
    e.evaluate(&roots);
    assert_eq!(
        sd.get_diff(&e, fs.as_ref()),
        ModifiedFileSet::Nothing,
        "nothing changed since the last evaluation"
    );

    std::fs::write(dir.join("a.txt"), "two-longer").unwrap(); // size change
    std::fs::remove_file(dir.join("gone.txt")).unwrap();
    std::fs::write(dir.join("absent.txt"), "now exists").unwrap();
    std::fs::write(dir.join("d").join("new"), "n").unwrap();
    let ModifiedFileSet::Keys(dirty) = sd.get_diff(&e, fs.as_ref()) else {
        panic!("expected changes")
    };
    for k in &roots {
        assert!(dirty.contains(k), "{k} should be reported dirty: {dirty:?}");
    }
    crate::diff::apply_diff(&e, &ModifiedFileSet::Keys(dirty));
    let r = e.evaluate(&roots);
    assert!(matches!(
        r.get::<FileStateValue>(&keys[0]),
        Some(FileStateValue::RegularFile { size: 10, .. })
    ));
    assert_eq!(
        r.get::<FileStateValue>(&keys[1]),
        Some(&FileStateValue::NonExistent)
    );
    assert!(matches!(
        r.get::<FileStateValue>(&keys[2]),
        Some(FileStateValue::RegularFile { .. })
    ));
    assert_eq!(
        r.get::<DirectoryListingStateValue>(&dirk)
            .unwrap()
            .entries
            .len(),
        1
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn racy_files_are_always_rechecked_but_pruned() {
    let dir = tmpdir("racy");
    let ws = PathFragment::create(dir.to_str().unwrap());
    std::fs::write(dir.join("f"), "same-size-1").unwrap();
    let fs: Arc<dyn FileSystem> = Arc::new(LocalFileSystem::default());
    let mut e = InMemoryMemoizingEvaluator::new();
    e.register_function(
        SkyFunctionName::FILE_STATE,
        Arc::new(FileStateFunction::new(Arc::clone(&fs))),
    );
    let k = SkyKey::FileState(rooted(&ws, "f"));
    e.evaluate(std::slice::from_ref(&k));
    // The file was just written => within the racy window => reported even though stat matches.
    let sd = StatDiffAwareness { threads: 1 };
    let ModifiedFileSet::Keys(d) = sd.get_diff(&e, fs.as_ref()) else {
        panic!("racy file must be reported")
    };
    assert_eq!(d, vec![k.clone()]);
    // Same-size rewrite within the same millisecond is caught by the digest recheck.
    std::fs::write(dir.join("f"), "same-size-2").unwrap();
    crate::diff::apply_diff(&e, &ModifiedFileSet::Keys(d));
    let r = e.evaluate(std::slice::from_ref(&k));
    let FileStateValue::RegularFile { digest, .. } = r.get::<FileStateValue>(&k).unwrap() else {
        panic!()
    };
    assert_eq!(*digest, fs.get_digest(&ws.get_relative_str("f")).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn explicit_diff_awareness_maps_paths_to_keys() {
    let (fs, ws) = mem_world(&[("d/f", "x"), ("d/g", "y")]);
    let dynfs: Arc<dyn FileSystem> = fs.clone();
    let mut e = InMemoryMemoizingEvaluator::new();
    e.register_function(
        SkyFunctionName::FILE_STATE,
        Arc::new(FileStateFunction::new(Arc::clone(&dynfs))),
    );
    e.register_function(
        SkyFunctionName::DIRECTORY_LISTING,
        Arc::new(DirectoryListingStateFunction { fs: dynfs }),
    );
    let f = SkyKey::FileState(rooted(&ws, "d/f"));
    let g = SkyKey::FileState(rooted(&ws, "d/g"));
    let dir = SkyKey::DirectoryListingState(rooted(&ws, "d"));
    e.evaluate(&[f.clone(), g.clone(), dir.clone()]);
    let ex = ExplicitDiffAwareness::new();
    assert_eq!(ex.get_diff(&e, fs.as_ref()), ModifiedFileSet::Nothing);
    ex.notify_modified(ws.get_relative_str("d/f"));
    let ModifiedFileSet::Keys(k) = ex.get_diff(&e, fs.as_ref()) else {
        panic!()
    };
    assert!(
        k.contains(&f) && k.contains(&dir) && !k.contains(&g),
        "{k:?}"
    );
    assert_eq!(
        ex.get_diff(&e, fs.as_ref()),
        ModifiedFileSet::Nothing,
        "pending diff is consumed"
    );
}

// ---------------------------------------------------------------------------- executor

fn local_exec(dir: &std::path::Path) -> (Arc<SkyframeExecutor>, PathFragment) {
    let ws = PathFragment::create(dir.to_str().unwrap());
    let raw: Arc<dyn FileSystem> = Arc::new(LocalFileSystem::default());
    (SkyframeExecutor::new(raw, ws.clone()), ws)
}

fn loader_for(exec: &Arc<SkyframeExecutor>, ws: &PathFragment) -> Arc<PackageLoader> {
    let mut cfg = LoaderConfig::new(
        exec.recording_fs(),
        Arc::new(SimpleRepoResolver::new(ws.clone())),
    );
    cfg.package_cache = Some(exec.package_cache());
    PackageLoader::new(cfg)
}

#[test]
fn packages_are_cached_across_commands_and_invalidated_by_file_changes() {
    let dir = tmpdir("exec");
    for p in ["a", "b"] {
        std::fs::create_dir_all(dir.join(p)).unwrap();
        std::fs::write(
            dir.join(p).join("BUILD"),
            format!("filegroup(name = \"{p}\", srcs = [])\n"),
        )
        .unwrap();
    }
    // Make BUILD files non-racy.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for p in ["a", "b"] {
        std::fs::File::options()
            .write(true)
            .open(dir.join(p).join("BUILD"))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    let (exec, ws) = local_exec(&dir);

    // Command 1.
    let r = exec.sync("opts-v1");
    assert_eq!(r.dirtied_nodes, 0);
    let l1 = loader_for(&exec, &ws);
    let a1 = l1.load_package(&pkg("a")).unwrap();
    l1.load_package(&pkg("b")).unwrap();
    assert!(a1.targets.contains_key("a"));
    assert_eq!(exec.package_loads(), 2);

    // Command 2 (fresh loader, like a new command): nothing changed => zero loads.
    let r = exec.sync("opts-v1");
    assert_eq!((r.modified_keys, r.dirtied_nodes), (0, 0), "{r:?}");
    let l2 = loader_for(&exec, &ws);
    l2.load_package(&pkg("a")).unwrap();
    l2.load_package(&pkg("b")).unwrap();
    assert_eq!(exec.package_loads(), 2, "served from the graph");

    // Edit one BUILD file (size changes), run command 3: only that package is reloaded.
    std::fs::write(
        dir.join("a").join("BUILD"),
        "filegroup(name = \"a\", srcs = [])\nfilegroup(name = \"a2\", srcs = [])\n",
    )
    .unwrap();
    let r = exec.sync("opts-v1");
    assert!(r.modified_keys >= 1 && r.dirtied_nodes >= 2, "{r:?}");
    let l3 = loader_for(&exec, &ws);
    let a3 = l3.load_package(&pkg("a")).unwrap();
    l3.load_package(&pkg("b")).unwrap();
    assert!(a3.targets.contains_key("a2"));
    assert_eq!(exec.package_loads(), 3, "only package a reloaded");

    // Deleting the BUILD file turns the package into an error; restoring it recovers.
    std::fs::remove_file(dir.join("b").join("BUILD")).unwrap();
    exec.sync("opts-v1");
    let l4 = loader_for(&exec, &ws);
    let e = l4.load_package(&pkg("b")).unwrap_err();
    assert!(e.message.contains("no such package 'b'"), "{}", e.message);
    std::fs::write(
        dir.join("b").join("BUILD"),
        "filegroup(name = \"b\", srcs = [])\n",
    )
    .unwrap();
    exec.sync("opts-v1");
    assert!(loader_for(&exec, &ws).load_package(&pkg("b")).is_ok());

    // Changing the loading options rebuilds every package, but equal results are pruned.
    let before = exec.package_loads();
    let r = exec.sync("opts-v2");
    assert!(r.loading_options_changed);
    let l5 = loader_for(&exec, &ws);
    l5.load_package(&pkg("a")).unwrap();
    l5.load_package(&pkg("b")).unwrap();
    assert_eq!(exec.package_loads(), before + 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unchanged_bzl_file_is_not_reread_but_changed_bzl_invalidates_dependents_only() {
    let dir = tmpdir("exec-bzl");
    std::fs::create_dir_all(dir.join("x")).unwrap();
    std::fs::create_dir_all(dir.join("y")).unwrap();
    std::fs::write(dir.join("BUILD"), "").unwrap();
    std::fs::write(
        dir.join("defs.bzl"),
        "def mk(name):\n    native.filegroup(name = name, srcs = [])\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("x/BUILD"),
        "load(\"//:defs.bzl\", \"mk\")\nmk(\"x\")\n",
    )
    .unwrap();
    std::fs::write(dir.join("y/BUILD"), "filegroup(name = \"y\", srcs = [])\n").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for f in ["BUILD", "defs.bzl", "x/BUILD", "y/BUILD"] {
        std::fs::File::options()
            .write(true)
            .open(dir.join(f))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    let (exec, ws) = local_exec(&dir);
    exec.sync("o");
    let l = loader_for(&exec, &ws);
    let px = l.load_package(&pkg("x")).unwrap();
    assert!(
        px.targets.contains_key("x"),
        "targets={:?} events={:?}",
        px.targets.keys().collect::<Vec<_>>(),
        px.events
    );
    l.load_package(&pkg("y")).unwrap();
    assert_eq!(exec.package_loads(), 2);
    std::fs::write(
        dir.join("defs.bzl"),
        "def mk(name):\n    native.filegroup(name = name + \"_new\", srcs = [])\n",
    )
    .unwrap();
    exec.sync("o");
    let l = loader_for(&exec, &ws);
    let x = l.load_package(&pkg("x")).unwrap();
    l.load_package(&pkg("y")).unwrap();
    assert!(
        x.targets.contains_key("x_new"),
        "{:?}",
        x.targets.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        exec.package_loads(),
        3,
        "only the package that loads defs.bzl is reloaded"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn discard_analysis_cache_removes_analysis_nodes_and_dirties_dependents() {
    struct Const;
    impl SkyFunction for Const {
        fn compute(
            &self,
            _k: &SkyKey,
            env: &mut Environment<'_>,
        ) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
            let _ = env;
            Ok(Some(Arc::new(StringSkyValue("v".into()))))
        }
    }
    let (fs, ws) = mem_world(&[]);
    let mut exec = SkyframeExecutor::new(fs as Arc<dyn FileSystem>, ws);
    assert!(exec.register(SkyFunctionName::ACTION_EXECUTION, Arc::new(Const)));
    let k = SkyKey::ActionExecution("a".into());
    exec.evaluator.evaluate(std::slice::from_ref(&k));
    assert!(exec.evaluator.contains(&k));
    assert_eq!(exec.discard_analysis_cache(), 1);
    assert!(!exec.evaluator.contains(&k));
}

// ---------------------------------------------------------------------------- actions

struct FakeActions {
    graph: HashMap<&'static str, (Vec<&'static str>, Vec<&'static str>)>, // id -> (source rel paths, generated ids)
    ws: PathFragment,
    fs: Arc<InMemoryFileSystem>,
    ran: Mutex<Vec<String>>,
    /// Outputs only depend on this many leading bytes of the first source (to test pruning).
    prefix_only: bool,
}

impl ActionRunner for FakeActions {
    fn inputs(&self, id: &str) -> Option<ActionInputs> {
        let (srcs, gens) = self.graph.get(id)?;
        Some(ActionInputs {
            sources: srcs.iter().map(|s| rooted(&self.ws, s)).collect(),
            generated: gens.iter().map(|g| g.to_string()).collect(),
        })
    }
    fn run(&self, id: &str) -> Result<String, String> {
        self.ran.lock().unwrap().push(id.to_string());
        let (srcs, _) = self.graph.get(id).ok_or("unknown")?;
        let mut h = String::new();
        for s in srcs {
            let b = self
                .fs
                .read_file(&self.ws.get_relative_str(s))
                .map_err(|e| e.to_string())?;
            let b = if self.prefix_only {
                b[..1.min(b.len())].to_vec()
            } else {
                b
            };
            h.push_str(&String::from_utf8_lossy(&b));
        }
        Ok(format!("{id}:{h}"))
    }
}

#[test]
fn action_execution_nodes_rerun_only_when_inputs_change_and_prune_equal_outputs() {
    let (fs, ws) = mem_world(&[("src/a.c", "AAA"), ("src/b.c", "BBB")]);
    let mut graph = HashMap::new();
    graph.insert("compile_a", (vec!["src/a.c"], vec![]));
    graph.insert("compile_b", (vec!["src/b.c"], vec![]));
    graph.insert("link", (vec![], vec!["compile_a", "compile_b"]));
    let runner = Arc::new(FakeActions {
        graph,
        ws: ws.clone(),
        fs: Arc::clone(&fs),
        ran: Mutex::new(vec![]),
        prefix_only: true,
    });
    let mut e = old_style_evaluator(&fs, &ws);
    e.register_function(
        SkyFunctionName::ACTION_EXECUTION,
        Arc::new(ActionExecutionFunction {
            runner: runner.clone(),
        }),
    );
    let link = SkyKey::ActionExecution("link".into());
    let r = e.evaluate(std::slice::from_ref(&link));
    assert_eq!(
        r.get::<ActionExecutionValue>(&link).unwrap().output_digest,
        "link:"
    );
    assert_eq!(runner.ran.lock().unwrap().len(), 3);

    // Change a.c but keep its first byte: compile_a re-runs, produces the same output, so link does not.
    runner.ran.lock().unwrap().clear();
    fs.write_file(&ws.get_relative_str("src/a.c"), b"Axx")
        .unwrap();
    e.invalidate(&[SkyKey::FileState(rooted(&ws, "src/a.c"))]);
    e.evaluate(std::slice::from_ref(&link));
    assert_eq!(*runner.ran.lock().unwrap(), vec!["compile_a".to_string()]);

    // No changes: nothing runs.
    runner.ran.lock().unwrap().clear();
    e.evaluate(std::slice::from_ref(&link));
    assert!(runner.ran.lock().unwrap().is_empty());
}

#[test]
fn action_execution_without_pruning_propagates() {
    let (fs, ws) = mem_world(&[("src/a.c", "AAA")]);
    let mut graph = HashMap::new();
    graph.insert("compile", (vec!["src/a.c"], vec![]));
    graph.insert("link", (vec![], vec!["compile"]));
    let runner = Arc::new(FakeActions {
        graph,
        ws: ws.clone(),
        fs: Arc::clone(&fs),
        ran: Mutex::new(vec![]),
        prefix_only: false,
    });
    let mut e = old_style_evaluator(&fs, &ws);
    e.register_function(
        SkyFunctionName::ACTION_EXECUTION,
        Arc::new(ActionExecutionFunction {
            runner: runner.clone(),
        }),
    );
    let link = SkyKey::ActionExecution("link".into());
    e.evaluate(std::slice::from_ref(&link));
    runner.ran.lock().unwrap().clear();
    fs.write_file(&ws.get_relative_str("src/a.c"), b"CHANGED")
        .unwrap();
    e.invalidate(&[SkyKey::FileState(rooted(&ws, "src/a.c"))]);
    let r = e.evaluate(std::slice::from_ref(&link));
    assert_eq!(
        r.get::<ActionExecutionValue>(&link).unwrap().output_digest,
        "link:"
    );
    assert_eq!(
        runner.ran.lock().unwrap().len(),
        2,
        "compile and link both re-ran"
    );
}

#[test]
fn loading_signature_changes_with_module_files_and_options() {
    let (fs, ws) = mem_world(&[("MODULE.bazel", "module(name = \"m\")\n")]);
    let s1 = loading_signature(fs.as_ref(), &ws, &["a".into()]);
    assert_eq!(s1, loading_signature(fs.as_ref(), &ws, &["a".into()]));
    assert_ne!(s1, loading_signature(fs.as_ref(), &ws, &["b".into()]));
    fs.write_file(
        &ws.get_relative_str("MODULE.bazel"),
        b"module(name = \"m2\")\n",
    )
    .unwrap();
    assert_ne!(s1, loading_signature(fs.as_ref(), &ws, &["a".into()]));
}

#[test]
fn package_lookup_follows_symlinks_detects_cycles_and_probes_sequentially() {
    let (fs, ws) = mem_world(&[
        ("pkg/BUILD.real", "filegroup(name = \"ok\", srcs = [])\n"),
        ("external/BUILD", "filegroup(name = \"ext\", srcs = [])\n"),
        (
            "prio/BUILD.bazel",
            "filegroup(name = \"from_bazel\", srcs = [])\n",
        ),
        (
            "prio/BUILD",
            "filegroup(name = \"from_build\", srcs = [])\n",
        ),
    ]);
    fs.create_symbolic_link(
        &ws.get_relative_str("pkg/BUILD"),
        &PathFragment::create("BUILD.real"),
    )
    .unwrap();
    fs.create_directory_and_parents(&ws.get_relative_str("cyc"))
        .unwrap();
    fs.create_symbolic_link(
        &ws.get_relative_str("cyc/BUILD"),
        &PathFragment::create("BUILD"),
    )
    .unwrap();

    let e = old_style_evaluator(&fs, &ws);

    // Symlinked BUILD file resolves and loads package.
    let pkg_k = SkyKey::Package(pkg("pkg"));
    let r = e.evaluate(std::slice::from_ref(&pkg_k));
    assert!(!r.has_error(), "{:?}", r.errors);
    assert!(r
        .get::<PackageValue>(&pkg_k)
        .unwrap()
        .targets
        .contains_key("ok"));

    // Symlink cycle fails persistently with cycle message.
    let cyc_k = SkyKey::Package(pkg("cyc"));
    let r_cyc = e.evaluate(std::slice::from_ref(&cyc_k));
    assert!(r_cyc.has_error());
    assert!(
        r_cyc.errors[&cyc_k].message.contains("Symlink cycle"),
        "{:?}",
        r_cyc.errors
    );

    // `//external` in main repo is forced non-existent without reading external/BUILD.
    let ext_k = SkyKey::Package(pkg("external"));
    let r_ext = e.evaluate(std::slice::from_ref(&ext_k));
    assert!(r_ext.has_error());

    // `prio` has BUILD.bazel: BUILD is never requested as a dep, so changing BUILD does not dirty prio lookup.
    let prio_lookup = SkyKey::PackageLookup(pkg("prio"));
    let r_prio = e.evaluate(std::slice::from_ref(&prio_lookup));
    assert!(
        r_prio
            .get::<PackageLookupValue>(&prio_lookup)
            .unwrap()
            .package_exists
    );
    let deps = e.direct_deps(&prio_lookup);
    assert!(
        !deps.contains(&SkyKey::FileState(rooted(&ws, "prio/BUILD"))),
        "BUILD should not be probed when BUILD.bazel exists"
    );
}

#[test]
fn notrack_incremental_state_clears_graph_on_sync() {
    let (fs, ws) = mem_world(&[("a/BUILD", "filegroup(name = \"a\", srcs = [])\n")]);
    let exec = SkyframeExecutor::new(fs as Arc<dyn FileSystem>, ws);
    exec.sync("v1");
    let k = SkyKey::PackageLookup(pkg("a"));
    exec.evaluator.evaluate(std::slice::from_ref(&k));
    assert!(exec.evaluator.contains(&k));

    exec.set_track_incremental_state(false);
    assert!(!exec.evaluator.contains(&k));
    exec.evaluator.evaluate(std::slice::from_ref(&k));
    assert!(exec.evaluator.contains(&k));
    exec.sync("v1");
    assert!(
        !exec.evaluator.contains(&k),
        "sync with tracking off clears evaluator"
    );
}

#[test]
fn containing_package_lookup_and_symlinked_ancestor_file_resolution() {
    let (fs, ws) = mem_world(&[
        ("top/BUILD", "filegroup(name = \"t\", srcs = [])\n"),
        ("real/sub/file.txt", "hello"),
    ]);
    fs.create_symbolic_link(&ws.get_relative_str("link"), &PathFragment::create("real"))
        .unwrap();
    fs.create_directory_and_parents(&ws.get_relative_str("real2/sub"))
        .unwrap();
    fs.write_file(&ws.get_relative_str("real2/sub/file.txt"), b"updated")
        .unwrap();

    let exec = SkyframeExecutor::new(fs.clone() as Arc<dyn FileSystem>, ws.clone());

    let contain_k = SkyKey::ContainingPackageLookup(pkg("top/nested/dir"));
    let r = exec.evaluator.evaluate(std::slice::from_ref(&contain_k));
    assert_eq!(
        r.get::<ContainingPackageLookupValue>(&contain_k),
        Some(&ContainingPackageLookupValue::ContainingPackage {
            containing_package_name: pkg("top"),
            containing_package_root: ws.clone(),
        })
    );

    let orphan_k = SkyKey::ContainingPackageLookup(pkg("orphan/nested"));
    let r_orphan = exec.evaluator.evaluate(std::slice::from_ref(&orphan_k));
    assert_eq!(
        r_orphan.get::<ContainingPackageLookupValue>(&orphan_k),
        Some(&ContainingPackageLookupValue::NoContainingPackage)
    );

    // FileFunction resolves symlinked ancestor directory and records FileState on the symlink.
    let file_k = SkyKey::File(rooted(&ws, "link/sub/file.txt"));
    let rf1 = exec.evaluator.evaluate(std::slice::from_ref(&file_k));
    let fv1 = rf1.get::<FileValue>(&file_k).unwrap();
    assert!(fv1.exists && fv1.is_file);
    assert_eq!(fv1.real_path, rooted(&ws, "real/sub/file.txt"));

    // Retargeting `link -> real2` invalidates `link/sub/file.txt` via `FileState(link)`.
    fs.delete(&ws.get_relative_str("link")).unwrap();
    fs.create_symbolic_link(&ws.get_relative_str("link"), &PathFragment::create("real2"))
        .unwrap();
    exec.sync("v1");
    let rf2 = exec.evaluator.evaluate(std::slice::from_ref(&file_k));
    let fv2 = rf2.get::<FileValue>(&file_k).unwrap();
    assert_eq!(fv2.real_path, rooted(&ws, "real2/sub/file.txt"));
}

#[test]
fn error_info_child_merging_walkable_graph_and_modified_file_set() {
    let cyc = ErrorInfo::from_cycle(vec![], vec![SkyKey::Precomputed("c1".into())]);
    let cat = ErrorInfo::from_error(
        &SkyKey::Precomputed("e1".into()),
        SkyFunctionError::catastrophic_transient("boom"),
    );
    assert!(cat.is_directly_transient());
    let parent = SkyKey::Precomputed("parent".into());
    let merged = ErrorInfo::from_child_errors(&parent, [&cyc, &cat]);
    assert!(!merged.is_directly_transient());
    assert!(merged.is_transitively_transient());
    assert!(merged.is_catastrophic());
    assert_eq!(merged.error.as_ref().unwrap().message, "boom");
    assert_eq!(merged.cycles.len(), 1);

    // ModifiedFileSet set equality and EverythingModified invalidation.
    let k1 = SkyKey::Precomputed("a".into());
    let k2 = SkyKey::Precomputed("b".into());
    assert_eq!(
        ModifiedFileSet::Keys(vec![k2.clone(), k1.clone(), k1.clone()]),
        ModifiedFileSet::Keys(vec![k1.clone(), k2.clone()])
    );
    assert!(ModifiedFileSet::EverythingModified
        .union(ModifiedFileSet::EverythingDeleted)
        .treat_everything_as_deleted());

    // WalkableGraph queries + NotComparableSkyValue never prunes.
    struct NcFn;
    impl SkyFunction for NcFn {
        fn compute(
            &self,
            _key: &SkyKey,
            _env: &mut Environment<'_>,
        ) -> Result<Option<Arc<dyn SkyValue>>, SkyFunctionError> {
            Ok(Some(Arc::new(NotComparableSkyValue(42i32))))
        }
    }
    let mut eval = InMemoryMemoizingEvaluator::new();
    eval.register_function(SkyFunctionName("NC"), Arc::new(NcFn));
    let nc_key = SkyKey::Custom {
        function_name: SkyFunctionName("NC"),
        argument: "line1\nline2".into(),
    };
    assert_eq!(nc_key.canonical_name(), "NC:line1_line2");
    let r1 = eval.evaluate(std::slice::from_ref(&nc_key));
    assert!(!r1.has_error());
    assert_eq!(
        eval.get_successful_values(std::slice::from_ref(&nc_key))
            .len(),
        1
    );
    let missing_key = SkyKey::Precomputed("missing".into());
    let me = eval.get_missing_and_exceptions(&[nc_key.clone(), missing_key.clone()]);
    assert!(!me.contains_key(&nc_key));
    assert_eq!(me.get(&missing_key), Some(&None));

    eval.invalidate(std::slice::from_ref(&nc_key));
    let r2 = eval.evaluate(std::slice::from_ref(&nc_key));
    assert!(
        !r2.stats.pruned.contains(&nc_key),
        "NotComparableSkyValue must not be pruned"
    );
}
