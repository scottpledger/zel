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

#[test]
fn javatests_one_way_visibility_and_genquery_strict_scope() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    fs.create_directory_and_parents(&root).unwrap();
    write(
        &fs,
        &root,
        "java/com/foo/BUILD",
        "filegroup(name = 'lib', visibility = ['//visibility:private'])\n",
    );
    write(
        &fs,
        &root,
        "javatests/com/foo/BUILD",
        "filegroup(name = 'tst', srcs = ['//java/com/foo:lib'], visibility = ['//visibility:private'])\n",
    );
    write(&fs, &root, "other/BUILD", "filegroup(name = 'outside')\n");

    let loader = PackageLoader::new(LoaderConfig::new(
        fs,
        Arc::new(SimpleRepoResolver::new(root)),
    ));
    let resolver = PatternResolver::new(Arc::clone(&loader), PathFragment::empty());

    let ok_vis = run_query(
        &resolver,
        "visible(//javatests/com/foo:tst, //java/com/foo:lib)",
        &QueryOpts::default(),
        &FormatOpts::default(),
    );
    assert_eq!(ok_vis.exit_code, 0, "{:?}", ok_vis.stderr);
    assert_eq!(
        String::from_utf8_lossy(&ok_vis.stdout).trim(),
        "//java/com/foo:lib"
    );

    let rev_vis = run_query(
        &resolver,
        "visible(//java/com/foo:lib, //javatests/com/foo:tst)",
        &QueryOpts::default(),
        &FormatOpts::default(),
    );
    assert_eq!(rev_vis.exit_code, 0, "{:?}", rev_vis.stderr);
    assert!(
        String::from_utf8_lossy(&rev_vis.stdout).trim().is_empty(),
        "javatests visibility must be one-way"
    );

    let oos = zel_query::run_genquery(
        &loader,
        "deps(//other:outside)",
        &["//javatests/com/foo:tst".to_string()],
        true,
        &[],
    );
    let err = oos.unwrap_err();
    assert!(
        err.contains("is not within the scope of the query"),
        "{err}"
    );

    let transitive_ok = zel_query::run_genquery(
        &loader,
        "deps(//javatests/com/foo:tst)",
        &["//javatests/com/foo:tst".to_string()],
        true,
        &[],
    )
    .unwrap();
    let txt = String::from_utf8_lossy(&transitive_ok);
    assert!(txt.contains("//java/com/foo:lib"), "{txt}");
}

#[test]
fn cquery_noimplicit_keeps_select_conditions_and_files_unions_default_group() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    fs.create_directory_and_parents(&root).unwrap();
    write(&fs, &root, "BUILD", "");
    write(
        &fs,
        &root,
        "defs.bzl",
        r#"
def _r(ctx):
    f1 = ctx.actions.declare_file(ctx.label.name + ".f1")
    f2 = ctx.actions.declare_file(ctx.label.name + ".f2")
    ctx.actions.write(f1, "1")
    ctx.actions.write(f2, "2")
    return [
        DefaultInfo(files = depset([f1])),
        OutputGroupInfo(default = depset([f2])),
    ]
dual_rule = rule(implementation = _r, attrs = {"srcs": attr.label_list(allow_files = True)})
"#,
    );
    write(
        &fs,
        &root,
        "pkg/BUILD",
        r#"
load("//:defs.bzl", "dual_rule")
config_setting(name = "cs_opt", values = {"compilation_mode": "opt"})
dual_rule(
    name = "d",
    srcs = select({":cs_opt": ["a.txt"], "//conditions:default": ["b.txt"]}),
)
"#,
    );
    write(&fs, &root, "pkg/a.txt", "a");
    write(&fs, &root, "pkg/b.txt", "b");

    let loader = PackageLoader::new(LoaderConfig::new(
        fs,
        Arc::new(SimpleRepoResolver::new(root)),
    ));
    let resolver = PatternResolver::new(Arc::clone(&loader), PathFragment::empty());
    let tr = TargetResolver::new(loader, AnalysisConfig::default());

    let cq_deps = run_post_analysis(
        &resolver,
        Arc::clone(&tr),
        BuildConfigurationKey::default(),
        "deps(//pkg:d)",
        &QueryOpts {
            implicit_deps: false,
            ..Default::default()
        },
        &FormatOpts::default(),
        &PostOpts {
            output: "label".into(),
            ..Default::default()
        },
    );
    assert_eq!(cq_deps.exit_code, 0, "{:?}", cq_deps.stderr);
    let deps_txt = String::from_utf8_lossy(&cq_deps.stdout);
    assert!(
        deps_txt.contains("//pkg:cs_opt"),
        "select condition should stay visible under --noimplicit_deps: {deps_txt}"
    );

    let cq_files = run_post_analysis(
        &resolver,
        Arc::clone(&tr),
        BuildConfigurationKey::default(),
        "//pkg:d",
        &QueryOpts::default(),
        &FormatOpts::default(),
        &PostOpts {
            output: "files".into(),
            ..Default::default()
        },
    );
    assert_eq!(cq_files.exit_code, 0, "{:?}", cq_files.stderr);
    let files_txt = String::from_utf8_lossy(&cq_files.stdout);
    assert!(files_txt.contains("d.f1"), "{files_txt}");
    assert!(files_txt.contains("d.f2"), "{files_txt}");

    let cq_proto = run_post_analysis(
        &resolver,
        tr,
        BuildConfigurationKey::default(),
        "//pkg:d",
        &QueryOpts::default(),
        &FormatOpts::default(),
        &PostOpts {
            output: "textproto".into(),
            transitions: "lite".into(),
            ..Default::default()
        },
    );
    assert_eq!(cq_proto.exit_code, 0, "{:?}", cq_proto.stderr);
    let proto_txt = String::from_utf8_lossy(&cq_proto.stdout);
    assert!(
        proto_txt.contains("configured_rule_input"),
        "cquery proto --transitions=lite should populate configured_rule_input: {proto_txt}"
    );
}
