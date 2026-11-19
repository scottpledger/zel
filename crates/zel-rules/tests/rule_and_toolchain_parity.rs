use std::sync::Arc;
use zel_analysis::BuildConfigurationKey;
use zel_cmdline::{Label, RepositoryName};
use zel_packages::{LoaderConfig, PackageLoader, SimpleRepoResolver};
use zel_rules::{AnalysisConfig, TargetResolver};
use zel_starlark::Value;
use zel_vfs::{FileSystem, InMemoryFileSystem, PathFragment};

fn write(fs: &InMemoryFileSystem, root: &PathFragment, rel: &str, content: &str) {
    let p = root.get_relative_str(rel);
    if let Some(parent) = p.parent_directory() {
        fs.create_directory_and_parents(&parent).unwrap();
    }
    fs.write_file(&p, content.as_bytes()).unwrap();
}

fn resolver_for(fs: Arc<InMemoryFileSystem>, root: PathFragment) -> Arc<TargetResolver> {
    let lc = LoaderConfig::new(
        fs as Arc<dyn FileSystem>,
        Arc::new(SimpleRepoResolver::new(root)),
    );
    TargetResolver::new(PackageLoader::new(lc), AnalysisConfig::new())
}

#[test]
fn ctx_features_merges_package_rule_and_global_features() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    write(
        &fs,
        &root,
        "defs.bzl",
        r#"
Info = provider(fields = ["on", "off"])
def _impl(ctx):
    return [Info(on = ctx.features, off = ctx.disabled_features)]
feat_rule = rule(implementation = _impl)
"#,
    );
    write(
        &fs,
        &root,
        "BUILD",
        r#"
load(":defs.bzl", "feat_rule")
package(features = ["a", "-b", "c"])
feat_rule(name = "r", features = ["b", "-c", "d", "layering_check"])
"#,
    );

    let r = resolver_for(fs, root);
    let mut cfg = BuildConfigurationKey::target("k8", "fastbuild");
    cfg.options
        .insert("features".into(), "no_layering_check,global_on,-a".into());
    let ct = r
        .analyze(&Label::parse_canonical("//:r").unwrap(), &cfg)
        .unwrap();
    let Value::ProviderInstance { fields, .. } = &ct.providers["//:defs.bzl%Info"] else {
        panic!("expected Info");
    };
    let on = fields["on"].to_string_list();
    let off = fields["off"].to_string_list();
    // Package + rule enables b,d,layering_check, disables c. Global disables a and layering_check (via no_layering_check) and enables global_on.
    assert!(on.contains(&"b".to_string()), "{on:?}");
    assert!(on.contains(&"d".to_string()), "{on:?}");
    assert!(on.contains(&"global_on".to_string()), "{on:?}");
    assert!(!on.contains(&"a".to_string()), "{on:?}");
    assert!(!on.contains(&"layering_check".to_string()), "{on:?}");
    assert!(off.contains(&"a".to_string()), "{off:?}");
    assert!(off.contains(&"c".to_string()), "{off:?}");
    assert!(off.contains(&"layering_check".to_string()), "{off:?}");
}

#[test]
fn ctx_version_file_placeholders_and_package_relative_label() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    write(
        &fs,
        &root,
        "rules/rules.bzl",
        r#"
Info = provider(fields = ["vf", "inf", "ok_ph", "bad_ph", "lbl"])
def _impl(ctx):
    return [Info(
        vf = ctx.version_file.path,
        inf = ctx.info_file.path,
        ok_ph = ctx.check_placeholders("%{name}", ["name"]),
        bad_ph = ctx.check_placeholders("%{name}", ["abc"]),
        lbl = str(ctx.package_relative_label(":some_target")),
    )]
my_rule = rule(implementation = _impl)
"#,
    );
    write(&fs, &root, "rules/BUILD", "");
    write(
        &fs,
        &root,
        "test/BUILD",
        "load('//rules:rules.bzl', 'my_rule')\nmy_rule(name = 't')\n",
    );

    let r = resolver_for(fs, root);
    let ct = r
        .analyze(
            &Label::parse_canonical("//test:t").unwrap(),
            &BuildConfigurationKey::target("k8", "fastbuild"),
        )
        .unwrap();
    let Value::ProviderInstance { fields, .. } = &ct.providers["//rules:rules.bzl%Info"] else {
        panic!("expected Info");
    };
    assert_eq!(fields["vf"].as_str(), Some("bazel-out/volatile-status.txt"));
    assert_eq!(fields["inf"].as_str(), Some("bazel-out/stable-status.txt"));
    assert_eq!(fields["ok_ph"], Value::Bool(true));
    assert_eq!(fields["bad_ph"], Value::Bool(false));
    assert_eq!(fields["lbl"].as_str(), Some("@@//test:some_target"));
}

#[test]
fn genrule_location_repo_mapping_aliases_and_template_variable_info() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    let ext = PathFragment::create("/ext_bar");
    write(&fs, &root, "bin.sh", "#!/bin/sh\necho hi\n");
    write(
        &fs,
        &root,
        "defs.bzl",
        r#"
def _tv_impl(ctx):
    return [platform_common.TemplateVariableInfo({"FOO": "bar_val"})]
tv = rule(implementation = _tv_impl)
"#,
    );
    write(
        &fs,
        &root,
        "BUILD",
        r#"
load(":defs.bzl", "tv")
tv(name = "my_tv")
filegroup(name = "real_f", srcs = ["bin.sh"])
alias(name = "aliased_f", actual = ":real_f")
genrule(
    name = "g",
    srcs = [":aliased_f", "@bar//:baz"],
    tools = ["bin.sh"],
    toolchains = [":my_tv"],
    outs = ["out.txt"],
    cmd = "$(location bin.sh) $(location :aliased_f) $(location @bar//:baz) $(FOO) > $@",
)
"#,
    );
    write(&fs, &ext, "BUILD", "exports_files(['baz'])\n");
    write(&fs, &ext, "baz", "hello");

    let mut rr = SimpleRepoResolver::new(root);
    rr.external.insert("bar+".into(), ext);
    rr.apparent
        .insert("bar".into(), RepositoryName::create("bar+").unwrap());
    let lc = LoaderConfig::new(fs as Arc<dyn FileSystem>, Arc::new(rr));
    let r = TargetResolver::new(PackageLoader::new(lc), AnalysisConfig::new());

    let ct = r
        .analyze(
            &Label::parse_canonical("//:g").unwrap(),
            &BuildConfigurationKey::target("k8", "fastbuild"),
        )
        .unwrap();
    let argv = ct.typed_actions[0].command_line().0;
    let cmd = argv.last().unwrap();
    assert!(cmd.contains("./bin.sh"), "{cmd}");
    assert!(cmd.contains("bar_val"), "{cmd}");
    assert!(cmd.contains("external/bar+/baz"), "{cmd}");
}

#[test]
fn target_compatible_with_propagation_and_toolchain_type_alias() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    write(
        &fs,
        &root,
        "defs.bzl",
        r#"
def _tc_impl(ctx):
    return [platform_common.ToolchainInfo(val = "ok")]
tc_rule = rule(implementation = _tc_impl)

Info = provider(fields = ["by_alias", "by_real"])
def _use_impl(ctx):
    return [Info(
        by_alias = ctx.toolchains["//:alias_tt"].val,
        by_real = ctx.toolchains["//:real_tt"].val,
    )]
use_tc = rule(implementation = _use_impl, toolchains = ["//:alias_tt"])
"#,
    );
    write(
        &fs,
        &root,
        "BUILD",
        r#"
load(":defs.bzl", "tc_rule", "use_tc")
constraint_setting(name = "cs")
constraint_value(name = "cv", constraint_setting = ":cs")
toolchain_type(name = "real_tt")
alias(name = "alias_tt", actual = ":real_tt")
tc_rule(name = "tc_impl")
toolchain(name = "tc", toolchain_type = ":real_tt", toolchain = ":tc_impl")
use_tc(name = "u")
filegroup(name = "incompat", srcs = [], target_compatible_with = [":cv"])
filegroup(name = "dep_on_incompat", srcs = [":incompat"])
"#,
    );

    let lc = LoaderConfig::new(
        fs as Arc<dyn FileSystem>,
        Arc::new(SimpleRepoResolver::new(root)),
    );
    let mut acfg = AnalysisConfig::new();
    acfg.registered_toolchains = vec![Label::parse_canonical("//:tc").unwrap()];
    let r = TargetResolver::new(PackageLoader::new(lc), acfg);
    let cfg = BuildConfigurationKey::target("k8", "fastbuild");

    let ct = r
        .analyze(&Label::parse_canonical("//:u").unwrap(), &cfg)
        .unwrap();
    let Value::ProviderInstance { fields, .. } = &ct.providers["//:defs.bzl%Info"] else {
        panic!("expected Info");
    };
    assert_eq!(fields["by_alias"].as_str(), Some("ok"));
    assert_eq!(fields["by_real"].as_str(), Some("ok"));

    let ct_inc = r
        .analyze(&Label::parse_canonical("//:dep_on_incompat").unwrap(), &cfg)
        .unwrap();
    let inc = ct_inc
        .incompatible
        .as_ref()
        .expect("expected IncompatiblePlatform");
    assert_eq!(
        inc.constraints
            .iter()
            .map(|c| c.canonical_form())
            .collect::<Vec<_>>(),
        vec!["//:cv"]
    );
}

#[test]
fn transition_on_starlark_flag_and_label_flag() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    write(
        &fs,
        &root,
        "defs.bzl",
        r#"
BSI = provider(fields = ["value"])
def _flag_impl(ctx):
    return [BSI(value = ctx.build_setting_value)]
str_flag = rule(implementation = _flag_impl, build_setting = config.string(flag = True))

def _tr_impl(settings, attr):
    return {
        "//:sflag": settings["//:sflag"] + "-edited",
        "//:lflag": "//:b",
    }
my_tr = transition(implementation = _tr_impl, inputs = ["//:sflag"], outputs = ["//:sflag", "//:lflag"])

Info = provider(fields = ["s", "l"])
def _dep_impl(ctx):
    return [Info(s = ctx.attr._s[BSI].value, l = str(ctx.attr._l[DefaultInfo].files.to_list()))]
dep_rule = rule(
    implementation = _dep_impl,
    attrs = {
        "_s": attr.label(default = "//:sflag"),
        "_l": attr.label(default = "//:lflag"),
    },
)

def _top_impl(ctx):
    return [ctx.attr.dep[Info]]
top_rule = rule(implementation = _top_impl, attrs = {"dep": attr.label(cfg = my_tr)})
"#,
    );
    write(
        &fs,
        &root,
        "BUILD",
        r#"
load(":defs.bzl", "str_flag", "dep_rule", "top_rule")
str_flag(name = "sflag", build_setting_default = "apple")
filegroup(name = "a", srcs = [])
filegroup(name = "b", srcs = [])
label_flag(name = "lflag", build_setting_default = ":a")
dep_rule(name = "d")
top_rule(name = "top", dep = ":d")
"#,
    );

    let r = resolver_for(fs, root);
    let ct = r
        .analyze(
            &Label::parse_canonical("//:top").unwrap(),
            &BuildConfigurationKey::target("k8", "fastbuild"),
        )
        .unwrap();
    let Value::ProviderInstance { fields, .. } = &ct.providers["//:defs.bzl%Info"] else {
        panic!("expected Info");
    };
    assert_eq!(fields["s"].as_str(), Some("apple-edited"));
}

#[test]
fn lazy_toolchain_resolution_skips_unrelated_toolchains_and_later_external_patterns() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zel_rules::PatternExpander;

    struct CountingExpander {
        inner_loader: Arc<PackageLoader>,
        expanded_patterns: std::sync::Mutex<Vec<String>>,
        calls: AtomicUsize,
    }

    impl std::fmt::Debug for CountingExpander {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("CountingExpander").finish_non_exhaustive()
        }
    }

    impl PatternExpander for CountingExpander {
        fn expand(&self, patterns: &[String], rule_class: &str) -> Result<Vec<Label>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let pr = zel_packages::PatternResolver::new(
                self.inner_loader.clone(),
                PathFragment::empty(),
            );
            let mut out = Vec::new();
            for p in patterns {
                self.expanded_patterns.lock().unwrap().push(p.clone());
                let outcome = pr.resolve_patterns(std::slice::from_ref(p));
                if let Some(e) = outcome.errors.first() {
                    return Err(e.message.clone());
                }
                for t in outcome.targets {
                    if let zel_packages::Target::Rule(r) = t {
                        if r.rule_class == rule_class && !out.contains(&r.label) {
                            out.push(r.label);
                        }
                    }
                }
            }
            Ok(out)
        }

        fn expand_toolchain_pattern(
            &self,
            pattern: &str,
        ) -> Result<Vec<zel_rules::RegisteredToolchain>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.expanded_patterns
                .lock()
                .unwrap()
                .push(pattern.to_string());
            let pr = zel_packages::PatternResolver::new(
                self.inner_loader.clone(),
                PathFragment::empty(),
            );
            let outcome = pr.resolve_patterns(std::slice::from_ref(&pattern.to_string()));
            if let Some(e) = outcome.errors.first() {
                return Err(e.message.clone());
            }
            let mut out = Vec::new();
            for t in outcome.targets {
                if let zel_packages::Target::Rule(r) = t {
                    if r.rule_class == "toolchain" {
                        let raw_toolchain_type = match r.attr_or_default("toolchain_type") {
                            Some(zel_packages::AttrValue::Label(Some(l))) => Some(l),
                            _ => None,
                        };
                        out.push(zel_rules::RegisteredToolchain {
                            label: r.label,
                            raw_toolchain_type,
                        });
                    }
                }
            }
            Ok(out)
        }
    }

    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    write(
        &fs,
        &root,
        "defs.bzl",
        r#"
def _tc_impl(ctx):
    return [platform_common.ToolchainInfo(val = ctx.attr.val)]
tc_rule = rule(implementation = _tc_impl, attrs = {"val": attr.string()})

Info = provider(fields = ["val"])
def _use_impl(ctx):
    return [Info(val = ctx.toolchains["//:alias_a"].val)]
use_a = rule(implementation = _use_impl, toolchains = ["//:alias_a"])
"#,
    );
    write(
        &fs,
        &root,
        "BUILD",
        r#"
load(":defs.bzl", "tc_rule", "use_a")
toolchain_type(name = "type_a")
alias(name = "alias_a", actual = ":type_a")
toolchain_type(name = "type_b")
tc_rule(name = "impl_a", val = "alpha")
tc_rule(name = "impl_b", val = "beta")
toolchain(name = "tc_b", toolchain_type = ":type_b", toolchain = ":impl_b")
toolchain(name = "tc_a", toolchain_type = ":type_a", toolchain = ":impl_a")
use_a(name = "u1")
use_a(name = "u2")
"#,
    );
    let lc = LoaderConfig::new(
        fs as Arc<dyn FileSystem>,
        Arc::new(SimpleRepoResolver::new(root)),
    );
    let loader = PackageLoader::new(lc);
    let expander = Arc::new(CountingExpander {
        inner_loader: loader.clone(),
        expanded_patterns: std::sync::Mutex::new(Vec::new()),
        calls: AtomicUsize::new(0),
    });
    let mut acfg = AnalysisConfig::new();
    acfg.expander = Some(expander.clone());
    // Main-repo pattern satisfies `type_a`; trailing external pattern for a nonexistent repo must
    // never be expanded because `type_a` is already matched.
    acfg.toolchain_patterns = vec![
        "//:all".to_string(),
        "@@unrelated_external_repo//:all".to_string(),
    ];
    let r = TargetResolver::new(loader, acfg);
    let cfg = BuildConfigurationKey::target("k8", "fastbuild");

    for target in ["//:u1", "//:u2"] {
        let ct = r
            .analyze(&Label::parse_canonical(target).unwrap(), &cfg)
            .unwrap();
        let Value::ProviderInstance { fields, .. } = &ct.providers["//:defs.bzl%Info"] else {
            panic!("expected Info");
        };
        assert_eq!(fields["val"].as_str(), Some("alpha"));
    }

    // Pattern expansion is memoized across targets and never touched the trailing external repo.
    assert_eq!(expander.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *expander.expanded_patterns.lock().unwrap(),
        vec!["//:all".to_string()]
    );
    // Unrelated toolchain `//:tc_b` (different toolchain_type) must never have been analyzed.
    let analyzed_labels: Vec<String> = r
        .analyzed_targets()
        .iter()
        .map(|ct| ct.label.canonical_form())
        .collect();
    assert!(
        !analyzed_labels.iter().any(|l| l == "//:tc_b"),
        "unrelated toolchain //:tc_b should not be analyzed: {analyzed_labels:?}"
    );
}
