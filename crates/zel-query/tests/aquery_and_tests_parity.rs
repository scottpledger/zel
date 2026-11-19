use std::sync::Arc;
use zel_analysis::BuildConfigurationKey;
use zel_cmdline::RepositoryName;
use zel_packages::{LoaderConfig, PackageLoader, PatternResolver, SimpleRepoResolver};
use zel_query::format::{FormatOpts, OrderOutput};
use zel_query::post_analysis::{run_post_analysis, PostOpts};
use zel_query::run_query;
use zel_query::target_env::QueryOpts;
use zel_rules::{AnalysisConfig, TargetResolver};
use zel_vfs::{FileSystem, InMemoryFileSystem, PathFragment};

fn write(fs: &InMemoryFileSystem, root: &PathFragment, rel: &str, content: &str) {
    let p = root.get_relative_str(rel);
    if let Some(parent) = p.parent_directory() {
        fs.create_directory_and_parents(&parent).unwrap();
    }
    fs.write_file(&p, content.as_bytes()).unwrap();
}

#[test]
fn aquery_rejects_nested_action_filter_and_query_tests_handles_size_manual_plus() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    fs.create_directory_and_parents(&root).unwrap();
    write(
        &fs,
        &root,
        "defs.bzl",
        r##"
def _t_impl(ctx):
    exe = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.write(exe, "#!/bin/sh\nexit 0\n", is_executable = True)
    return [DefaultInfo(executable = exe, files = depset([exe]))]
my_test = rule(implementation = _t_impl, test = True)
"##,
    );
    write(
        &fs,
        &root,
        "pkg/BUILD",
        r#"
load("//:defs.bzl", "my_test")
my_test(name = "small_t", size = "small", tags = ["t1"])
my_test(name = "large_t", size = "large", tags = ["t1"])
test_suite(name = "suite_small", tags = ["manual", "+small"])
"#,
    );
    write(&fs, &root, "BUILD", "");

    let resolver = PatternResolver::new(
        PackageLoader::new(LoaderConfig::new(
            fs.clone(),
            Arc::new(SimpleRepoResolver::new(root.clone())),
        )),
        PathFragment::empty(),
    );

    let q_out = run_query(
        &resolver,
        "tests(//pkg:suite_small)",
        &QueryOpts::default(),
        &FormatOpts::default(),
    );
    assert_eq!(q_out.exit_code, 0, "{:?}", q_out.stderr);
    assert_eq!(
        String::from_utf8_lossy(&q_out.stdout).trim(),
        "//pkg:small_t"
    );

    let tr = TargetResolver::new(resolver.loader().clone(), AnalysisConfig::new());
    let aq_out = run_post_analysis(
        &resolver,
        tr,
        BuildConfigurationKey::default(),
        "deps(inputs('abc', //pkg:small_t))",
        &QueryOpts::default(),
        &FormatOpts {
            order: OrderOutput::Auto,
            ..Default::default()
        },
        &PostOpts {
            aquery: true,
            output: "text".into(),
            ..Default::default()
        },
    );
    assert_ne!(aq_out.exit_code, 0);
    let err = aq_out.stderr.join("\n");
    assert!(
        err.contains(
            "produce actions, and therefore can't be the input of other function types: deps"
        ),
        "{err}"
    );
}

#[test]
fn query_package_group_respects_repo_mapping() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    let ext = PathFragment::create("/ext_veg");
    fs.create_directory_and_parents(&root).unwrap();
    fs.create_directory_and_parents(&ext).unwrap();
    write(
        &fs,
        &root,
        "BUILD",
        "package_group(name = 'pg', packages = ['@my_veggies//cucumber/...'])\nfilegroup(name = 'vis_target', visibility = ['//:pg'])\n",
    );
    write(&fs, &ext, "cucumber/BUILD", "filegroup(name = 'c')\n");

    let mut rr = SimpleRepoResolver::new(root);
    rr.external.insert("veggies+".into(), ext);
    rr.apparent.insert(
        "my_veggies".into(),
        RepositoryName::create("veggies+").unwrap(),
    );
    let resolver = PatternResolver::new(
        PackageLoader::new(LoaderConfig::new(fs, Arc::new(rr))),
        PathFragment::empty(),
    );

    let out = run_query(
        &resolver,
        "visible(@my_veggies//cucumber:c, //:vis_target)",
        &QueryOpts::default(),
        &FormatOpts::default(),
    );
    assert_eq!(out.exit_code, 0, "{:?}", out.stderr);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "//:vis_target");
}
