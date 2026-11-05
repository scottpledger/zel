//! Configuration fragments (`ctx.fragments.<name>`): Starlark views of Bazel's
//! `CppConfiguration`, `JavaConfiguration`, `AppleConfiguration`, `ObjcConfiguration`,
//! `ProtoConfiguration`, `PythonConfiguration` and `PlatformConfiguration`, with the option
//! defaults of `CppOptions`, `JavaOptions`, `AppleCommandLineOptions`, `ObjcCommandLineOptions`
//! and `ProtoConfiguration` (Bazel 9). Options come from `BuildConfigurationKey::options`
//! (bare option name → value; list options are comma-joined).

use crate::starlark_rule::{mk_list, mk_struct};
use crate::BuildConfigurationKey;
use zel_cmdline::Label;
use zel_starlark::{
    ClosureEnv, Depset, DepsetOrder, Expr, Location, ParamDef, Stmt, UserFunctionDef, Value,
};
use std::sync::Arc;

/// A method of a native fragment that returns a constant (`cpp.force_pic()`): a function
/// `(*args, **kwargs)` returning `v`.
fn m(name: &str, v: Value) -> (String, Value) {
    let loc = Location::new("<fragment>", 0, 0);
    let param = |n: &str, is_args: bool, is_kwargs: bool| ParamDef {
        name: n.to_string(),
        default: None,
        is_args,
        is_kwargs,
        is_kw_only: false,
    };
    let f = Value::UserFunction(Arc::new(UserFunctionDef {
        name: name.to_string(),
        params: vec![param("args", true, false), param("kwargs", false, true)],
        body: vec![Stmt::Return(Some(Expr::Literal(v)), loc.clone())],
        location: loc,
        module: None,
        closure: ClosureEnv::default(),
    }));
    (name.to_string(), f)
}

fn mk(fields: Vec<(String, Value)>) -> Value {
    Value::Struct(Arc::new(zel_starlark::StructValue::new(fields.into_iter().collect())))
}

fn f(name: &str, v: Value) -> (String, Value) {
    (name.to_string(), v)
}

struct Opts<'a>(&'a BuildConfigurationKey);

impl<'a> Opts<'a> {
    fn raw(&self, k: &str) -> Option<&String> {
        self.0.options.get(k)
    }
    fn s(&self, k: &str, d: &str) -> String {
        self.raw(k).cloned().unwrap_or_else(|| d.to_string())
    }
    fn opt_s(&self, k: &str) -> Value {
        match self.raw(k).filter(|v| !v.is_empty()) {
            Some(v) => Value::String(v.clone()),
            None => Value::None,
        }
    }
    fn b(&self, k: &str, d: bool) -> bool {
        match self.raw(k).map(|v| v.as_str()) {
            Some("1") | Some("true") | Some("yes") => true,
            Some("0") | Some("false") | Some("no") => false,
            _ => d,
        }
    }
    fn list(&self, k: &str) -> Vec<String> {
        self.raw(k).map(|v| v.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect()).unwrap_or_default()
    }
    fn list_v(&self, k: &str) -> Value {
        mk_list(self.list(k).into_iter().map(Value::String).collect())
    }
    fn label(&self, k: &str) -> Value {
        match self.raw(k).filter(|v| !v.is_empty()).and_then(|v| Label::parse_canonical(v).ok()) {
            Some(l) => Value::Label(l),
            None => Value::None,
        }
    }
}

fn sv(s: &str) -> Value {
    Value::String(s.to_string())
}

fn apple_platform(platform_type: &str, device: bool) -> Value {
    let (name, plist) = match (platform_type, device) {
        ("ios", true) => ("ios_device", "iPhoneOS"),
        ("ios", false) => ("ios_simulator", "iPhoneSimulator"),
        ("tvos", true) => ("tvos_device", "AppleTVOS"),
        ("tvos", false) => ("tvos_simulator", "AppleTVSimulator"),
        ("watchos", true) => ("watchos_device", "WatchOS"),
        ("watchos", false) => ("watchos_simulator", "WatchSimulator"),
        ("visionos", true) => ("visionos_device", "XROS"),
        ("visionos", false) => ("visionos_simulator", "XRSimulator"),
        ("catalyst", _) => ("catalyst", "MacOSX"),
        _ => ("macos", "MacOSX"),
    };
    let pt = if platform_type.is_empty() { "macos" } else { platform_type };
    mk_struct(vec![
        ("name", sv(name)),
        ("name_in_plist", sv(plist)),
        ("platform_type", sv(pt)),
        ("is_device", Value::Bool(device)),
    ])
}

/// The Starlark value of fragment `name` in `cfg`; `None` for unknown fragments.
pub fn fragment(name: &str, cfg: &BuildConfigurationKey) -> Option<Value> {
    let o = Opts(cfg);
    let mode = cfg.compilation_mode.as_str();
    Some(match name {
        "cpp" => {
            let strip = o.s("strip", "sometimes");
            let strip_binaries = strip == "always" || (strip == "sometimes" && mode == "fastbuild");
            let fission = o.list("fission");
            let fission_active = fission.iter().any(|m| m == mode || m == "yes") && !fission.iter().any(|m| m == "no");
            let omitfp = o.b("experimental_omitfp", false);
            let mut copts: Vec<Value> = Vec::new();
            if omitfp {
                copts.extend(["-fomit-frame-pointer", "-fasynchronous-unwind-tables", "-DNO_FRAME_POINTER"].map(sv));
            }
            copts.extend(o.list("copt").into_iter().map(Value::String));
            let fdo_path = Value::None;
            mk(vec![
                // Struct fields.
                f("copts", mk_list(copts)),
                f("cxxopts", o.list_v("cxxopt")),
                f("conlyopts", o.list_v("conlyopt")),
                f("objccopts", o.list_v("objccopt")),
                f("linkopts", o.list_v("linkopt")),
                f("lto_backend_options", o.list_v("ltobackendopt")),
                f("_dont_enable_host_nonhost", Value::Bool(o.b("incompatible_dont_enable_host_nonhost_crosstool_features", true))),
                f("custom_malloc", o.label("custom_malloc")),
                f("fdo_optimize", Value::None),
                f("fdo_prefetch_hints", o.label("fdo_prefetch_hints")),
                f("fdo_profile", o.label("fdo_profile")),
                f("cs_fdo_profile", o.label("cs_fdo_profile")),
                f("propeller_optimize", o.label("propeller_optimize")),
                f("xbinary_fdo", o.label("xbinary_fdo")),
                f("memprof_profile", o.label("memprof_profile")),
                f("libc_top", o.label("grte_top")),
                f("proto_profile_path", o.opt_s("proto_profile_path")),
                f("apple_generate_dsym", Value::Bool(o.b("apple_generate_dsym", false))),
                f("objc_generate_linkmap", Value::Bool(o.b("objc_generate_linkmap", false))),
                f("objc_should_strip_binary", Value::Bool(o.b("objc_enable_binary_stripping", false) && strip_binaries)),
                // Methods.
                m("compilation_mode", sv(mode)),
                m("lto_index_options", o.list_v("ltoindexopt")),
                m("minimum_os_version", o.opt_s("minimum_os_version")),
                m("dynamic_mode", sv(&o.s("dynamic_mode", "default").to_uppercase())),
                m("start_end_lib", Value::Bool(o.b("start_end_lib", true))),
                m("legacy_whole_archive", Value::Bool(o.b("legacy_whole_archive", true))),
                m("incompatible_remove_legacy_whole_archive", Value::Bool(o.b("incompatible_remove_legacy_whole_archive", true))),
                m("interface_shared_objects", Value::Bool(o.b("interface_shared_objects", true))),
                m("fdo_path", Value::None),
                m("cs_fdo_path", Value::None),
                m("fdo_instrument", Value::None),
                m("cs_fdo_instrument", Value::None),
                m("propeller_optimize_absolute_cc_profile", Value::None),
                m("propeller_optimize_absolute_ld_profile", Value::None),
                m("_fdo_prefetch_hints_label", o.label("fdo_prefetch_hints")),
                m("use_llvm_coverage_map_format", Value::Bool(o.b("experimental_use_llvm_covmap", false))),
                m("generate_llvm_lcov", Value::Bool(o.b("experimental_generate_llvm_lcov", false))),
                m("incompatible_use_specific_tool_files", Value::Bool(true)),
                m("include_scanning", Value::Bool(false)),
                m("should_generate_dotd_files", Value::Bool(o.b("cc_dotd_files", true))),
                m("objc_should_generate_dotd_files", Value::Bool(o.b("objc_use_dotd_pruning", true))),
                m("objc_enable_binary_stripping", Value::Bool(o.b("objc_enable_binary_stripping", false))),
                m("experimental_cc_implementation_deps", Value::Bool(true)),
                m("experimental_cpp_modules", Value::Bool(o.b("experimental_cpp_modules", false))),
                m("experimental_link_static_libraries_once", Value::Bool(o.b("experimental_link_static_libraries_once", true))),
                m("proto_profile", Value::Bool(o.b("proto_profile", true))),
                m("save_feature_state", Value::Bool(o.b("experimental_save_feature_state", false))),
                m("save_temps", Value::Bool(o.b("save_temps", false))),
                m("force_pic", Value::Bool(o.b("force_pic", false))),
                m("share_native_deps", Value::Bool(o.b("share_native_deps", true))),
                m("process_headers_in_dependencies", Value::Bool(o.b("process_headers_in_dependencies", false))),
                m("fission_active_for_current_compilation_mode", Value::Bool(fission_active)),
                m("build_test_dwp", Value::Bool(o.b("build_test_dwp", false))),
                m("should_strip_binaries", Value::Bool(strip_binaries)),
                m("strip_opts", o.list_v("stripopt")),
                m("grte_top", o.label("grte_top")),
                m("disable_nocopts", Value::Bool(true)),
            ])
        }
        "platform" => {
            let platform_label = |s: Option<&String>| match s.and_then(|p| Label::parse_canonical(p).ok()) {
                Some(l) => Value::Label(l),
                None => Value::Label(Label::parse_canonical("@platforms//host:host").unwrap()),
            };
            mk_struct(vec![
                ("platform", platform_label(cfg.platforms.first())),
                ("host_platform", platform_label(cfg.options.get("host_platform"))),
            ])
        }
        "java" => {
            let flags = o.list("javacopt");
            mk(vec![
                // Struct fields.
                f("default_javac_flags", mk_list(flags.iter().cloned().map(Value::String).collect())),
                f(
                    "default_javac_flags_depset",
                    Value::Depset(Arc::new(
                        Depset::new(DepsetOrder::Default, flags.into_iter().map(Value::String).collect(), vec![]).ok()?,
                    )),
                ),
                f("strict_java_deps", sv(&o.s("experimental_strict_java_deps", "default").to_lowercase())),
                f("one_version_enforcement_level", sv(&o.s("experimental_one_version_enforcement", "OFF").to_uppercase())),
                f("one_version_enforcement_on_java_tests", Value::Bool(o.b("one_version_enforcement_on_java_tests", true))),
                f("multi_release_deploy_jars", Value::Bool(true)),
                f("run_android_lint", Value::Bool(o.b("experimental_run_android_lint_on_java_rules", false))),
                f("default_jvm_opts", o.list_v("jvmopt")),
                f("plugins", o.list_v("plugin")),
                // Methods.
                m("use_ijars", Value::Bool(o.b("use_ijars", true))),
                m("use_header_compilation", Value::Bool(o.b("java_header_compilation", true))),
                m("use_header_compilation_direct_deps", Value::Bool(true)),
                m("reduce_java_classpath", sv(&o.s("experimental_java_classpath", "bazel").to_uppercase())),
                m("enforce_explicit_java_test_deps", Value::Bool(o.b("explicit_java_test_deps", false))),
                m("generate_java_deps", Value::Bool(o.b("java_deps", true))),
                m("disallow_java_import_exports", Value::Bool(o.b("incompatible_disallow_java_import_exports", false))),
                m("auto_create_java_test_deploy_jars", Value::Bool(false)),
            ])
        }
        // `BazelPythonConfiguration`.
        "bazel_py" => mk_struct(vec![
            ("python_import_all_repositories", Value::Bool(o.b("experimental_python_import_all_repositories", true))),
            ("python_path", sv(&{
                let p = o.s("python_path", "");
                if p.is_empty() { if cfg_is_windows() { "python".to_string() } else { "python3".to_string() } } else { p }
            })),
        ]),
        "py" => mk_struct(vec![
            ("default_python_version", sv("PY3")),
            ("build_python_zip", Value::Bool(o.b("build_python_zip", cfg_is_windows()))),
            ("default_to_explicit_init_py", Value::Bool(o.b("incompatible_default_to_explicit_init_py", false))),
        ]),
        "proto" => mk_struct(vec![
            ("experimental_protoc_opts", o.list_v("protocopt")),
            ("cc_proto_library_header_suffixes", mk_list(o.list("cc_proto_library_header_suffixes").into_iter().map(Value::String).chain(if o.raw("cc_proto_library_header_suffixes").is_none() { Some(sv(".pb.h")) } else { None }).collect())),
            ("cc_proto_library_source_suffixes", mk_list(o.list("cc_proto_library_source_suffixes").into_iter().map(Value::String).chain(if o.raw("cc_proto_library_source_suffixes").is_none() { Some(sv(".pb.cc")) } else { None }).collect())),
            ("strict_proto_deps", sv(&o.s("strict_proto_deps", "error"))),
            ("strict_public_imports", sv(&o.s("strict_public_imports", "off"))),
        ]),
        "objc" => mk(vec![
            f("copts_for_current_compilation_mode", mk_list(vec![])),
            f("strip_executable_safely", Value::Bool(o.b("incompatible_strip_executable_safely", false))),
            f("builtin_objc_strip_action", Value::Bool(o.b("incompatible_builtin_objc_strip_action", true))),
            f("alwayslink_by_default", Value::Bool(o.b("incompatible_objc_alwayslink_by_default", false))),
            f("disallow_sdk_frameworks_attributes", Value::Bool(o.b("incompatible_disallow_sdk_frameworks_attributes", false))),
            m("target_should_alwayslink", Value::Bool(o.b("incompatible_objc_alwayslink_by_default", false))),
        ]),
        "apple" => {
            let ptype = o.s("apple_platform_type", "macos");
            let device = !matches!(ptype.as_str(), "ios" | "tvos" | "watchos" | "visionos") || !cfg.cpu.starts_with("ios_sim");
            let min = |k: &str| o.opt_s(k);
            mk_struct(vec![
                ("single_arch_platform", apple_platform(&ptype, device)),
                ("single_arch_cpu", sv(cfg.cpu.trim_start_matches("darwin_"))),
                ("xcode_version_flag", o.opt_s("xcode_version")),
                ("ios_minimum_os_flag", min("ios_minimum_os")),
                ("watchos_minimum_os_flag", min("watchos_minimum_os")),
                ("tvos_minimum_os_flag", min("tvos_minimum_os")),
                ("macos_minimum_os_flag", min("macos_minimum_os")),
                ("prefer_mutual_xcode", Value::Bool(o.b("experimental_prefer_mutual_xcode", true))),
                ("include_xcode_exec_requirements", Value::Bool(o.b("experimental_include_xcode_execution_requirements", false))),
            ])
        }
        "coverage" => mk_struct(vec![(
            "collect_code_coverage",
            Value::Bool(cfg.options.get("collect_code_coverage").map(|v| v == "true" || v == "1").unwrap_or(false)),
        )]),
        _ => mk_struct(vec![]),
    })
}

fn cfg_is_windows() -> bool {
    cfg!(windows)
}

/// The label a `configuration_field(fragment, name)` default resolves to in `cfg`
/// (`@StarlarkConfigurationField` of the native fragments), `None` if the field is unset.
pub fn late_bound_label(fragment: &str, name: &str, cfg: &BuildConfigurationKey) -> Option<Label> {
    let tools = |p: &str| Label::parse_canonical(&format!("@@bazel_tools//{p}")).ok();
    let opt = |k: &str| cfg.options.get(k).filter(|v| !v.is_empty()).and_then(|v| Label::parse_canonical(v).ok());
    match (fragment, name) {
        ("apple", "xcode_config_label") => opt("xcode_version_config").or_else(|| tools("tools/cpp:host_xcodes")),
        ("proto", "proto_compiler") => opt("proto_compiler").or_else(|| tools("tools/proto:protoc")),
        ("proto", "proto_toolchain_for_java") => opt("proto_toolchain_for_java").or_else(|| tools("tools/proto:java_toolchain")),
        ("proto", "proto_toolchain_for_java_lite") | ("proto", "proto_toolchain_for_javalite") => {
            opt("proto_toolchain_for_javalite").or_else(|| tools("tools/proto:javalite_toolchain"))
        }
        ("proto", "proto_toolchain_for_cc") => opt("proto_toolchain_for_cc").or_else(|| tools("tools/proto:cc_toolchain")),
        ("cpp", "libc_top") => opt("grte_top"),
        ("cpp", "zipper") => {
            let fdo = ["fdo_optimize", "fdo_profile", "memprof_profile", "xbinary_fdo"].iter().any(|k| cfg.options.get(*k).is_some_and(|v| !v.is_empty()));
            if fdo { tools("tools/zip:unzip_fdo") } else { None }
        }
        ("coverage", "output_generator") => {
            let on = cfg.options.get("collect_code_coverage").is_some_and(|v| v == "true" || v == "1");
            if on { tools("tools/test:lcov_merger") } else { None }
        }
        (_, n) => opt(n),
    }
}
