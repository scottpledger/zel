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

#[test]
fn windows_dll_library_with_rules_cc_and_builtin_cc_common() {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let rules_cc = repo_root.join("third_party/bcr/rules_cc/0.2.17");
    let skylib = repo_root.join("third_party/bcr/bazel_skylib/1.8.2");
    let platforms = repo_root.join("third_party/bcr/platforms/1.0.0");
    let bazel_tools = repo_root.join("crates/zel-bazel-tools/resources");
    assert!(rules_cc.exists(), "missing vendored rules_cc");

    let tmp_base =
        std::env::temp_dir().join(format!("zel-win-dll-rules-cc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_base);
    let ws_dir = tmp_base.join("ws");
    let proxy_dir = tmp_base.join("cc_compatibility_proxy");
    let protobuf = tmp_base.join("com_google_protobuf");
    let host_platform = tmp_base.join("host_platform");
    std::fs::create_dir_all(ws_dir.join("tc")).unwrap();
    std::fs::create_dir_all(&proxy_dir).unwrap();
    std::fs::create_dir_all(protobuf.join("bazel/common")).unwrap();
    std::fs::create_dir_all(&host_platform).unwrap();
    std::fs::write(protobuf.join("bazel/common/BUILD"), "").unwrap();
    std::fs::write(
        protobuf.join("bazel/common/proto_info.bzl"),
        "ProtoInfo = provider(fields = [])\n",
    )
    .unwrap();
    std::fs::write(host_platform.join("BUILD"), "").unwrap();
    std::fs::write(
        host_platform.join("constraints.bzl"),
        "HOST_CONSTRAINTS = []\n",
    )
    .unwrap();

    // Match @cc_compatibility_proxy generated by rules_cc/cc/extensions.bzl on Bazel 9.3.2.
    std::fs::write(proxy_dir.join("BUILD"), "").unwrap();
    std::fs::write(
        proxy_dir.join("proxy.bzl"),
        r#"
load("@rules_cc//cc/private/rules_impl:cc_binary.bzl", _cc_binary = "cc_binary")
load("@rules_cc//cc/private/rules_impl:cc_import.bzl", _cc_import = "cc_import")
load("@rules_cc//cc/private/rules_impl:cc_library.bzl", _cc_library = "cc_library")
load("@rules_cc//cc/private/rules_impl:cc_shared_library.bzl", _cc_shared_library = "cc_shared_library")
load("@rules_cc//cc/private/rules_impl:cc_static_library.bzl", _cc_static_library = "cc_static_library")
load("@rules_cc//cc/private/rules_impl:cc_test.bzl", _cc_test = "cc_test")
load("@rules_cc//cc/private/rules_impl:objc_import.bzl", _objc_import = "objc_import")
load("@rules_cc//cc/private/rules_impl:objc_library.bzl", _objc_library = "objc_library")
load("@rules_cc//cc/private/rules_impl/fdo:fdo_prefetch_hints.bzl", _fdo_prefetch_hints = "fdo_prefetch_hints")
load("@rules_cc//cc/private/rules_impl/fdo:fdo_profile.bzl", _fdo_profile = "fdo_profile")
load("@rules_cc//cc/private/rules_impl/fdo:memprof_profile.bzl", _memprof_profile = "memprof_profile")
load("@rules_cc//cc/private/rules_impl/fdo:propeller_optimize.bzl", _propeller_optimize = "propeller_optimize")
load("@rules_cc//cc/private/rules_impl:cc_toolchain.bzl", _cc_toolchain = "cc_toolchain")
load("@rules_cc//cc/private/rules_impl:cc_toolchain_alias.bzl", _cc_toolchain_alias = "cc_toolchain_alias")

cc_binary = _cc_binary
cc_import = _cc_import
cc_library = _cc_library
cc_shared_library = _cc_shared_library
cc_static_library = _cc_static_library
cc_test = _cc_test
objc_import = _objc_import
objc_library = _objc_library
fdo_prefetch_hints = _fdo_prefetch_hints
fdo_profile = _fdo_profile
memprof_profile = _memprof_profile
propeller_optimize = _propeller_optimize
cc_toolchain = _cc_toolchain
cc_toolchain_alias = _cc_toolchain_alias
"#,
    )
    .unwrap();
    std::fs::write(
        proxy_dir.join("symbols.bzl"),
        r#"
load("@rules_cc//cc/private:cc_common.bzl", _cc_common = "cc_common")
load("@rules_cc//cc/private:cc_info.bzl", _CcInfo = "CcInfo")
load("@rules_cc//cc/private:cc_shared_library_info.bzl", _CcSharedLibraryInfo = "CcSharedLibraryInfo")
load("@rules_cc//cc/private:debug_package_info.bzl", _DebugPackageInfo = "DebugPackageInfo")
load("@rules_cc//cc/private:objc_info.bzl", _ObjcInfo = "ObjcInfo")
load("@rules_cc//cc/private/toolchain_config:cc_toolchain_config_info.bzl", _CcToolchainConfigInfo = "CcToolchainConfigInfo")

cc_common = _cc_common
CcInfo = _CcInfo
DebugPackageInfo = _DebugPackageInfo
CcToolchainConfigInfo = _CcToolchainConfigInfo
ObjcInfo = _ObjcInfo
new_objc_provider = _ObjcInfo
CcSharedLibraryInfo = _CcSharedLibraryInfo
"#,
    )
    .unwrap();

    // Real Windows MSVC C++ toolchain using @rules_cc windows_cc_toolchain_config.bzl.
    std::fs::write(
        ws_dir.join("tc/BUILD"),
        r#"
load("@rules_cc//cc/toolchains:cc_toolchain.bzl", "cc_toolchain")
load("@rules_cc//cc/private/toolchain:windows_cc_toolchain_config.bzl", "cc_toolchain_config")

package(default_visibility = ["//visibility:public"])

filegroup(name = "empty", srcs = [])

cc_toolchain_config(
    name = "msvc_x64_config",
    cpu = "x64_windows",
    compiler = "msvc-cl",
    host_system_name = "local",
    target_system_name = "local",
    target_libc = "msvcrt",
    abi_version = "local",
    abi_libc_version = "local",
    toolchain_identifier = "msvc_x64",
    msvc_env_tmp = "C:/Temp",
    msvc_env_path = "C:/MSVC/bin",
    msvc_env_include = "C:/MSVC/include",
    msvc_env_lib = "C:/MSVC/lib",
    msvc_cl_path = "C:/MSVC/bin/cl.exe",
    msvc_ml_path = "C:/MSVC/bin/ml64.exe",
    msvc_link_path = "C:/MSVC/bin/link.exe",
    msvc_lib_path = "C:/MSVC/bin/lib.exe",
    cxx_builtin_include_directories = ["C:/MSVC/include"],
    tool_paths = {
        "ar": "C:/MSVC/bin/lib.exe",
        "ml": "C:/MSVC/bin/ml64.exe",
        "cpp": "C:/MSVC/bin/cl.exe",
        "gcc": "C:/MSVC/bin/cl.exe",
        "gcov": "wrapper/bin/msvc_nop.bat",
        "ld": "C:/MSVC/bin/link.exe",
        "nm": "wrapper/bin/msvc_nop.bat",
        "objcopy": "wrapper/bin/msvc_nop.bat",
        "objdump": "wrapper/bin/msvc_nop.bat",
        "strip": "wrapper/bin/msvc_nop.bat",
    },
    archiver_flags = ["/MACHINE:X64"],
    default_link_flags = ["/MACHINE:X64"],
)

cc_toolchain(
    name = "cc-compiler-x64_windows",
    toolchain_identifier = "msvc_x64",
    toolchain_config = ":msvc_x64_config",
    all_files = ":empty",
    ar_files = ":empty",
    as_files = ":empty",
    compiler_files = ":empty",
    dwp_files = ":empty",
    linker_files = ":empty",
    objcopy_files = ":empty",
    strip_files = ":empty",
    supports_param_files = 1,
)

toolchain(
    name = "cc-toolchain-x64_windows",
    toolchain = ":cc-compiler-x64_windows",
    toolchain_type = "@bazel_tools//tools/cpp:toolchain_type",
)
"#,
    )
    .unwrap();

    // Bazel's examples/windows/dll/windows_dll_library.bzl pattern + a custom Starlark rule
    // exercising predeclared builtin `cc_common.link` & `cc_common.create_library_to_link`
    // directly against the @rules_cc CcToolchainInfo.
    std::fs::write(
        ws_dir.join("windows_dll_library.bzl"),
        r#"
load("@rules_cc//cc:cc_binary.bzl", "cc_binary")
load("@rules_cc//cc:cc_import.bzl", "cc_import")
load("@rules_cc//cc:cc_library.bzl", "cc_library")

def windows_dll_library(
        name,
        srcs = [],
        hdrs = [],
        deps = [],
        visibility = None,
        **kwargs):
    dll_name = name + ".dll"
    import_lib_name = name + "_import_lib"
    import_target_name = name + "_dll_import"

    cc_binary(
        name = dll_name,
        srcs = srcs + hdrs,
        deps = deps,
        linkshared = 1,
        **kwargs
    )

    native.filegroup(
        name = import_lib_name,
        srcs = [":" + dll_name],
        output_group = "interface_library",
    )

    cc_import(
        name = import_target_name,
        interface_library = ":" + import_lib_name,
        shared_library = ":" + dll_name,
    )

    cc_library(
        name = name,
        hdrs = hdrs,
        visibility = visibility,
        deps = deps + [":" + import_target_name],
    )

BuiltinLinkInfo = provider(fields = ["dll_short", "iface_short", "wrapped_iface_short"])

def _builtin_cc_common_dll_impl(ctx):
    # Predeclared `cc_common` (NOT loaded from @rules_cc) operating on @rules_cc CcToolchainInfo!
    tc = ctx.toolchains["@bazel_tools//tools/cpp:toolchain_type"].cc
    fc = cc_common.configure_features(
        ctx = ctx,
        cc_toolchain = tc,
        requested_features = ctx.features,
        unsupported_features = ctx.disabled_features,
    )
    _, comp_outputs = cc_common.compile(
        actions = ctx.actions,
        feature_configuration = fc,
        cc_toolchain = tc,
        srcs = ctx.files.srcs,
        name = ctx.label.name,
    )
    link_outputs = cc_common.link(
        actions = ctx.actions,
        feature_configuration = fc,
        cc_toolchain = tc,
        compilation_outputs = comp_outputs,
        name = ctx.label.name,
        output_type = "dynamic_library",
    )
    ltl = link_outputs.library_to_link
    wrapped = cc_common.create_library_to_link(
        actions = ctx.actions,
        feature_configuration = fc,
        cc_toolchain = tc,
        dynamic_library = ltl.dynamic_library,
        interface_library = ltl.interface_library,
    )
    return [
        DefaultInfo(files = depset([ltl.dynamic_library, ltl.interface_library])),
        BuiltinLinkInfo(
            dll_short = ltl.dynamic_library.short_path,
            iface_short = ltl.interface_library.short_path,
            wrapped_iface_short = wrapped.interface_library.short_path,
        ),
    ]

builtin_cc_common_dll = rule(
    implementation = _builtin_cc_common_dll_impl,
    fragments = ["cpp"],
    toolchains = ["@bazel_tools//tools/cpp:toolchain_type"],
    attrs = {"srcs": attr.label_list(allow_files = True)},
)
"#,
    )
    .unwrap();

    std::fs::write(
        ws_dir.join("hellolib.h"),
        "__declspec(dllexport) int greet();\n",
    )
    .unwrap();
    std::fs::write(
        ws_dir.join("hellolib.cc"),
        "#include \"hellolib.h\"\nint greet() { return 7; }\n",
    )
    .unwrap();
    std::fs::write(
        ws_dir.join("hello.cc"),
        "#include \"hellolib.h\"\nint main() { return greet(); }\n",
    )
    .unwrap();
    std::fs::write(
        ws_dir.join("BUILD"),
        r#"
load("@rules_cc//cc:cc_binary.bzl", "cc_binary")
load(":windows_dll_library.bzl", "builtin_cc_common_dll", "windows_dll_library")

windows_dll_library(
    name = "hellolib",
    srcs = ["hellolib.cc"],
    hdrs = ["hellolib.h"],
    defines = ["COMPILING_DLL"],
)

cc_binary(
    name = "hello",
    srcs = ["hello.cc"],
    deps = [":hellolib"],
)

builtin_cc_common_dll(
    name = "raw_builtin",
    srcs = ["hellolib.cc"],
)
"#,
    )
    .unwrap();

    let mut repo_resolver =
        SimpleRepoResolver::new(PathFragment::create(&ws_dir.to_string_lossy()));
    repo_resolver
        .apparent
        .insert(String::new(), RepositoryName::main());
    for (name, path) in [
        ("rules_cc", &rules_cc),
        ("bazel_skylib", &skylib),
        ("platforms", &platforms),
        ("com_google_protobuf", &protobuf),
        ("host_platform", &host_platform),
        ("bazel_tools", &bazel_tools),
        ("cc_compatibility_proxy", &proxy_dir),
    ] {
        repo_resolver.external.insert(
            name.to_string(),
            PathFragment::create(&path.to_string_lossy()),
        );
        repo_resolver
            .apparent
            .insert(name.to_string(), RepositoryName::create(name).unwrap());
    }

    let lc = LoaderConfig::new(
        Arc::new(zel_vfs::LocalFileSystem::default()),
        Arc::new(repo_resolver),
    );
    let mut acfg = AnalysisConfig::new();
    acfg.registered_toolchains =
        vec![Label::parse_canonical("//tc:cc-toolchain-x64_windows").unwrap()];
    let resolver = TargetResolver::new(PackageLoader::new(lc), acfg);
    let cfg = BuildConfigurationKey::target("x64_windows", "fastbuild");

    // 1. Analyze hellolib_import_lib (filegroup with output_group = "interface_library").
    let import_fg = resolver
        .analyze(
            &Label::parse_canonical("//:hellolib_import_lib").unwrap(),
            &cfg,
        )
        .unwrap();
    let fg_paths: Vec<String> = import_fg
        .default_info
        .files
        .iter()
        .map(|f| f.root_relative_path.path_string().to_string())
        .collect();
    assert_eq!(fg_paths, vec!["hellolib.dll.if.lib"]);

    // 2. Analyze client cc_binary //:hello linking against windows_dll_library //:hellolib.
    let hello_ct = resolver
        .analyze(&Label::parse_canonical("//:hello").unwrap(), &cfg)
        .unwrap();
    assert_eq!(
        hello_ct
            .default_info
            .executable
            .as_ref()
            .unwrap()
            .root_relative_path
            .path_string(),
        "hello.exe"
    );
    let hello_link = hello_ct
        .typed_actions
        .iter()
        .find(|a| a.mnemonic == "CppLink")
        .expect("expected CppLink action on //:hello");
    let (hello_argv, _) = hello_link.command_line();
    assert_eq!(hello_argv[0], "C:/MSVC/bin/link.exe");
    let hello_inputs: Vec<String> = hello_link
        .inputs
        .iter()
        .map(|i| i.root_relative_path.path_string().to_string())
        .collect();
    assert!(
        hello_inputs
            .iter()
            .any(|p| p.ends_with("hellolib.dll.if.lib")),
        "expected hellolib.dll.if.lib in //:hello CppLink inputs: {hello_inputs:?}"
    );

    // 3. Analyze //:raw_builtin exercising predeclared builtin cc_common.link & create_library_to_link
    //    directly with @rules_cc's Windows CcToolchainInfo.
    let builtin_ct = resolver
        .analyze(&Label::parse_canonical("//:raw_builtin").unwrap(), &cfg)
        .unwrap();
    let Value::ProviderInstance { fields, .. } =
        &builtin_ct.providers["//:windows_dll_library.bzl%BuiltinLinkInfo"]
    else {
        panic!("expected BuiltinLinkInfo");
    };
    assert_eq!(fields["dll_short"].as_str(), Some("raw_builtin.dll"));
    assert_eq!(fields["iface_short"].as_str(), Some("raw_builtin.if.lib"));
    assert_eq!(
        fields["wrapped_iface_short"].as_str(),
        Some("raw_builtin.if.lib")
    );

    let _ = std::fs::remove_dir_all(&tmp_base);
}

#[test]
fn param_file_formats_flag_per_line_groups_and_tree_inputs() {
    use zel_analysis::action::{
        gcc_escape, render_param_file, windows_param_escape, ActionKind, ParamFileFormat,
    };

    assert_eq!(gcc_escape(""), "''");
    assert_eq!(gcc_escape("quote\".cc"), "quote\\\".cc");
    assert_eq!(gcc_escape("space .cc"), "space\\ .cc");
    assert_eq!(windows_param_escape(""), "\"\"");
    assert_eq!(windows_param_escape("plain"), "plain");
    assert_eq!(windows_param_escape("foo bar"), "\"foo bar\"");
    assert_eq!(windows_param_escape("a\\\"b"), "a\\\\\\\"b");
    assert_eq!(
        render_param_file(&["foo bar".into(), "".into()], ParamFileFormat::GccQuoted),
        "foo\\ bar\n''\n"
    );
    assert_eq!(
        render_param_file(&["foo bar".into(), "".into()], ParamFileFormat::Windows),
        "\"foo bar\"\n\"\"\n"
    );

    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    write(
        &fs,
        &root,
        "defs.bzl",
        r##"
def _impl(ctx):
    tree = ctx.actions.declare_directory("my_tree")
    ctx.actions.run_shell(outputs = [tree], command = "mkdir -p " + tree.path)

    written = ctx.actions.declare_file("written.params")
    wargs = ctx.actions.args()
    wargs.set_param_file_format("flag_per_line")
    wargs.add_joined("--a", ["b", "c"], join_with = ",")
    wargs.add("--nod")
    wargs.add("pos1")
    wargs.add("pos2")
    wargs.add_all(["grp1", "grp2"])
    wargs.add_all("--foo", ["bar1", "bar2"])
    wargs.add_all("", ["--raw", "x", "y"])
    wargs.add_all([tree])
    ctx.actions.write(written, wargs)

    spilled_out = ctx.actions.declare_file("spilled.out")
    tool = ctx.actions.declare_file("tool.sh")
    ctx.actions.write(tool, "#!/bin/sh\n", is_executable = True)
    sargs = ctx.actions.args()
    sargs.set_param_file_format("flag_per_line")
    sargs.use_param_file("%s", use_always = True)
    sargs.add("--foo", "bar")
    sargs.add("pos1")
    sargs.add_joined("--joined", ["a", "b", "c"], join_with = ",")
    sargs.add("pos2")
    ctx.actions.run(executable = tool, outputs = [spilled_out], arguments = [sargs])
    return [DefaultInfo(files = depset([written, spilled_out]))]

my_rule = rule(implementation = _impl)
"##,
    );
    write(
        &fs,
        &root,
        "BUILD",
        "load(':defs.bzl', 'my_rule')\nmy_rule(name = 't')\n",
    );

    let r = resolver_for(fs, root);
    let ct = r
        .analyze(
            &Label::parse_canonical("//:t").unwrap(),
            &BuildConfigurationKey::target("k8", "fastbuild"),
        )
        .unwrap();

    let write_act = ct
        .typed_actions
        .iter()
        .find(|a| {
            a.outputs
                .iter()
                .any(|o| o.exec_path.path_string().ends_with("/written.params"))
        })
        .expect("write action");
    assert!(
        write_act.inputs.iter().any(|i| i.is_tree_artifact),
        "FileWrite with Args must retain tree-artifact inputs"
    );
    if let ActionKind::FileWrite { content, .. } = &write_act.kind {
        let body = String::from_utf8(content.render()).unwrap();
        assert!(
            body.contains("--a=b,c\n--nod\npos1\npos2\ngrp1=grp2\n--foo=bar1 bar2\n--raw x y\n")
        );
    } else {
        panic!("expected FileWrite");
    }

    let spawn_act = ct
        .typed_actions
        .iter()
        .find(|a| {
            a.outputs
                .iter()
                .any(|o| o.exec_path.path_string().ends_with("/spilled.out"))
        })
        .expect("spawn action");
    let (argv, params) = spawn_act.command_line();
    assert_eq!(params.len(), 1);
    assert_eq!(
        String::from_utf8(params[0].1.clone()).unwrap(),
        "--foo=bar\n--joined=a,b,c\n"
    );
    assert!(argv[1].ends_with(".params"));
    assert_eq!(&argv[2..], &["pos1", "pos2"]);
}

#[test]
fn config_feature_flag_platform_flags_flag_alias_transition_and_fragments() {
    use zel_analysis::build_options::{
        convert_raws, find_option, match_per_label_options, ConfigValue,
    };
    use zel_analysis::NativeSetting;

    // `--flag_alias` List<Map.Entry<String,Label>> conversion.
    let (_, fa_opt) = find_option("flag_alias").unwrap();
    let converted = convert_raws(
        fa_opt,
        &["my_alias=//pkg:flag_a".into(), "other=//pkg:flag_b".into()],
        &|s| Ok(format!("@@{s}")),
        "k8",
    )
    .unwrap();
    assert_eq!(
        converted,
        ConfigValue::List(vec![
            "my_alias=@@//pkg:flag_a".into(),
            "other=@@//pkg:flag_b".into()
        ])
    );

    // `--per_file_copt` RegexFilter + Options matching.
    let (_, pfc_opt) = find_option("per_file_copt").unwrap();
    let ConfigValue::List(pfc_entries) = convert_raws(
        pfc_opt,
        &["foo/.*\\.cc,-foo/skip\\.cc@-O3,-DFAST".into()],
        &|s| Ok(s.to_string()),
        "k8",
    )
    .unwrap() else {
        panic!("expected list");
    };
    assert_eq!(
        match_per_label_options(
            &pfc_entries,
            Some(&Label::parse_canonical("//foo:lib").unwrap()),
            "foo/keep.cc"
        ),
        vec!["-O3", "-DFAST"]
    );
    assert!(match_per_label_options(
        &pfc_entries,
        Some(&Label::parse_canonical("//foo:lib").unwrap()),
        "foo/skip.cc"
    )
    .is_empty());

    // `to_exec_for` propagates `tool_java_{language,runtime}_version` -> `java_{language,runtime}_version`.
    let mut base_cfg = BuildConfigurationKey::target("k8", "fastbuild");
    base_cfg
        .options
        .insert("tool_java_language_version".into(), "21".into());
    base_cfg
        .options
        .insert("tool_java_runtime_version".into(), "remotejdk_21".into());
    base_cfg.native.insert(
        "java_launcher".into(),
        NativeSetting {
            raw: "@@//tools:custom_launcher".into(),
            value: ConfigValue::Scalar("@@//tools:custom_launcher".into()),
        },
    );
    base_cfg.native.insert(
        "ios_sdk_version".into(),
        NativeSetting {
            raw: "17.2".into(),
            value: ConfigValue::Scalar("17.2".into()),
        },
    );
    let exec_cfg = base_cfg.to_exec_for("@@platforms//host:host");
    assert_eq!(
        exec_cfg
            .options
            .get("java_language_version")
            .map(String::as_str),
        Some("21")
    );
    assert_eq!(
        exec_cfg
            .options
            .get("java_runtime_version")
            .map(String::as_str),
        Some("remotejdk_21")
    );
    assert_eq!(
        zel_analysis::fragments::late_bound_label("java", "launcher", &base_cfg),
        Some(Label::parse_canonical("@@//tools:custom_launcher").unwrap())
    );
    let Value::Struct(apple_frag) = zel_analysis::fragments::fragment("apple", &base_cfg)
        .unwrap()
        .unwrap()
    else {
        panic!("expected apple struct");
    };
    assert_eq!(
        apple_frag
            .fields
            .get("ios_sdk_version_flag")
            .map(|v| v.to_string())
            .as_deref(),
        Some("17.2")
    );

    // `config_feature_flag`, `PlatformInfo.flags`, `--flag_alias` transition, and `ctx.build_file_path` guard.
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
        "//command_line_option:aliased_opt": "transitioned " + settings["//command_line_option:aliased_opt"],
        "//:ff": "configured",
        "//:ls": ":other_target",
    }
tr = transition(
    implementation = _tr_impl,
    inputs = ["//command_line_option:aliased_opt"],
    outputs = ["//command_line_option:aliased_opt", "//:ff", "//:ls"],
)

Info = provider(fields = ["flag_val", "ff_val", "ls_files"])
def _leaf_impl(ctx):
    return [Info(
        flag_val = ctx.attr._f[BSI].value,
        ff_val = ctx.attr.ff[config_common.FeatureFlagInfo].value,
        ls_files = [f.short_path for f in ctx.attr._ls[DefaultInfo].files.to_list()],
    )]
leaf = rule(
    implementation = _leaf_impl,
    attrs = {
        "_f": attr.label(default = "//:foo_starlark"),
        "ff": attr.label(),
        "_ls": attr.label(default = "//:ls"),
    },
)

def _top_impl(ctx):
    return [ctx.attr.dep[Info]]
top = rule(implementation = _top_impl, attrs = {"dep": attr.label(cfg = tr)})

def _bfp_impl(ctx):
    _ = ctx.build_file_path
    return []
bfp_rule = rule(implementation = _bfp_impl)
"#,
    );
    write(&fs, &root, "other.txt", "ok");
    write(
        &fs,
        &root,
        "BUILD",
        r#"
load(":defs.bzl", "str_flag", "leaf", "top", "bfp_rule")
str_flag(name = "foo_starlark", build_setting_default = "default_foo")
config_feature_flag(name = "ff", allowed_values = ["default", "configured"], default_value = "default")
config_feature_flag(name = "ff_no_default", allowed_values = ["only"])
filegroup(name = "init_target", srcs = [])
filegroup(name = "other_target", srcs = ["other.txt"])
label_setting(name = "ls", build_setting_default = ":init_target")
leaf(name = "l", ff = ":ff")
top(name = "t", dep = ":l")
platform(name = "p_parent", flags = ["--cpu=arm"])
platform(name = "p_child", parents = [":p_parent"], flags = ["--cpu=k8"])
bfp_rule(name = "bfp")
"#,
    );

    let r = resolver_for(fs, root);
    let mut cfg = BuildConfigurationKey::target("k8", "fastbuild");
    cfg.native.insert(
        "flag_alias".into(),
        NativeSetting {
            raw: String::new(),
            value: ConfigValue::List(vec!["aliased_opt=@@//:foo_starlark".into()]),
        },
    );

    let ct = r
        .analyze(&Label::parse_canonical("//:t").unwrap(), &cfg)
        .unwrap();
    let Value::ProviderInstance { fields, .. } = &ct.providers["//:defs.bzl%Info"] else {
        panic!("expected Info");
    };
    assert_eq!(
        fields["flag_val"].as_str(),
        Some("transitioned default_foo")
    );
    assert_eq!(fields["ff_val"].as_str(), Some("configured"));
    assert_eq!(fields["ls_files"].repr(), "[\"other.txt\"]");

    let ff_def = r
        .analyze(&Label::parse_canonical("//:ff").unwrap(), &cfg)
        .unwrap();
    let Value::ProviderInstance {
        fields: ff_fields, ..
    } = &ff_def.providers["FeatureFlagInfo"]
    else {
        panic!("expected FeatureFlagInfo");
    };
    assert_eq!(ff_fields["value"].as_str(), Some("default"));

    let no_def_err = r
        .analyze(&Label::parse_canonical("//:ff_no_default").unwrap(), &cfg)
        .unwrap_err();
    assert!(
        no_def_err.contains("without a value present"),
        "got: {no_def_err}"
    );

    let p_child = r
        .analyze(&Label::parse_canonical("//:p_child").unwrap(), &cfg)
        .unwrap();
    let Value::ProviderInstance { fields: pf, .. } = &p_child.providers["PlatformInfo"] else {
        panic!("expected PlatformInfo");
    };
    assert_eq!(pf["flags"].repr(), "[\"--cpu=arm\", \"--cpu=k8\"]");

    let bfp_err = r
        .analyze(&Label::parse_canonical("//:bfp").unwrap(), &cfg)
        .unwrap_err();
    assert!(
        bfp_err.contains("Use ctx.label.package + '/BUILD' instead of ctx.build_file_path."),
        "got: {bfp_err}"
    );
}

#[test]
fn package_spec_contains_tags_propagation_optional_toolchains_and_prereq_checks() {
    use zel_analysis::build_options::ConfigValue;
    use zel_analysis::{ActionKind, FileContent, NativeSetting};

    zel_rules::set_genquery_hook(Box::new(|_loader, _expr, _scope, _strict, _opts| {
        Ok(b"//a:one\n//a:two\n".to_vec())
    }));

    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    write(&fs, &root, "a/src.cc", "int x;");
    write(
        &fs,
        &root,
        "rule/defs.bzl",
        r##"
Info = provider(fields = ["in_group", "opt_tc", "id_cfg"])

def _noconf_impl(ctx):
    return [Info(in_group = False, opt_tc = "", id_cfg = ctx.bin_dir.path)]
noconf_rule = rule(implementation = _noconf_impl, cfg = config.none())

def _check_impl(ctx):
    pg = ctx.attr.pg[PackageSpecificationInfo]
    out = ctx.actions.declare_file(ctx.label.name + ".out")
    tool = ctx.actions.declare_file(ctx.label.name + ".tool")
    ctx.actions.write(tool, "#!/bin/sh\n", is_executable = True)
    ctx.actions.run(executable = tool, outputs = [out], mnemonic = "CustomSpawn")
    tc_info = ctx.toolchains["//tc:opt_type"]
    return [
        DefaultInfo(files = depset([out])),
        Info(
            in_group = pg.contains(Label("//sub/pkg:anything")) and not pg.contains(Label("//other:x")),
            opt_tc = tc_info.tag if tc_info else "missing",
            id_cfg = ctx.attr.nc[Info].id_cfg,
        ),
    ]

check_rule = rule(
    implementation = _check_impl,
    attrs = {
        "pg": attr.label(providers = [PackageSpecificationInfo]),
        "nc": attr.label(),
        "_priv_tool": attr.label(default = "//priv:secret", allow_single_file = True),
    },
    toolchains = [config_common.toolchain_type("//tc:opt_type", mandatory = False)],
)

def _no_files_impl(ctx):
    return []
no_files_rule = rule(
    implementation = _no_files_impl,
    attrs = {"deps": attr.label_list(allow_files = False)},
)
"##,
    );
    write(&fs, &root, "rule/BUILD", "exports_files(['defs.bzl'])\n");
    write(
        &fs,
        &root,
        "priv/BUILD",
        "exports_files(['secret'], visibility = ['//a:__pkg__'])\n",
    );
    write(&fs, &root, "priv/secret", "shh");
    write(
        &fs,
        &root,
        "tc/BUILD",
        r#"
package(default_visibility = ["//visibility:public"])
constraint_setting(name = "slot")
constraint_value(name = "alpha_val", constraint_setting = ":slot")
constraint_value(name = "beta_val", constraint_setting = ":slot")
platform(name = "plat1", constraint_values = [":alpha_val"])
platform(name = "plat2", constraint_values = [":beta_val"])
toolchain_type(name = "opt_type")
toolchain_type(name = "private_type", visibility = ["//visibility:private"])
"#,
    );
    write(
        &fs,
        &root,
        "tc/tc.bzl",
        r#"
def _ti(ctx):
    return [platform_common.ToolchainInfo(tag = ctx.attr.tag)]
tc_impl = rule(implementation = _ti, attrs = {"tag": attr.string()})
"#,
    );
    write(
        &fs,
        &root,
        "tc_reg/BUILD",
        r#"
load("//tc:tc.bzl", "tc_impl")
tc_impl(name = "impl_beta", tag = "from_plat2")
toolchain(
    name = "tc_beta",
    toolchain_type = "//tc:opt_type",
    exec_compatible_with = ["//tc:beta_val"],
    toolchain = ":impl_beta",
)
tc_impl(name = "impl_testonly", tag = "to", testonly = True)
toolchain(
    name = "tc_testonly",
    toolchain_type = "//tc:opt_type",
    toolchain = ":impl_testonly",
)
toolchain(
    name = "tc_invis_type",
    toolchain_type = "//tc:private_type",
    toolchain = ":impl_beta",
)
"#,
    );
    write(
        &fs,
        &root,
        "a/BUILD",
        r#"
load("//rule:defs.bzl", "noconf_rule", "check_rule", "no_files_rule")
package_group(name = "leaf_pg", packages = ["//sub/..."])
package_group(name = "root_pg", includes = [":leaf_pg"])
noconf_rule(name = "nc")
check_rule(name = "chk", pg = ":root_pg", nc = ":nc", tags = ["no-remote", "local"])
filegroup(name = "empty_og", srcs = [":nc"], output_group = "nonexistent_group")
genrule(name = "stamped", outs = ["stamped.txt"], cmd = "echo ok > $@", stamp = 1)
genquery(name = "gq", expression = "//a:all", scope = [":nc"], compressed_output = True)
no_files_rule(name = "bad_src", deps = [":src.cc"])
"#,
    );

    let lc = LoaderConfig::new(
        fs as Arc<dyn FileSystem>,
        Arc::new(SimpleRepoResolver::new(root)),
    );
    let loader = PackageLoader::new(lc);
    let mut acfg = AnalysisConfig::new();
    acfg.execution_platform_patterns = vec!["//tc:plat1".into(), "//tc:plat2".into()];
    acfg.toolchain_patterns = vec!["//tc_reg:tc_beta".into()];
    let r = TargetResolver::new(loader.clone(), acfg);

    let mut cfg1 = BuildConfigurationKey::target("k8", "fastbuild");
    cfg1.native.insert(
        "modify_execution_info".into(),
        NativeSetting {
            raw: String::new(),
            value: ConfigValue::List(vec![
                "ExecutionInfoModifier{option=CustomSpawn=+custom-req, expressions=[]}".into(),
            ]),
        },
    );
    let mut cfg2 = cfg1.clone();
    cfg2.options.insert("features".into(), "extra_feat".into());

    let ct1 = r
        .analyze(&Label::parse_canonical("//a:chk").unwrap(), &cfg1)
        .unwrap();
    let ct2 = r
        .analyze(&Label::parse_canonical("//a:chk").unwrap(), &cfg2)
        .unwrap();
    let Value::ProviderInstance { fields: f1, .. } = &ct1.providers["//rule:defs.bzl%Info"] else {
        panic!("expected Info");
    };
    let Value::ProviderInstance { fields: f2, .. } = &ct2.providers["//rule:defs.bzl%Info"] else {
        panic!("expected Info");
    };
    assert_eq!(f1["in_group"], Value::Bool(true));
    assert_eq!(f1["opt_tc"].as_str(), Some("from_plat2"));
    assert_eq!(f1["id_cfg"], f2["id_cfg"]);

    let spawn = ct1
        .typed_actions
        .iter()
        .find(|a| a.mnemonic == "CustomSpawn")
        .expect("CustomSpawn");
    assert!(spawn.execution_requirements.contains_key("no-remote"));
    assert!(spawn.execution_requirements.contains_key("local"));
    assert!(spawn.execution_requirements.contains_key("custom-req"));

    // Missing output_group on filegroup contributes nothing instead of failing.
    let fg = r
        .analyze(&Label::parse_canonical("//a:empty_og").unwrap(), &cfg1)
        .unwrap();
    assert!(fg.default_info.files.is_empty());

    // Stamped genrule stages stable-status.txt and volatile-status.txt.
    let gr = r
        .analyze(&Label::parse_canonical("//a:stamped").unwrap(), &cfg1)
        .unwrap();
    let gr_inputs: Vec<&str> = gr.typed_actions[0]
        .inputs
        .iter()
        .map(|i| i.exec_path.path_string())
        .collect();
    assert!(gr_inputs.contains(&"bazel-out/stable-status.txt"));
    assert!(gr_inputs.contains(&"bazel-out/volatile-status.txt"));

    // Compressed genquery writes gzip bytes.
    let gq = r
        .analyze(&Label::parse_canonical("//a:gq").unwrap(), &cfg1)
        .unwrap();
    if let ActionKind::FileWrite {
        content: FileContent::Bytes(gz),
        ..
    } = &gq.typed_actions[0].kind
    {
        assert_eq!(&gz[..2], &[0x1f, 0x8b]);
        let decompressed = zel_util::deflate::gunzip(gz).unwrap();
        assert_eq!(decompressed, b"//a:one\n//a:two\n");
    } else {
        panic!("expected FileWrite Bytes");
    }

    // Disallowed source file in `allow_files = False` fails with `is misplaced here`.
    let bad_err = r
        .analyze(&Label::parse_canonical("//a:bad_src").unwrap(), &cfg1)
        .unwrap_err();
    assert!(
        bad_err.contains("source file") && bad_err.contains("is misplaced here"),
        "got: {bad_err}"
    );

    // Non-test rule resolving a `testonly = True` toolchain fails.
    let mut acfg_to = AnalysisConfig::new();
    acfg_to.toolchain_patterns = vec!["//tc_reg:tc_testonly".into()];
    let r_to = TargetResolver::new(loader.clone(), acfg_to);
    let to_err = r_to
        .analyze(&Label::parse_canonical("//a:chk").unwrap(), &cfg1)
        .unwrap_err();
    assert!(
        to_err.contains("depends on testonly") && to_err.contains("impl_testonly"),
        "got: {to_err}"
    );

    // `toolchain` checks `toolchain_type` visibility.
    let invis_err = r
        .analyze(
            &Label::parse_canonical("//tc_reg:tc_invis_type").unwrap(),
            &cfg1,
        )
        .unwrap_err();
    assert!(
        invis_err.contains("Visibility error") && invis_err.contains("//tc:private_type"),
        "got: {invis_err}"
    );
}

#[test]
fn filegroup_missing_output_group_and_genrule_escapes_script_spill_and_stamp() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    write(&fs, &root, "a.txt", "a");
    write(&fs, &root, "b.txt", "b");
    let big_echo = "x".repeat(70_000);
    write(
        &fs,
        &root,
        "BUILD",
        &format!(
            r#"
filegroup(name = "base", srcs = ["a.txt"])
filegroup(name = "missing_og", srcs = [":base"], output_group = "nonexistent_group")

genrule(
    name = "escaped_dollar",
    srcs = ["a.txt", "b.txt"],
    outs = ["esc.out"],
    cmd = "echo $$(<) $$< > $@",
    stamp = 1,
    tags = ["requires-darwin", "block-network", "cpu:4", "disable-remote"],
)

genrule(
    name = "spilled_script",
    outs = ["spilled.out"],
    cmd = "echo {big_echo} > $@",
)
"#
        ),
    );
    let r = resolver_for(fs, root);
    let cfg = BuildConfigurationKey::target("k8", "fastbuild");

    // Missing output_group on filegroup yields empty files rather than failing analysis.
    let fg = r
        .analyze(&Label::parse_canonical("//:missing_og").unwrap(), &cfg)
        .unwrap();
    assert!(fg.default_info.files.is_empty());

    // Escaped `$$(<)` and `$$<` with 2 srcs succeeds, stamps status files, and uses /bin/bash for requires-darwin.
    let esc = r
        .analyze(&Label::parse_canonical("//:escaped_dollar").unwrap(), &cfg)
        .unwrap();
    assert_eq!(esc.typed_actions.len(), 1);
    let act = &esc.typed_actions[0];
    assert!(act
        .inputs
        .iter()
        .any(|i| i.exec_path.path_string() == "bazel-out/stable-status.txt"));
    assert!(act
        .inputs
        .iter()
        .any(|i| i.exec_path.path_string() == "bazel-out/volatile-status.txt"));
    assert_eq!(
        act.execution_requirements.get("cpu").map(String::as_str),
        Some("4")
    );
    assert!(act.execution_requirements.contains_key("block-network"));
    assert!(act.execution_requirements.contains_key("disable-remote"));
    let zel_analysis::ActionKind::Spawn { executable, .. } = &act.kind else {
        panic!("expected Spawn");
    };
    assert_eq!(executable, "/bin/bash");

    // Commands > 64KB spill to `.genrule_script.sh` via a FileWrite action.
    let spilled = r
        .analyze(&Label::parse_canonical("//:spilled_script").unwrap(), &cfg)
        .unwrap();
    assert_eq!(spilled.typed_actions.len(), 2);
    assert_eq!(spilled.typed_actions[0].mnemonic, "FileWrite");
    assert!(spilled.typed_actions[0].outputs[0]
        .exec_path
        .path_string()
        .ends_with("spilled_script.genrule_script.sh"));
}

#[test]
fn analysis_test_in_build_thread_and_test_suite_semantics() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    write(
        &fs,
        &root,
        "defs.bzl",
        r##"
def check_impl(ctx):
    return [AnalysisTestResultInfo(success = True, message = "ok")]

def _tok_impl(ctx):
    exe = ctx.actions.declare_file(ctx.label.name + ".sh")
    ctx.actions.write(exe, "#!/bin/sh\nexit 0\n", is_executable = True)
    rf = ctx.runfiles(files = [exe])
    return [
        DefaultInfo(executable = exe, runfiles = rf),
        testing.ExecutionInfo({"requires-network": ""}, exec_group = "custom_grp"),
        testing.TestEnvironment({"MY_VAR": "1"}, inherited_environment = ["HOME"]),
    ]

custom_exec_test = rule(
    implementation = _tok_impl,
    test = True,
    exec_groups = {"custom_grp": exec_group()},
)
"##,
    );
    write(
        &fs,
        &root,
        "BUILD",
        r#"
package(default_testonly = False)
load(":defs.bzl", "check_impl", "custom_exec_test")

testing.analysis_test(
    name = "inline_check",
    implementation = check_impl,
)

custom_exec_test(
    name = "t_fast",
    tags = ["fast"],
)

custom_exec_test(
    name = "t_slow",
    tags = ["slow"],
)

alias(
    name = "alias_fast",
    actual = ":t_fast",
)

test_suite(
    name = "fast_suite",
    tests = [":alias_fast", ":t_slow"],
    tags = ["fast", "-slow", "manual"],
)

test_suite(
    name = "implicit_suite",
    tags = ["fast"],
)
"#,
    );
    let r = resolver_for(fs, root);
    let cfg = BuildConfigurationKey::target("k8", "fastbuild");

    let at = r
        .analyze(&Label::parse_canonical("//:inline_check").unwrap(), &cfg)
        .unwrap();
    assert!(at.is_test);
    assert_eq!(at.rule_class, "inline_check_test");

    let t_fast = r
        .analyze(&Label::parse_canonical("//:t_fast").unwrap(), &cfg)
        .unwrap();
    assert_eq!(t_fast.test_exec_group.as_deref(), Some("custom_grp"));
    assert_eq!(
        t_fast
            .run_environment
            .as_ref()
            .and_then(|re| re.environment.get("MY_VAR"))
            .map(String::as_str),
        Some("1")
    );

    let suite = r
        .analyze(&Label::parse_canonical("//:fast_suite").unwrap(), &cfg)
        .unwrap();
    // test_suite is testonly by default even when package(default_testonly = False), is_test = false,
    // and omits compilation_outputs.
    assert!(suite.testonly);
    assert!(!suite.is_test);
    assert!(!suite
        .output_groups
        .groups
        .contains_key("compilation_outputs"));
    let Value::ProviderInstance { fields, .. } = &suite.providers["TransitiveTestsInfo"] else {
        panic!("expected TransitiveTestsInfo");
    };
    let Value::List(tests_list) = &fields["tests"] else {
        panic!("expected tests list");
    };
    assert_eq!(
        tests_list.lock().unwrap().elements,
        vec![Value::Label(Label::parse_canonical("//:t_fast").unwrap())]
    );

    let imp_suite = r
        .analyze(&Label::parse_canonical("//:implicit_suite").unwrap(), &cfg)
        .unwrap();
    let Value::ProviderInstance {
        fields: imp_fields, ..
    } = &imp_suite.providers["TransitiveTestsInfo"]
    else {
        panic!("expected TransitiveTestsInfo");
    };
    let Value::List(imp_list) = &imp_fields["tests"] else {
        panic!("expected tests list");
    };
    assert_eq!(
        imp_list.lock().unwrap().elements,
        vec![Value::Label(Label::parse_canonical("//:t_fast").unwrap())]
>>>>>>> 1638190f (Align genrule, filegroup, Python builtins, testing, test_suite, and repo recorded inputs with Bazel 9.3.2)
    );
}

#[test]
fn unsplit_repeatable_flags_preserve_commas_across_starlark_transitions() {
    let fs = Arc::new(InMemoryFileSystem::default());
    let root = PathFragment::create("/ws");
    write(
        &fs,
        &root,
        "defs.bzl",
        r#"
FlagsInfo = provider(fields = ["copts", "javacopts"])

def _tr_impl(settings, attr):
    return {
        "//command_line_option:copt": settings["//command_line_option:copt"] + ["-Wl,-rpath,/opt", "-DFOO=a, b"],
        "//command_line_option:javacopt": ["-XepOpt:Foo=a,b", "-XDbar='1, 2'"],
    }

tr = transition(
    implementation = _tr_impl,
    inputs = ["//command_line_option:copt"],
    outputs = ["//command_line_option:copt", "//command_line_option:javacopt"],
)

def _leaf_impl(ctx):
    return [FlagsInfo(
        copts = list(ctx.fragments.cpp.copts),
        javacopts = list(ctx.fragments.java.default_javac_flags),
    )]

leaf = rule(
    implementation = _leaf_impl,
    cfg = tr,
    fragments = ["cpp", "java"],
)
"#,
    );
    write(
        &fs,
        &root,
        "BUILD",
        "load(':defs.bzl', 'leaf')\nleaf(name = 't')\n",
    );

    let lc = LoaderConfig::new(
        fs as Arc<dyn FileSystem>,
        Arc::new(SimpleRepoResolver::new(root)),
    );
    let r = TargetResolver::new(PackageLoader::new(lc), AnalysisConfig::new());

    let mut cli_cfg = BuildConfigurationKey::target("k8", "fastbuild");
    cli_cfg
        .options
        .insert("copt".into(), "-Wl,-rpath,/base".into());
    cli_cfg.native.insert(
        "copt".into(),
        zel_analysis::NativeSetting {
            raw: "-Wl,-rpath,/base".into(),
            value: zel_analysis::build_options::ConfigValue::List(vec!["-Wl,-rpath,/base".into()]),
        },
    );

    let ct = r
        .analyze(&Label::parse_canonical("//:t").unwrap(), &cli_cfg)
        .unwrap();
    let Value::ProviderInstance { fields, .. } = &ct.providers["//:defs.bzl%FlagsInfo"] else {
        panic!("expected FlagsInfo");
    };
    let Value::List(copts) = &fields["copts"] else {
        panic!("expected copts list");
    };
    assert_eq!(
        copts.lock().unwrap().elements,
        vec![
            Value::String("-Wl,-rpath,/base".into()),
            Value::String("-Wl,-rpath,/opt".into()),
            Value::String("-DFOO=a, b".into()),
        ]
    );
    let Value::List(javacopts) = &fields["javacopts"] else {
        panic!("expected javacopts list");
    };
    let jc: Vec<String> = javacopts
        .lock()
        .unwrap()
        .elements
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let tail = &jc[jc.len() - 2..];
    assert_eq!(
        tail,
        &["-XepOpt:Foo=a,b".to_string(), "-XDbar=1, 2".to_string()]
    );
}
