use std::sync::Arc;
use zel_cmdline::{Label, PackageIdentifier, RepositoryMapping, RepositoryName};
use zel_packages::attr::{convert_plain, AttrType, AttrValue, ConvertCtx};
use zel_packages::glob::{evaluate, GlobContext, GlobOp};
use zel_packages::lookup::{EventKind, RepoResolver};
use zel_packages::{LoaderConfig, PackageLoader, PatternResolver, SimpleRepoResolver};
use zel_starlark::{StarlarkDict, StarlarkList, Value};
use zel_vfs::{FileSystem, InMemoryFileSystem, PathFragment};

fn write(fs: &InMemoryFileSystem, root: &PathFragment, rel: &str, content: &str) {
    let p = root.get_relative_str(rel);
    if let Some(parent) = p.parent_directory() {
        fs.create_directory_and_parents(&parent).unwrap();
    }
    fs.write_file(&p, content.as_bytes()).unwrap();
}

#[test]
fn label_list_dict_conversion_and_validation() {
    let pkg = PackageIdentifier::create_in_main_repo("quux").unwrap();
    let mapping = RepositoryMapping::always_fallback();
    let ctx = ConvertCtx {
        pkg: &pkg,
        mapping: &mapping,
        rule_class: "my_rule",
    };

    let mut d = StarlarkDict::new();
    let _ = d.insert(
        Value::String("foo".into()),
        Value::List(Arc::new(std::sync::Mutex::new(StarlarkList::new(vec![
            Value::String(":foo".into()),
            Value::String("//bang".into()),
        ])))),
    );
    let val = Value::Dict(Arc::new(std::sync::Mutex::new(d)));
    let conv = convert_plain(&val, AttrType::LabelListDict, "lld", &ctx).unwrap();
    match &conv {
        AttrValue::LabelListDict(entries) => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].0, "foo");
            assert_eq!(
                entries[0]
                    .1
                    .iter()
                    .map(|l| l.canonical_form())
                    .collect::<Vec<_>>(),
                vec!["//quux:foo", "//bang:bang"]
            );
        }
        other => panic!("expected LabelListDict, got {other:?}"),
    }
    assert_eq!(
        conv.labels()
            .into_iter()
            .map(|l| l.canonical_form())
            .collect::<Vec<_>>(),
        vec!["//quux:foo", "//bang:bang"]
    );

    let mut bad_key = StarlarkDict::new();
    let _ = bad_key.insert(
        Value::Int(zel_starlark::BigInt::from_i64(2)),
        Value::List(Arc::new(std::sync::Mutex::new(StarlarkList::new(vec![])))),
    );
    let err = convert_plain(
        &Value::Dict(Arc::new(std::sync::Mutex::new(bad_key))),
        AttrType::LabelListDict,
        "lld",
        &ctx,
    )
    .unwrap_err();
    assert!(
        err.contains("expected value of type 'string' for dict key element, but got 2 (int)"),
        "{err}"
    );

    let mut bad_val = StarlarkDict::new();
    let _ = bad_val.insert(Value::String("k".into()), Value::String("bar".into()));
    let err = convert_plain(
        &Value::Dict(Arc::new(std::sync::Mutex::new(bad_val))),
        AttrType::LabelListDict,
        "lld",
        &ctx,
    )
    .unwrap_err();
    assert!(
        err.contains(
            "expected value of type 'list(label)' for dict value element, but got \"bar\" (string)"
        ),
        "{err}"
    );
}

#[test]
fn glob_respects_bazelignore_and_repo_ignore_directories() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    fs.create_directory_and_parents(&root).unwrap();
    write(&fs, &root, "BUILD", "");
    write(&fs, &root, ".bazelignore", "ignored\nisolated/foo\n");
    write(&fs, &root, "REPO.bazel", "ignore_directories(['**/sub'])\n");
    write(&fs, &root, "a.js", "");
    write(&fs, &root, "ignored/file.js", "");
    write(&fs, &root, "isolated/foo/bar/nope.js", "");
    write(&fs, &root, "isolated/ok/yes.js", "");
    write(&fs, &root, "sub/subfile.js", "");

    let pkg_id = zel_cmdline::PackageIdentifier::create_in_main_repo("").unwrap();
    let lcfg = zel_packages::PackageLookupConfig {
        fs: Arc::clone(&fs) as Arc<dyn FileSystem>,
        resolver: Arc::new(SimpleRepoResolver::new(root.clone())),
        deleted_packages: Default::default(),
    };
    let gctx = GlobContext {
        fs: fs.as_ref(),
        package_dir: &root,
        skip_convenience_symlinks: false,
        lookup: Some((&lcfg, &pkg_id)),
    };
    let out = evaluate(&gctx, &["**/*.js".into()], &[], GlobOp::Files).matches;
    assert_eq!(out, vec!["a.js", "isolated/ok/yes.js"]);
}

#[test]
fn bzl_cache_invalidates_when_file_changes() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    fs.create_directory_and_parents(&root).unwrap();
    write(&fs, &root, "BUILD", "");
    write(&fs, &root, "pkg/BUILD", "");
    write(&fs, &root, "pkg/foo.bzl", "y = 1\n");

    let cfg = LoaderConfig::new(
        Arc::clone(&fs) as Arc<dyn FileSystem>,
        Arc::new(SimpleRepoResolver::new(root.clone())),
    );
    let loader = PackageLoader::new(cfg);
    let lbl = Label::parse_canonical("//pkg:foo.bzl").unwrap();
    assert_eq!(
        loader.load_bzl_export(&lbl, "y").unwrap(),
        Value::Int(zel_starlark::BigInt::from_i64(1))
    );

    write(&fs, &root, "pkg/foo.bzl", "y = 2\n");
    assert_eq!(
        loader.load_bzl_export(&lbl, "y").unwrap(),
        Value::Int(zel_starlark::BigInt::from_i64(2))
    );
}

#[test]
fn bzl_rule_toolchains_resolve_against_defining_repo() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    let ext = PathFragment::create("/ext_repo");
    fs.create_directory_and_parents(&root).unwrap();
    fs.create_directory_and_parents(&ext).unwrap();
    write(
        &fs,
        &root,
        "BUILD",
        "load('@ext//pkg:defs.bzl', 'my_rule')\nmy_rule(name = 't')\n",
    );
    write(&fs, &ext, "pkg/BUILD", "");
    write(
        &fs,
        &ext,
        "pkg/defs.bzl",
        "def _impl(ctx): return []\nmy_rule = rule(implementation = _impl, toolchains = ['//toolchain:foo_type'], exec_compatible_with = ['//plat:c'])\n",
    );

    let mut resolver = SimpleRepoResolver::new(root);
    resolver.external.insert("ext+".into(), ext);
    resolver
        .apparent
        .insert("ext".into(), RepositoryName::create("ext+").unwrap());
    let cfg = LoaderConfig::new(Arc::clone(&fs) as Arc<dyn FileSystem>, Arc::new(resolver));
    let loader = PackageLoader::new(cfg);
    let pkg = loader
        .load_package(&PackageIdentifier::create_in_main_repo("").unwrap())
        .unwrap();
    let rule = match pkg.targets.get("t").unwrap() {
        zel_packages::Target::Rule(r) => r,
        _ => panic!("expected rule"),
    };
    let rc = rule.extra.rule_class.as_ref().unwrap();
    let spec = rc.starlark.as_ref().unwrap();
    let tc_list = spec.kwargs.get("toolchains").unwrap().to_string_list();
    let ecw_list = spec
        .kwargs
        .get("exec_compatible_with")
        .unwrap()
        .to_string_list();
    assert_eq!(tc_list, vec!["@@ext+//toolchain:foo_type"]);
    assert_eq!(ecw_list, vec!["@@ext+//plat:c"]);
}

#[test]
fn aspect_required_providers_flat_list_is_conjunction() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    fs.create_directory_and_parents(&root).unwrap();
    write(
        &fs,
        &root,
        "defs.bzl",
        r#"
A = provider()
B = provider()
def _asp_impl(target, ctx):
    return []
asp_flat = aspect(implementation = _asp_impl, required_providers = [A, B])
asp_or = aspect(implementation = _asp_impl, required_providers = [[A], [B]])
"#,
    );
    write(&fs, &root, "BUILD", "");

    let cfg = LoaderConfig::new(
        Arc::clone(&fs) as Arc<dyn FileSystem>,
        Arc::new(SimpleRepoResolver::new(root)),
    );
    let loader = PackageLoader::new(cfg);
    let bzl = Label::parse_canonical("//:defs.bzl").unwrap();
    let flat = zel_packages::aspect::aspect_of(&loader.load_bzl_export(&bzl, "asp_flat").unwrap())
        .unwrap();
    let or_asp =
        zel_packages::aspect::aspect_of(&loader.load_bzl_export(&bzl, "asp_or").unwrap()).unwrap();
    assert_eq!(flat.required_providers.len(), 1);
    assert_eq!(flat.required_providers[0].len(), 2);
    assert_eq!(or_asp.required_providers.len(), 2);
    assert_eq!(or_asp.required_providers[0].len(), 1);
    assert_eq!(or_asp.required_providers[1].len(), 1);
}

#[test]
fn smart_negation_and_keep_going_and_symlinks_in_resolve() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    fs.create_directory_and_parents(&root).unwrap();
    write(&fs, &root, "BUILD", "");
    write(&fs, &root, "foo/BUILD", "filegroup(name = 'f')\n");
    write(&fs, &root, "foo/foo/BUILD", "syntax error here !!!\n");

    let cfg = LoaderConfig::new(
        Arc::clone(&fs) as Arc<dyn FileSystem>,
        Arc::new(SimpleRepoResolver::new(root.clone())),
    );
    let loader = PackageLoader::new(cfg);
    let pr = PatternResolver::new(loader, PathFragment::empty());
    let out = pr.resolve_patterns(&["//foo/...".into(), "-//foo/foo/...".into()]);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(
        out.targets
            .iter()
            .map(|t| t.label().canonical_form())
            .collect::<Vec<_>>(),
        vec!["//foo:f"]
    );

    write(&fs, &root, "b/b1/BUILD", "filegroup(name = 'good')\n");
    write(&fs, &root, "b/bad/BUILD", "invalid build file\n");
    let out_kg = pr.resolve_patterns_with_options(&["//b/...".into()], true, true);
    assert!(out_kg
        .targets
        .iter()
        .any(|t| t.label().canonical_form() == "//b/b1:good"));
    assert!(!out_kg.errors.is_empty());

    write(&fs, &root, "a1/b1/BUILD", "filegroup(name = 'ok')\n");
    fs.create_symbolic_link(&root.get_relative_str("a2"), &root.get_relative_str("a1"))
        .unwrap();
    fs.create_symbolic_link(
        &root.get_relative_str("a1/circular"),
        &root.get_relative_str("a1"),
    )
    .unwrap();
    let out_sym =
        pr.resolve_patterns_with_options(&["//a1/...".into(), "//a2/...".into()], true, true);
    let labels: Vec<String> = out_sym
        .targets
        .iter()
        .map(|t| t.label().canonical_form())
        .collect();
    assert!(labels.contains(&"//a1/b1:ok".to_string()), "{labels:?}");
    assert!(labels.contains(&"//a2/b1:ok".to_string()), "{labels:?}");
    assert!(
        out_sym
            .events
            .iter()
            .any(|e| e.message.contains("Symlink cycle")),
        "{:?}",
        out_sym.events
    );
}

#[test]
fn blazel_utils_helpers_resolve_in_defining_bzl_and_reject_unmapped_repos() {
    use std::collections::HashMap;
    use zel_packages::RepoResolver;

    struct ScopedResolver {
        main: PathFragment,
        helper: PathFragment,
        dep: PathFragment,
    }
    impl RepoResolver for ScopedResolver {
        fn repo_root(&self, repo: &RepositoryName) -> Option<PathFragment> {
            if !repo.is_visible() {
                return None;
            }
            if repo.is_main() {
                return Some(self.main.clone());
            }
            match repo.name() {
                "helper+" => Some(self.helper.clone()),
                "dep+2.0" => Some(self.dep.clone()),
                _ => None,
            }
        }
        fn mapping(&self, owner: &RepositoryName) -> RepositoryMapping {
            let mut m = HashMap::new();
            if owner.is_main() {
                m.insert(String::new(), RepositoryName::main());
                m.insert("helper".into(), RepositoryName::create("helper+").unwrap());
                // Main repo maps `caller_only`, NOT `helper_dep`.
                m.insert(
                    "caller_only".into(),
                    RepositoryName::create("dep+2.0").unwrap(),
                );
            } else if owner.name() == "helper+" {
                // Defining `.bzl` in `helper+` maps `helper_dep` (and NOT `platforms`, `bazel_tools`, or `caller_only`).
                m.insert(
                    "helper_dep".into(),
                    RepositoryName::create("dep+2.0").unwrap(),
                );
            }
            RepositoryMapping::new(m, owner.clone())
        }
    }

    let fs = Arc::new(InMemoryFileSystem::default());
    let main = PathFragment::create("/ws");
    let helper = PathFragment::create("/ext/helper+");
    let dep = PathFragment::create("/ext/dep+2.0");
    write(&fs, &dep, "BUILD", "");
    write(&fs, &helper, "lib/BUILD", "");
    write(
        &fs,
        &helper,
        "lib/blazel_utils.bzl",
        r#"
def _to_label(repo_name):
    if not repo_name.startswith("@"):
        repo_name = "@" + repo_name
    return Label(repo_name + "//:unused")

def get_canonical_repo_name(apparent_repo_name):
    return _to_label(apparent_repo_name).workspace_name

def get_repo_root(apparent_repo_name):
    return _to_label(apparent_repo_name).workspace_root
"#,
    );
    write(
        &fs,
        &main,
        "BUILD",
        r#"
load("@helper//lib:blazel_utils.bzl", "get_canonical_repo_name", "get_repo_root")
genrule(
    name = "info",
    outs = ["out.txt"],
    cmd = "|".join([
        get_canonical_repo_name("helper_dep"),
        get_repo_root("helper_dep"),
        get_canonical_repo_name(""),
        get_repo_root(""),
    ]) + " > $@",
)
"#,
    );

    let resolver: Arc<dyn RepoResolver> = Arc::new(ScopedResolver {
        main: main.clone(),
        helper,
        dep,
    });

    // Default layout: external/<canonical>.
    let cfg_default = LoaderConfig::new(
        Arc::clone(&fs) as Arc<dyn FileSystem>,
        Arc::clone(&resolver),
    );
    let pkg_default = PackageLoader::new(cfg_default)
        .load_package(&PackageIdentifier::create_in_main_repo("").unwrap())
        .unwrap();
    assert!(!pkg_default.contains_errors, "{:?}", pkg_default.events);
    let cmd_default = match pkg_default.targets.get("info").unwrap() {
        zel_packages::Target::Rule(r) => match r.extra.attr_values.get("cmd").unwrap() {
            zel_packages::attr::AttrValue::Str(s) => s.clone(),
            other => panic!("unexpected cmd: {other:?}"),
        },
        _ => panic!("expected rule"),
    };
    assert_eq!(cmd_default, "dep+2.0|external/dep+2.0|| > $@");

    // Sibling layout: ../<canonical>.
    let mut cfg_sibling = LoaderConfig::new(
        Arc::clone(&fs) as Arc<dyn FileSystem>,
        Arc::clone(&resolver),
    );
    cfg_sibling.sibling_repository_layout = true;
    let pkg_sibling = PackageLoader::new(cfg_sibling)
        .load_package(&PackageIdentifier::create_in_main_repo("").unwrap())
        .unwrap();
    assert!(!pkg_sibling.contains_errors, "{:?}", pkg_sibling.events);
    let cmd_sibling = match pkg_sibling.targets.get("info").unwrap() {
        zel_packages::Target::Rule(r) => match r.extra.attr_values.get("cmd").unwrap() {
            zel_packages::attr::AttrValue::Str(s) => s.clone(),
            other => panic!("unexpected cmd: {other:?}"),
        },
        _ => panic!("expected rule"),
    };
    assert_eq!(cmd_sibling, "dep+2.0|../dep+2.0|| > $@");

    // Calling with a repo only mapped in the caller (`caller_only`), a typo (`helper_deb`),
    // `platforms`, or `bazel_tools` (absent from `helper+`'s mapping) must fail on invalid Label.
    for (bad_call, expected) in [
        (
            "get_canonical_repo_name('caller_only')",
            "'workspace_name' is not allowed on invalid Label @@[unknown repo 'caller_only' requested from @@helper+]//:unused",
        ),
        (
            "get_repo_root('helper_deb')",
            "'workspace_root' is not allowed on invalid Label @@[unknown repo 'helper_deb' requested from @@helper+ (did you mean 'helper_dep'?)]//:unused",
        ),
        (
            "get_canonical_repo_name('platforms')",
            "'workspace_name' is not allowed on invalid Label @@[unknown repo 'platforms' requested from @@helper+]//:unused",
        ),
        (
            "get_repo_root('bazel_tools')",
            "'workspace_root' is not allowed on invalid Label @@[unknown repo 'bazel_tools' requested from @@helper+]//:unused",
        ),
    ] {
        write(
            &fs,
            &main,
            "bad/BUILD",
            &format!(
                "load('@helper//lib:blazel_utils.bzl', 'get_canonical_repo_name', 'get_repo_root')\nx = {bad_call}\n"
            ),
        );
        let loader = PackageLoader::new(LoaderConfig::new(
            Arc::clone(&fs) as Arc<dyn FileSystem>,
            Arc::clone(&resolver),
        ));
        let pkg = loader
            .load_package(&PackageIdentifier::create_in_main_repo("bad").unwrap())
            .unwrap();
        assert!(pkg.contains_errors, "expected error for {bad_call}");
        let errs = pkg
            .events
            .iter()
            .map(|e| e.render())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(errs.contains(expected), "for {bad_call}: {errs}");
    }
}

#[test]
fn rule_output_matching_rule_name_emits_warning_without_failing_package() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    fs.create_directory_and_parents(&root).unwrap();
    write(
        &fs,
        &root,
        "pkg/BUILD",
        "genrule(name = 'hello_world', outs = ['hello_world', 'other.out'], cmd = 'touch $@')\n",
    );

    let cfg = LoaderConfig::new(
        Arc::clone(&fs) as Arc<dyn FileSystem>,
        Arc::new(SimpleRepoResolver::new(root)),
    );
    let loader = PackageLoader::new(cfg);
    let pkg = loader
        .load_package(&PackageIdentifier::create_in_main_repo("pkg").unwrap())
        .unwrap();
    assert!(!pkg.contains_errors, "{:?}", pkg.events);
    assert!(
        pkg.events.iter().any(|e| {
            e.kind == EventKind::Warning
                && e.message
                    == "target 'hello_world' is both a rule and a file; please choose another name for the rule"
        }),
        "{:?}",
        pkg.events
    );
    match pkg.targets.get("hello_world") {
        Some(zel_packages::Target::Rule(r)) => {
            assert_eq!(r.outs, vec!["hello_world", "other.out"]);
        }
        other => panic!("expected Rule target for hello_world, got {other:?}"),
    }
}

#[test]
fn symbolic_macros_eager_expansion_finalizer_snapshot_attrs_none_and_recursion() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    fs.create_directory_and_parents(&root).unwrap();
    write(
        &fs,
        &root,
        "pkg/defs.bzl",
        r#"
def _ord_impl(name, visibility):
    native.filegroup(name = name + "_inner_lib", visibility = visibility)

ord_macro = macro(implementation = _ord_impl)

def _dropped_impl(name, visibility, **kwargs):
    native.filegroup(name = name + "_inner_lib", visibility = visibility, **kwargs)

dropped_srcs_macro = macro(
    implementation = _dropped_impl,
    inherit_attrs = native.filegroup,
    attrs = {"srcs": None},
)

def _inner_fin_impl(name, visibility):
    native.filegroup(name = name + "_inner_lib", visibility = visibility)

inner_finalizer = macro(implementation = _inner_fin_impl, finalizer = True)

def _fin_impl(name, visibility):
    native.filegroup(name = name + "_inner_lib", visibility = visibility)
    ord_macro(name = name + "_inner_macro")
    inner_finalizer(name = name + "_inner_finalizer")
    keys = list(native.existing_rules().keys())
    expected = [
        "top_before",
        "macro_before_inner_lib",
        "top_after",
        "macro_after_inner_lib",
    ]
    if keys != expected:
        fail("unexpected finalizer snapshot: %s vs %s" % (keys, expected))
    if native.existing_rule(name + "_inner_lib") != None:
        fail("finalizer saw its own target")

my_finalizer = macro(implementation = _fin_impl, finalizer = True)
"#,
    );
    write(
        &fs,
        &root,
        "pkg/BUILD",
        r#"
load(":defs.bzl", "ord_macro", "dropped_srcs_macro", "my_finalizer")
filegroup(name = "top_before")
ord_macro(name = "macro_before")
my_finalizer(name = "fin1")
_chk1 = None if "macro_before_inner_lib" in existing_rules() else fail("ordinary macro was not expanded eagerly before subsequent BUILD statements")
_chk2 = fail("finalizer ran too early during BUILD evaluation") if "fin1_inner_lib" in existing_rules() else None
filegroup(name = "top_after")
dropped_srcs_macro(name = "macro_after")
my_finalizer(name = "fin2")
"#,
    );

    let cfg = LoaderConfig::new(
        Arc::clone(&fs) as Arc<dyn FileSystem>,
        Arc::new(SimpleRepoResolver::new(root.clone())),
    );
    let loader = PackageLoader::new(cfg);
    let pkg = loader
        .load_package(&PackageIdentifier::create_in_main_repo("pkg").unwrap())
        .unwrap();
    assert!(!pkg.contains_errors, "{:?}", pkg.events);
    assert!(pkg.targets.contains_key("fin1_inner_lib"));
    assert!(pkg.targets.contains_key("fin1_inner_finalizer_inner_lib"));
    assert!(pkg.targets.contains_key("fin2_inner_lib"));

    // Passing dropped `srcs` attribute fails with no such attribute.
    write(
        &fs,
        &root,
        "dropped_err/BUILD",
        "load('//pkg:defs.bzl', 'dropped_srcs_macro')\ndropped_srcs_macro(name = 'x', srcs = [])\n",
    );
    let p_drop = loader
        .load_package(&PackageIdentifier::create_in_main_repo("dropped_err").unwrap())
        .unwrap();
    assert!(p_drop.contains_errors);
    assert!(
        p_drop.events.iter().any(|e| e
            .message
            .contains("no such attribute 'srcs' in 'dropped_srcs_macro' macro")),
        "{:?}",
        p_drop.events
    );

    // Direct and indirect symbolic macro recursion detection.
    write(
        &fs,
        &root,
        "rec/recursive.bzl",
        r#"
def _direct(name, visibility):
    direct_m(name = name + "_x")

direct_m = macro(implementation = _direct)

def _a(name, visibility):
    macro_b(name = name + "_b")

def _b(name, visibility):
    macro_a(name = name + "_a")

macro_a = macro(implementation = _a)
macro_b = macro(implementation = _b)
"#,
    );
    write(
        &fs,
        &root,
        "rec/BUILD",
        "load(':recursive.bzl', 'direct_m', 'macro_a')\ndirect_m(name = 'abc')\nmacro_a(name = 'ind')\n",
    );
    let p_rec = loader
        .load_package(&PackageIdentifier::create_in_main_repo("rec").unwrap())
        .unwrap();
    assert!(p_rec.contains_errors);
    let rec_msgs: Vec<&str> = p_rec.events.iter().map(|e| e.message.as_str()).collect();
    let joined = rec_msgs.join("\n");
    assert!(
        joined.contains("macro 'abc_x' is a direct recursive call of 'abc'. Macro instantiation traceback (most recent call last):")
            && joined.contains("\tPackage //rec, macro 'abc' of type //rec:recursive.bzl%direct_m"),
        "{joined}"
    );
    assert!(
        joined.contains("macro 'ind_b_a' is an indirect recursive call of 'ind'. Macro instantiation traceback (most recent call last):")
            && joined.contains("\tPackage //rec, macro 'ind_b' of type //rec:recursive.bzl%macro_b"),
        "{joined}"
    );
}

#[test]
fn bzl_load_visibility_and_extension_checks() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    let ext = PathFragment::create("/ext_repo");
    fs.create_directory_and_parents(&root).unwrap();
    fs.create_directory_and_parents(&ext).unwrap();

    write(&fs, &root, "b/BUILD", "");
    write(&fs, &root, "b/priv.bzl", "visibility('private')\nX = 1\n");
    write(&fs, &root, "b/allow_a.bzl", "visibility(['//a'])\nX = 2\n");
    write(
        &fs,
        &root,
        "b/allow_a_sub.bzl",
        "visibility(['//a/...'])\nX = 3\n",
    );
    write(&fs, &root, "b/not_bzl.txt", "X = 4\n");
    write(&fs, &ext, "lib/BUILD", "");
    write(&fs, &ext, "lib/bar.bzl", "visibility(['//...'])\nY = 9\n");

    write(&fs, &root, "a/BUILD", "load('//b:allow_a.bzl', 'X')\n");
    write(&fs, &root, "a/sub/BUILD", "load('//b:allow_a.bzl', 'X')\n");
    write(
        &fs,
        &root,
        "a/sub_ok/BUILD",
        "load('//b:allow_a_sub.bzl', 'X')\n",
    );
    write(&fs, &root, "c/BUILD", "load('//c:foo.bzl', 'Z')\n");
    write(
        &fs,
        &root,
        "c/foo.bzl",
        "load('//b:priv.bzl', 'X')\nZ = X\n",
    );
    write(&fs, &root, "d/BUILD", "load('//d:bad_ext.bzl', 'Z')\n");
    write(
        &fs,
        &root,
        "d/bad_ext.bzl",
        "load('//b:not_bzl.txt', 'X')\nZ = X\n",
    );
    write(
        &fs,
        &root,
        "cross/BUILD",
        "load('@repo//lib:bar.bzl', 'Y')\n",
    );

    let mut resolver = SimpleRepoResolver::new(root.clone());
    resolver.external.insert("repo+".into(), ext);
    resolver
        .apparent
        .insert("repo".into(), RepositoryName::create("repo+").unwrap());
    let resolver: Arc<dyn RepoResolver> = Arc::new(resolver);

    let mut cfg = LoaderConfig::new(
        Arc::clone(&fs) as Arc<dyn FileSystem>,
        Arc::clone(&resolver),
    );
    cfg.experimental_enable_scl_dialect = false;
    let loader = PackageLoader::new(cfg);

    let p_a = loader
        .load_package(&PackageIdentifier::create_in_main_repo("a").unwrap())
        .unwrap();
    assert!(!p_a.contains_errors, "{:?}", p_a.events);

    let p_sub_ok = loader
        .load_package(&PackageIdentifier::create_in_main_repo("a/sub_ok").unwrap())
        .unwrap();
    assert!(!p_sub_ok.contains_errors, "{:?}", p_sub_ok.events);

    // `visibility(["//a"])` does NOT allow `//a/sub`.
    let err_sub = loader
        .load_package(&PackageIdentifier::create_in_main_repo("a/sub").unwrap())
        .unwrap_err();
    assert!(
        err_sub.events.iter().any(|e| {
            e.message == "Starlark file //b:allow_a.bzl is not visible for loading from package //a/sub."
        }),
        "{err_sub:?}"
    );

    // Transitive `.bzl` visibility violation reports `module //c:foo.bzl contains .bzl load visibility violations`.
    let err_c = loader
        .load_package(&PackageIdentifier::create_in_main_repo("c").unwrap())
        .unwrap_err();
    assert!(
        err_c.events.iter().any(|e| {
            e.message == "Starlark file //b:priv.bzl is not visible for loading from package //c."
        }) && err_c
            .message
            .contains("module //c:foo.bzl contains .bzl load visibility violations"),
        "{err_c:?}"
    );

    // `visibility(["//..."])` in `@repo//lib:bar.bzl` is same-repo only and rejects main `//cross`.
    let err_cross = loader
        .load_package(&PackageIdentifier::create_in_main_repo("cross").unwrap())
        .unwrap_err();
    assert!(
        err_cross.events.iter().any(|e| {
            e.message
                == "Starlark file @@repo+//lib:bar.bzl is not visible for loading from package //cross."
        }),
        "{err_cross:?}"
    );

    // Non-`.bzl` load inside `.bzl` with `--noexperimental_enable_scl_dialect` omits `.scl`.
    let err_d = loader
        .load_package(&PackageIdentifier::create_in_main_repo("d").unwrap())
        .unwrap_err();
    assert!(
        err_d.events.iter().any(|e| e
            .message
            .contains("The label must reference a file with extension \".bzl\"")
            && !e.message.contains(".scl"))
            && err_d
                .message
                .contains("module //d:bad_ext.bzl has invalid load statements"),
        "{err_d:?}"
    );

    // `--nocheck_bzl_visibility` downgrades visibility errors to warnings and allows loading.
    let mut warn_cfg = LoaderConfig::new(Arc::clone(&fs) as Arc<dyn FileSystem>, resolver);
    warn_cfg.check_bzl_visibility = false;
    let warn_loader = PackageLoader::new(warn_cfg);
    let p_warn = warn_loader
        .load_package(&PackageIdentifier::create_in_main_repo("a/sub").unwrap())
        .unwrap();
    assert!(!p_warn.contains_errors, "{:?}", p_warn.events);
    assert!(
        p_warn.events.iter().any(|e| e.kind == EventKind::Warning
            && e.message
                .contains("Continuing because --nocheck_bzl_visibility is active")),
        "{:?}",
        p_warn.events
    );
}

#[test]
fn testing_execution_info_defaults_exec_group_and_glob_rejects_question_mark() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    fs.create_directory_and_parents(&root).unwrap();
    write(&fs, &root, "BUILD", "");
    write(
        &fs,
        &root,
        "exec_info.bzl",
        "INFO = testing.ExecutionInfo({'supports-workers': '1'})\nGRP = INFO.exec_group\n",
    );
    let cfg = LoaderConfig::new(
        Arc::clone(&fs) as Arc<dyn FileSystem>,
        Arc::new(SimpleRepoResolver::new(root.clone())),
    );
    let loader = PackageLoader::new(cfg);
    let bzl = Label::parse_canonical("//:exec_info.bzl").unwrap();
    assert_eq!(
        loader.load_bzl_export(&bzl, "GRP").unwrap(),
        Value::String("test".into())
    );

    let gctx = GlobContext {
        fs: fs.as_ref(),
        package_dir: &root,
        skip_convenience_symlinks: false,
        lookup: None,
    };
    let err = zel_packages::glob::evaluate_checked(&gctx, &["?.txt".into()], &[], GlobOp::Files)
        .unwrap_err();
    match err {
        zel_packages::glob::GlobEvalError::BadPattern(msg) => {
            assert_eq!(msg, "invalid glob pattern '?.txt': wildcard ? forbidden");
        }
        other => panic!("expected BadPattern, got {other:?}"),
    }
}

#[test]
fn recursive_traversal_sentinel_external_execroot_and_bazelignore_warning() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    fs.create_directory_and_parents(&root).unwrap();
    write(&fs, &root, "BUILD", "filegroup(name = 'root_fg')\n");
    write(&fs, &root, ".bazelignore", "ignoreme\n");
    write(&fs, &root, "ignoreme/BUILD", "filegroup(name = 'ig')\n");
    write(
        &fs,
        &root,
        "ignoreme/deep/BUILD",
        "filegroup(name = 'igd')\n",
    );

    // Top-level `external/` is skipped by `//...` when sibling_repository_layout is false.
    write(
        &fs,
        &root,
        "external/extpkg/BUILD",
        "filegroup(name = 'ext_fg')\n",
    );

    // Symlink target under `<output_base>/execroot` is skipped at any depth.
    let exec_out = PathFragment::create("/tmp/ob/execroot/_main/pkg");
    fs.create_directory_and_parents(&exec_out).unwrap();
    fs.write_file(
        &exec_out.get_relative_str("BUILD"),
        b"filegroup(name = 'from_execroot')\n",
    )
    .unwrap();
    fs.create_symbolic_link(&root.get_relative_str("convenience"), &exec_out)
        .unwrap();
    fs.create_directory_and_parents(&root.get_relative_str("d"))
        .unwrap();
    fs.create_symbolic_link(&root.get_relative_str("d/c"), &exec_out)
        .unwrap();

    // `DONT_FOLLOW_SYMLINKS_WHEN_TRAVERSING_THIS_DIRECTORY_VIA_A_RECURSIVE_TARGET_PATTERN`
    let real_target = PathFragment::create("/real_ext_dir");
    fs.create_directory_and_parents(&real_target).unwrap();
    fs.write_file(
        &real_target.get_relative_str("BUILD"),
        b"filegroup(name = 'linked_fg')\n",
    )
    .unwrap();
    write(
        &fs,
        &root,
        "a/b/DONT_FOLLOW_SYMLINKS_WHEN_TRAVERSING_THIS_DIRECTORY_VIA_A_RECURSIVE_TARGET_PATTERN",
        "",
    );
    write(
        &fs,
        &root,
        "a/b/not_symlink/BUILD",
        "filegroup(name = 'real_fg')\n",
    );
    fs.create_symbolic_link(&root.get_relative_str("a/b/symlink"), &real_target)
        .unwrap();
    fs.create_directory_and_parents(&root.get_relative_str("a/c"))
        .unwrap();
    fs.create_symbolic_link(&root.get_relative_str("a/c/symlink"), &real_target)
        .unwrap();

    let cfg = LoaderConfig::new(
        Arc::clone(&fs) as Arc<dyn FileSystem>,
        Arc::new(SimpleRepoResolver::new(root)),
    );
    let loader = PackageLoader::new(cfg);
    let pr = PatternResolver::new(loader, PathFragment::empty());

    let all = pr.resolve_patterns(&["//...".into()]);
    assert!(all.errors.is_empty(), "{:?}", all.errors);
    let all_labels: Vec<String> = all
        .targets
        .iter()
        .map(|t| t.label().canonical_form())
        .collect();
    assert!(all_labels.contains(&"//:root_fg".to_string()));
    assert!(all_labels.contains(&"//a/b/not_symlink:real_fg".to_string()));
    assert!(all_labels.contains(&"//a/c/symlink:linked_fg".to_string()));
    assert!(
        !all_labels.contains(&"//a/b/symlink:linked_fg".to_string()),
        "{all_labels:?}"
    );
    assert!(
        !all_labels.contains(&"//external/extpkg:ext_fg".to_string()),
        "{all_labels:?}"
    );
    assert!(
        !all_labels.contains(&"//convenience:from_execroot".to_string()),
        "{all_labels:?}"
    );
    assert!(
        !all_labels.contains(&"//d/c:from_execroot".to_string()),
        "{all_labels:?}"
    );

    // Recursive pattern rooted in `.bazelignore` directory succeeds with warning.
    let ig1 = pr.resolve_patterns(&["//ignoreme/deep/...".into()]);
    assert!(ig1.errors.is_empty(), "{:?}", ig1.errors);
    assert!(ig1.targets.is_empty());
    assert!(
        ig1.events.iter().any(|e| {
            e.kind == EventKind::Warning
                && e.message == "Pattern '//ignoreme/deep/...' was filtered out by ignored directory 'ignoreme'"
        }),
        "{:?}",
        ig1.events
    );

    // `//ignoreme/deep/... -//ignoreme/...` is pruned by negative pattern before warning.
    let ig_neg_parent =
        pr.resolve_patterns(&["//ignoreme/deep/...".into(), "-//ignoreme/...".into()]);
    assert!(
        ig_neg_parent.errors.is_empty(),
        "{:?}",
        ig_neg_parent.errors
    );
    assert!(ig_neg_parent.targets.is_empty());
    assert!(
        ig_neg_parent.events.is_empty(),
        "{:?}",
        ig_neg_parent.events
    );

    // `//ignoreme/... -//ignoreme/deep/...` still logs warning on `//ignoreme/...`.
    let ig_neg_child =
        pr.resolve_patterns(&["//ignoreme/...".into(), "-//ignoreme/deep/...".into()]);
    assert!(ig_neg_child.errors.is_empty(), "{:?}", ig_neg_child.errors);
    assert!(ig_neg_child.targets.is_empty());
    assert!(
        ig_neg_child.events.iter().any(|e| {
            e.kind == EventKind::Warning
                && e.message
                    == "Pattern '//ignoreme/...' was filtered out by ignored directory 'ignoreme'"
        }),
        "{:?}",
        ig_neg_child.events
    );
}

#[test]
fn bzl_load_visibility_and_finalizer_macros() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    write(&fs, &root, "priv/BUILD", "");
    write(
        &fs,
        &root,
        "priv/secret.bzl",
        "visibility('//allowed/...')\nSECRET = 42\n",
    );
    write(
        &fs,
        &root,
        "allowed/sub/BUILD",
        "load('//priv:secret.bzl', 'SECRET')\nfilegroup(name = 'fg_' + str(SECRET))\n",
    );
    write(
        &fs,
        &root,
        "denied/BUILD",
        "load('//priv:secret.bzl', 'SECRET')\nfilegroup(name = 'bad')\n",
    );

    let resolver = SimpleRepoResolver::new(root.clone());
    let mut cfg = LoaderConfig::new(fs.clone(), Arc::new(resolver));
    cfg.use_prelude = false;
    let loader = PackageLoader::new(cfg);

    let ok_pkg = loader
        .load_package(&PackageIdentifier::create_in_main_repo("allowed/sub").unwrap())
        .unwrap();
    assert!(!ok_pkg.contains_errors, "{:?}", ok_pkg.events);
    assert!(ok_pkg.targets.contains_key("fg_42"));

    let bad_err = loader
        .load_package(&PackageIdentifier::create_in_main_repo("denied").unwrap())
        .unwrap_err();
    assert!(
        bad_err
            .message
            .contains("contains .bzl load visibility violations")
            && bad_err
                .events
                .iter()
                .any(|e| e.message.contains("is not visible for loading from")),
        "{bad_err}: {:?}",
        bad_err.events
    );

    // Finalizer macro sees rules declared later in BUILD, and inherit_attrs = "common" works.
    write(
        &fs,
        &root,
        "fin/defs.bzl",
        r#"
def _fin_impl(name, visibility, **kwargs):
    names = sorted(native.existing_rules().keys())
    native.filegroup(name = name + "_" + "_".join(names), visibility = visibility)

fin_macro = macro(
    implementation = _fin_impl,
    finalizer = True,
    inherit_attrs = "common",
    attrs = {"testonly": None},
)
"#,
    );
    write(
        &fs,
        &root,
        "fin/BUILD",
        r#"
load(":defs.bzl", "fin_macro")
fin_macro(name = "summary")
filegroup(name = "alpha")
filegroup(name = "beta")
"#,
    );
    let fin_pkg = loader
        .load_package(&PackageIdentifier::create_in_main_repo("fin").unwrap())
        .unwrap();
    assert!(!fin_pkg.contains_errors, "{:?}", fin_pkg.events);
    assert!(
        fin_pkg.targets.contains_key("summary_alpha_beta"),
        "{:?}",
        fin_pkg.targets.keys().collect::<Vec<_>>()
    );
}

#[test]
fn rule_inheritance_initializers_transitions_and_attr_types() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    write(
        &fs,
        &root,
        "pkg/defs.bzl",
        r#"
def _base_init(**kwargs):
    out = dict(kwargs)
    out["msg"] = (out.get("msg") or "") + "[base]"
    return out

def _child_init(**kwargs):
    out = dict(kwargs)
    out["msg"] = (out.get("msg") or "") + "[child]"
    return out

def _impl(ctx):
    return [DefaultInfo()]

base_rule = rule(
    implementation = _impl,
    extendable = True,
    initializer = _base_init,
    attrs = {
        "msg": attr.string(default = "hi"),
        "mapping": attr.label_list_dict(),
    },
)

child_rule = rule(
    implementation = _impl,
    parent = base_rule,
    initializer = _child_init,
    attrs = {
        "extra": attr.int(values = [1, 2]),
    },
)

if type(attr.string()) != "Attribute":
    fail("expected Attribute type, got " + type(attr.string()))

tt = config_common.toolchain_type("//pkg:my_tc", mandatory = False)
if str(tt.toolchain_type) != "@@//pkg:my_tc" or tt.mandatory != False:
    fail("unexpected toolchain_type struct: " + repr(tt))

def _t1(settings, attr):
    return {"//command_line_option:cpu": "k8"}

def _t2(settings, attr):
    return {"//command_line_option:compilation_mode": "opt"}

t1 = transition(implementation = _t1, inputs = [], outputs = ["//command_line_option:cpu"])
t2 = transition(implementation = _t2, inputs = [], outputs = ["//command_line_option:compilation_mode"])
composed = t1.and_then(t2)

s_flag = rule(
    implementation = _impl,
    build_setting = config.string_set(flag = True, repeatable = True),
)

def make_analysis_test(name, dep):
    testing.analysis_test(
        name = name,
        implementation = _impl,
        attrs = {"dep": attr.label()},
        attr_values = {"dep": dep},
    )
"#,
    );
    write(
        &fs,
        &root,
        "pkg/BUILD",
        r#"
load(":defs.bzl", "child_rule", "make_analysis_test", "s_flag")
child_rule(name = "c", msg = "start", extra = 2)
s_flag(name = "sf", build_setting_default = ["a", "b"])
make_analysis_test(name = "my_check", dep = ":c")
"#,
    );

    let resolver = SimpleRepoResolver::new(root.clone());
    let mut cfg = LoaderConfig::new(fs.clone(), Arc::new(resolver));
    cfg.use_prelude = false;
    let loader = PackageLoader::new(cfg);
    let pkg = loader
        .load_package(&PackageIdentifier::create_in_main_repo("pkg").unwrap())
        .unwrap();
    assert!(!pkg.contains_errors, "{:?}", pkg.events);
    let Some(zel_packages::Target::Rule(r)) = pkg.targets.get("c") else {
        panic!("missing rule c");
    };
    assert_eq!(
        r.attrs.get("msg"),
        Some(&Value::String("start[child][base]".into()))
    );
    assert!(pkg.targets.contains_key("my_check"));
}
