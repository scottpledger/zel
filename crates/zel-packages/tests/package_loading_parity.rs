use std::sync::Arc;
use zel_cmdline::{Label, PackageIdentifier, RepositoryMapping, RepositoryName};
use zel_packages::attr::{convert_plain, AttrType, AttrValue, ConvertCtx};
use zel_packages::glob::{evaluate, GlobContext, GlobOp};
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
